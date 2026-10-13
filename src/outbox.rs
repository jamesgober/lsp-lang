//! The outgoing side of the transport: a buffer the endpoint writes framed
//! messages into, and the session loop writes to the client in one `write`.
//!
//! The endpoint wants an owned, `Send` sink. The client's writer is neither
//! (a `&mut Vec<u8>` in a test, a locked stdout), and the session loop is the
//! only thread that ever sends. So the sink only appends to a shared buffer,
//! and the loop empties that buffer into the writer after every message it
//! handles and every job it runs: one system call per turn, however many
//! notifications a handler sent, and none for a turn that sent nothing.

use std::{
    io::{self, Write},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use rpc_io::{FrameError, Framing, Sink};

use crate::json::write_str;

/// Capacity above which a drained buffer is released instead of reused.
const KEEP_CAPACITY: usize = 1 << 20;

/// The shared buffer.
#[derive(Clone, Debug, Default)]
pub(crate) struct Outbox {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Outbox {
    fn lock(&self) -> MutexGuard<'_, Vec<u8>> {
        // The only code that runs under this lock appends bytes; a panic
        // elsewhere cannot leave it half-written, so poison is ignored.
        self.bytes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A sink for the endpoint that appends to this buffer.
    pub(crate) fn sink(&self) -> BufferSink {
        BufferSink {
            outbox: self.clone(),
        }
    }

    /// Writes everything buffered to `output` and flushes it. Does nothing
    /// when nothing is buffered.
    pub(crate) fn write_to<W: Write>(&self, output: &mut W) -> io::Result<()> {
        let mut bytes = std::mem::take(&mut *self.lock());
        if bytes.is_empty() {
            return Ok(());
        }
        output.write_all(&bytes)?;
        output.flush()?;
        bytes.clear();
        if bytes.capacity() <= KEEP_CAPACITY {
            let mut buffer = self.lock();
            if buffer.is_empty() {
                *buffer = bytes;
            }
        }
        Ok(())
    }

    /// Answers a message the framing skipped for being over the size limit.
    ///
    /// Its id was never read, so the answer goes under `null`, as JSON-RPC
    /// requires for a request that cannot be identified.
    pub(crate) fn refuse_oversized(&self, error: FrameError) {
        let mut body = String::from(
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32600,\"message\":",
        );
        write_str(&mut body, &format!("the message is too large: {error}"));
        body.push_str("}}");
        Framing::ContentLength.encode(body.as_bytes(), &mut self.lock());
    }

    /// Frames the notification `method` with `params` (JSON the caller wrote
    /// and vouches for) straight into the buffer.
    pub(crate) fn push_notification(&self, method: &str, params: &str) {
        const OPEN: &str = "{\"jsonrpc\":\"2.0\",\"method\":";
        const MIDDLE: &str = ",\"params\":";
        let mut name = String::with_capacity(method.len() + 2);
        write_str(&mut name, method);
        let length = OPEN.len() + name.len() + MIDDLE.len() + params.len() + 1;
        let mut out = self.lock();
        out.reserve(length + 32);
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(length.to_string().as_bytes());
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(OPEN.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(MIDDLE.as_bytes());
        out.extend_from_slice(params.as_bytes());
        out.push(b'}');
    }

    /// The bytes waiting to be written, for tests.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> Vec<u8> {
        self.lock().clone()
    }
}

/// Appends each message, framed, to an [`Outbox`].
#[derive(Debug)]
pub(crate) struct BufferSink {
    outbox: Outbox,
}

impl Sink for BufferSink {
    fn send(&mut self, message: &str) -> io::Result<()> {
        Framing::ContentLength.encode(message.as_bytes(), &mut self.outbox.lock());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sink_frames_and_write_to_drains_in_one_write() {
        let outbox = Outbox::default();
        let mut sink = outbox.sink();
        assert!(sink.send("{}").is_ok());
        assert!(sink.send("[1]").is_ok());
        assert_eq!(
            outbox.pending(),
            b"Content-Length: 2\r\n\r\n{}Content-Length: 3\r\n\r\n[1]"
        );
        let mut written = Vec::new();
        assert!(outbox.write_to(&mut written).is_ok());
        assert_eq!(written.len(), 23 + 24);
        assert!(outbox.pending().is_empty());
        // Nothing buffered: nothing written.
        let mut again = Vec::new();
        assert!(outbox.write_to(&mut again).is_ok());
        assert!(again.is_empty());
    }

    #[test]
    fn test_refuse_oversized_answers_under_null() {
        let outbox = Outbox::default();
        outbox.refuse_oversized(FrameError::TooLarge {
            length: Some(9),
            limit: 3,
        });
        let text = String::from_utf8_lossy(&outbox.pending()).into_owned();
        assert!(text.contains("\"id\":null"));
        assert!(text.contains("-32600"));
        assert!(text.contains("too large"));
    }

    #[test]
    fn test_push_notification_frames_exactly_as_the_transport_would() {
        let outbox = Outbox::default();
        outbox.push_notification("a/\"b\"", r#"{"x":[1,2]}"#);
        let body = r#"{"jsonrpc":"2.0","method":"a/\"b\"","params":{"x":[1,2]}}"#;
        let mut expected = Vec::new();
        Framing::ContentLength.encode(body.as_bytes(), &mut expected);
        assert_eq!(outbox.pending(), expected);
    }

    #[test]
    fn test_write_to_surfaces_a_failing_writer() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let outbox = Outbox::default();
        assert!(outbox.sink().send("1").is_ok());
        assert!(outbox.write_to(&mut Broken).is_err());
    }
}
