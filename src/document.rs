//! [`Document`]: an open text document — its text, its incrementally
//! maintained syntax tree, and the conversions between protocol positions and
//! byte offsets.

use std::fmt;

use incremental_lang::{Grammar, Span, Tree};

use crate::{
    lines::LineIndex,
    position::{Encoding, Position, Range},
};

/// A document the client has open, kept in step with the editor.
///
/// The server creates one on `textDocument/didOpen`, applies every
/// `textDocument/didChange` to it, and drops it on `textDocument/didClose`;
/// handlers see it, read-only, in [`Handler::changed`](crate::Handler::changed)
/// and through [`Context::document`](crate::Context::document).
///
/// Each document pairs an [`incremental_lang::Document`] with a line index.
/// An edit from the client is converted from its protocol range to a byte
/// span, applied to the text, and reparsed incrementally — only the innermost
/// node around the change that can be reparsed on its own is rebuilt — and the
/// line index is patched rather than rebuilt. The parsing work of a keystroke
/// is proportional to the edit; what remains linear in the file is cheap —
/// moving the text after the edit (one `memmove`) and shifting the line starts
/// after it (one vectorized pass).
///
/// Positions and offsets convert in both directions, in the session's
/// [`Encoding`]: [`position`](Document::position) and
/// [`range`](Document::range) turn byte offsets and spans from the syntax tree
/// into protocol positions for results and diagnostics;
/// [`offset`](Document::offset) and [`span`](Document::span) turn a request's
/// positions into byte offsets to look up in the tree. Both directions are
/// total: out-of-range input is clamped as the protocol prescribes, never
/// rejected.
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
/// # fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
/// use lsp_lang::{Context, Document, Handler, Position, Server};
///
/// /// Records where the word "fn" first appears in each version of a document.
/// #[derive(Default)]
/// struct FindFn {
///     found: Vec<Option<Position>>,
/// }
///
/// impl Handler<Plain> for FindFn {
///     fn changed(&mut self, doc: &Document<Plain>, _cx: &mut Context<'_, Plain>) {
///         let at = doc.text().find("fn").map(|i| doc.position(i as u32));
///         self.found.push(at);
///     }
/// }
///
/// let input = [
///     frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
///     frame(r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":
///         {"uri":"file:///a","languageId":"x","version":1,"text":"// 😀\nfn main"}}}"#),
///     // Replace the comment line with nothing.
///     frame(r#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":
///         {"uri":"file:///a","version":2},"contentChanges":[{"range":
///         {"start":{"line":0,"character":0},"end":{"line":1,"character":0}},"text":""}]}}"#),
///     frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
///     frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
/// ]
/// .concat();
///
/// let mut server = Server::new(Plain, FindFn::default());
/// server.run(input.as_bytes(), Vec::new())?;
/// assert_eq!(
///     server.handler().found,
///     [Some(Position::new(1, 0)), Some(Position::new(0, 0))],
/// );
/// # Ok::<(), lsp_lang::Error>(())
/// ```
pub struct Document<G: Grammar> {
    uri: Box<str>,
    version: i32,
    encoding: Encoding,
    lines: LineIndex,
    source: incremental_lang::Document<G>,
}

impl<G: Grammar> Document<G> {
    /// Opens a document: indexes its lines and parses it once.
    pub(crate) fn open(
        grammar: G,
        uri: &str,
        version: i32,
        text: String,
        encoding: Encoding,
    ) -> Result<Self, incremental_lang::Error> {
        let lines = LineIndex::new(&text);
        let source = incremental_lang::Document::new(grammar, text)?;
        Ok(Self {
            uri: uri.into(),
            version,
            encoding,
            lines,
            source,
        })
    }

    /// Applies one content change: replaces `range` (the whole text when
    /// `None`) with `text`, reparses, and patches the line index.
    ///
    /// The change is transactional: on error the document is unchanged.
    pub(crate) fn change(
        &mut self,
        range: Option<Range>,
        text: &str,
    ) -> Result<(), incremental_lang::Error> {
        let span = match range {
            Some(range) => self.span(range),
            None => Span::new(0, self.source.text().len() as u32),
        };
        // What was rebuilt is the incremental document's business; the line
        // index only needs the edit itself.
        let _reparse = self.source.edit(span, text)?;
        // The edit succeeded, so the new text, and with it `text`, fits in u32.
        self.lines.edit(
            self.source.text(),
            span.start().to_u32(),
            span.end().to_u32(),
            text.len() as u32,
        );
        Ok(())
    }

    /// Converts many byte offsets at once (see [`LineIndex::positions`]);
    /// `order` is scratch.
    pub(crate) fn positions(&self, offsets: &[u32], order: &mut Vec<u32>, out: &mut Vec<Position>) {
        self.lines
            .positions(self.source.text(), offsets, self.encoding, order, out);
    }

    /// Records the version the client gave the latest change.
    pub(crate) fn set_version(&mut self, version: i32) {
        self.version = version;
    }

    /// Returns the document's URI, exactly as the client sent it.
    ///
    /// URIs are compared as opaque strings; the server does not normalize them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Context, Document, Handler};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// fn is_test_file<G: Grammar>(doc: &Document<G>) -> bool {
    ///     doc.uri().ends_with("_test.lisp")
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the version the client assigned to the current text.
    ///
    /// The client increases it with every change. Diagnostics published for a
    /// document carry it, so the client can discard results computed for text
    /// it has since replaced.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// fn describe<G: Grammar>(doc: &Document<G>) -> String {
    ///     format!("{} (v{})", doc.uri(), doc.version())
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Returns the current text.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// fn word_count<G: Grammar>(doc: &Document<G>) -> usize {
    ///     doc.text().split_whitespace().count()
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn text(&self) -> &str {
        self.source.text()
    }

    /// Returns the current syntax tree.
    ///
    /// The tree is the grammar's lossless concrete syntax tree of
    /// [`text`](Document::text); its spans are byte offsets into that text,
    /// ready for [`range`](Document::range). Clone it (one reference-count
    /// increment) to keep a snapshot that later edits leave untouched — to hand
    /// to a worker thread, for instance.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Range};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // The ranges of the root's direct children, ready to send to a client.
    /// fn outline<G: Grammar>(doc: &Document<G>) -> Vec<Range> {
    ///     doc.tree().root().children().map(|child| doc.range(child.span())).collect()
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn tree(&self) -> &Tree<G::Kind> {
        self.source.tree()
    }

    /// Returns the underlying [`incremental_lang::Document`], for its grammar
    /// and its reparse statistics.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // How often edits to this document were served by a single node.
    /// fn hit_rate<G: Grammar>(doc: &Document<G>) -> f64 {
    ///     let stats = doc.incremental().stats();
    ///     stats.partial as f64 / stats.edits.max(1) as f64
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn incremental(&self) -> &incremental_lang::Document<G> {
        &self.source
    }

    /// Returns the encoding this document's positions are counted in — the
    /// one negotiated for the session.
    ///
    /// The conversion methods apply it already; it matters only to a handler
    /// that computes columns itself.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Encoding};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// fn columns_are_bytes<G: Grammar>(doc: &Document<G>) -> bool {
    ///     doc.encoding() == Encoding::Utf8
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Converts a byte offset into the text to a protocol position.
    ///
    /// An offset past the end of the text maps to the end; one inside a
    /// multi-byte character maps to that character. An offset between the
    /// `\r` and `\n` of one line break is not a caret position: it maps to the
    /// column just past the `\r`, which [`offset`](Document::offset) clamps
    /// back to the `\r` itself. Every other offset round-trips exactly.
    ///
    /// Cost: a binary search over the line starts, plus — for UTF-16 — a
    /// vectorized count over the line's text before the offset.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Position};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // The position of the first occurrence of `needle`.
    /// fn find<G: Grammar>(doc: &Document<G>, needle: &str) -> Option<Position> {
    ///     doc.text().find(needle).map(|at| doc.position(at as u32))
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn position(&self, offset: u32) -> Position {
        self.lines
            .position(self.source.text(), offset, self.encoding)
    }

    /// Converts a protocol position to a byte offset into the text.
    ///
    /// Out-of-range input is clamped as the protocol prescribes: a line past
    /// the last maps to the end of the text, and a column past the end of its
    /// line maps to the end of that line, before the line break. A column that
    /// lands inside a character (between the halves of a UTF-16 surrogate
    /// pair, say) maps to the start of that character. The result is always a
    /// character boundary of the text.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Position};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // The word under the cursor, for a hover request.
    /// fn word_at<G: Grammar>(doc: &Document<G>, cursor: Position) -> &str {
    ///     let text = doc.text();
    ///     let at = doc.offset(cursor) as usize;
    ///     let is_word = |c: char| c.is_alphanumeric() || c == '_';
    ///     let start = text[..at].rfind(|c| !is_word(c)).map_or(0, |i| i + 1);
    ///     let end = text[at..].find(|c| !is_word(c)).map_or(text.len(), |i| at + i);
    ///     &text[start..end]
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn offset(&self, position: Position) -> u32 {
        self.lines
            .offset(self.source.text(), position, self.encoding)
    }

    /// Converts a byte span — from the syntax tree or a diagnostic — to a
    /// protocol range.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Range};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // The range covering the whole document.
    /// fn everything<G: Grammar>(doc: &Document<G>) -> Range {
    ///     doc.range(doc.tree().root().span())
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn range(&self, span: Span) -> Range {
        Range::new(
            self.position(span.start().to_u32()),
            self.position(span.end().to_u32()),
        )
    }

    /// Converts a protocol range to a byte span of the text.
    ///
    /// Each end is converted as by [`offset`](Document::offset); a range whose
    /// ends arrive reversed yields the span between them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::{Document, Range};
    /// # use lsp_lang::incremental_lang::Grammar;
    /// // The text a client selected, for a code action.
    /// fn selected<G: Grammar>(doc: &Document<G>, range: Range) -> &str {
    ///     let span = doc.span(range);
    ///     &doc.text()[span.start().to_usize()..span.end().to_usize()]
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn span(&self, range: Range) -> Span {
        Span::new(self.offset(range.start), self.offset(range.end))
    }
}

impl<G: Grammar> fmt::Debug for Document<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("uri", &self.uri)
            .field("version", &self.version)
            .field("encoding", &self.encoding)
            .field("len", &self.source.text().len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Chars;

    type Result = std::result::Result<(), incremental_lang::Error>;

    fn open(
        text: &str,
        encoding: Encoding,
    ) -> std::result::Result<Document<Chars>, incremental_lang::Error> {
        Document::open(Chars, "file:///t", 1, text.to_owned(), encoding)
    }

    #[test]
    fn test_change_with_range_applies_utf16_edit() -> Result {
        let mut doc = open("a😀b\nc", Encoding::Utf16)?;
        // Replace the emoji: columns 1..3 in UTF-16.
        let range = Range::new(Position::new(0, 1), Position::new(0, 3));
        doc.change(Some(range), "é")?;
        assert_eq!(doc.text(), "aéb\nc");
        assert_eq!(doc.position(5), Position::new(1, 0));
        assert_eq!(doc.lines, LineIndex::new(doc.text()));
        Ok(())
    }

    #[test]
    fn test_change_without_range_replaces_everything() -> Result {
        let mut doc = open("old\ntext", Encoding::Utf8)?;
        doc.change(None, "new")?;
        assert_eq!(doc.text(), "new");
        assert_eq!(doc.lines, LineIndex::new("new"));
        Ok(())
    }

    #[test]
    fn test_range_and_span_round_trip() -> Result {
        let doc = open("one\r\ntwo😀three", Encoding::Utf16)?;
        let span = Span::new(5, 12);
        let range = doc.range(span);
        assert_eq!(range, Range::new(Position::new(1, 0), Position::new(1, 5)));
        assert_eq!(doc.span(range), span);
        Ok(())
    }

    #[test]
    fn test_debug_shows_identity_not_text() -> Result {
        let doc = open("secret", Encoding::Utf16)?;
        let shown = format!("{doc:?}");
        assert!(shown.contains("file:///t") && !shown.contains("secret"));
        Ok(())
    }
}
