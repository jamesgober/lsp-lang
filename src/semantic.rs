//! Semantic tokens: the [`SemanticTokensLegend`] a server declares, the
//! [`SemanticTokens`] it answers with, and a [`SemanticTokensBuilder`] that
//! turns byte spans from a syntax tree into the protocol's delta-encoded
//! integers.
//!
//! The protocol sends a document's highlighting as a flat list of integers,
//! five per token: the line (relative to the previous token), the column
//! (relative to the previous token when on the same line), the length, an index
//! into the legend's token types, and a bit mask of the legend's modifiers.
//! The builder does the encoding: it sorts the tokens, drops overlaps, converts
//! byte offsets to lines and columns in the session's encoding, and splits a
//! token that spans lines (a block comment) into one token per line, which
//! every client accepts.

use incremental_lang::{Grammar, Span};
use jsonx_lib::Value;

use crate::{
    document::Document,
    json::{to_value, write_str, write_u64},
};

/// The token types and modifiers a server uses, sent once in the
/// `initialize` result. Tokens refer to them by index.
///
/// A token's type is an index into [`token_types`](SemanticTokensLegend::token_types);
/// its modifiers are a bit mask, where bit `i` stands for entry `i` of
/// [`token_modifiers`](SemanticTokensLegend::token_modifiers). Clients map the
/// names to colors through the editor theme, so prefer the standard names
/// ([`standard`](SemanticTokensLegend::standard)) where they fit.
///
/// # Examples
///
/// ```
/// use lsp_lang::SemanticTokensLegend;
///
/// let legend = SemanticTokensLegend::new(&["keyword", "number"], &["declaration"]);
/// assert_eq!(legend.token_type("number"), Some(1));
/// assert_eq!(legend.modifier("declaration"), Some(0b1));
/// assert_eq!(legend.token_type("missing"), None);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SemanticTokensLegend {
    /// The token type names; a token's type is an index into this list.
    pub token_types: Vec<String>,
    /// The token modifier names; bit `i` of a token's modifiers is entry `i`.
    pub token_modifiers: Vec<String>,
}

impl SemanticTokensLegend {
    /// The standard token type names (`namespace` through `decorator`) and
    /// modifier names (`declaration` through `defaultLibrary`) of LSP 3.17, in
    /// the protocol's order.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokensLegend;
    ///
    /// let legend = SemanticTokensLegend::standard();
    /// assert_eq!(legend.token_type("keyword"), Some(15));
    /// assert_eq!(legend.modifier("readonly"), Some(1 << 2));
    /// ```
    #[must_use]
    pub fn standard() -> Self {
        Self::new(
            &[
                "namespace",
                "type",
                "class",
                "enum",
                "interface",
                "struct",
                "typeParameter",
                "parameter",
                "variable",
                "property",
                "enumMember",
                "event",
                "function",
                "method",
                "macro",
                "keyword",
                "modifier",
                "comment",
                "string",
                "number",
                "regexp",
                "operator",
                "decorator",
            ],
            &[
                "declaration",
                "definition",
                "readonly",
                "static",
                "deprecated",
                "abstract",
                "async",
                "modification",
                "documentation",
                "defaultLibrary",
            ],
        )
    }

    /// A legend of the given token types and modifiers.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokensLegend;
    ///
    /// let legend = SemanticTokensLegend::new(&["keyword"], &[]);
    /// assert_eq!(legend.token_types, ["keyword"]);
    /// ```
    #[must_use]
    pub fn new(token_types: &[&str], token_modifiers: &[&str]) -> Self {
        Self {
            token_types: token_types.iter().map(|&name| name.to_owned()).collect(),
            token_modifiers: token_modifiers
                .iter()
                .map(|&name| name.to_owned())
                .collect(),
        }
    }

    /// The index of the token type `name`, to put in a token.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokensLegend;
    ///
    /// let legend = SemanticTokensLegend::new(&["keyword", "number"], &[]);
    /// assert_eq!(legend.token_type("number"), Some(1));
    /// assert_eq!(legend.token_type("string"), None);
    /// ```
    #[must_use]
    pub fn token_type(&self, name: &str) -> Option<u32> {
        self.token_types
            .iter()
            .position(|t| t == name)
            .and_then(|i| u32::try_from(i).ok())
    }

    /// The bit of the modifier `name`, to OR into a token's modifiers. `None`
    /// for an unknown name or one past the 32 bits the mask has.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokensLegend;
    ///
    /// let legend = SemanticTokensLegend::new(&[], &["declaration", "readonly"]);
    /// assert_eq!(legend.modifier("readonly"), Some(0b10));
    /// assert_eq!(legend.modifier("static"), None);
    /// ```
    #[must_use]
    pub fn modifier(&self, name: &str) -> Option<u32> {
        let index = self.token_modifiers.iter().position(|m| m == name)?;
        1u32.checked_shl(u32::try_from(index).ok()?)
    }

    /// The protocol's JSON for the legend.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokensLegend;
    ///
    /// let json = SemanticTokensLegend::new(&["keyword"], &[]).to_json();
    /// assert_eq!(json["tokenTypes"][0], "keyword");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"tokenTypes\":");
        write_names(out, &self.token_types);
        out.push_str(",\"tokenModifiers\":");
        write_names(out, &self.token_modifiers);
        out.push('}');
    }
}

fn write_names(out: &mut String, names: &[String]) {
    out.push('[');
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_str(out, name);
    }
    out.push(']');
}

/// A document's semantic tokens, delta-encoded as the protocol sends them.
///
/// Build one with a [`SemanticTokensBuilder`]. The data is a multiple of five
/// integers, one group of five per token. The server assigns the `resultId`
/// itself, which is what makes `textDocument/semanticTokens/full/delta`
/// possible.
///
/// # Examples
///
/// ```
/// use lsp_lang::SemanticTokens;
///
/// // Two tokens: line 0 column 4 length 3 type 1; then the same line, 6 columns on.
/// let tokens = SemanticTokens::from_data(vec![0, 4, 3, 1, 0, 0, 6, 2, 0, 1]);
/// assert_eq!(tokens.len(), 2);
/// assert_eq!(tokens.to_json()["data"][6], 6);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SemanticTokens {
    data: Vec<u32>,
    result_id: Option<String>,
}

impl SemanticTokens {
    /// Wraps already delta-encoded data. A length that is not a multiple of
    /// five is a bug in the caller; the server refuses to send it, answering
    /// the request with an internal error.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// let tokens = SemanticTokens::from_data(vec![0, 4, 3, 1, 0]);
    /// assert_eq!(tokens.len(), 1);
    /// ```
    #[must_use]
    pub fn from_data(data: Vec<u32>) -> Self {
        Self {
            data,
            result_id: None,
        }
    }

    /// The delta-encoded integers.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// assert_eq!(SemanticTokens::from_data(vec![0, 0, 1, 0, 0]).data(), [0, 0, 1, 0, 0]);
    /// ```
    #[must_use]
    pub fn data(&self) -> &[u32] {
        &self.data
    }

    /// The number of tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// assert_eq!(SemanticTokens::from_data(vec![0; 10]).len(), 2);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len() / 5
    }

    /// Whether there are no tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// assert!(SemanticTokens::default().is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The `resultId` the answer carries, once the server has assigned one.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// // The server assigns the id when it sends the tokens.
    /// assert_eq!(SemanticTokens::from_data(vec![]).result_id(), None);
    /// ```
    #[must_use]
    pub fn result_id(&self) -> Option<&str> {
        self.result_id.as_deref()
    }

    pub(crate) fn with_result_id(mut self, id: String) -> Self {
        self.result_id = Some(id);
        self
    }

    pub(crate) fn into_data(self) -> Vec<u32> {
        self.data
    }

    /// The protocol's JSON for these tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SemanticTokens;
    ///
    /// let json = SemanticTokens::from_data(vec![0, 1, 2, 3, 4]).to_json();
    /// assert_eq!(json.to_string(), r#"{"data":[0,1,2,3,4]}"#);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push('{');
        if let Some(id) = &self.result_id {
            out.push_str("\"resultId\":");
            write_str(out, id);
            out.push(',');
        }
        out.push_str("\"data\":");
        write_data(out, &self.data);
        out.push('}');
    }
}

fn write_data(out: &mut String, data: &[u32]) {
    out.push('[');
    for (i, &n) in data.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_u64(out, u64::from(n));
    }
    out.push(']');
}

/// The single edit that turns `previous` into `next`, aligned to whole tokens,
/// or `None` when they are equal. `(start, delete_count, replacement)` in
/// integers, as `SemanticTokensEdit` counts them.
pub(crate) fn diff(previous: &[u32], next: &[u32]) -> Option<(usize, usize, Vec<u32>)> {
    if previous == next {
        return None;
    }
    let prefix = previous
        .iter()
        .zip(next)
        .take_while(|(a, b)| a == b)
        .count();
    // Whole tokens only: some clients index edits by token.
    let start = prefix - prefix % 5;
    let room = previous.len().min(next.len()) - start;
    let suffix = previous
        .iter()
        .rev()
        .zip(next.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = suffix - suffix % 5;
    let delete = previous.len() - start - suffix;
    let replacement = next[start..next.len() - suffix].to_vec();
    Some((start, delete, replacement))
}

/// Writes `{"resultId":…,"edits":[…]}` for the edit from [`diff`].
pub(crate) fn write_delta(
    out: &mut String,
    result_id: &str,
    edit: Option<(usize, usize, Vec<u32>)>,
) {
    out.push_str("{\"resultId\":");
    write_str(out, result_id);
    out.push_str(",\"edits\":[");
    if let Some((start, delete, data)) = edit {
        out.push_str("{\"start\":");
        write_u64(out, start as u64);
        out.push_str(",\"deleteCount\":");
        write_u64(out, delete as u64);
        if !data.is_empty() {
            out.push_str(",\"data\":");
            write_data(out, &data);
        }
        out.push('}');
    }
    out.push_str("]}");
}

/// Collects tokens by byte span and encodes them for the protocol.
///
/// Push tokens in any order; [`build`](SemanticTokensBuilder::build) sorts them,
/// drops a token that overlaps an earlier one (the protocol does not allow
/// overlaps), clamps spans to the text, converts to lines and columns in the
/// session's encoding, and splits tokens that cross a line break.
///
/// # Examples
///
/// ```
/// # use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
/// # #[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// # enum K { Root, Text }
/// # impl TokenKind for K {}
/// # #[derive(Clone)]
/// # struct Plain;
/// # impl Grammar for Plain {
/// #     type Kind = K;
/// #     const ROOT: K = K::Root;
/// #     fn lex(&self, text: &str, tokens: &mut Vec<Token<K>>) {
/// #         if !text.is_empty() { tokens.push(Token::new(K::Text, Span::new(0, text.len() as u32))); }
/// #     }
/// #     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
/// #         b.start(K::Root);
/// #         while !b.at_end() { b.bump(); }
/// #         b.finish();
/// #     }
/// # }
/// use lsp_lang::{Handler, SemanticTokens, SemanticTokensBuilder, SemanticTokensLegend};
/// use lsp_lang::{Context, Document, Features, ResponseError};
///
/// /// Highlights every `fn` as a keyword.
/// struct Keywords(SemanticTokensLegend);
///
/// impl Handler<Plain> for Keywords {
///     fn features(&self) -> Features {
///         Features::new().semantic_tokens(self.0.clone())
///     }
///
///     fn semantic_tokens(
///         &mut self,
///         doc: &Document<Plain>,
///         _: &mut Context<'_, Plain>,
///     ) -> Result<SemanticTokens, ResponseError> {
///         let keyword = self.0.token_type("keyword").unwrap_or(0);
///         let mut tokens = SemanticTokensBuilder::new(doc);
///         for (at, _) in doc.text().match_indices("fn") {
///             tokens.push(Span::new(at as u32, at as u32 + 2), keyword, 0);
///         }
///         Ok(tokens.build())
///     }
/// }
/// ```
pub struct SemanticTokensBuilder<'d, G: Grammar> {
    document: &'d Document<G>,
    tokens: Vec<Pushed>,
}

#[derive(Clone, Copy, Debug)]
struct Pushed {
    start: u32,
    end: u32,
    token_type: u32,
    modifiers: u32,
}

impl<'d, G: Grammar> SemanticTokensBuilder<'d, G> {
    /// A builder for `document`'s tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::incremental_lang::{Grammar, Span};
    /// use lsp_lang::{Document, SemanticTokensBuilder};
    ///
    /// fn start<G: Grammar>(doc: &Document<G>) -> usize {
    ///     SemanticTokensBuilder::new(doc).len()
    /// }
    /// ```
    #[must_use]
    pub fn new(document: &'d Document<G>) -> Self {
        Self {
            document,
            tokens: Vec::new(),
        }
    }

    /// Adds a token over the byte `span`, of legend type `token_type`, with the
    /// legend modifier bits `modifiers`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::incremental_lang::{Grammar, Span};
    /// use lsp_lang::{Document, SemanticTokensBuilder};
    ///
    /// fn keyword<G: Grammar>(doc: &Document<G>) -> usize {
    ///     let mut tokens = SemanticTokensBuilder::new(doc);
    ///     tokens.push(Span::new(0, 2), 0, 0);
    ///     tokens.len()
    /// }
    /// ```
    pub fn push(&mut self, span: Span, token_type: u32, modifiers: u32) {
        self.tokens.push(Pushed {
            start: span.start().to_u32(),
            end: span.end().to_u32(),
            token_type,
            modifiers,
        });
    }

    /// The number of tokens pushed so far (before overlaps are dropped and
    /// multi-line tokens split).
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::incremental_lang::{Grammar, Span};
    /// use lsp_lang::{Document, SemanticTokensBuilder};
    ///
    /// fn count<G: Grammar>(doc: &Document<G>) -> usize {
    ///     let mut tokens = SemanticTokensBuilder::new(doc);
    ///     tokens.push(Span::new(0, 2), 0, 0);
    ///     tokens.len()
    /// }
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether nothing was pushed.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::incremental_lang::{Grammar, Span};
    /// use lsp_lang::{Document, SemanticTokensBuilder};
    ///
    /// fn nothing<G: Grammar>(doc: &Document<G>) -> bool {
    ///     SemanticTokensBuilder::new(doc).is_empty()
    /// }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Encodes the tokens.
    ///
    /// Cost is `O(n log n)` in the number of tokens plus the text they cover.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::incremental_lang::{Grammar, Span};
    /// use lsp_lang::{Document, SemanticTokensBuilder};
    ///
    /// fn encode<G: Grammar>(doc: &Document<G>) -> Vec<u32> {
    ///     let mut tokens = SemanticTokensBuilder::new(doc);
    ///     tokens.push(Span::new(0, 2), 1, 0);
    ///     tokens.build().data().to_vec()
    /// }
    /// ```
    #[must_use]
    pub fn build(self) -> SemanticTokens {
        let document = self.document;
        let text = document.text();
        let mut tokens = self.tokens;
        tokens.sort_by_key(|t| (t.start, t.end));

        // Clamp to the text and to character boundaries, then drop what is
        // empty or overlaps the token before it.
        let mut kept: Vec<Pushed> = Vec::with_capacity(tokens.len());
        let mut covered = 0u32;
        for mut token in tokens {
            token.start = floor_boundary(text, token.start);
            token.end = floor_boundary(text, token.end);
            if token.start >= token.end || token.start < covered {
                continue;
            }
            covered = token.end;
            kept.push(token);
        }

        let starts: Vec<u32> = kept.iter().map(|t| t.start).collect();
        let (mut order, mut positions) = (Vec::new(), Vec::new());
        document.positions(&starts, &mut order, &mut positions);

        let bytes = text.as_bytes();
        let mut data = Vec::with_capacity(kept.len() * 5);
        let (mut prev_line, mut prev_column) = (0u32, 0u32);
        let mut emit = |line: u32, column: u32, length: u32, token: &Pushed| {
            let delta_line = line.saturating_sub(prev_line);
            let delta_start = if delta_line == 0 {
                column.saturating_sub(prev_column)
            } else {
                column
            };
            data.extend_from_slice(&[
                delta_line,
                delta_start,
                length,
                token.token_type,
                token.modifiers,
            ]);
            (prev_line, prev_column) = (line, column);
        };
        for (token, position) in kept.iter().zip(&positions) {
            let (end, mut at) = (token.end as usize, token.start as usize);
            let (mut line, mut column) = (position.line, position.character);
            loop {
                let line_end = crate::lines::find_line_break(&bytes[at..end]).map(|i| at + i);
                let segment_end = line_end.unwrap_or(end);
                if segment_end > at {
                    let length = document.width_of(at, segment_end);
                    emit(line, column, length, token);
                }
                let Some(break_at) = line_end else { break };
                // `\r\n` is one break.
                let mut next = break_at + 1;
                if bytes[break_at] == b'\r' && bytes.get(next) == Some(&b'\n') {
                    next += 1;
                }
                if next >= end {
                    break;
                }
                (at, line, column) = (next, line.saturating_add(1), 0);
            }
        }
        SemanticTokens {
            data,
            result_id: None,
        }
    }
}

/// `offset` clamped to the text and rounded down to a character boundary.
fn floor_boundary(text: &str, offset: u32) -> u32 {
    let mut at = (offset as usize).min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at as u32
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{position::Encoding, testing::Chars};

    // Tests open trusted text with a fixed grammar, so a failure to open is a
    // bug in the test itself.
    #[allow(clippy::expect_used)]
    fn doc(text: &str, encoding: Encoding) -> Document<Chars> {
        Document::open(Chars, "file:///t", 1, text.to_owned(), encoding)
            .expect("the test document opens")
    }

    fn span(a: u32, b: u32) -> Span {
        Span::new(a, b)
    }

    #[test]
    fn test_tokens_are_delta_encoded_across_lines() {
        let d = doc("ab cd\nef", Encoding::Utf16);
        let mut b = SemanticTokensBuilder::new(&d);
        b.push(span(3, 5), 2, 1);
        b.push(span(0, 2), 1, 0);
        b.push(span(6, 8), 3, 0);
        let t = b.build();
        // (0,0) len 2; (0,3) len 2 → deltaStart 3; (1,0) len 2 → deltaLine 1.
        assert_eq!(t.data(), [0, 0, 2, 1, 0, 0, 3, 2, 2, 1, 1, 0, 2, 3, 0]);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn test_utf16_columns_and_lengths_count_surrogate_pairs() {
        // "😀x": the emoji is 4 bytes, 2 UTF-16 units.
        let d = doc("😀x", Encoding::Utf16);
        let mut b = SemanticTokensBuilder::new(&d);
        b.push(span(0, 4), 0, 0);
        b.push(span(4, 5), 1, 0);
        assert_eq!(b.build().data(), [0, 0, 2, 0, 0, 0, 2, 1, 1, 0]);
        let d = doc("😀x", Encoding::Utf8);
        let mut b = SemanticTokensBuilder::new(&d);
        b.push(span(0, 4), 0, 0);
        b.push(span(4, 5), 1, 0);
        assert_eq!(b.build().data(), [0, 0, 4, 0, 0, 0, 4, 1, 1, 0]);
    }

    #[test]
    fn test_multi_line_tokens_split_per_line() {
        let d = doc("a/*x\r\ny\n*/b", Encoding::Utf16);
        let mut b = SemanticTokensBuilder::new(&d);
        // The comment from byte 1 to byte 10 covers three lines.
        b.push(span(1, 10), 7, 0);
        let t = b.build();
        assert_eq!(
            t.data(),
            [
                0, 1, 3, 7, 0, // "/*x"
                1, 0, 1, 7, 0, // "y"
                1, 0, 2, 7, 0, // "*/"
            ]
        );
    }

    #[test]
    fn test_overlapping_empty_and_out_of_range_tokens_are_dropped_or_clamped() {
        let d = doc("abcdef", Encoding::Utf16);
        let mut b = SemanticTokensBuilder::new(&d);
        b.push(span(0, 4), 1, 0);
        b.push(span(2, 5), 2, 0); // overlaps the first: dropped
        b.push(span(4, 4), 3, 0); // empty: dropped
        b.push(span(4, 99), 4, 0); // clamped to the text
        let t = b.build();
        assert_eq!(t.data(), [0, 0, 4, 1, 0, 0, 4, 2, 4, 0]);
        assert!(SemanticTokensBuilder::new(&d).build().is_empty());
    }

    #[test]
    fn test_token_boundaries_inside_a_character_round_down() {
        let d = doc("é", Encoding::Utf16);
        let mut b = SemanticTokensBuilder::new(&d);
        b.push(span(1, 2), 0, 0); // starts inside the 2-byte é
        // start 1 → 0, end 2: the whole é.
        assert_eq!(b.build().data(), [0, 0, 1, 0, 0]);
    }

    #[test]
    fn test_diff_is_one_token_aligned_edit() {
        let a: Vec<u32> = (0..15).collect();
        assert_eq!(diff(&a, &a), None);
        let mut b = a.clone();
        b[7] = 99;
        assert_eq!(diff(&a, &b), Some((5, 5, b[5..10].to_vec())));
        // Grown at the end.
        let mut c = a.clone();
        c.extend([1, 2, 3, 4, 5]);
        assert_eq!(diff(&a, &c), Some((15, 0, vec![1, 2, 3, 4, 5])));
        // Shrunk to nothing.
        assert_eq!(diff(&a, &[]), Some((0, 15, vec![])));
        // Applying the edit reproduces the new data.
        for (old, new) in [(&a, &b), (&a, &c)] {
            if let Some((start, delete, data)) = diff(old, new) {
                let mut applied = old.clone();
                let _removed: Vec<u32> = applied.splice(start..start + delete, data).collect();
                assert_eq!(&applied, new);
            }
        }
    }

    #[test]
    fn test_write_delta_and_full_shapes() {
        let mut out = String::new();
        write_delta(&mut out, "7", Some((5, 5, vec![1, 2, 3, 4, 5])));
        assert_eq!(
            out,
            r#"{"resultId":"7","edits":[{"start":5,"deleteCount":5,"data":[1,2,3,4,5]}]}"#
        );
        out.clear();
        write_delta(&mut out, "8", None);
        assert_eq!(out, r#"{"resultId":"8","edits":[]}"#);
        out.clear();
        write_delta(&mut out, "9", Some((0, 5, vec![])));
        assert_eq!(
            out,
            r#"{"resultId":"9","edits":[{"start":0,"deleteCount":5}]}"#
        );
        let full = SemanticTokens::from_data(vec![1, 2, 3, 4, 5]).with_result_id("3".into());
        assert_eq!(
            full.to_json().to_string(),
            r#"{"resultId":"3","data":[1,2,3,4,5]}"#
        );
    }

    #[test]
    fn test_legend_lookups_and_json() {
        let legend = SemanticTokensLegend::standard();
        assert_eq!(legend.token_types.len(), 23);
        assert_eq!(legend.token_modifiers.len(), 10);
        assert_eq!(legend.modifier("defaultLibrary"), Some(1 << 9));
        let json = SemanticTokensLegend::new(&["a"], &["m"]).to_json();
        assert_eq!(
            json.to_string(),
            r#"{"tokenTypes":["a"],"tokenModifiers":["m"]}"#
        );
        let many = SemanticTokensLegend {
            token_types: vec![],
            token_modifiers: (0..33).map(|i| i.to_string()).collect(),
        };
        assert_eq!(many.modifier("31"), Some(1 << 31));
        assert_eq!(many.modifier("32"), None);
    }

    fn tokens_strategy() -> impl Strategy<Value = Vec<u32>> {
        // Whole tokens from a tiny alphabet, so prefixes and suffixes collide.
        prop::collection::vec(prop::collection::vec(0u32..3, 5), 0..8)
            .prop_map(|tokens| tokens.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// The edit turns the previous data into the next, is aligned to whole
        /// tokens, and is absent exactly when they are equal.
        #[test]
        fn prop_diff_applied_to_the_previous_gives_the_next(
            previous in tokens_strategy(),
            next in tokens_strategy(),
        ) {
            match diff(&previous, &next) {
                None => prop_assert_eq!(&previous, &next),
                Some((start, delete, data)) => {
                    prop_assert_eq!(start % 5, 0);
                    prop_assert_eq!(delete % 5, 0);
                    prop_assert_eq!(data.len() % 5, 0);
                    prop_assert!(start + delete <= previous.len());
                    let mut applied = previous.clone();
                    let _removed: Vec<u32> = applied.splice(start..start + delete, data).collect();
                    prop_assert_eq!(&applied, &next);
                    prop_assert_ne!(&previous, &next);
                }
            }
        }
    }
}
