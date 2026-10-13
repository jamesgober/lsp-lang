//! Test support: in-memory pipes and a scripted client that talks to a running
//! server over them, the way an editor talks over stdio.
//!
//! The one-shot tests hand the server a complete script and read everything it
//! wrote afterwards. Those cannot express a conversation (send a request, wait
//! for its answer, react), and they cannot hold the server in the middle of
//! one. [`Live`] can: the server runs on its own thread, reading from one pipe
//! and writing to another, and the test sends and receives message by message.

#![allow(dead_code)] // Each test file uses a different subset.

use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{Arc, Condvar, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use lsp_lang::{Error, Handler, Server, Value, json};

/// How long a test waits for the server before it calls the server stuck.
pub const PATIENCE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct PipeState {
    bytes: VecDeque<u8>,
    closed: bool,
}

type Shared = Arc<(Mutex<PipeState>, Condvar)>;

/// The writing end of an in-memory pipe. Dropping it closes the pipe: the
/// reader sees end of input once it has read what was written.
pub struct PipeWriter {
    shared: Shared,
}

/// The reading end of an in-memory pipe.
pub struct PipeReader {
    shared: Shared,
    /// Most bytes one `read` returns, to exercise messages split across reads.
    chunk: usize,
}

/// A pipe: bytes written to the first end are read from the second.
pub fn pipe() -> (PipeWriter, PipeReader) {
    let shared: Shared = Arc::default();
    (
        PipeWriter {
            shared: Arc::clone(&shared),
        },
        PipeReader {
            shared,
            chunk: usize::MAX,
        },
    )
}

impl PipeReader {
    /// Limits every `read` to `chunk` bytes.
    pub fn with_chunk(mut self, chunk: usize) -> Self {
        self.chunk = chunk.max(1);
        self
    }

    /// Reads what is available, waiting up to `patience` for the first byte.
    /// `Ok(0)` is end of input; a timeout is an error of kind `TimedOut`.
    pub fn read_within(&mut self, buf: &mut [u8], patience: Duration) -> io::Result<usize> {
        let (lock, wake) = &*self.shared;
        let deadline = Instant::now() + patience;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if !state.bytes.is_empty() {
                let n = buf.len().min(self.chunk).min(state.bytes.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = state.bytes.pop_front().unwrap_or(0);
                }
                return Ok(n);
            }
            if state.closed {
                return Ok(0);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            state = wake
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.read_within(buf, PATIENCE) {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            other => other,
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (lock, wake) = &*self.shared;
        lock.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bytes
            .extend(buf);
        wake.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        wake.notify_all();
    }
}

/// Frames `body` as the base protocol does.
pub fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// A server running on its own thread behind a pair of pipes, and a client that
/// drives it message by message.
pub struct Live<H: Handler<crate::Lisp> + Send + 'static> {
    to_server: Option<PipeWriter>,
    from_server: PipeReader,
    pending: Vec<u8>,
    next_id: u64,
    /// Every message received so far that was not asked for by `recv_response`
    /// or `recv_method`, in order.
    pub skipped: Vec<Value<'static>>,
    thread: Option<JoinHandle<Ended<H>>>,
}

/// How a session ended, and the server it ran on.
type Ended<H> = (Result<(), Error>, Server<crate::Lisp, H>);

impl<H: Handler<crate::Lisp> + Send + 'static> Live<H> {
    /// Starts `server` behind pipes; the server reads in `chunk`-byte pieces.
    pub fn start_chunked(mut server: Server<crate::Lisp, H>, chunk: usize) -> Self {
        let (to_server, server_input) = pipe();
        let (server_output, from_server) = pipe();
        let thread = thread::spawn(move || {
            let result = server.run(server_input.with_chunk(chunk), server_output);
            (result, server)
        });
        Self {
            to_server: Some(to_server),
            from_server,
            pending: Vec::new(),
            next_id: 0,
            skipped: Vec::new(),
            thread: Some(thread),
        }
    }

    /// Starts `server` behind pipes.
    pub fn start(server: Server<crate::Lisp, H>) -> Self {
        Self::start_chunked(server, usize::MAX)
    }

    /// Writes bytes to the server exactly as given.
    pub fn send_bytes(&mut self, bytes: &[u8]) {
        if let Some(pipe) = &mut self.to_server {
            // A server that has ended has dropped its end; the test notices
            // that when it reads.
            let _written = pipe.write_all(bytes);
        }
    }

    /// Sends one framed message.
    pub fn send(&mut self, message: &Value<'_>) {
        self.send_bytes(&frame(&message.to_string()));
    }

    /// Sends one framed body exactly as given.
    pub fn send_raw(&mut self, body: &str) {
        self.send_bytes(&frame(body));
    }

    /// Sends a request with the next id and returns the id.
    pub fn request(&mut self, method: &str, params: Value<'_>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Sends a notification.
    pub fn notify(&mut self, method: &str, params: Value<'_>) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// `initialize` offering `encodings`, waits for its answer, then sends
    /// `initialized`. Returns the answer.
    pub fn initialize(&mut self, encodings: &[&str]) -> Value<'static> {
        let id = self.request(
            "initialize",
            json!({ "capabilities": { "general": { "positionEncodings": encodings.to_vec() } } }),
        );
        let answer = self.recv_response(id);
        self.notify("initialized", json!({}));
        answer
    }

    /// Opens a document.
    pub fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "lisp", "version": 1, "text": text } }),
        );
    }

    /// `shutdown`, waits for its answer, then `exit`. Returns the answer.
    pub fn shutdown(&mut self) -> Value<'static> {
        let id = self.request("shutdown", Value::Null);
        let answer = self.recv_response(id);
        self.notify("exit", Value::Null);
        answer
    }

    /// Reads the next message from the server, or `None` if it ended or went
    /// quiet for [`PATIENCE`].
    pub fn recv(&mut self) -> Option<Value<'static>> {
        loop {
            if let Some(message) = self.take_message() {
                return Some(message);
            }
            let mut buf = [0u8; 4096];
            match self.from_server.read_within(&mut buf, PATIENCE) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
            }
        }
    }

    /// Reads messages until the response to request `id`, keeping the others
    /// in [`skipped`](Live::skipped). `null` if the server ended first.
    pub fn recv_response(&mut self, id: u64) -> Value<'static> {
        while let Some(message) = self.recv() {
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
            self.skipped.push(message);
        }
        Value::Null
    }

    /// Reads messages until one with this `method`, keeping the others.
    pub fn recv_method(&mut self, method: &str) -> Value<'static> {
        while let Some(message) = self.recv() {
            if message["method"] == method {
                return message;
            }
            self.skipped.push(message);
        }
        Value::Null
    }

    /// Closes the server's input, waits for it to end, and returns how it
    /// ended, the server (for its handler), and everything not yet read.
    pub fn finish(
        mut self,
    ) -> (
        Result<(), Error>,
        Server<crate::Lisp, H>,
        Vec<Value<'static>>,
    ) {
        drop(self.to_server.take());
        let mut rest = std::mem::take(&mut self.skipped);
        while let Some(message) = self.recv() {
            rest.push(message);
        }
        let Some(thread) = self.thread.take() else {
            return (Err(Error::Closed), unreachable_server(), rest);
        };
        match thread.join() {
            Ok((result, server)) => (result, server, rest),
            Err(_) => (Err(Error::Closed), unreachable_server(), rest),
        }
    }

    /// Cuts one complete framed message out of the bytes read so far.
    fn take_message(&mut self) -> Option<Value<'static>> {
        let split = self.pending.windows(4).position(|w| w == b"\r\n\r\n")?;
        let header = String::from_utf8_lossy(&self.pending[..split]).into_owned();
        let len = header
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|n| n.trim().parse::<usize>().ok())?;
        let end = split + 4 + len;
        if self.pending.len() < end {
            return None;
        }
        let body: Vec<u8> = self.pending.drain(..end).skip(split + 4).collect();
        Some(lsp_lang::jsonx_lib::parse_bytes(&body).map_or(Value::Null, Value::into_owned))
    }
}

/// A placeholder for the panics-in-the-server-thread path, which the tests
/// treat as failure through the `Err(Closed)` they pair it with. It exists
/// only because `finish` must return a server.
#[allow(clippy::panic)]
fn unreachable_server<H: Handler<crate::Lisp>>() -> Server<crate::Lisp, H> {
    panic!("the server thread panicked");
}
