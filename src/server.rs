//! [`Server`]: the protocol engine — reads messages, runs the lifecycle, keeps
//! documents in sync, and calls the [`Handler`] for the rest.

use std::{
    any::Any,
    collections::BTreeSet,
    fmt,
    io::{BufRead, Write},
    panic::{self, AssertUnwindSafe},
};

use incremental_lang::Grammar;
use serde::Serialize;
use serde_json::{Map, Value, value::RawValue};

use crate::{
    context::{Context, MAX_PENDING_REQUESTS, Wire, Workspace},
    document::Document,
    error::Error,
    handler::Handler,
    json::{Json, Reply, ResponseError},
    position::Encoding,
    protocol::{
        ContentChange, DidChange, DidClose, DidOpen, Envelope, InitializeParams, Salvage,
        WireError, is_valid_id,
    },
    transport::read_message,
};

/// Default for [`Server::with_message_limit`]: 256 MiB.
const DEFAULT_MESSAGE_LIMIT: usize = 256 * 1024 * 1024;

/// `MessageType` values for `window/logMessage`.
const LOG_ERROR: u8 = 1;
const LOG_WARNING: u8 = 2;

/// A language server: the protocol engine around a [`Handler`].
///
/// `Server` speaks the Language Server Protocol over any byte stream — stdin
/// and stdout for an editor, a socket, or an in-memory buffer in a test. It
/// owns everything that is the same for every language:
///
/// - **Framing.** Messages are read and written with the base protocol's
///   `Content-Length` headers. Incoming JSON is parsed in place: the envelope
///   is read without building a value tree, and parameters reach the handler
///   as raw [`Json`] to deserialize into its own types.
/// - **Lifecycle.** `initialize` negotiates the position [`Encoding`] and
///   merges the handler's capabilities with the server's own; requests before
///   it are refused with `ServerNotInitialized`; `shutdown` and `exit` end the
///   session, and requests after `shutdown` are refused with `InvalidRequest`.
/// - **Document sync.** `didOpen`, `didChange` (incremental or whole-text),
///   and `didClose` maintain a [`Document`] per open file, each incrementally
///   reparsed with the grammar `G`, and the handler hears about each change
///   through [`Handler::changed`]. A change that cannot be applied never
///   loses the document: it keeps its last good state, marked out of step,
///   until a whole-text change or a reopen brings it back (see
///   [`Handler::changed`]).
/// - **Errors.** Malformed JSON and invalid messages are answered with the
///   JSON-RPC error codes (under the request's own id whenever it can be
///   read); problems with notifications, which cannot be answered, are
///   reported to the client with `window/logMessage`. A panic in the grammar
///   or the handler is contained where panics unwind: it costs one request
///   (answered with `InternalError`) or is logged, never the session.
///
/// The server is synchronous and single-threaded: it handles one message at a
/// time and writes everything the handler sent, with a single write and
/// flush, before reading the next. That keeps latency to the minimum the
/// handler allows, with no locks, queues, or executor between the client and
/// the handler.
///
/// # Examples
///
/// A complete server binary — this is all the protocol plumbing a language
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
/// use std::io;
/// use std::process::ExitCode;
/// use lsp_lang::Server;
///
/// fn main() -> ExitCode {
///     let mut server = Server::new(MyGrammar, MyHandler);
///     match server.run(io::stdin().lock(), io::stdout().lock()) {
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
    limit: usize,
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
            limit: DEFAULT_MESSAGE_LIMIT,
        }
    }

    /// Sets the largest message body accepted, in bytes. The default is 256
    /// MiB.
    ///
    /// The limit bounds the memory a single message can claim. A message that
    /// announces a larger body ends the session with [`Error::TooLarge`]
    /// before any of the body is read. Opening a file sends its whole text in
    /// one message (JSON-escaped, so somewhat larger than the file), so the
    /// limit also caps the size of document the server accepts.
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
    /// let input = "Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}";
    /// let result = server.run(input.as_bytes(), Vec::new());
    /// assert!(matches!(result, Err(Error::TooLarge { len: 17, limit: 16 })));
    /// ```
    #[must_use]
    pub fn with_message_limit(mut self, bytes: usize) -> Self {
        self.limit = bytes;
        self
    }

    /// Returns the handler.
    ///
    /// Useful after [`run`](Server::run) returns — to inspect what a handler
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
    /// use lsp_lang::{Context, Handler, Json, Server};
    ///
    /// #[derive(Default)]
    /// struct Counter { notifications: usize }
    ///
    /// impl Handler<Plain> for Counter {
    ///     fn notification(&mut self, _: &str, _: Json<'_>, _: &mut Context<'_, Plain>) {
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
    /// For an editor, pass locked stdin and stdout. `input` should be buffered
    /// (`StdinLock` is); every message is written to `output` with one
    /// `write_all` and flushed at once, so `output` need not be.
    ///
    /// Each call is a fresh session — uninitialized, with no documents open —
    /// while the grammar and handler carry over.
    ///
    /// Nothing the client sends and no panic in the grammar or handler ends
    /// the session (where panics unwind); only the transport and the
    /// lifecycle do, as listed below.
    ///
    /// # Errors
    ///
    /// `Ok(())` means a clean stop: `shutdown`, then `exit`. Otherwise:
    ///
    /// - [`Error::ExitWithoutShutdown`] — `exit` came without `shutdown`.
    /// - [`Error::Disconnected`] — the input ended before `exit`.
    /// - [`Error::Io`] — reading or writing failed.
    /// - [`Error::InvalidHeader`] — the framing broke and the stream cannot be
    ///   resynchronized.
    /// - [`Error::TooLarge`] — a message exceeded the
    ///   [limit](Server::with_message_limit).
    ///
    /// In every error case the protocol expects the process to exit with a
    /// non-zero status.
    ///
    /// # Examples
    ///
    /// Driving a server from memory — the pattern for testing a handler:
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
    pub fn run<R: BufRead, W: Write>(&mut self, mut input: R, mut output: W) -> Result<(), Error> {
        let mut session = Session::new();
        let (mut body, mut line) = (Vec::new(), Vec::new());
        loop {
            if !read_message(&mut input, &mut body, &mut line, self.limit)? {
                return Err(Error::Disconnected);
            }
            let exit = self.dispatch(&body, &mut session);
            self.deliver_cancelled(&mut session);
            session.wire.outbox.flush(&mut output)?;
            if exit {
                return if session.phase == Phase::ShuttingDown {
                    Ok(())
                } else {
                    Err(Error::ExitWithoutShutdown)
                };
            }
        }
    }

    /// Handles one message. Returns `true` on `exit`.
    fn dispatch(&mut self, body: &[u8], s: &mut Session<G>) -> bool {
        let envelope: Envelope<'_> = match serde_json::from_slice(body) {
            Ok(envelope) => envelope,
            Err(e) => {
                // Valid JSON of the wrong shape (an array, say) is an invalid
                // request; anything else failed to parse at all.
                let code = if e.is_data() {
                    ResponseError::INVALID_REQUEST
                } else {
                    ResponseError::PARSE_ERROR
                };
                let error = ResponseError::new(code, format!("unreadable message: {e}"));
                // A request that is readable enough to have a valid id is
                // answered under it, so the client can match the error to it.
                s.wire.outbox.respond(Salvage::reply_id(body), Err(&error));
                return false;
            }
        };
        let params = envelope
            .params
            .map_or(Json::NULL, |p| Json::from_raw(p.get()));
        match (envelope.method, envelope.id) {
            (Some(method), Some(id)) if is_valid_id(id.get()) => {
                self.request(&method, id.get(), params, s);
                false
            }
            (Some(_), Some(_)) => {
                // `null`, a fraction, an object: not an id this protocol
                // allows, and not one to echo back.
                let error = ResponseError::new(
                    ResponseError::INVALID_REQUEST,
                    "a request id must be a string or an integer",
                );
                s.wire.outbox.respond("null", Err(&error));
                false
            }
            (Some(method), None) => self.notification(&method, params, s),
            (None, Some(id)) => {
                // Without a method, this is the client answering one of the
                // server's requests. Answering it in turn would put an error
                // under an id that, in the other direction, means one of the
                // client's own requests, so a malformed one is only logged.
                self.response(id, envelope.result, envelope.error, s);
                false
            }
            (None, None) if envelope.result.is_some() || envelope.error.is_some() => {
                s.ignored_response("null");
                false
            }
            (None, None) => {
                let error = ResponseError::new(
                    ResponseError::INVALID_REQUEST,
                    "message has no `method`, `id`, `result`, or `error`",
                );
                s.wire.outbox.respond("null", Err(&error));
                false
            }
        }
    }

    fn request(&mut self, method: &str, id: &str, params: Json<'_>, s: &mut Session<G>) {
        let result = match (s.phase, method) {
            (Phase::Uninitialized, "initialize") => return self.initialize(id, params, s),
            (Phase::Uninitialized, _) => Err(ResponseError::new(
                ResponseError::SERVER_NOT_INITIALIZED,
                format!("`{method}` received before `initialize`"),
            )),
            (Phase::Running, "initialize") => Err(ResponseError::new(
                ResponseError::INVALID_REQUEST,
                "`initialize` may only be sent once",
            )),
            (Phase::Running, "shutdown") => {
                s.phase = Phase::ShuttingDown;
                Ok(Reply::null())
            }
            (Phase::Running, _) => {
                let handler = &mut self.handler;
                let mut cx = s.context();
                caught(|| handler.request(method, params, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))
            }
            (Phase::ShuttingDown, _) => Err(ResponseError::new(
                ResponseError::INVALID_REQUEST,
                format!("`{method}` received after `shutdown`"),
            )),
        };
        s.respond(id, &result);
    }

    /// Answers `initialize`. Everything the handler sends while it runs is
    /// held back and sent after the response: until the server has answered
    /// with its capabilities the client may not be ready for anything else,
    /// and the response coming first is the one order that is always safe.
    ///
    /// If `initialize` fails, the session stays uninitialized and what the
    /// handler sent is discarded, never written; any requests among it are
    /// forgotten, since nothing will answer them.
    fn initialize(&mut self, id: &str, params: Json<'_>, s: &mut Session<G>) {
        let mark = s.wire.outbox.mark();
        let result = self.negotiate(params, s);
        let held = s.wire.outbox.hold_from(mark);
        s.respond(id, &result);
        if result.is_ok() {
            s.wire.outbox.release(&held);
        } else {
            s.wire.requests.forget();
        }
    }

    /// Negotiates the encoding, asks the handler for its capabilities, and
    /// adds the server's own.
    fn negotiate(&mut self, params: Json<'_>, s: &mut Session<G>) -> Result<Reply, ResponseError> {
        let client = params
            .parse::<Option<InitializeParams<'_>>>()?
            .unwrap_or_default();
        let encoding = Encoding::negotiate(client.position_encodings());
        let handler = &mut self.handler;
        let mut cx = s.context();
        let answer = caught(|| handler.initialize(params, &mut cx))
            .unwrap_or_else(|why| Err(handler_panicked("initialize", &why)));
        let mut capabilities = match answer? {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        let sync = capabilities
            .entry("textDocumentSync")
            .or_insert_with(|| Value::Object(Map::new()));
        if !sync.is_object() {
            *sync = Value::Object(Map::new());
        }
        if let Value::Object(sync) = sync {
            // Whatever the handler put here is superseded: the server owns
            // document synchronization.
            let _superseded = sync.insert("openClose".into(), Value::Bool(true));
            let _superseded = sync.insert("change".into(), Value::from(INCREMENTAL_SYNC));
        }
        let _superseded = capabilities.insert("positionEncoding".into(), encoding.as_str().into());

        #[derive(Serialize)]
        struct InitializeResult {
            capabilities: Map<String, Value>,
        }
        let reply = Reply::new(&InitializeResult { capabilities })?;
        s.encoding = encoding;
        s.phase = Phase::Running;
        Ok(reply)
    }

    fn notification(&mut self, method: &str, params: Json<'_>, s: &mut Session<G>) -> bool {
        if method == "exit" {
            return true;
        }
        if s.phase != Phase::Running {
            return false;
        }
        match method {
            "textDocument/didOpen" => self.did_open(params, s),
            "textDocument/didChange" => self.did_change(params, s),
            "textDocument/didClose" => self.did_close(params, s),
            _ => self.contained(
                s,
                || format!("`{method}`"),
                |handler, cx| handler.notification(method, params, cx),
            ),
        }
        false
    }

    fn did_open(&mut self, params: Json<'_>, s: &mut Session<G>) {
        let opened = match params.parse::<DidOpen<'_>>() {
            Ok(p) => p.text_document,
            Err(e) => return s.ignored("textDocument/didOpen", e.message()),
        };
        let uri: Box<str> = opened.uri.into();
        let grammar = self.grammar.clone();
        let (version, text, encoding) = (opened.version, opened.text, s.encoding);
        match guarded(|| Document::open(grammar, &uri, version, text, encoding)) {
            Ok(document) => {
                let _was_unopened = s.unopened.remove(&uri);
                let _replaced = s.workspace.insert(uri.clone(), document);
                self.changed(&uri, s);
            }
            Err(why) => {
                // Keep what there is: a document already open under this URI
                // stays, at its last good state, marked out of step; a new one
                // is remembered so a whole-text change can still open it.
                let message = match s.workspace.get_mut(&uri) {
                    Some(document) => {
                        document.mark_stale();
                        format!(
                            "could not reopen `{uri}`: {why}; keeping version {} until {RESYNC}",
                            document.version()
                        )
                    }
                    None => {
                        let _fresh = s.unopened.insert(uri.clone());
                        format!("could not open `{uri}`: {why}; it stays closed until {RESYNC}")
                    }
                };
                s.wire.outbox.log(LOG_ERROR, &message);
            }
        }
    }

    fn did_change(&mut self, params: Json<'_>, s: &mut Session<G>) {
        let change = match params.parse::<DidChange<'_>>() {
            Ok(p) => p,
            Err(e) => return s.ignored("textDocument/didChange", e.message()),
        };
        let uri = &*change.text_document.uri;
        let version = change.text_document.version;
        let edits = &change.content_changes[..];
        // A whole-text change supersedes every edit before it and applies
        // whatever state this copy is in: it is how a document out of step
        // with the client gets back in step.
        let mut from = edits.iter().rposition(|edit| edit.range.is_none());
        if !s.workspace.contains_key(uri) {
            if !self.open_from_change(uri, version, edits, from, s) {
                return;
            }
            // Opened from the whole-text change; the edits after it remain.
            from = from.map(|at| at + 1);
        }
        let Some(document) = s.workspace.get_mut(uri) else {
            return;
        };
        let edits = match from {
            Some(at) => edits.get(at..).unwrap_or_default(),
            None if document.is_stale() => {
                let message = format!(
                    "ignored a change to `{uri}`, which is out of step; waiting for {RESYNC}"
                );
                return s.wire.outbox.log(LOG_WARNING, &message);
            }
            None if version <= document.version() => {
                // Versions only increase. A repeat or an older version means
                // this change was already applied, or others went missing.
                document.mark_stale();
                let message = format!(
                    "`{uri}` is out of step: a change to version {version} arrived after version {}; \
                     keeping version {} until {RESYNC}",
                    document.version(),
                    document.version()
                );
                return s.wire.outbox.log(LOG_WARNING, &message);
            }
            None => edits,
        };
        match apply(document, edits) {
            Ok(recovered) => {
                document.synced(version);
                if let Some(fault) = recovered {
                    // In step, but the grammar has a bug worth knowing about.
                    let message = format!(
                        "reparsing an edit to `{uri}` incrementally failed ({fault});                          the whole text was parsed instead"
                    );
                    s.wire.outbox.log(LOG_WARNING, &message);
                }
                self.changed(uri, s);
            }
            Err(failure) => {
                document.mark_stale();
                let message = format!(
                    "`{uri}` is out of step: {failure}; keeping version {} until {RESYNC}",
                    document.version()
                );
                s.wire.outbox.log(failure.level(), &message);
            }
        }
    }

    /// Handles a change to a document that is not open. If its `didOpen`
    /// failed and the change carries a whole text (at `whole`), the document
    /// is opened from that text, and `true` returned; otherwise the change is
    /// logged and ignored.
    fn open_from_change(
        &mut self,
        uri: &str,
        version: i32,
        edits: &[ContentChange<'_>],
        whole: Option<usize>,
        s: &mut Session<G>,
    ) -> bool {
        let failed_open = s.unopened.contains(uri);
        let text = whole
            .filter(|_| failed_open)
            .and_then(|at| edits.get(at))
            .map(|edit| edit.text.to_string());
        let Some(text) = text else {
            let message = if failed_open {
                format!(
                    "ignored a change to `{uri}`, which could not be opened; waiting for {RESYNC}"
                )
            } else {
                format!("ignored a change to `{uri}`, which is not open")
            };
            s.wire.outbox.log(LOG_WARNING, &message);
            return false;
        };
        let (grammar, encoding) = (self.grammar.clone(), s.encoding);
        match guarded(|| Document::open(grammar, uri, version, text, encoding)) {
            Ok(document) => {
                let _was_unopened = s.unopened.remove(uri);
                let _fresh = s.workspace.insert(uri.into(), document);
                true
            }
            Err(why) => {
                let message = format!("could not open `{uri}` from a whole-text change: {why}");
                s.wire.outbox.log(LOG_ERROR, &message);
                false
            }
        }
    }

    fn did_close(&mut self, params: Json<'_>, s: &mut Session<G>) {
        let closed = match params.parse::<DidClose<'_>>() {
            Ok(p) => p.text_document,
            Err(e) => return s.ignored("textDocument/didClose", e.message()),
        };
        let uri = &*closed.uri;
        let _was_unopened = s.unopened.remove(uri);
        if s.workspace.remove(uri).is_some() {
            self.contained(
                s,
                || format!("`textDocument/didClose` for `{uri}`"),
                |handler, cx| handler.closed(uri, cx),
            );
        }
    }

    /// Calls [`Handler::changed`] for the open document `uri`.
    fn changed(&mut self, uri: &str, s: &mut Session<G>) {
        let Session {
            workspace, wire, ..
        } = s;
        let Some(document) = workspace.get(uri) else {
            return;
        };
        let handler = &mut self.handler;
        let mut cx = Context::new(workspace, wire);
        if let Err(why) = caught(|| handler.changed(document, &mut cx)) {
            let message = format!("the handler panicked in a change to `{uri}`: {why}");
            wire.outbox.log(LOG_ERROR, &message);
        }
    }

    /// Calls a handler method that has no result. A panic in it is contained:
    /// it is logged to the client as having happened during `during()`, and
    /// the session carries on.
    fn contained(
        &mut self,
        s: &mut Session<G>,
        during: impl FnOnce() -> String,
        call: impl FnOnce(&mut H, &mut Context<'_, G>),
    ) {
        let handler = &mut self.handler;
        let mut cx = s.context();
        if let Err(why) = caught(|| call(handler, &mut cx)) {
            let message = format!("the handler panicked in {}: {why}", during());
            s.wire.outbox.log(LOG_ERROR, &message);
        }
    }

    /// Delivers [`REQUEST_CANCELLED`](ResponseError::REQUEST_CANCELLED) for
    /// each of the server's requests cancelled to stay under the cap on
    /// outstanding requests. Requests a handler sends while hearing of these
    /// cancellations wait for the next message, so a handler that answers
    /// every cancellation with a new request cannot keep this loop going.
    fn deliver_cancelled(&mut self, s: &mut Session<G>) {
        let cancelled = s.wire.requests.take_cancelled();
        for id in cancelled {
            let error = ResponseError::new(
                ResponseError::REQUEST_CANCELLED,
                format!(
                    "cancelled without an answer: more than {MAX_PENDING_REQUESTS} requests \
                     were outstanding, and this was the oldest"
                ),
            );
            self.contained(
                s,
                || format!("the cancellation of request {id}"),
                |handler, cx| handler.response(id, Err(error), cx),
            );
        }
    }

    /// Delivers the client's answer to one of the server's requests. Only an
    /// answer to a request still outstanding reaches the handler; anything
    /// else is logged and dropped, never answered.
    fn response(
        &mut self,
        id: &RawValue,
        result: Option<&RawValue>,
        error: Option<&RawValue>,
        s: &mut Session<G>,
    ) {
        let outstanding = id
            .get()
            .parse::<u64>()
            .ok()
            .filter(|&id| s.wire.requests.settle(id));
        let Some(id) = outstanding else {
            return s.ignored_response(id.get());
        };
        let result = match (error, result) {
            (Some(error), _) => Err(client_error(error)),
            (None, Some(result)) => Ok(Json::from_raw(result.get())),
            (None, None) => Err(ResponseError::new(
                ResponseError::INTERNAL_ERROR,
                "the client's response had neither `result` nor `error`",
            )),
        };
        self.contained(
            s,
            || format!("the answer to request {id}"),
            |handler, cx| handler.response(id, result, cx),
        );
    }
}

/// How a document out of step with the client is recovered, for log messages.
const RESYNC: &str = "a whole-text change or a reopen";

/// Applies a `didChange`'s edits, in order, to `document`.
///
/// Each edit is placed (see [`Document::edit_span`]) and reparsed
/// incrementally. If the incremental reparse fails — the grammar panicked or
/// broke its contract on the fragment — the same edit is applied again by
/// splicing the text and parsing it whole, which a grammar fault confined to
/// the fragment path survives.
///
/// All or nothing: if any edit fails, the document is put back to the text it
/// had before the first, its last state the client also had. Only a change of
/// several edits needs that, so only then is the starting text kept.
///
/// On success, returns the first incremental fault that a full reparse
/// recovered from, if any, so the grammar bug behind it can be reported.
fn apply<G: Grammar + Clone>(
    document: &mut Document<G>,
    edits: &[ContentChange<'_>],
) -> Result<Option<Fault>, Failure> {
    let saved = (edits.len() > 1).then(|| document.text().to_owned());
    let mut recovered = None;
    for (at, edit) in edits.iter().enumerate() {
        match apply_one(document, edit) {
            Ok(fault) => recovered = recovered.or(fault),
            Err(failure) => {
                if let Some(saved) = saved.filter(|_| at > 0) {
                    // The saved text parsed before; if the grammar now fails
                    // on it, the document keeps the edits applied so far. It
                    // is marked out of step either way.
                    let _restored = guarded(|| document.reset(saved));
                }
                return Err(failure);
            }
        }
    }
    Ok(recovered)
}

/// Applies one edit: incrementally, or by full reparse if that fails, in
/// which case the incremental fault is returned.
fn apply_one<G: Grammar + Clone>(
    document: &mut Document<G>,
    edit: &ContentChange<'_>,
) -> Result<Option<Fault>, Failure> {
    let span = document.edit_span(edit.range).ok_or(Failure::Misplaced)?;
    let text = &*edit.text;
    match guarded(|| document.edit(span, text)) {
        Ok(()) => Ok(None),
        Err(Fault::Edit(
            error @ (incremental_lang::Error::TooLarge { .. }
            | incremental_lang::Error::OutOfBounds { .. }
            | incremental_lang::Error::NotCharBoundary { .. }),
        )) => Err(Failure::Refused(error)),
        Err(fault) => match guarded(|| document.rebuild(span, text)) {
            Ok(()) => Ok(Some(fault)),
            Err(fault) => Err(Failure::Unparsable(fault)),
        },
    }
}

/// Why a `didChange` could not be applied.
enum Failure {
    /// An edit's range names a line the document does not have.
    Misplaced,
    /// The edit was refused before any parsing: the text would outgrow 4 GiB.
    Refused(incremental_lang::Error),
    /// The grammar failed on the new text, incrementally and from scratch.
    Unparsable(Fault),
}

impl Failure {
    /// The `window/logMessage` level: the client's text drifting from the
    /// server's copy is a warning; the grammar failing on it is a bug, an
    /// error.
    fn level(&self) -> u8 {
        match self {
            Failure::Misplaced | Failure::Refused(_) => LOG_WARNING,
            Failure::Unparsable(_) => LOG_ERROR,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Misplaced => {
                f.write_str("an edit's range names a line the document does not have")
            }
            Failure::Refused(error) => write!(f, "an edit could not be applied: {error}"),
            Failure::Unparsable(fault) => write!(f, "an edit could not be parsed: {fault}"),
        }
    }
}

/// What went wrong in grammar code run under [`guarded`].
#[derive(Debug, PartialEq, Eq)]
enum Fault {
    /// `incremental-lang` refused the work.
    Edit(incremental_lang::Error),
    /// The grammar panicked; the panic's message, if it had one.
    Panic(String),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Edit(error) => error.fmt(f),
            Fault::Panic(why) => write!(f, "the grammar panicked: {why}"),
        }
    }
}

/// Reads the `error` of a client's response as leniently as it can: a missing
/// message reads as empty, a missing or out-of-range code as `INTERNAL_ERROR`,
/// and an error that is not an object at all as `INTERNAL_ERROR` saying so.
fn client_error(raw: &RawValue) -> ResponseError {
    match serde_json::from_str::<WireError<'_>>(raw.get()) {
        Ok(error) => {
            let code = error
                .code
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(ResponseError::INTERNAL_ERROR);
            ResponseError::new(code, error.message)
        }
        Err(_) => ResponseError::new(
            ResponseError::INTERNAL_ERROR,
            format!("malformed error from the client: {}", clip(raw.get())),
        ),
    }
}

/// Runs grammar code (opening or editing a document), turning a panic into a
/// [`Fault`], so a faulty grammar costs at most one document's freshness,
/// never the session.
///
/// `incremental-lang` leaves a document untouched when a grammar panic is
/// caught, and so do [`Document::rebuild`] and [`Document::reset`], so
/// nothing observes a half-finished state. (In a binary built with
/// `panic = "abort"` there is nothing to catch, and the process ends as that
/// setting asks.)
fn guarded<T>(work: impl FnOnce() -> Result<T, incremental_lang::Error>) -> Result<T, Fault> {
    match caught(work) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Fault::Edit(error)),
        Err(why) => Err(Fault::Panic(why)),
    }
}

/// Runs `work`, turning a panic into its message.
///
/// Used for every call into code the server does not own — the grammar and
/// the handler — so a bug there costs one request, notification, or document,
/// not the session. The server's own state stays consistent across a caught
/// panic: the handler sees the workspace only through a shared reference, and
/// the outgoing-message buffer and the request bookkeeping commit each message
/// whole. The handler's own state is whatever its panic left.
fn caught<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(work)).map_err(|payload| panic_message(&*payload))
}

/// A panic payload as text: the message of `panic!` and friends, clipped, or
/// a placeholder for any other payload.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    match text {
        Some(text) => clip_to(text, MAX_PANIC_MESSAGE),
        None => "(a panic without a message)".to_owned(),
    }
}

/// Longest panic message quoted to the client, in bytes.
const MAX_PANIC_MESSAGE: usize = 256;

/// The `INTERNAL_ERROR` a request is answered with when its handler panicked.
fn handler_panicked(method: &str, why: &str) -> ResponseError {
    ResponseError::new(
        ResponseError::INTERNAL_ERROR,
        format!("the server failed while handling `{method}`: the handler panicked: {why}"),
    )
}

/// At most the first 64 bytes of `text` (on a character boundary), with an
/// ellipsis when cut, so client-controlled text quoted in a log stays short.
fn clip(text: &str) -> String {
    clip_to(text, 64)
}

/// At most the first `max` bytes of `text`, on a character boundary, with an
/// ellipsis when cut.
fn clip_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &text[..end])
}

impl<G, H> fmt::Debug for Server<G, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("message_limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// `TextDocumentSyncKind.Incremental`: changes arrive as ranged edits.
const INCREMENTAL_SYNC: u8 = 2;

/// Where a session is in the protocol's lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Before a successful `initialize`.
    Uninitialized,
    /// Between `initialize` and `shutdown`.
    Running,
    /// After `shutdown`, waiting for `exit`.
    ShuttingDown,
}

/// The state of one [`Server::run`].
struct Session<G: Grammar> {
    phase: Phase,
    encoding: Encoding,
    workspace: Workspace<G>,
    /// URIs the client opened but the grammar could not parse: not in the
    /// workspace, but a whole-text change to one opens it. Bounded, like the
    /// workspace, by the documents the client holds open.
    unopened: BTreeSet<Box<str>>,
    wire: Wire,
}

impl<G: Grammar> Session<G> {
    fn new() -> Self {
        Self {
            phase: Phase::Uninitialized,
            encoding: Encoding::default(),
            workspace: Workspace::new(),
            unopened: BTreeSet::new(),
            wire: Wire::default(),
        }
    }

    fn context(&mut self) -> Context<'_, G> {
        Context::new(&self.workspace, &mut self.wire)
    }

    /// Sends the response to the request whose raw id is `id`.
    fn respond(&mut self, id: &str, result: &Result<Reply, ResponseError>) {
        match result {
            Ok(reply) => self.wire.outbox.respond(id, Ok(reply.as_str())),
            Err(error) => self.wire.outbox.respond(id, Err(error)),
        }
    }

    /// Reports a notification whose parameters could not be read.
    fn ignored(&mut self, method: &str, why: &str) {
        let message = format!("ignored `{method}`: {why}");
        self.wire.outbox.log(LOG_ERROR, &message);
    }

    /// Reports a response that answers no outstanding request of the server's.
    fn ignored_response(&mut self, id: &str) {
        let message = format!(
            "ignored a response with id {}, which answers no request the server is waiting on",
            clip(id)
        );
        self.wire.outbox.log(LOG_WARNING, &message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_short_text_is_unchanged() {
        assert_eq!(clip("\"abc\""), "\"abc\"");
        assert_eq!(clip(&"x".repeat(64)), "x".repeat(64));
    }

    #[test]
    fn test_clip_long_text_is_cut_on_a_char_boundary() {
        let clipped = clip(&"é".repeat(100));
        assert!(clipped.ends_with('…'));
        assert!(clipped.len() <= 64 + '…'.len_utf8());
        assert_eq!(clip(&"[".repeat(2_000_000)).len(), 64 + '…'.len_utf8());
    }

    fn read(json: &str) -> Option<(i32, String)> {
        let raw: &RawValue = serde_json::from_str(json).ok()?;
        let error = client_error(raw);
        Some((error.code(), error.message().to_owned()))
    }

    #[test]
    fn test_client_error_reads_leniently() {
        assert_eq!(
            read(r#"{"code":-32601,"message":"no"}"#),
            Some((-32601, "no".to_owned()))
        );
        assert_eq!(read(r#"{"code":-32601}"#), Some((-32601, String::new())));
        assert_eq!(
            read(r#"{"code":99999999999}"#).map(|e| e.0),
            Some(ResponseError::INTERNAL_ERROR)
        );
        assert_eq!(
            read("null").map(|e| e.0),
            Some(ResponseError::INTERNAL_ERROR)
        );
    }

    #[test]
    // The point of the test is a panic inside `guarded`, standing in for a
    // grammar bug.
    #[allow(clippy::panic)]
    fn test_guarded_turns_a_panic_into_an_error() {
        let result: Result<(), Fault> = guarded(|| panic!("grammar bug"));
        assert_eq!(result, Err(Fault::Panic("grammar bug".to_owned())));
        assert_eq!(guarded(|| Ok(7)), Ok(7));
        let refused = incremental_lang::Error::Tokens { offset: 3 };
        assert_eq!(guarded::<()>(|| Err(refused)), Err(Fault::Edit(refused)));
    }

    #[test]
    #[allow(clippy::panic)]
    fn test_caught_reports_the_panic_message_clipped() {
        let owned = caught(|| panic!("{}", "x".repeat(1000)));
        assert!(owned.is_err_and(|why| why.len() <= MAX_PANIC_MESSAGE + '…'.len_utf8()));
        assert_eq!(
            caught(|| panic!("static")),
            Err::<(), _>("static".to_owned())
        );
        let opaque = caught(|| std::panic::panic_any(7_u8));
        assert_eq!(
            opaque,
            Err::<(), _>("(a panic without a message)".to_owned())
        );
        assert_eq!(caught(|| 5), Ok(5));
    }

    #[test]
    fn test_failure_levels_separate_drift_from_grammar_bugs() {
        assert_eq!(Failure::Misplaced.level(), LOG_WARNING);
        let too_large = incremental_lang::Error::TooLarge { len: 1 << 33 };
        assert_eq!(Failure::Refused(too_large).level(), LOG_WARNING);
        let panicked = Failure::Unparsable(Fault::Panic("bug".to_owned()));
        assert_eq!(panicked.level(), LOG_ERROR);
        assert!(panicked.to_string().contains("the grammar panicked: bug"));
    }
}
