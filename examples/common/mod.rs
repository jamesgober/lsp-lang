//! A small, complete language server shared by the examples, the integration
//! tests, and the benchmarks, and a template for wiring a real language.
//!
//! The language is a tiny Lisp:
//!
//! ```text
//! ; comments run to the end of the line
//! (define width 80)
//! (define area (* width height))   ; `height` is not defined: an error
//! ```
//!
//! [`Lisp`] is its grammar (every list reparses on its own). [`analyze`] finds
//! syntax and name errors, resolving names with `symbol-lang` and reporting
//! them with its ready-made diagnostics. [`LispServer`] is the handler:
//! diagnostics on every change, hover, go-to-definition, completion, an
//! outline, and semantic tokens. [`Client`] and [`messages`] script a session
//! in memory and read the server's replies.

#![allow(dead_code)] // Each example uses a different subset.

use std::collections::{HashMap, HashSet};

use intern_lang::Interner;
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::incremental_lang::{Builder, Element, Grammar, Node, Span, Token, TokenKind, Tree};
use lsp_lang::{
    CompletionItem, CompletionItemKind, Context, Diagnostic, Document, DocumentSymbol, Features,
    Handler, Hover, Location, Position, ResponseError, SemanticTokens, SemanticTokensBuilder,
    SemanticTokensLegend, SymbolKind, Value, json,
};
use symbol_lang::{SymbolTable, duplicate_definition, unresolved_name};

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

/// One kind type names both nodes (`Root`, `List`) and tokens (the rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Root,
    List,
    Open,
    Close,
    Atom,
    Space,
    Comment,
}

impl TokenKind for Kind {
    fn is_trivia(&self) -> bool {
        matches!(self, Kind::Space | Kind::Comment)
    }
}

/// The Lisp grammar. A unit struct, so the server's per-document clone is free.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lisp;

impl Grammar for Lisp {
    type Kind = Kind;
    const ROOT: Kind = Kind::Root;

    fn lex(&self, text: &str, tokens: &mut Vec<Token<Kind>>) {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let start = i;
            let kind = match bytes[i] {
                b'(' => {
                    i += 1;
                    Kind::Open
                }
                b')' => {
                    i += 1;
                    Kind::Close
                }
                b';' => {
                    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                        i += 1;
                    }
                    Kind::Comment
                }
                b if b.is_ascii_whitespace() => {
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    Kind::Space
                }
                _ => {
                    while i < bytes.len() && !is_delimiter(bytes[i]) {
                        i += 1;
                    }
                    Kind::Atom
                }
            };
            tokens.push(Token::new(kind, Span::new(start as u32, i as u32)));
        }
    }

    fn parse(&self, kind: Kind, b: &mut Builder<'_, Kind>) {
        if kind == Kind::List {
            return list(b);
        }
        b.start(Kind::Root);
        while !b.at_end() {
            if b.at(Kind::Open) { list(b) } else { b.bump() }
        }
        b.finish();
    }

    fn is_reparsable(&self, kind: Kind) -> bool {
        kind == Kind::List
    }
}

fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b';') || b.is_ascii_whitespace()
}

fn list(b: &mut Builder<'_, Kind>) {
    b.start(Kind::List);
    b.bump(); // `(`
    // Test for the closer before the end, so a balanced list never looks past
    // its own `)`: the check that lets incremental-lang reparse it alone.
    while !b.at(Kind::Close) && !b.at_end() {
        if b.at(Kind::Open) { list(b) } else { b.bump() }
    }
    b.eat(Kind::Close);
    b.finish();
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

/// Names every program can use without defining them.
pub const BUILTINS: &[&str] = &[
    "define", "+", "-", "*", "/", "=", "<", ">", "if", "print", "list",
];

/// What a name at some place in the text refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meaning {
    /// Defined by `(define name …)` at this span.
    Defined(Span),
    Builtin,
    Number,
    Unresolved,
}

/// The result of analysing one version of a document.
#[derive(Debug, Default)]
pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    /// Every atom, in text order, with what it means.
    pub atoms: Vec<(Span, Meaning)>,
    /// Every comment, in text order.
    pub comments: Vec<Span>,
    /// The names defined, in text order.
    pub definitions: Vec<(String, Span)>,
}

impl Analysis {
    /// The atom whose span contains `offset`, or ends at it (a cursor just
    /// after a name still points at it).
    pub fn atom_at(&self, offset: u32) -> Option<(Span, Meaning)> {
        let i = self
            .atoms
            .partition_point(|(span, _)| span.end().to_u32() < offset);
        self.atoms
            .get(i)
            .copied()
            .filter(|(span, _)| span.start().to_u32() <= offset)
    }
}

/// The significant (non-trivia) children of a node.
fn significant(node: Node<'_, Kind>) -> impl Iterator<Item = Element<'_, Kind>> {
    node.children().filter(|e| !e.kind().is_trivia())
}

fn text_of(text: &str, span: Span) -> &str {
    &text[span.start().to_usize()..span.end().to_usize()]
}

/// Finds syntax errors (unclosed lists, stray `)`) and name errors (undefined
/// and duplicate names), and classifies every atom.
pub fn analyze(text: &str, tree: &Tree<Kind>) -> Analysis {
    let mut analysis = Analysis::default();
    let root = tree.root();

    // Syntax. Every list must end with its own `)`; a `)` directly under the
    // root closes nothing.
    for element in significant(root) {
        if element.kind() == Kind::Close {
            analysis.diagnostics.push(Diagnostic::new(
                Severity::Error,
                "unexpected `)`",
                Label::new(element.span(), "there is no open list to close"),
            ));
        }
    }
    for list in root.descendants().filter(|n| n.kind() == Kind::List) {
        let closed = significant(list)
            .last()
            .is_some_and(|e| e.kind() == Kind::Close);
        if !closed {
            let open = Span::new(
                list.span().start().to_u32(),
                list.span().start().to_u32() + 1,
            );
            analysis.diagnostics.push(
                Diagnostic::new(
                    Severity::Error,
                    "unclosed list",
                    Label::new(open, "this `(` is never closed"),
                )
                .with_help("add a `)`"),
            );
        }
    }

    // Names. Definitions first, so a name can be used above its definition.
    let mut names = Interner::new();
    let mut table: SymbolTable<Span> = SymbolTable::new();
    let mut binding_sites = HashSet::new();
    for list in root.descendants().filter(|n| n.kind() == Kind::List) {
        let mut parts = significant(list).skip(1); // past `(`
        let (Some(head), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if head.kind() != Kind::Atom
            || text_of(text, head.span()) != "define"
            || name.kind() != Kind::Atom
        {
            continue;
        }
        let spelling = text_of(text, name.span());
        let symbol = names.intern(spelling);
        let _new = binding_sites.insert(name.span());
        if let Some(&first) = table.lookup_local(symbol) {
            analysis.diagnostics.push(
                duplicate_definition(spelling, name.span(), first)
                    .with_help("remove or rename one of them"),
            );
        } else {
            let _previous = table.define(symbol, name.span());
            analysis
                .definitions
                .push((spelling.to_owned(), name.span()));
        }
    }

    for token in root.tokens() {
        let span = token.span();
        if *token.kind() == Kind::Comment {
            analysis.comments.push(span);
            continue;
        }
        if *token.kind() != Kind::Atom {
            continue;
        }
        let spelling = text_of(text, span);
        let meaning = if spelling.parse::<f64>().is_ok() {
            Meaning::Number
        } else if binding_sites.contains(&span) {
            names
                .get(spelling)
                .and_then(|s| table.lookup(s))
                .map_or(Meaning::Unresolved, |&d| Meaning::Defined(d))
        } else if BUILTINS.contains(&spelling) {
            Meaning::Builtin
        } else if let Some(&def) = names.get(spelling).and_then(|s| table.lookup(s)) {
            Meaning::Defined(def)
        } else {
            analysis.diagnostics.push(unresolved_name(spelling, span));
            Meaning::Unresolved
        };
        analysis.atoms.push((span, meaning));
    }
    analysis
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// The semantic-token legend of the Lisp server.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend::new(
        &["keyword", "number", "variable", "comment"],
        &["declaration"],
    )
}

/// The Lisp language server: diagnostics, hover, go-to-definition,
/// completion, an outline, and semantic tokens.
///
/// It keeps the latest analysis of each open document, so every request
/// answers from it without reanalysing.
#[derive(Debug, Default)]
pub struct LispServer {
    analyses: HashMap<String, Analysis>,
    /// Number of analyses run, for the examples to report.
    pub analysed: usize,
}

impl Handler<Lisp> for LispServer {
    fn features(&self) -> Features {
        Features::new()
            .hover()
            .definition()
            .completion(&["(", " "])
            .document_symbols()
            .semantic_tokens(legend())
    }

    fn changed(&mut self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
        let analysis = analyze(doc.text(), doc.tree());
        cx.publish_diagnostics(doc, &analysis.diagnostics);
        let _previous = self.analyses.insert(doc.uri().to_owned(), analysis);
        self.analysed += 1;
    }

    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Lisp>) {
        let _forgotten = self.analyses.remove(uri);
        cx.clear_diagnostics(uri);
    }

    fn hover(
        &mut self,
        doc: &Document<Lisp>,
        at: Position,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Option<Hover>, ResponseError> {
        let Some((span, meaning)) = self.atom_at(doc, at) else {
            return Ok(None);
        };
        let name = text_of(doc.text(), span);
        let contents = match meaning {
            Meaning::Defined(at) => {
                format!(
                    "`{name}`: defined on line {}",
                    doc.position(at.start().to_u32()).line + 1
                )
            }
            Meaning::Builtin => format!("`{name}`: builtin"),
            Meaning::Number => format!("`{name}`: number"),
            Meaning::Unresolved => format!("`{name}`: not defined"),
        };
        Ok(Some(Hover::markdown(contents).with_range(doc.range(span))))
    }

    fn definition(
        &mut self,
        doc: &Document<Lisp>,
        at: Position,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Vec<Location>, ResponseError> {
        Ok(match self.atom_at(doc, at) {
            Some((_, Meaning::Defined(defined))) => {
                vec![Location::new(doc.uri(), doc.range(defined))]
            }
            _ => Vec::new(),
        })
    }

    fn completion(
        &mut self,
        doc: &Document<Lisp>,
        _: Position,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Vec<CompletionItem>, ResponseError> {
        let mut items: Vec<CompletionItem> = BUILTINS
            .iter()
            .map(|b| CompletionItem::new(*b).with_kind(CompletionItemKind::Keyword))
            .collect();
        if let Some(analysis) = self.analyses.get(doc.uri()) {
            items.extend(analysis.definitions.iter().map(|(name, at)| {
                CompletionItem::new(name.as_str())
                    .with_kind(CompletionItemKind::Variable)
                    .with_detail(format!(
                        "line {}",
                        doc.position(at.start().to_u32()).line + 1
                    ))
            }));
        }
        Ok(items)
    }

    fn document_symbols(
        &mut self,
        doc: &Document<Lisp>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Vec<DocumentSymbol>, ResponseError> {
        Ok(symbols(doc, doc.tree().root(), 0))
    }

    fn semantic_tokens(
        &mut self,
        doc: &Document<Lisp>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<SemanticTokens, ResponseError> {
        let mut tokens = SemanticTokensBuilder::new(doc);
        if let Some(analysis) = self.analyses.get(doc.uri()) {
            let legend = legend();
            let (keyword, number, variable, comment) = (
                legend.token_type("keyword").unwrap_or(0),
                legend.token_type("number").unwrap_or(0),
                legend.token_type("variable").unwrap_or(0),
                legend.token_type("comment").unwrap_or(0),
            );
            for &(span, meaning) in &analysis.atoms {
                let kind = match meaning {
                    Meaning::Builtin => keyword,
                    Meaning::Number => number,
                    Meaning::Defined(_) | Meaning::Unresolved => variable,
                };
                tokens.push(span, kind, 0);
            }
            for &span in &analysis.comments {
                tokens.push(span, comment, 0);
            }
        }
        Ok(tokens.build())
    }
}

impl LispServer {
    /// The atom at a request's position in `doc`.
    fn atom_at(&self, doc: &Document<Lisp>, at: Position) -> Option<(Span, Meaning)> {
        self.analyses.get(doc.uri())?.atom_at(doc.offset(at))
    }
}

/// The outline of a list: each `(define name …)` inside it, with the defines
/// nested in its value as children. Depth-limited, since the tree is the
/// client's text.
fn symbols(doc: &Document<Lisp>, node: Node<'_, Kind>, depth: usize) -> Vec<DocumentSymbol> {
    if depth > 32 {
        return Vec::new();
    }
    let mut found = Vec::new();
    for child in significant(node) {
        let Element::Node(list) = child else { continue };
        if list.kind() != Kind::List {
            continue;
        }
        let mut parts = significant(list).skip(1);
        let (Some(head), Some(name)) = (parts.next(), parts.next()) else {
            found.extend(symbols(doc, list, depth + 1));
            continue;
        };
        if head.kind() == Kind::Atom
            && text_of(doc.text(), head.span()) == "define"
            && name.kind() == Kind::Atom
        {
            let is_function = parts.next().is_some_and(|v| v.kind() == Kind::List);
            let kind = if is_function {
                SymbolKind::Function
            } else {
                SymbolKind::Variable
            };
            let mut symbol = DocumentSymbol::new(
                text_of(doc.text(), name.span()),
                kind,
                doc.range(list.span()),
                doc.range(name.span()),
            );
            symbol.children = symbols(doc, list, depth + 1);
            found.push(symbol);
        } else {
            found.extend(symbols(doc, list, depth + 1));
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Scripting a session
// ---------------------------------------------------------------------------

/// Frames `body` as the base protocol does.
pub fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Builds the byte stream an editor would send.
#[derive(Default)]
pub struct Client {
    bytes: Vec<u8>,
    next_id: u64,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one framed message.
    pub fn send(&mut self, message: &Value<'_>) -> &mut Self {
        self.bytes.extend_from_slice(&frame(&message.to_string()));
        self
    }

    /// Appends one framed message with `body` exactly as given, for malformed
    /// JSON, which a `Value` cannot represent.
    pub fn send_raw(&mut self, body: &str) -> &mut Self {
        self.bytes.extend_from_slice(&frame(body));
        self
    }

    /// Appends bytes exactly as given, framing and all.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// Appends a request with the next id.
    pub fn request(&mut self, method: &str, params: Value<'_>) -> &mut Self {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    /// The id the most recent request was sent with.
    pub fn last_id(&self) -> u64 {
        self.next_id
    }

    /// Appends a notification.
    pub fn notify(&mut self, method: &str, params: Value<'_>) -> &mut Self {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// `$/cancelRequest` for request `id`.
    pub fn cancel(&mut self, id: u64) -> &mut Self {
        self.notify("$/cancelRequest", json!({ "id": id }))
    }

    /// `initialize` (offering `encodings`), then `initialized`.
    pub fn initialize(&mut self, encodings: &[&str]) -> &mut Self {
        self.request(
            "initialize",
            json!({ "capabilities": { "general": { "positionEncodings": encodings.to_vec() } } }),
        )
        .notify("initialized", json!({}))
    }

    pub fn open(&mut self, uri: &str, text: &str) -> &mut Self {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "lisp", "version": 1, "text": text } }),
        )
    }

    /// One ranged edit: replace `(line, character)..(line, character)` with `text`.
    pub fn edit(
        &mut self,
        uri: &str,
        version: i32,
        start: (u32, u32),
        end: (u32, u32),
        text: &str,
    ) -> &mut Self {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": start.0, "character": start.1 },
                        "end": { "line": end.0, "character": end.1 },
                    },
                    "text": text,
                }],
            }),
        )
    }

    /// A request that takes a position, such as hover.
    pub fn at(&mut self, method: &str, uri: &str, line: u32, character: u32) -> &mut Self {
        self.request(
            method,
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        )
    }

    /// A request that takes only a document, such as `documentSymbol`.
    pub fn of(&mut self, method: &str, uri: &str) -> &mut Self {
        self.request(method, json!({ "textDocument": { "uri": uri } }))
    }

    /// `shutdown`, then `exit`.
    pub fn shutdown(&mut self) -> &mut Self {
        self.request("shutdown", Value::Null)
            .notify("exit", Value::Null)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Splits a server's output into its JSON messages.
pub fn messages(output: &[u8]) -> Vec<Value<'static>> {
    let mut found = Vec::new();
    let mut rest = output;
    while let Some(split) = rest.windows(4).position(|w| w == b"\r\n\r\n") {
        let header = String::from_utf8_lossy(&rest[..split]);
        let Some(len) = header
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|n| n.trim().parse::<usize>().ok())
        else {
            break;
        };
        let Some(body) = rest.get(split + 4..split + 4 + len) else {
            break;
        };
        // A batch response is an array of responses; read it as its members.
        match jsonx_lib_parse(body) {
            Value::Array(members) => found.extend(members.into_vec()),
            other => found.push(other),
        }
        rest = &rest[split + 4 + len..];
    }
    found
}

/// Parses one message body; `null` for anything that is not JSON.
fn jsonx_lib_parse(body: &[u8]) -> Value<'static> {
    lsp_lang::jsonx_lib::parse_bytes(body).map_or(Value::Null, Value::into_owned)
}
