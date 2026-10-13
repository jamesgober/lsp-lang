//! [`Server`]: the protocol engine. It reads messages, runs the lifecycle,
//! keeps documents in sync, and calls the [`Handler`] for the rest.

use std::{
    fmt,
    io::{self, Read, Write},
};

use incremental_lang::Grammar;
use rpc_io::{Cancellation, Config, Decoder, Endpoint, Framing, Gate, Limits};

use crate::{
    context::Wire,
    error::Error,
    handler::Handler,
    jobs::{Frontend, router},
    outbox::{BufferSink, Outbox},
    session::Session,
};

/// Default for [`Server::with_message_limit`]: 256 MiB.
const DEFAULT_MESSAGE_LIMIT: usize = 256 * 1024 * 1024;

/// Most jobs allowed to queue before they are run even though more input is
/// buffered. The queue is bounded anyway by one read's worth of messages and
/// by the transport's cap on requests in flight; this is the belt to those
/// braces.
const MAX_QUEUED: usize = 4096;

/// A language server: the protocol engine around a [`Handler`].
///
/// `Server` speaks the Language Server Protocol over any byte stream: stdin
/// and stdout for an editor, a socket, or an in-memory buffer in a test. It
/// owns everything that is the same for every language:
///
/// - **Transport.** JSON-RPC 2.0 over the base protocol's `Content-Length`
///   framing, by [`rpc_io`]: every request is answered exactly once, a
///   malformed message is answered with the error code the specification
///   prescribes (under the request's own id when it can be read), and a
///   message over the [size limit](Server::with_message_limit) is refused
///   without being buffered.
/// - **Lifecycle.** `initialize` negotiates the position [`Encoding`](crate::Encoding)
///   and advertises the handler's [`Features`](crate::Features) next to the
///   server's own capabilities; requests before it are refused with
///   `ServerNotInitialized`; `shutdown` and `exit` end the session, and
///   requests after `shutdown` are refused with `InvalidRequest`.
/// - **Document sync.** `didOpen`, `didChange` (incremental or whole-text),
///   and `didClose` maintain a [`Document`](crate::Document) per open file, each
///   incrementally reparsed with the grammar `G`, and the handler hears about
///   each change through [`Handler::changed`]. A change that cannot be applied
///   never loses the document: it keeps its last good state, marked out of
///   step, until a whole-text change or a reopen brings it back.
/// - **The typed features.** Hover, definition, completion, document symbols,
///   and semantic tokens (full and delta): the server reads each request,
///   finds the document, calls the handler, and writes the answer.
/// - **Cancellation.** `$/cancelRequest` cancels a request that has been
///   received but not started: the client gets `REQUEST_CANCELLED` at once and
///   the handler is never called for it.
/// - **Errors.** A panic in the grammar or the handler is contained where
///   panics unwind: it costs one request (answered with `InternalError`) or is
///   logged, never the session.
///
/// # Threads and cancellation
///
/// The server is synchronous and single-threaded: it handles one message at a
/// time, and runs queued work only when the input has nothing more buffered,
/// so a cancellation that arrives together with, or behind, the request it
/// names (the usual case for a request waiting on slow work ahead of it) is
/// read first. A handler that is already running is not interrupted; handlers
/// that do slow work should snapshot the tree ([`Document::tree`](crate::Document::tree)
/// clones in O(1)) and keep each call short. Everything written to the client
/// during one message or one job is written with a single `write`.
///
/// # Examples
///
/// A complete server binary: this is all the protocol plumbing a language
/// needs:
///
/// ```no_run
/// # use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
/// # #[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// # enum K { Root, Text }
/// # impl TokenKind for K {}
/// # #[derive(Clone)]
/// # struct MyGrammar;
/// # impl Grammar for MyGrammar {
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
/// # struct MyHandler;
/// # impl lsp_lang::Handler<MyGrammar> for MyHandler {}
/// use std::process::ExitCode;
///
/// fn main() -> ExitCode {
///     match lsp_lang::serve_stdio(MyGrammar, MyHandler) {
///         Ok(()) => ExitCode::SUCCESS,
///         Err(err) => {
///             eprintln!("language server stopped: {err}");
///             ExitCode::FAILURE
///         }
///     }
/// }
/// ```
pub struct Server<G, H> {
    grammar: G,
    handler: H,
    limits: Limits,
    info: Option<(String, String)>,
    custom_requests: Vec<Box<str>>,
    custom_notifications: Vec<Box<str>>,
}

impl<G: Grammar + Clone, H: Handler<G>> Server<G, H> {
    /// Creates a server that parses documents with `grammar` and hands
    /// everything language-specific to `handler`.
    ///
    /// Each opened document gets its own clone of `grammar`; a grammar is
    /// typically a unit struct, so cloning it costs nothing.
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
    /// use lsp_lang::{Handler, Server};
    ///
    /// // A handler that relies entirely on the defaults: document sync only.
    /// struct SyncOnly;
    /// impl Handler<Plain> for SyncOnly {}
    ///
    /// let server = Server::new(Plain, SyncOnly);
    /// ```
    #[must_use]
    pub fn new(grammar: G, handler: H) -> Self {
        Self {
            grammar,
            handler,
            limits: limits_for(DEFAULT_MESSAGE_LIMIT),
            info: None,
            custom_requests: Vec::new(),
            custom_notifications: Vec::new(),
        }
    }

    /// Sets the largest message body accepted, in bytes. The default is 256
    /// MiB.
    ///
    /// The limit bounds the memory a single message can claim. A message that
    /// announces a larger body is skipped as it arrives, without being
    /// buffered, and refused with `InvalidRequest` under a `null` id (its id was
    /// never read); the session goes on. Opening a file sends its whole text in
    /// one message (JSON-escaped, so somewhat larger than the file), so the
    /// limit also caps the size of document the server accepts. The limit
    /// applies to the longest string in a message as well.
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
    /// # struct SyncOnly;
    /// # impl lsp_lang::Handler<Plain> for SyncOnly {}
    /// use lsp_lang::{Error, Server};
    ///
    /// let mut server = Server::new(Plain, SyncOnly).with_message_limit(16);
    /// // A 17-byte message is refused; then the input ends.
    /// let input = "Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}";
    /// let mut output = Vec::new();
    /// let result = server.run(input.as_bytes(), &mut output);
    /// assert!(matches!(result, Err(Error::Disconnected)));
    /// assert!(String::from_utf8_lossy(&output).contains("too large"));
    /// ```
    #[must_use]
    pub fn with_message_limit(mut self, bytes: usize) -> Self {
        self.limits = limits_for(bytes);
        self
    }

    /// Sends `name` and `version` as `serverInfo` in the `initialize` result,
    /// which clients show in their logs.
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
    /// # struct SyncOnly;
    /// # impl lsp_lang::Handler<Plain> for SyncOnly {}
    /// use lsp_lang::Server;
    ///
    /// let server = Server::new(Plain, SyncOnly).with_info("plain-ls", "0.1.0");
    /// ```
    #[must_use]
    pub fn with_info(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.info = Some((name.into(), version.into()));
        self
    }

    /// Registers a request method the server forwards to
    /// [`Handler::request`], for methods outside the protocol's own or the
    /// typed features (`lsk/explain`, `workspace/executeCommand`).
    ///
    /// Without registration a request for the method is answered
    /// `MethodNotFound`. A method that is one of the server's own is not
    /// affected: the built-in handling wins.
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
    /// # struct SyncOnly;
    /// # impl lsp_lang::Handler<Plain> for SyncOnly {}
    /// use lsp_lang::Server;
    ///
    /// let server = Server::new(Plain, SyncOnly).custom_request("plain/stats");
    /// ```
    #[must_use]
    pub fn custom_request(mut self, method: &str) -> Self {
        self.custom_requests.push(method.into());
        self
    }

    /// Registers a notification method the server forwards to
    /// [`Handler::notification`], beyond the standard ones it already
    /// forwards (`initialized`, `textDocument/didSave`,
    /// `workspace/didChangeConfiguration`, and a few more).
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
    /// # struct SyncOnly;
    /// # impl lsp_lang::Handler<Plain> for SyncOnly {}
    /// use lsp_lang::Server;
    ///
    /// let server = Server::new(Plain, SyncOnly).custom_notification("plain/refresh");
    /// ```
    #[must_use]
    pub fn custom_notification(mut self, method: &str) -> Self {
        self.custom_notifications.push(method.into());
        self
    }

    /// Returns the handler.
    ///
    /// Useful after [`run`](Server::run) returns, to inspect what a handler
    /// recorded in a test, for instance.
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
    /// use lsp_lang::{Context, Handler, Server, Value};
    ///
    /// #[derive(Default)]
    /// struct Counter { notifications: usize }
    ///
    /// impl Handler<Plain> for Counter {
    ///     fn notification(&mut self, _: &str, _: &Value<'_>, _: &mut Context<'_, Plain>) {
    ///         self.notifications += 1;
    ///     }
    /// }
    ///
    /// let server = Server::new(Plain, Counter::default());
    /// assert_eq!(server.handler().notifications, 0);
    /// ```
    #[inline]
    #[must_use]
    pub fn handler(&self) -> &H {
        &self.handler
    }

    /// Serves one session: reads messages from `input` and writes responses
    /// and notifications to `output` until the client sends `exit`.
    ///
    /// For an editor, pass locked stdin and stdout, or call
    /// [`serve_stdio`](crate::serve_stdio). `input` need not be buffered (the
    /// framing reads in 8 KiB chunks); everything sent during one message or
    /// one queued job is written to `output` with one `write_all` and flushed
    /// at once, so `output` need not be buffered either.
    ///
    /// Each call is a fresh session (uninitialized, with no documents open)
    /// while the grammar and handler carry over.
    ///
    /// Nothing the client sends and no panic in the grammar or handler ends
    /// the session (where panics unwind); only the transport and the lifecycle
    /// do, as listed below.
    ///
    /// # Errors
    ///
    /// `Ok(())` means a clean stop: `shutdown`, then `exit`. Otherwise:
    ///
    /// - [`Error::ExitWithoutShutdown`]: `exit` came without `shutdown`.
    /// - [`Error::Disconnected`]: the input ended before `exit`, possibly
    ///   inside a message.
    /// - [`Error::Io`]: reading or writing failed.
    /// - [`Error::Frame`]: the framing broke and the stream cannot be
    ///   resynchronized.
    ///
    /// In every error case the protocol expects the process to exit with a
    /// non-zero status. Requests received before the failure have been answered
    /// first.
    ///
    /// # Examples
    ///
    /// Driving a server from memory, the pattern for testing a handler:
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
    /// # struct SyncOnly;
    /// # impl lsp_lang::Handler<Plain> for SyncOnly {}
    /// use lsp_lang::Server;
    ///
    /// fn frame(json: &str) -> String {
    ///     format!("Content-Length: {}\r\n\r\n{json}", json.len())
    /// }
    ///
    /// let input = [
    ///     frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
    ///     frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
    ///     frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    /// ]
    /// .concat();
    ///
    /// let mut output = Vec::new();
    /// Server::new(Plain, SyncOnly).run(input.as_bytes(), &mut output)?;
    ///
    /// let output = String::from_utf8(output).unwrap();
    /// assert!(output.contains(r#""positionEncoding":"utf-16""#));
    /// assert!(output.contains(r#"{"jsonrpc":"2.0","id":2,"result":null}"#));
    /// # Ok::<(), lsp_lang::Error>(())
    /// ```
    pub fn run<R: Read, W: Write>(&mut self, mut input: R, mut output: W) -> Result<(), Error> {
        let outbox = Outbox::default();
        let config = Config::new()
            .with_limits(self.limits)
            .with_cancellation(Some(Cancellation::lsp()))
            .with_gate(Some(Gate::new("initialize").allow("exit")));
        let router = router(&self.custom_requests, &self.custom_notifications);
        let mut endpoint =
            Endpoint::new(outbox.sink(), router, Frontend::default()).with_config(config);
        let wire = Wire::new(endpoint.peer(), outbox.clone());
        let mut session = Session::new(
            self.grammar.clone(),
            &mut self.handler,
            wire,
            self.info.clone(),
        );
        let mut transport = Transport {
            endpoint: &mut endpoint,
            session: &mut session,
            outbox: &outbox,
            output: &mut output,
        };
        let outcome = transport.drive(&mut input, &self.limits);
        // Close first: jobs still queued drop their responders into a closed
        // connection, which asks nothing of the client.
        endpoint.close();
        let flushed = outbox.write_to(&mut output).map_err(Error::Io);
        outcome.and(flushed)
    }
}

/// Serves one session on the process's standard input and output, as an
/// editor starts a language server: the one-call entry point.
///
/// # Errors
///
/// As [`Server::run`].
///
/// # Examples
///
/// ```no_run
/// # use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
/// # #[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// # enum K { Root, Text }
/// # impl TokenKind for K {}
/// # #[derive(Clone)]
/// # struct MyGrammar;
/// # impl Grammar for MyGrammar {
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
/// # struct MyHandler;
/// # impl lsp_lang::Handler<MyGrammar> for MyHandler {}
/// fn main() -> Result<(), lsp_lang::Error> {
///     lsp_lang::serve_stdio(MyGrammar, MyHandler)
/// }
/// ```
pub fn serve_stdio<G, H>(grammar: G, handler: H) -> Result<(), Error>
where
    G: Grammar + Clone,
    H: Handler<G>,
{
    Server::new(grammar, handler).run(io::stdin().lock(), io::stdout().lock())
}

/// The transport limits for a message limit of `bytes`: the framing's cap on a
/// body, and the JSON parser's cap on any one string, both at `bytes`. A
/// document's text is one string, and a string may fill a message.
fn limits_for(bytes: usize) -> Limits {
    Limits::new()
        .with_max_message_bytes(bytes)
        .with_json(jsonx_lib::Limits::new().with_max_string_bytes(bytes))
}

/// The running pieces of one session, borrowed together for the read loop.
struct Transport<'a, 'h, G: Grammar, H, W> {
    endpoint: &'a mut Endpoint<Frontend, BufferSink>,
    session: &'a mut Session<'h, G, H>,
    outbox: &'a Outbox,
    output: &'a mut W,
}

impl<G: Grammar + Clone, H: Handler<G>, W: Write> Transport<'_, '_, G, H, W> {
    /// Reads and handles messages until the session ends.
    ///
    /// The loop alternates between three things. It hands every complete
    /// message the decoder holds to the endpoint, which answers what it can on
    /// its own (malformed input, unknown methods, cancellations) and queues the
    /// rest as jobs. When the decoder has nothing more, it runs the queue. Only
    /// then does it read. A request is therefore queued before it is run, and a
    /// `$/cancelRequest` that arrives with it, or behind it in the same read,
    /// is applied first.
    fn drive<R: Read>(&mut self, input: &mut R, limits: &Limits) -> Result<(), Error> {
        let mut decoder: Decoder = Framing::ContentLength.decoder(limits);
        loop {
            loop {
                match decoder.next_frame() {
                    Ok(Some(body)) => {
                        self.endpoint.state_mut().activity = false;
                        let _flow = self.endpoint.handle(body)?;
                        // Only an answer (or a batch, which may hold one) can
                        // settle a request of the server's own; a message that
                        // ran a handler cannot, so a flood of those costs no
                        // scan of the outstanding requests.
                        let answer = !self.endpoint.state().activity
                            || body.trim_ascii_start().first() == Some(&b'[');
                        self.flush(answer)?;
                        let front = self.endpoint.state();
                        // `initialize` is run at once: until it is answered the
                        // endpoint refuses what follows it. `exit` ends the
                        // session, so nothing after it is read.
                        if front.eager || front.exit || front.jobs.len() >= MAX_QUEUED {
                            self.run_queue()?;
                        }
                        if let Some(end) = self.session.finished.take() {
                            return end;
                        }
                    }
                    Ok(None) => break,
                    Err(error) if error.is_recoverable() => {
                        self.outbox.refuse_oversized(error);
                        self.flush(false)?;
                    }
                    Err(error) => {
                        self.run_queue()?;
                        return Err(Error::Frame(error));
                    }
                }
            }
            self.run_queue()?;
            if let Some(end) = self.session.finished.take() {
                return end;
            }
            let space = decoder.space();
            let read = loop {
                match input.read(space) {
                    Ok(n) => break n,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(Error::Io(error)),
                }
            };
            if read == 0 {
                break;
            }
            decoder.commit(read);
        }
        // The input ended. A body cut short by the end is a disconnect, like
        // an input that ends between messages without `exit`.
        self.run_queue()?;
        match self.session.finished.take() {
            Some(end) => end,
            None => Err(Error::Disconnected),
        }
    }

    /// Runs queued jobs, in order, until the queue is empty or the session
    /// ends, writing what each sent as soon as it is done.
    fn run_queue(&mut self) -> Result<(), Error> {
        while let Some(job) = self.endpoint.state_mut().jobs.pop_front() {
            self.session.run(job);
            self.session.poll_calls();
            if let Some(failure) = self.session.wire.failure.take() {
                if !matches!(failure, Error::InvalidParams(_)) {
                    return Err(failure);
                }
            }
            self.flush(true)?;
            if self.session.finished.is_some() {
                break;
            }
        }
        self.endpoint.state_mut().eager = false;
        Ok(())
    }

    /// Writes what is buffered, first letting a handler hear any outcome of its
    /// own requests that the message just handled settled (`answer`).
    fn flush(&mut self, answer: bool) -> Result<(), Error> {
        if answer {
            self.session.poll_calls();
        }
        self.outbox.write_to(self.output).map_err(Error::Io)
    }
}

impl<G, H> fmt::Debug for Server<G, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("message_limit", &self.limits.max_message_bytes)
            .finish_non_exhaustive()
    }
}
