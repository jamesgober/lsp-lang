//! [`ResponseError`]: the failed outcome of a request.

use std::fmt;

use jsonx_lib::Value;
use rpc_io::{ErrorCode, ErrorObject};

use crate::error::Error;

/// A JSON-RPC error: the failed outcome of a request, sent back to the client
/// in place of a result.
///
/// A handler returns one to refuse or fail a request; the server also sends
/// them itself for protocol violations (a request before `initialize`, an
/// unknown method, malformed JSON). The associated constants are the codes
/// JSON-RPC and the protocol define; pick the one that tells the client what to
/// do next. For instance [`CONTENT_MODIFIED`](ResponseError::CONTENT_MODIFIED)
/// asks it to retry against the newer text, while
/// [`REQUEST_FAILED`](ResponseError::REQUEST_FAILED) says the request was
/// understood but could not be served.
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
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseError {
    code: i32,
    message: String,
    data: Option<Value<'static>>,
}

impl ResponseError {
    /// Invalid JSON was received. Sent by the server; a handler never sees it.
    pub const PARSE_ERROR: i32 = -32700;
    /// The message is not a valid request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist or is not supported. The default answer of
    /// the [`Handler`](crate::Handler) methods a language does not implement.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// The parameters are invalid. What the `from_json` conversions return on
    /// a mismatch.
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
    /// The client cancelled the request (`$/cancelRequest`).
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
            data: None,
        }
    }

    /// Attaches structured details (`error.data`) for clients that act on
    /// them.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, ResponseError};
    ///
    /// let err = ResponseError::new(ResponseError::REQUEST_FAILED, "stale")
    ///     .with_data(json!({ "expected": 4 }));
    /// assert_eq!(err.data().and_then(|d| d["expected"].as_i64()), Some(4));
    /// ```
    #[must_use]
    pub fn with_data(mut self, data: Value<'static>) -> Self {
        self.data = Some(data);
        self
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

    /// Returns the structured details, if any were attached or received.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::ResponseError;
    ///
    /// assert!(ResponseError::new(-1, "boom").data().is_none());
    /// ```
    #[inline]
    #[must_use]
    pub fn data(&self) -> Option<&Value<'static>> {
        self.data.as_ref()
    }

    /// An `INVALID_PARAMS` error: the shape of a request's parameters was
    /// wrong.
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, message)
    }

    /// A `METHOD_NOT_FOUND` error for `what` (a method or a feature).
    pub(crate) fn unsupported(what: &str) -> Self {
        Self::new(Self::METHOD_NOT_FOUND, format!("unhandled method `{what}`"))
    }
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for ResponseError {}

impl From<Error> for ResponseError {
    /// Turns a library error met while serving a request, in practice a send
    /// that failed, into an [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR)
    /// response.
    fn from(error: Error) -> Self {
        Self::new(Self::INTERNAL_ERROR, error.to_string())
    }
}

impl From<jsonx_lib::Error> for ResponseError {
    /// A JSON error met while reading a request's parameters becomes an
    /// [`INVALID_PARAMS`](ResponseError::INVALID_PARAMS) response.
    fn from(error: jsonx_lib::Error) -> Self {
        Self::new(Self::INVALID_PARAMS, format!("invalid params: {error}"))
    }
}

impl From<ErrorObject> for ResponseError {
    /// Reads the transport's error object: a code outside `i32` reads as
    /// `INTERNAL_ERROR`, and `data` that does not parse is dropped.
    fn from(error: ErrorObject) -> Self {
        let code = i32::try_from(error.code().0).unwrap_or(Self::INTERNAL_ERROR);
        let mut out = Self::new(code, error.message());
        out.data = error
            .data()
            .and_then(|raw| raw.parse().ok())
            .map(Value::into_owned);
        out
    }
}

impl From<ResponseError> for ErrorObject {
    fn from(error: ResponseError) -> Self {
        let mut out = ErrorObject::new(ErrorCode(i64::from(error.code)), error.message);
        if let Some(data) = error.data {
            out = out.with_data(data);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;

    #[test]
    fn test_response_error_from_error_is_internal() {
        let err = ResponseError::from(Error::Disconnected);
        assert_eq!(err.code(), ResponseError::INTERNAL_ERROR);
        assert!(!err.message().is_empty());
    }

    #[test]
    fn test_response_error_round_trips_through_the_transport_object() {
        let err = ResponseError::new(-32803, "stale").with_data(json!({ "n": 1 }));
        let object = ErrorObject::from(err.clone());
        assert_eq!(object.code(), ErrorCode(-32803));
        assert_eq!(ResponseError::from(object), err);
    }

    #[test]
    fn test_response_error_from_huge_code_is_internal() {
        let object = ErrorObject::new(ErrorCode(i64::MAX), "big");
        assert_eq!(
            ResponseError::from(object).code(),
            ResponseError::INTERNAL_ERROR
        );
    }

    #[test]
    fn test_response_error_from_json_error_is_invalid_params() {
        let err = jsonx_lib::parse("[").err().map(ResponseError::from);
        assert_eq!(err.map(|e| e.code()), Some(ResponseError::INVALID_PARAMS));
    }
}
