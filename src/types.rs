//! Small protocol types shared by the typed features: [`Location`], [`Hover`],
//! [`MarkupKind`], and [`TextEdit`].

use jsonx_lib::Value;

use crate::{
    json::{req_object, req_str, to_value, write_str},
    position::Range,
    response::ResponseError,
};

/// A place in a file: a document URI and a range in it.
///
/// What [`Handler::definition`](crate::Handler::definition) returns. The range
/// is in the session's [`Encoding`](crate::Encoding), like every protocol
/// position; a [`Document`](crate::Document) produces one from a byte span.
///
/// # Examples
///
/// ```
/// use lsp_lang::{Location, Position, Range};
///
/// let at = Location::new("file:///a.lisp", Range::new(Position::new(1, 0), Position::new(1, 4)));
/// assert_eq!(at.to_json()["uri"], "file:///a.lisp");
/// assert_eq!(Location::from_json(&at.to_json())?, at);
/// # Ok::<(), lsp_lang::ResponseError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    /// The document, as the client names it.
    pub uri: String,
    /// The range inside it.
    pub range: Range,
}

impl Location {
    /// A location in the document `uri`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Location, Range};
    ///
    /// assert_eq!(Location::new("file:///a", Range::default()).uri, "file:///a");
    /// ```
    #[must_use]
    pub fn new(uri: impl Into<String>, range: Range) -> Self {
        Self {
            uri: uri.into(),
            range,
        }
    }

    /// The protocol's JSON for this location.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Location, Range};
    ///
    /// assert_eq!(Location::new("file:///a", Range::default()).to_json()["uri"], "file:///a");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    /// Reads a location from `{"uri": …, "range": …}`.
    ///
    /// # Errors
    ///
    /// [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS) when a member is
    /// missing or has the wrong type.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, Location};
    ///
    /// assert!(Location::from_json(&json!({ "uri": "file:///a" })).is_err());
    /// ```
    pub fn from_json(value: &Value<'_>) -> Result<Self, ResponseError> {
        Ok(Self {
            uri: req_str(value, "uri")?.to_owned(),
            range: Range::from_json(req_object(value, "range")?)?,
        })
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"uri\":");
        write_str(out, &self.uri);
        out.push_str(",\"range\":");
        self.range.write(out);
        out.push('}');
    }
}

/// How a block of text is to be read: plain or Markdown.
///
/// # Examples
///
/// ```
/// use lsp_lang::MarkupKind;
///
/// assert_eq!(MarkupKind::Markdown.as_str(), "markdown");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MarkupKind {
    /// Plain text (`"plaintext"`).
    PlainText,
    /// Markdown (`"markdown"`), the default for hover.
    #[default]
    Markdown,
}

impl MarkupKind {
    /// The protocol's name for this kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::MarkupKind;
    ///
    /// assert_eq!(MarkupKind::PlainText.as_str(), "plaintext");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MarkupKind::PlainText => "plaintext",
            MarkupKind::Markdown => "markdown",
        }
    }
}

/// Writes a `MarkupContent`: `{"kind":…,"value":…}`.
pub(crate) fn write_markup(out: &mut String, kind: MarkupKind, value: &str) {
    out.push_str("{\"kind\":\"");
    out.push_str(kind.as_str());
    out.push_str("\",\"value\":");
    write_str(out, value);
    out.push('}');
}

/// The answer to `textDocument/hover`: text for the user, optionally with the
/// range it describes.
///
/// # Examples
///
/// ```
/// use lsp_lang::{Hover, Position, Range};
///
/// let hover = Hover::markdown("`width`: defined on line 1")
///     .with_range(Range::new(Position::new(0, 8), Position::new(0, 13)));
/// let json = hover.to_json();
/// assert_eq!(json["contents"]["kind"], "markdown");
/// assert_eq!(json["range"]["start"]["character"], 8);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Hover {
    /// The text shown.
    pub contents: String,
    /// How `contents` is read.
    pub kind: MarkupKind,
    /// The range the hover describes; clients highlight it.
    pub range: Option<Range>,
}

impl Hover {
    /// A hover of Markdown text.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Hover;
    ///
    /// assert_eq!(Hover::markdown("`x`").to_json()["contents"]["kind"], "markdown");
    /// ```
    #[must_use]
    pub fn markdown(contents: impl Into<String>) -> Self {
        Self {
            contents: contents.into(),
            kind: MarkupKind::Markdown,
            range: None,
        }
    }

    /// A hover of plain text.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Hover;
    ///
    /// assert_eq!(Hover::plain_text("x").to_json()["contents"]["kind"], "plaintext");
    /// ```
    #[must_use]
    pub fn plain_text(contents: impl Into<String>) -> Self {
        Self {
            contents: contents.into(),
            kind: MarkupKind::PlainText,
            range: None,
        }
    }

    /// Sets the range the hover describes.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Hover, Range};
    ///
    /// let hover = Hover::markdown("x").with_range(Range::default());
    /// assert!(hover.to_json().get("range").is_some());
    /// ```
    #[must_use]
    pub fn with_range(mut self, range: Range) -> Self {
        self.range = Some(range);
        self
    }

    /// The protocol's JSON for this hover.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Hover;
    ///
    /// let json = Hover::markdown("`x`").to_json();
    /// assert_eq!(json["contents"]["value"], "`x`");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"contents\":");
        write_markup(out, self.kind, &self.contents);
        if let Some(range) = &self.range {
            out.push_str(",\"range\":");
            range.write(out);
        }
        out.push('}');
    }
}

/// A replacement of a range of text, as completion items and code actions
/// carry them.
///
/// # Examples
///
/// ```
/// use lsp_lang::{Position, Range, TextEdit};
///
/// let edit = TextEdit::new(Range::new(Position::new(0, 1), Position::new(0, 3)), "ab");
/// assert_eq!(edit.to_json()["newText"], "ab");
/// assert_eq!(TextEdit::from_json(&edit.to_json())?, edit);
/// # Ok::<(), lsp_lang::ResponseError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextEdit {
    /// The text replaced.
    pub range: Range,
    /// What replaces it.
    pub new_text: String,
}

impl TextEdit {
    /// An edit replacing `range` with `new_text`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Range, TextEdit};
    ///
    /// assert_eq!(TextEdit::new(Range::default(), "x").new_text, "x");
    /// ```
    #[must_use]
    pub fn new(range: Range, new_text: impl Into<String>) -> Self {
        Self {
            range,
            new_text: new_text.into(),
        }
    }

    /// The protocol's JSON for this edit.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Range, TextEdit};
    ///
    /// assert_eq!(TextEdit::new(Range::default(), "x").to_json()["newText"], "x");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    /// Reads an edit from `{"range": …, "newText": …}`.
    ///
    /// # Errors
    ///
    /// [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS) when a member is
    /// missing or has the wrong type.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, TextEdit};
    ///
    /// assert!(TextEdit::from_json(&json!({ "newText": "x" })).is_err());
    /// ```
    pub fn from_json(value: &Value<'_>) -> Result<Self, ResponseError> {
        Ok(Self {
            range: Range::from_json(req_object(value, "range")?)?,
            new_text: req_str(value, "newText")?.to_owned(),
        })
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"range\":");
        self.range.write(out);
        out.push_str(",\"newText\":");
        write_str(out, &self.new_text);
        out.push('}');
    }
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;
    use crate::position::Position;

    fn range() -> Range {
        Range::new(Position::new(1, 2), Position::new(3, 4))
    }

    #[test]
    fn test_location_round_trips() {
        let at = Location::new("file:///\"q\"", range());
        assert_eq!(Location::from_json(&at.to_json()).ok(), Some(at));
        assert!(Location::from_json(&json!({ "uri": "x" })).is_err());
        assert!(Location::from_json(&json!({ "uri": 3, "range": {} })).is_err());
    }

    #[test]
    fn test_hover_json_shapes() {
        let plain = Hover::plain_text("t\n\"x\"").to_json();
        assert_eq!(plain["contents"]["kind"], "plaintext");
        assert_eq!(plain["contents"]["value"], "t\n\"x\"");
        assert!(plain.get("range").is_none());
        let ranged = Hover::markdown("m").with_range(range()).to_json();
        assert_eq!(ranged["range"]["end"]["line"], 3);
    }

    #[test]
    fn test_text_edit_round_trips_and_rejects_bad_shapes() {
        let edit = TextEdit::new(range(), "é\n");
        assert_eq!(TextEdit::from_json(&edit.to_json()).ok(), Some(edit));
        assert!(TextEdit::from_json(&json!({ "range": {}, "newText": "" })).is_err());
        assert!(TextEdit::from_json(&json!({ "newText": "" })).is_err());
    }
}
