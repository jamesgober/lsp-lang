//! [`Error`]: what ends a session, or fails a write to the client.

use std::{fmt, io};

use jsonx_lib::Kind;
use rpc_io::FrameError;

/// Something went wrong in the server itself, as opposed to a request that
/// failed, which is a [`ResponseError`](crate::ResponseError) sent to the
/// client.
///
/// [`Server::run`](crate::Server::run) returns one when the session cannot go
/// on: the connection failed or closed, the client's framing broke, or the
/// client exited without the shutdown handshake. In each case the editor is
/// gone or the stream is unusable, so the right response is to exit the
/// process with a non-zero status, as the protocol asks. [`Error::Closed`] and
/// [`Error::InvalidParams`] are the exceptions: they come from a handler's
/// own sends ([`Context::notify`](crate::Context::notify),
/// [`Context::request`](crate::Context::request)) and describe that one send.
///
/// Malformed JSON *inside* a well-framed message is not an `Error`: the server
/// answers it with a protocol error and carries on, as it does for a message
/// over the size limit.
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

    /// The client's input ended before it sent `exit`, typically because the
    /// editor closed or crashed, or ended partway through a message.
    Disconnected,

    /// The input lost its base-protocol framing, so the stream cannot be split
    /// into messages any more: no `Content-Length`, a malformed length, an
    /// over-long or malformed header. A message that is merely over the size
    /// limit is not this error; it is skipped and refused, and the session
    /// goes on (see
    /// [`Server::with_message_limit`](crate::Server::with_message_limit)).
    Frame(FrameError),

    /// The client sent `exit` without a `shutdown` request first. The protocol
    /// asks the server to exit with status `1` in this case.
    ExitWithoutShutdown,

    /// The connection is closed, so nothing more can be sent: the session has
    /// ended, or writing to the client failed earlier.
    Closed,

    /// A notification's or request's `params` was not an array, an object, or
    /// `null`. Nothing was sent. Inside a request handler, `?` turns it into an
    /// [`INTERNAL_ERROR`](crate::ResponseError::INTERNAL_ERROR) response.
    InvalidParams(Kind),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "language server I/O failed: {e}"),
            Error::Disconnected => f.write_str("the client closed the connection before `exit`"),
            Error::Frame(e) => write!(f, "invalid message framing: {e}"),
            Error::ExitWithoutShutdown => {
                f.write_str("the client sent `exit` without requesting `shutdown` first")
            }
            Error::Closed => f.write_str("the connection is closed"),
            Error::InvalidParams(kind) => {
                write!(f, "params must be an array, an object, or null, not {kind}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Frame(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<rpc_io::Error> for Error {
    /// Maps the transport's error: framing and I/O failures keep their
    /// meaning, bad outgoing params stay bad params, and anything else about a
    /// closed or already-answered connection is [`Error::Closed`].
    fn from(error: rpc_io::Error) -> Self {
        match error {
            rpc_io::Error::Io(e) => Error::Io(e),
            rpc_io::Error::Frame(e) => Error::Frame(e),
            rpc_io::Error::InvalidParams(kind) => Error::InvalidParams(kind),
            _ => Error::Closed,
        }
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
            Error::Frame(FrameError::MissingContentLength),
            Error::ExitWithoutShutdown,
            Error::Closed,
            Error::InvalidParams(Kind::Number),
        ];
        for e in cases {
            assert!(!e.to_string().is_empty());
        }
        assert!(
            Error::InvalidParams(Kind::Number)
                .to_string()
                .contains("number")
        );
    }

    #[test]
    fn test_error_source_exposes_wrapped_errors() {
        assert!(Error::Io(io::Error::other("pipe")).source().is_some());
        assert!(Error::Frame(FrameError::InvalidHeader).source().is_some());
        assert!(Error::Disconnected.source().is_none());
    }

    #[test]
    fn test_error_from_transport_error() {
        assert!(matches!(Error::from(rpc_io::Error::Closed), Error::Closed));
        assert!(matches!(
            Error::from(rpc_io::Error::AlreadyAnswered),
            Error::Closed
        ));
        assert!(matches!(
            Error::from(rpc_io::Error::InvalidParams(Kind::Bool)),
            Error::InvalidParams(Kind::Bool)
        ));
        assert!(matches!(
            Error::from(rpc_io::Error::Frame(FrameError::Truncated)),
            Error::Frame(FrameError::Truncated)
        ));
    }
}
