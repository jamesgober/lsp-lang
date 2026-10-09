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
    /// Set when the client's text could not be followed; see
    /// [`is_stale`](Document::is_stale).
    stale: bool,
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
            stale: false,
        })
    }

    /// Converts a content change's range to the byte span it replaces: the
    /// whole text when `range` is `None`.
    ///
    /// Returns `None` when the range names a line the document does not have
    /// — more than one past its last line. The client's text then has lines
    /// this copy lacks, so the two have drifted apart and the edit cannot be
    /// placed. The line just past the last (column 0 or any other) is accepted
    /// and means the end of the text, as clients use it for "end of document".
    /// Every other out-of-range part of a position is clamped as
    /// [`offset`](Document::offset) describes: a column past the end of its
    /// line is the end of that line, as the protocol prescribes.
    pub(crate) fn edit_span(&self, range: Option<Range>) -> Option<Span> {
        let Some(range) = range else {
            return Some(Span::new(0, self.source.text().len() as u32));
        };
        let lines = self.lines.len();
        let exists = |at: Position| (at.line as usize) <= lines;
        (exists(range.start) && exists(range.end)).then(|| self.span(range))
    }

    /// Applies one edit by incremental reparse: replaces `span` with `text`,
    /// reparses the innermost reparsable node around it, and patches the line
    /// index.
    ///
    /// Transactional: on error, or a panic in the grammar, the document is
    /// unchanged (`incremental-lang` swaps the text and tree in only once the
    /// new tree is complete, and the line index is patched after that).
    pub(crate) fn edit(&mut self, span: Span, text: &str) -> Result<(), incremental_lang::Error> {
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

    /// Whether this copy is known to differ from the client's text: an edit
    /// could not be placed or parsed, so the document kept its last good
    /// state. Only a whole-text change or a reopen brings it back in step.
    pub(crate) fn is_stale(&self) -> bool {
        self.stale
    }

    /// Marks the document as out of step with the client.
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Records the version the client gave the latest change, which this copy
    /// now matches.
    pub(crate) fn synced(&mut self, version: i32) {
        self.version = version;
        self.stale = false;
    }

    /// Converts many byte offsets at once (see [`LineIndex::positions`]);
    /// `order` is scratch.
    pub(crate) fn positions(&self, offsets: &[u32], order: &mut Vec<u32>, out: &mut Vec<Position>) {
        self.lines
            .positions(self.source.text(), offsets, self.encoding, order, out);
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

impl<G: Grammar + Clone> Document<G> {
    /// Applies one edit by full reparse: splices `text` into the text in
    /// place of `span`, then parses the result from scratch with a clone of
    /// the grammar. The fallback for an edit whose incremental reparse failed
    /// (a grammar panic or contract break in the reparsed fragment), which a
    /// whole-text parse may still survive.
    ///
    /// Transactional: nothing changes unless the new parse succeeds. The
    /// incremental statistics start over with the new document.
    pub(crate) fn rebuild(
        &mut self,
        span: Span,
        text: &str,
    ) -> Result<(), incremental_lang::Error> {
        let old = self.source.text();
        let (start, end) = (span.start().to_usize(), span.end().to_usize());
        if end > old.len() {
            return Err(incremental_lang::Error::OutOfBounds {
                span,
                len: old.len() as u32,
            });
        }
        if let Some(&offset) = [start, end].iter().find(|&&at| !old.is_char_boundary(at)) {
            return Err(incremental_lang::Error::NotCharBoundary {
                offset: offset as u32,
            });
        }
        let mut spliced = String::with_capacity(old.len() - (end - start) + text.len());
        spliced.push_str(&old[..start]);
        spliced.push_str(text);
        spliced.push_str(&old[end..]);
        self.reset(spliced)
    }

    /// Replaces the text with `text`, parsed from scratch. Transactional, like
    /// [`rebuild`](Document::rebuild).
    pub(crate) fn reset(&mut self, text: String) -> Result<(), incremental_lang::Error> {
        let source = incremental_lang::Document::new(self.source.grammar().clone(), text)?;
        self.lines = LineIndex::new(source.text());
        self.source = source;
        Ok(())
    }
}

impl<G: Grammar> fmt::Debug for Document<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("uri", &self.uri)
            .field("version", &self.version)
            .field("encoding", &self.encoding)
            .field("len", &self.source.text().len())
            .field("stale", &self.stale)
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

    /// Applies one content change the way the server does on the fast path.
    fn change(doc: &mut Document<Chars>, range: Option<Range>, text: &str) -> Result {
        let span = doc
            .edit_span(range)
            .ok_or(incremental_lang::Error::OutOfBounds {
                span: Span::new(0, 0),
                len: 0,
            })?;
        doc.edit(span, text)
    }

    #[test]
    fn test_change_with_range_applies_utf16_edit() -> Result {
        let mut doc = open("a😀b\nc", Encoding::Utf16)?;
        // Replace the emoji: columns 1..3 in UTF-16.
        let range = Range::new(Position::new(0, 1), Position::new(0, 3));
        change(&mut doc, Some(range), "é")?;
        assert_eq!(doc.text(), "aéb\nc");
        assert_eq!(doc.position(5), Position::new(1, 0));
        assert_eq!(doc.lines, LineIndex::new(doc.text()));
        Ok(())
    }

    #[test]
    fn test_change_without_range_replaces_everything() -> Result {
        let mut doc = open("old\ntext", Encoding::Utf8)?;
        change(&mut doc, None, "new")?;
        assert_eq!(doc.text(), "new");
        assert_eq!(doc.lines, LineIndex::new("new"));
        Ok(())
    }

    #[test]
    fn test_edit_span_refuses_lines_the_document_does_not_have() -> Result {
        let doc = open(
            "ab
cd",
            Encoding::Utf16,
        )?; // two lines: 0 and 1
        let at = |line, character| Position::new(line, character);
        let range = |a, b| Some(Range::new(a, b));
        // A column past the end of its line clamps, as the protocol says.
        assert_eq!(
            doc.edit_span(range(at(0, 99), at(0, 99))),
            Some(Span::new(2, 2))
        );
        // The line just past the last means the end of the text.
        assert_eq!(
            doc.edit_span(range(at(2, 0), at(2, 7))),
            Some(Span::new(5, 5))
        );
        // Further than that, the client has lines this copy lacks.
        assert_eq!(doc.edit_span(range(at(3, 0), at(3, 0))), None);
        assert_eq!(doc.edit_span(range(at(0, 0), at(u32::MAX, 0))), None);
        // A whole-text change always applies.
        assert_eq!(doc.edit_span(None), Some(Span::new(0, 5)));
        Ok(())
    }

    #[test]
    fn test_rebuild_splices_and_reparses_from_scratch() -> Result {
        let mut doc = open(
            "one
two",
            Encoding::Utf8,
        )?;
        doc.rebuild(
            Span::new(3, 4),
            "😀
",
        )?;
        assert_eq!(
            doc.text(),
            "one😀
two"
        );
        assert_eq!(doc.lines, LineIndex::new(doc.text()));
        assert_eq!(doc.incremental().stats().full, 1);
        // Spans that cannot be placed are refused, leaving the text alone.
        let before = doc.text().to_owned();
        assert!(doc.rebuild(Span::new(4, 4), "x").is_err()); // inside the emoji
        assert!(doc.rebuild(Span::new(0, 99), "x").is_err());
        assert_eq!(doc.text(), before);
        Ok(())
    }

    #[test]
    fn test_stale_flag_clears_on_sync() -> Result {
        let mut doc = open("x", Encoding::Utf8)?;
        assert!(!doc.is_stale());
        doc.mark_stale();
        assert!(doc.is_stale() && doc.version() == 1);
        doc.synced(4);
        assert!(!doc.is_stale() && doc.version() == 4);
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
