//! Protocol positions: [`Position`], [`Range`], and the [`Encoding`] their
//! columns are counted in.

use serde::{Deserialize, Serialize};

/// A place in a document as the protocol spells it: a zero-based line and a
/// zero-based column within that line.
///
/// The column, `character`, is counted in the units of the session's
/// [`Encoding`] — UTF-16 code units unless the client offered something else
/// during `initialize`. A [`Document`](crate::Document) converts between
/// positions and the byte offsets the rest of the `-lang` family works in;
/// handlers rarely build positions by hand except to answer a request.
///
/// Serializes to and from the protocol's `{"line": …, "character": …}` shape,
/// so it can sit directly in a handler's own parameter and result types.
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
/// let json = lsp_lang::serde_json::to_string(&pos).unwrap();
/// assert_eq!(json, r#"{"line":2,"character":4}"#);
/// ```
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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
}

/// A stretch of a document between two [`Position`]s: `start` inclusive, `end`
/// exclusive, as in the protocol.
///
/// An empty range (`start == end`) marks a point, such as a cursor. Serializes
/// to and from the protocol's `{"start": …, "end": …}` shape.
///
/// # Examples
///
/// ```
/// use lsp_lang::{Position, Range};
///
/// let word = Range::new(Position::new(3, 4), Position::new(3, 9));
/// assert_eq!(word.start.character, 4);
///
/// let json = lsp_lang::serde_json::to_string(&word).unwrap();
/// assert_eq!(
///     json,
///     r#"{"start":{"line":3,"character":4},"end":{"line":3,"character":9}}"#
/// );
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
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
}
