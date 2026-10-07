//! [`Error`]: what ends a session, or fails a write to the client.

use std::{fmt, io};

/// Something went wrong in the server itself — as opposed to a request that
/// failed, which is a [`ResponseError`](crate::ResponseError) sent to the
/// client.
///
/// [`Server::run`](crate::Server::run) returns one when the session cannot go
/// on: the connection failed or closed, the client's framing broke, or the
/// client exited without the shutdown handshake. In each case the editor is
/// gone or the stream is unusable, so the right response is to exit the
/// process, with a non-zero status, as the protocol asks. [`Error::Json`] is
/// the exception: it comes from serializing a handler's own values
/// ([`Reply::new`](crate::Reply::new), [`Context::notify`](crate::Context::notify),
/// [`Context::request`](crate::Context::request)) and never ends the session.
///
/// Malformed JSON *inside* a well-framed message is not an `Error`: the server
/// answers it with a protocol error and carries on.
///
/// # Examples
///
/// ```
/// use lsp_lang::Error;
///
/// fn exit_code(result: Result<(), Error>) -> i32 {
///     match result {
///         Ok(()) => 0, // `shutdown` then `exit`: a clean stop.
///         Err(Error::ExitWithoutShutdown) => 1,
///         Err(_) => 1,
///     }
/// }
/// assert_eq!(exit_code(Err(Error::Disconnected)), 1);
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Reading from or writing to the client failed.
    ///
    /// The connection is broken; exit. The wrapped error is the transport's.
    Io(io::Error),

    /// The client's input ended before it sent `exit` — typically the editor
    /// closed or crashed — or ended partway through a message.
    Disconnected,

    /// A message header could not be read, so the stream cannot be split into
    /// messages any more. `reason` says which rule was broken (no
    /// `Content-Length`, a malformed length, an over-long header line).
    InvalidHeader {
        /// What was wrong with the header.
        reason: &'static str,
    },

    /// A message announced a body larger than the server's limit (see
    /// [`Server::with_message_limit`](crate::Server::with_message_limit)).
    /// The body is not read; the session ends rather than buffer it.
    TooLarge {
        /// The announced `Content-Length`, in bytes.
        len: usize,
        /// The configured limit, in bytes.
        limit: usize,
    },

    /// The client sent `exit` without a `shutdown` request first. The protocol
    /// asks the server to exit with status `1` in this case.
    ExitWithoutShutdown,

    /// A handler's value could not be serialized to JSON — a map with
    /// non-string keys, or a `Serialize` implementation that failed. Nothing
    /// was sent. Inside a request handler, `?` turns it into an
    /// [`INTERNAL_ERROR`](crate::ResponseError::INTERNAL_ERROR) response.
    Json(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "language server I/O failed: {e}"),
            Error::Disconnected => f.write_str("the client closed the connection before `exit`"),
            Error::InvalidHeader { reason } => write!(f, "invalid message header: {reason}"),
            Error::TooLarge { len, limit } => write!(
                f,
                "a message of {len} bytes exceeds the {limit} byte message limit"
            ),
            Error::ExitWithoutShutdown => {
                f.write_str("the client sent `exit` without requesting `shutdown` first")
            }
            Error::Json(e) => write!(f, "could not serialize a value to JSON: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn test_error_display_names_the_problem() {
        let cases = [
            Error::Io(io::Error::other("pipe")),
            Error::Disconnected,
            Error::InvalidHeader {
                reason: "no Content-Length",
            },
            Error::TooLarge { len: 10, limit: 5 },
            Error::ExitWithoutShutdown,
        ];
        for e in cases {
            assert!(!e.to_string().is_empty());
        }
        assert!(
            Error::TooLarge { len: 10, limit: 5 }
                .to_string()
                .contains("10")
        );
    }

    #[test]
    fn test_error_source_exposes_wrapped_errors() {
        assert!(Error::Io(io::Error::other("pipe")).source().is_some());
        assert!(Error::Disconnected.source().is_none());
    }
}
