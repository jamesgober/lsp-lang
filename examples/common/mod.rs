//! A small, complete language server shared by the examples, the integration
//! tests, and the benchmarks — and a template for wiring a real language.
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
//! diagnostics on every change, hover, and go-to-definition. [`Client`] and
//! [`messages`] script a session in memory and read the server's replies.

#![allow(dead_code)] // Each example uses a different subset.

use std::collections::{HashMap, HashSet};

use intern_lang::Interner;
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::incremental_lang::{Builder, Element, Grammar, Node, Span, Token, TokenKind, Tree};
use lsp_lang::{
    Context, Diagnostic, Document, Handler, Json, Position, Reply, ResponseError, Value, json,
};
use serde::Deserialize;
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
    // its own `)` — the check that lets incremental-lang reparse it alone.
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
        }
    }

    for token in root.tokens().filter(|t| *t.kind() == Kind::Atom) {
        let span = token.span();
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

/// The parameters of `textDocument/hover` and `textDocument/definition`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionParams<'a> {
    #[serde(borrow)]
    pub text_document: DocumentId<'a>,
    pub position: Position,
}

#[derive(Deserialize)]
pub struct DocumentId<'a> {
    pub uri: &'a str,
}

/// The Lisp language server: diagnostics, hover, and go-to-definition.
///
/// It keeps the latest analysis of each open document, so hover and
/// definition answer from it without reanalysing.
#[derive(Debug, Default)]
pub struct LispServer {
    analyses: HashMap<String, Analysis>,
    /// Number of analyses run, for the examples to report.
    pub analysed: usize,
}

impl Handler<Lisp> for LispServer {
    fn initialize(
        &mut self,
        _: Json<'_>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Value, ResponseError> {
        Ok(json!({ "hoverProvider": true, "definitionProvider": true }))
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

    fn request(
        &mut self,
        method: &str,
        params: Json<'_>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Reply, ResponseError> {
        match method {
            "textDocument/hover" => {
                let p: PositionParams<'_> = params.parse()?;
                let Some((doc, (span, meaning))) = self.lookup(cx, &p) else {
                    return Ok(Reply::null());
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
                Ok(Reply::new(&json!({
                    "contents": { "kind": "markdown", "value": contents },
                    "range": doc.range(span),
                }))?)
            }
            "textDocument/definition" => {
                let p: PositionParams<'_> = params.parse()?;
                match self.lookup(cx, &p) {
                    Some((doc, (_, Meaning::Defined(at)))) => Ok(Reply::new(
                        &json!({ "uri": doc.uri(), "range": doc.range(at) }),
                    )?),
                    _ => Ok(Reply::null()),
                }
            }
            _ => Err(ResponseError::new(
                ResponseError::METHOD_NOT_FOUND,
                format!("unhandled method `{method}`"),
            )),
        }
    }
}

impl LispServer {
    /// The open document and the atom at a request's position.
    fn lookup<'a>(
        &self,
        cx: &Context<'a, Lisp>,
        p: &PositionParams<'_>,
    ) -> Option<(&'a Document<Lisp>, (Span, Meaning))> {
        let doc = cx.document(p.text_document.uri)?;
        let atom = self
            .analyses
            .get(doc.uri())?
            .atom_at(doc.offset(p.position))?;
        Some((doc, atom))
    }
}

// ---------------------------------------------------------------------------
// Scripting a session
// ---------------------------------------------------------------------------

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
    pub fn send(&mut self, message: &Value) -> &mut Self {
        let body = message.to_string();
        self.bytes
            .extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        self.bytes.extend_from_slice(body.as_bytes());
        self
    }

    /// Appends one framed message with `body` exactly as given — for
    /// malformed JSON, which a `Value` cannot represent.
    pub fn send_raw(&mut self, body: &str) -> &mut Self {
        self.bytes.extend_from_slice(
            format!(
                "Content-Length: {}

",
                body.len()
            )
            .as_bytes(),
        );
        self.bytes.extend_from_slice(body.as_bytes());
        self
    }

    /// Appends a request with the next id.
    pub fn request(&mut self, method: &str, params: Value) -> &mut Self {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    /// Appends a notification.
    pub fn notify(&mut self, method: &str, params: Value) -> &mut Self {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// `initialize` (offering `encodings`), then `initialized`.
    pub fn initialize(&mut self, encodings: &[&str]) -> &mut Self {
        self.request(
            "initialize",
            json!({ "capabilities": { "general": { "positionEncodings": encodings } } }),
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

    pub fn at(&mut self, method: &str, uri: &str, line: u32, character: u32) -> &mut Self {
        self.request(
            method,
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
        )
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
pub fn messages(output: &[u8]) -> Vec<Value> {
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
        let body = &rest[split + 4..split + 4 + len];
        found.push(serde_json::from_slice(body).unwrap_or(Value::Null));
        rest = &rest[split + 4 + len..];
    }
    found
}
