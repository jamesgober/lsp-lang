//! The JSON a handler reads and writes: borrowed parameters ([`Json`]),
//! pre-serialized results ([`Reply`]), and protocol errors ([`ResponseError`]).

use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// A JSON value from an incoming message, kept as the raw text the client sent
/// and parsed only when, and into whatever type, the handler asks for.
///
/// The server never builds a value tree for a message. It reads the envelope in
/// place and hands the handler the parameters (or a response's result) as a
/// slice of the message. [`parse`](Json::parse) deserializes that slice
/// straight into the handler's own type, borrowing strings from the message
/// where they contain no escapes. Absent parameters read as `null`.
///
/// `Json` is a borrowed view: it is `Copy` and lives as long as the message.
///
/// # Examples
///
/// Parameters arrive as `Json` in [`Handler::request`](crate::Handler::request)
/// and [`Handler::notification`](crate::Handler::notification):
///
/// ```
/// use lsp_lang::{Json, Position};
///
/// #[derive(serde::Deserialize)]
/// struct Params<'a> {
///     #[serde(borrow)]
///     uri: &'a str,
///     position: Position,
/// }
///
/// fn uri_and_line(params: Json<'_>) -> Option<(&str, u32)> {
///     let p: Params<'_> = params.parse().ok()?;
///     Some((p.uri, p.position.line))
/// }
/// # let raw = r#"{"uri":"file:///a.lisp","position":{"line":3,"character":1}}"#;
/// # assert_eq!(uri_and_line(lsp_lang::Json::from_raw(raw)), Some(("file:///a.lisp", 3)));
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Json<'a> {
    raw: &'a str,
}

impl<'a> Json<'a> {
    /// `null`, standing in for absent parameters.
    pub(crate) const NULL: Json<'static> = Json { raw: "null" };

    /// Wraps raw JSON text.
    ///
    /// The server builds every `Json` a handler receives; this constructor is
    /// for driving a handler's logic directly — in its unit tests, say. The
    /// text is not checked here; [`parse`](Json::parse) reports any syntax
    /// error.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Json;
    ///
    /// let params = Json::from_raw(r#"{"line":1,"character":2}"#);
    /// let pos: lsp_lang::Position = params.parse()?;
    /// assert_eq!(pos.character, 2);
    /// # Ok::<(), lsp_lang::ResponseError>(())
    /// ```
    #[inline]
    #[must_use]
    pub const fn from_raw(raw: &'a str) -> Self {
        Self { raw }
    }

    /// Deserializes the value into `T`.
    ///
    /// `T` may borrow from the message (`&'a str` fields, `Cow<'a, str>` with
    /// `#[serde(borrow)]`), so reading a URI or a short string costs no
    /// allocation. Use [`Value`](crate::Value) to take the value whole.
    ///
    /// # Errors
    ///
    /// A [`ResponseError`] with code [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS)
    /// whose message is `serde_json`'s account of the mismatch, so a handler can
    /// return it to the client with `?` unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Json, ResponseError, Value};
    ///
    /// let params = Json::from_raw(r#"{"query":"main"}"#);
    ///
    /// // Into a type of your own, or a dynamic value.
    /// let value: Value = params.parse()?;
    /// assert_eq!(value["query"], "main");
    ///
    /// // A mismatch is an `invalid params` error, ready to send back.
    /// let err = params.parse::<u32>().unwrap_err();
    /// assert_eq!(err.code(), ResponseError::INVALID_PARAMS);
    /// # Ok::<(), ResponseError>(())
    /// ```
    pub fn parse<T: Deserialize<'a>>(self) -> Result<T, ResponseError> {
        serde_json::from_str(self.raw).map_err(|e| {
            ResponseError::new(
                ResponseError::INVALID_PARAMS,
                format!("invalid params: {e}"),
            )
        })
    }

    /// Returns the raw JSON text, exactly as the client sent it.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Json;
    ///
    /// assert_eq!(Json::from_raw("[1, 2]").as_str(), "[1, 2]");
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.raw
    }
}

impl fmt::Debug for Json<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.raw)
    }
}

/// A request's successful result, already serialized to JSON.
///
/// A handler answers a request with `Ok(Reply)`. Serializing the result
/// directly to text — rather than building a [`Value`](crate::Value) tree first
/// — keeps a large result such as a completion list to one pass and one
/// allocation; the server copies the text into the response envelope verbatim.
///
/// # Examples
///
/// ```
/// use lsp_lang::{json, Position, Range, Reply};
///
/// // Any `Serialize` type, including the crate's own position types…
/// let reply = Reply::new(&Range::new(Position::new(0, 0), Position::new(0, 3)))?;
/// assert_eq!(
///     reply.as_str(),
///     r#"{"start":{"line":0,"character":0},"end":{"line":0,"character":3}}"#
/// );
///
/// // …or a value built with `json!`.
/// let reply = Reply::new(&json!({ "contents": "a number" }))?;
/// assert_eq!(reply.as_str(), r#"{"contents":"a number"}"#);
///
/// // "No result" (no hover here, no definition found) is `null`.
/// assert_eq!(Reply::null().as_str(), "null");
/// # Ok::<(), lsp_lang::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    json: Cow<'static, str>,
}

impl Reply {
    /// Serializes `value` as the result.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] if `value`'s `Serialize` implementation fails — a map
    /// with non-string keys, or a custom implementation that returns an error.
    /// The `?` operator turns it into an
    /// [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR) response inside a
    /// request handler.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Reply;
    ///
    /// let reply = Reply::new(&["one", "two"])?;
    /// assert_eq!(reply.as_str(), r#"["one","two"]"#);
    /// # Ok::<(), lsp_lang::Error>(())
    /// ```
    pub fn new<T: Serialize + ?Sized>(value: &T) -> Result<Self, Error> {
        let json = serde_json::to_string(value).map_err(Error::Json)?;
        Ok(Self {
            json: Cow::Owned(json),
        })
    }

    /// The `null` result, without allocating.
    ///
    /// The protocol uses `null` for "nothing here": no hover at the position,
    /// no definition found, and the result of `shutdown`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Reply;
    ///
    /// assert_eq!(Reply::null(), Reply::new(&()).unwrap());
    /// ```
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        Self {
            json: Cow::Borrowed("null"),
        }
    }

    /// Returns the serialized JSON.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Reply;
    ///
    /// assert_eq!(Reply::new(&42)?.as_str(), "42");
    /// # Ok::<(), lsp_lang::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.json
    }
}

/// A JSON-RPC error: the failed outcome of a request, sent back to the client
/// in place of a result.
///
/// A handler returns one from [`Handler::request`](crate::Handler::request) to
/// refuse or fail a request; the server also sends them itself for protocol
/// violations (a request before `initialize`, an unknown method, malformed
/// JSON). The associated constants are the codes JSON-RPC and the protocol
/// define; pick the one that tells the client what to do next — for instance
/// [`CONTENT_MODIFIED`](ResponseError::CONTENT_MODIFIED) asks it to retry
/// against the newer text, while [`REQUEST_FAILED`](ResponseError::REQUEST_FAILED)
/// says the request was understood but could not be served.
///
/// # Examples
///
/// ```
/// use lsp_lang::ResponseError;
///
/// let err = ResponseError::new(ResponseError::REQUEST_FAILED, "no project loaded");
/// assert_eq!(err.code(), -32803);
/// assert_eq!(err.message(), "no project loaded");
/// assert_eq!(err.to_string(), "no project loaded (code -32803)");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResponseError {
    code: i32,
    message: String,
}

impl ResponseError {
    /// Invalid JSON was received. Sent by the server; a handler never sees it.
    pub const PARSE_ERROR: i32 = -32700;
    /// The message is not a valid request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist or is not supported. The default answer of
    /// [`Handler::request`](crate::Handler::request).
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// The parameters are invalid. What [`Json::parse`] returns on a mismatch.
    pub const INVALID_PARAMS: i32 = -32602;
    /// An internal error, such as a result that failed to serialize.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// A request arrived before `initialize`. Sent by the server.
    pub const SERVER_NOT_INITIALIZED: i32 = -32002;
    /// The request was understood but failed; the message says why.
    pub const REQUEST_FAILED: i32 = -32803;
    /// The server cancelled the request; the client may retry it.
    pub const SERVER_CANCELLED: i32 = -32802;
    /// The document changed while the request was served, so its result would
    /// be stale; the client should retry against the new content.
    pub const CONTENT_MODIFIED: i32 = -32801;
    /// The client cancelled the request.
    pub const REQUEST_CANCELLED: i32 = -32800;

    /// Builds an error with `code` and a human-readable `message`.
    ///
    /// The message is shown to the user by some clients; say what failed and,
    /// where it helps, what to do about it.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::ResponseError;
    ///
    /// let err = ResponseError::new(ResponseError::INVALID_PARAMS, "position is outside the document");
    /// assert_eq!(err.code(), ResponseError::INVALID_PARAMS);
    /// ```
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the error code.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::ResponseError;
    ///
    /// assert_eq!(ResponseError::new(-1, "x").code(), -1);
    /// ```
    #[inline]
    #[must_use]
    pub const fn code(&self) -> i32 {
        self.code
    }

    /// Returns the message.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::ResponseError;
    ///
    /// assert_eq!(ResponseError::new(-1, "boom").message(), "boom");
    /// ```
    #[inline]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for ResponseError {}

impl From<Error> for ResponseError {
    /// Turns a library error met while serving a request — in practice a
    /// result or notification that failed to serialize — into an
    /// [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR) response.
    fn from(error: Error) -> Self {
        Self::new(Self::INTERNAL_ERROR, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn test_json_parse_borrows_unescaped_strings() {
        let json = Json::from_raw(r#"{"uri":"file:///x"}"#);
        #[derive(Deserialize)]
        struct P<'a> {
            #[serde(borrow)]
            uri: Cow<'a, str>,
        }
        let parsed: Result<P<'_>, _> = json.parse();
        assert!(matches!(
            parsed.map(|p| p.uri),
            Ok(Cow::Borrowed("file:///x"))
        ));
    }

    #[test]
    fn test_json_parse_mismatch_returns_invalid_params() {
        let err = Json::NULL.parse::<u32>().err();
        assert_eq!(err.map(|e| e.code()), Some(ResponseError::INVALID_PARAMS));
    }

    #[test]
    fn test_reply_new_unserializable_value_returns_err() {
        let mut map = BTreeMap::new();
        let _previous = map.insert((1, 2), "tuple keys are not JSON object keys");
        assert!(matches!(Reply::new(&map), Err(Error::Json(_))));
    }

    #[test]
    fn test_response_error_from_error_is_internal() {
        let err = ResponseError::from(Error::Disconnected);
        assert_eq!(err.code(), ResponseError::INTERNAL_ERROR);
        assert!(!err.message().is_empty());
    }
}
