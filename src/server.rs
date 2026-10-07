//! [`Server`]: the protocol engine — reads messages, runs the lifecycle, keeps
//! documents in sync, and calls the [`Handler`] for the rest.

use std::{
    fmt,
    io::{BufRead, Write},
    panic::{self, AssertUnwindSafe},
};

use incremental_lang::Grammar;
use serde::Serialize;
use serde_json::{Map, Value, value::RawValue};

use crate::{
    context::{Context, Wire, Workspace},
    document::Document,
    error::Error,
    handler::Handler,
    json::{Json, Reply, ResponseError},
    position::Encoding,
    protocol::{DidChange, DidClose, DidOpen, Envelope, InitializeParams, WireError, is_valid_id},
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
///   through [`Handler::changed`].
/// - **Errors.** Malformed JSON and invalid messages are answered with the
///   JSON-RPC error codes; problems with notifications, which cannot be
///   answered, are reported to the client with `window/logMessage`.
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
                s.wire.outbox.respond("null", Err(&error));
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
            (Phase::Uninitialized, "initialize") => self.initialize(params, s),
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
            (Phase::Running, _) => self.handler.request(method, params, &mut s.context()),
            (Phase::ShuttingDown, _) => Err(ResponseError::new(
                ResponseError::INVALID_REQUEST,
                format!("`{method}` received after `shutdown`"),
            )),
        };
        match &result {
            Ok(reply) => s.wire.outbox.respond(id, Ok(reply.as_str())),
            Err(error) => s.wire.outbox.respond(id, Err(error)),
        }
    }

    /// Negotiates the encoding, asks the handler for its capabilities, and
    /// adds the server's own.
    fn initialize(&mut self, params: Json<'_>, s: &mut Session<G>) -> Result<Reply, ResponseError> {
        let client = params
            .parse::<Option<InitializeParams<'_>>>()?
            .unwrap_or_default();
        let encoding = Encoding::negotiate(client.position_encodings());
        let mut capabilities = match self.handler.initialize(params, &mut s.context())? {
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
            _ => self.handler.notification(method, params, &mut s.context()),
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
                let _replaced = s.workspace.insert(uri.clone(), document);
                self.changed(&uri, s);
            }
            Err(why) => {
                // A document already open under this URI no longer matches
                // the client's text either: drop it too.
                let message = format!("could not open `{uri}`: {why}");
                s.wire.outbox.log(LOG_ERROR, &message);
                if s.workspace.remove(&uri).is_some() {
                    self.handler.closed(&uri, &mut s.context());
                }
            }
        }
    }

    fn did_change(&mut self, params: Json<'_>, s: &mut Session<G>) {
        let change = match params.parse::<DidChange<'_>>() {
            Ok(p) => p,
            Err(e) => return s.ignored("textDocument/didChange", e.message()),
        };
        let uri = &*change.text_document.uri;
        let Some(document) = s.workspace.get_mut(uri) else {
            let message = format!("ignored a change to `{uri}`, which is not open");
            return s.wire.outbox.log(LOG_WARNING, &message);
        };
        let edits = &change.content_changes;
        let applied = guarded(|| {
            edits
                .iter()
                .try_for_each(|edit| document.change(edit.range, &edit.text))
        });
        match applied {
            Ok(()) => {
                document.set_version(change.text_document.version);
                self.changed(uri, s);
            }
            Err(why) => {
                // The document no longer matches the client's text, and the
                // edit cannot be retried. Drop it rather than serve stale
                // positions; the client reopens it on its next open.
                let _dropped = s.workspace.remove(uri);
                let message = format!("closed `{uri}`: an edit could not be applied: {why}");
                s.wire.outbox.log(LOG_ERROR, &message);
                self.handler.closed(uri, &mut s.context());
            }
        }
    }

    fn did_close(&mut self, params: Json<'_>, s: &mut Session<G>) {
        let closed = match params.parse::<DidClose<'_>>() {
            Ok(p) => p.text_document,
            Err(e) => return s.ignored("textDocument/didClose", e.message()),
        };
        if s.workspace.remove(&*closed.uri).is_some() {
            self.handler.closed(&closed.uri, &mut s.context());
        }
    }

    /// Calls [`Handler::changed`] for the open document `uri`.
    fn changed(&mut self, uri: &str, s: &mut Session<G>) {
        let Session {
            workspace, wire, ..
        } = s;
        if let Some(document) = workspace.get(uri) {
            let mut cx = Context::new(workspace, wire);
            self.handler.changed(document, &mut cx);
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
        self.handler.response(id, result, &mut s.context());
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

/// Runs grammar code (opening or editing a document), turning a panic into an
/// error so one faulty grammar costs one document, not the session.
///
/// `incremental-lang` leaves a document untouched when a grammar panic is
/// caught, but the server drops the document either way, so nothing observes
/// a half-finished state. (In a binary built with `panic = "abort"` there is
/// nothing to catch, and the process ends as that setting asks.)
fn guarded<T>(work: impl FnOnce() -> Result<T, incremental_lang::Error>) -> Result<T, String> {
    match panic::catch_unwind(AssertUnwindSafe(work)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the grammar panicked".to_owned()),
    }
}

/// At most the first 64 bytes of `text` (on a character boundary), with an
/// ellipsis when cut, so client-controlled text quoted in a log stays short.
fn clip(text: &str) -> String {
    if text.len() <= 64 {
        return text.to_owned();
    }
    let mut end = 64;
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
    wire: Wire,
}

impl<G: Grammar> Session<G> {
    fn new() -> Self {
        Self {
            phase: Phase::Uninitialized,
            encoding: Encoding::default(),
            workspace: Workspace::new(),
            wire: Wire::default(),
        }
    }

    fn context(&mut self) -> Context<'_, G> {
        Context::new(&self.workspace, &mut self.wire)
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
        let result: Result<(), String> = guarded(|| panic!("grammar bug"));
        assert_eq!(result, Err("the grammar panicked".to_owned()));
        assert_eq!(guarded(|| Ok(7)), Ok(7));
    }
}
