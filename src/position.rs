//! Protocol positions: [`Position`], [`Range`], and the [`Encoding`] their
//! columns are counted in.

use jsonx_lib::Value;

use crate::{
    json::{req_object, req_str, req_u32, to_value, write_u64},
    response::ResponseError,
};

/// A place in a document as the protocol spells it: a zero-based line and a
/// zero-based column within that line.
///
/// The column, `character`, is counted in the units of the session's
/// [`Encoding`]: UTF-16 code units unless the client offered UTF-8 during
/// `initialize`. A [`Document`](crate::Document) converts between positions and
/// the byte offsets the rest of the `-lang` family works in; handlers rarely
/// build positions by hand except to answer a request.
///
/// Converts to and from the protocol's `{"line": …, "character": …}` shape
/// ([`to_json`](Position::to_json), [`from_json`](Position::from_json)).
/// Positions order by line, then column.
///
/// # Examples
///
/// ```
/// use lsp_lang::Position;
///
/// let pos = Position::new(2, 4);
/// assert_eq!((pos.line, pos.character), (2, 4));
/// assert!(Position::new(1, 99) < Position::new(2, 0));
///
/// assert_eq!(pos.to_json().to_string(), r#"{"line":2,"character":4}"#);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    /// Zero-based line number. Lines end at `\n`, `\r\n`, or a lone `\r`.
    pub line: u32,
    /// Zero-based column within the line, in the session's [`Encoding`].
    pub character: u32,
}

impl Position {
    /// Builds a position from a zero-based line and column.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Position;
    ///
    /// assert_eq!(Position::new(0, 0), Position::default());
    /// ```
    #[inline]
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// The protocol's JSON for this position.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Position;
    ///
    /// assert_eq!(Position::new(1, 2).to_json()["character"], 2);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    /// Reads a position from `{"line": …, "character": …}`.
    ///
    /// # Errors
    ///
    /// [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS) when either member is
    /// missing or is not an unsigned 32-bit integer, so a handler can return it
    /// with `?`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, Position};
    ///
    /// let pos = Position::from_json(&json!({ "line": 3, "character": 9 }))?;
    /// assert_eq!(pos, Position::new(3, 9));
    /// assert!(Position::from_json(&json!({ "line": -1, "character": 0 })).is_err());
    /// # Ok::<(), lsp_lang::ResponseError>(())
    /// ```
    pub fn from_json(value: &Value<'_>) -> Result<Self, ResponseError> {
        if !matches!(value, Value::Object(_)) {
            return Err(ResponseError::invalid_params(format!(
                "invalid params: a position must be an object, not {}",
                value.kind()
            )));
        }
        Ok(Self {
            line: req_u32(value, "line")?,
            character: req_u32(value, "character")?,
        })
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"line\":");
        write_u64(out, u64::from(self.line));
        out.push_str(",\"character\":");
        write_u64(out, u64::from(self.character));
        out.push('}');
    }
}

/// A stretch of a document between two [`Position`]s: `start` inclusive, `end`
/// exclusive, as in the protocol.
///
/// An empty range (`start == end`) marks a point, such as a cursor.
///
/// # Examples
///
/// ```
/// use lsp_lang::{Position, Range};
///
/// let word = Range::new(Position::new(3, 4), Position::new(3, 9));
/// assert_eq!(word.start.character, 4);
/// assert_eq!(
///     word.to_json().to_string(),
///     r#"{"start":{"line":3,"character":4},"end":{"line":3,"character":9}}"#
/// );
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Range {
    /// Where the range begins (inclusive).
    pub start: Position,
    /// Where the range ends (exclusive).
    pub end: Position,
}

impl Range {
    /// Builds a range from its two ends.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Position, Range};
    ///
    /// let cursor = Range::new(Position::new(1, 2), Position::new(1, 2));
    /// assert_eq!(cursor.start, cursor.end);
    /// ```
    #[inline]
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// The protocol's JSON for this range.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Position, Range};
    ///
    /// let range = Range::new(Position::new(0, 0), Position::new(0, 3));
    /// assert_eq!(range.to_json()["end"]["character"], 3);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    /// Reads a range from `{"start": …, "end": …}`.
    ///
    /// # Errors
    ///
    /// [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS) when either end is
    /// missing or is not a position.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, Position, Range};
    ///
    /// let range = Range::from_json(&json!({
    ///     "start": { "line": 0, "character": 1 },
    ///     "end": { "line": 2, "character": 3 },
    /// }))?;
    /// assert_eq!(range, Range::new(Position::new(0, 1), Position::new(2, 3)));
    /// # Ok::<(), lsp_lang::ResponseError>(())
    /// ```
    pub fn from_json(value: &Value<'_>) -> Result<Self, ResponseError> {
        if !matches!(value, Value::Object(_)) {
            return Err(ResponseError::invalid_params(format!(
                "invalid params: a range must be an object, not {}",
                value.kind()
            )));
        }
        Ok(Self {
            start: Position::from_json(req_object(value, "start")?)?,
            end: Position::from_json(req_object(value, "end")?)?,
        })
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"start\":");
        self.start.write(out);
        out.push_str(",\"end\":");
        self.end.write(out);
        out.push('}');
    }
}

/// The unit a [`Position`]'s column is counted in.
///
/// The protocol's default is UTF-16 code units, which every client supports.
/// Since LSP 3.17 a client may offer others in `general.positionEncodings`;
/// the server picks UTF-8 whenever it is offered, because a byte column
/// converts to a byte offset with no scanning at all, and otherwise stays with
/// UTF-16, the one encoding every client must accept. The choice is announced
/// to the client in the `initialize` result and applies to every document for
/// the whole session.
///
/// | Variant | Unit | `é` | `😀` |
/// |---|---|---|---|
/// | `Utf8` | bytes | 2 | 4 |
/// | `Utf16` | UTF-16 code units | 1 | 2 |
///
/// # Examples
///
/// ```
/// use lsp_lang::Encoding;
///
/// assert_eq!(Encoding::default(), Encoding::Utf16);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Encoding {
    /// Columns count UTF-8 bytes (`"utf-8"`).
    Utf8,
    /// Columns count UTF-16 code units (`"utf-16"`), the protocol default.
    #[default]
    Utf16,
}

impl Encoding {
    /// The protocol's name for this encoding, as sent in `positionEncoding`.
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16 => "utf-16",
        }
    }

    /// Picks the session encoding from the client's offered list: UTF-8 when
    /// offered, else the protocol's mandatory UTF-16.
    pub(crate) fn negotiate<'a>(offered: impl IntoIterator<Item = &'a str>) -> Self {
        if offered.into_iter().any(|name| name == "utf-8") {
            Encoding::Utf8
        } else {
            Encoding::Utf16
        }
    }
}

/// Reads `textDocument` and `position` from a request's parameters: the shape
/// shared by hover, definition, and completion.
pub(crate) fn document_position(params: &Value<'_>) -> Result<(String, Position), ResponseError> {
    let uri = req_str(req_object(params, "textDocument")?, "uri")?.to_owned();
    let position = Position::from_json(req_object(params, "position")?)?;
    Ok((uri, position))
}

/// Reads `textDocument.uri`: the shape of document-wide requests.
pub(crate) fn document_uri(params: &Value<'_>) -> Result<String, ResponseError> {
    Ok(req_str(req_object(params, "textDocument")?, "uri")?.to_owned())
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;

    #[test]
    fn test_encoding_negotiate_prefers_utf8() {
        assert_eq!(Encoding::negotiate(["utf-16", "utf-8"]), Encoding::Utf8);
        assert_eq!(Encoding::negotiate(["utf-32", "utf-16"]), Encoding::Utf16);
        assert_eq!(Encoding::negotiate([]), Encoding::Utf16);
    }

    #[test]
    fn test_encoding_as_str_matches_protocol_names() {
        assert_eq!(Encoding::Utf8.as_str(), "utf-8");
        assert_eq!(Encoding::Utf16.as_str(), "utf-16");
    }

    #[test]
    fn test_position_orders_by_line_then_column() {
        assert!(Position::new(0, 9) < Position::new(1, 0));
        assert!(Position::new(1, 1) < Position::new(1, 2));
    }

    #[test]
    fn test_position_and_range_round_trip_through_json() {
        let range = Range::new(Position::new(0, u32::MAX), Position::new(u32::MAX, 0));
        assert_eq!(Range::from_json(&range.to_json()).ok(), Some(range));
    }

    #[test]
    fn test_position_from_json_rejects_wrong_shapes() {
        for bad in [
            json!(null),
            json!([1, 2]),
            json!({ "line": 1 }),
            json!({ "line": 1, "character": "2" }),
            json!({ "line": 1.5, "character": 2 }),
            json!({ "line": 4294967296_u64, "character": 2 }),
        ] {
            let err = Position::from_json(&bad).err();
            assert_eq!(
                err.map(|e| e.code()),
                Some(ResponseError::INVALID_PARAMS),
                "{bad}"
            );
        }
        assert!(Range::from_json(&json!({ "start": {} })).is_err());
        assert!(Range::from_json(&json!(5)).is_err());
    }

    #[test]
    fn test_document_position_reads_the_request_shape() {
        let params = json!({ "textDocument": { "uri": "file:///a" }, "position": { "line": 1, "character": 2 } });
        assert_eq!(
            document_position(&params).ok(),
            Some(("file:///a".to_owned(), Position::new(1, 2)))
        );
        assert!(document_position(&json!({ "position": {} })).is_err());
        assert_eq!(document_uri(&params).ok().as_deref(), Some("file:///a"));
    }
}
