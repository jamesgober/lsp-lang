//! [`Context`]: what a handler can reach while it runs, the open documents and
//! a channel back to the client.

use std::{collections::BTreeMap, fmt};

use diag_lang::Diagnostic;
use incremental_lang::Grammar;
use rpc_io::{Call, Json, Peer};

use crate::{
    diagnostics::{Scratch, write_clear, write_publish},
    document::Document,
    error::Error,
    json::{trusted_raw, write_i64, write_str},
    outbox::Outbox,
};

/// The open documents, keyed by URI. Ordered, so iteration, and anything a
/// handler publishes while iterating, is the same from run to run.
pub(crate) type Workspace<G> = BTreeMap<Box<str>, Document<G>>;

/// How serious a message shown or logged by the client is
/// (`MessageType` in the protocol).
///
/// # Examples
///
/// ```
/// use lsp_lang::MessageType;
///
/// assert_eq!(MessageType::Warning.as_u32(), 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessageType {
    /// Something failed (`1`).
    Error = 1,
    /// Something is probably wrong (`2`).
    Warning = 2,
    /// Information (`3`).
    Info = 3,
    /// Trace output for the client's log (`4`).
    Log = 4,
}

impl MessageType {
    /// The protocol's number for this type.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::MessageType;
    ///
    /// assert_eq!(MessageType::Log.as_u32(), 4);
    /// ```
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Everything a session sends with: the connection's sending handle, the
/// server's own requests awaiting an answer, and scratch space reused across
/// notifications.
pub(crate) struct Wire {
    pub(crate) peer: Peer,
    /// The server's own requests, until their outcome is delivered.
    pub(crate) calls: Vec<Call>,
    /// The first send that failed during the current call, for the session to
    /// act on once the handler returns.
    pub(crate) failure: Option<Error>,
    scratch: Scratch,
    text: String,
    /// Where notifications the crate writes itself may go straight to, once
    /// the session is running; see [`Wire::send_text`].
    outbox: Outbox,
    direct: bool,
}

impl Wire {
    pub(crate) fn new(peer: Peer, outbox: Outbox) -> Self {
        Self {
            peer,
            calls: Vec::new(),
            failure: None,
            scratch: Scratch::default(),
            text: String::new(),
            outbox,
            direct: false,
        }
    }

    /// Lets notifications the crate writes itself bypass the transport's
    /// message model (see [`Wire::send_text`]). Set when `initialize`
    /// succeeds, the first moment the client is ready for them.
    pub(crate) fn go_direct(&mut self, direct: bool) {
        self.direct = direct;
    }

    /// Sends `window/logMessage`.
    pub(crate) fn log(&mut self, kind: MessageType, message: &str) {
        self.text.clear();
        self.text.push_str("{\"type\":");
        write_i64(&mut self.text, i64::from(kind.as_u32()));
        self.text.push_str(",\"message\":");
        write_str(&mut self.text, message);
        self.text.push('}');
        self.send_text("window/logMessage");
    }

    /// Sends the notification `method` whose params are in `self.text`.
    ///
    /// A running session frames it straight into the outgoing buffer. The text
    /// is JSON this crate just wrote, so the transport's pass to validate it
    /// and its copies into the message model would only repeat the writer's
    /// work, and for a diagnostics notification of a few megabytes they are
    /// more than half the cost. (Debug builds, which include the tests, still
    /// validate every message before it is sent.) Before the session runs,
    /// the transport holds notifications until `initialize` is answered, so
    /// they go through it.
    fn send_text(&mut self, method: &str) {
        if self.direct && (!cfg!(debug_assertions) || trusted_raw(&self.text).is_ok()) {
            self.outbox.push_notification(method, &self.text);
            return;
        }
        let result = trusted_raw(&self.text)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
            .and_then(|raw| self.peer.notify(method, raw).map_err(Error::from));
        if let Err(error) = result {
            self.remember_failure(error);
        }
    }

    pub(crate) fn remember_failure(&mut self, error: Error) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
    }
}

/// A handler's view of the session for the duration of one call.
///
/// Every [`Handler`](crate::Handler) method receives one. Through it a handler
/// reads the open documents and talks back to the client: notifications
/// ([`notify`](Context::notify)), server-to-client requests
/// ([`request`](Context::request)), log messages ([`log`](Context::log)), and
/// diagnostics ([`publish_diagnostics`](Context::publish_diagnostics)).
///
/// Messages sent through a context are written to the client in one write, in
/// the order they were sent, as soon as the handler returns, before the server
/// reads the next message. The response to the request being handled, if there
/// is one, is written after them.
///
/// Documents borrowed from a context live for the whole call (`'a`), not just
/// for the borrow of the context, so a handler can hold a document and send
/// messages at the same time:
///
/// ```
/// # use lsp_lang::incremental_lang::Grammar;
/// use lsp_lang::{Context, Diagnostic};
///
/// fn republish<G: Grammar>(cx: &mut Context<'_, G>, uri: &str, found: &[Diagnostic]) {
///     if let Some(doc) = cx.document(uri) {
///         cx.publish_diagnostics(doc, found); // `doc` stays borrowed meanwhile
///     }
/// }
/// ```
pub struct Context<'a, G: Grammar> {
    workspace: &'a Workspace<G>,
    wire: &'a mut Wire,
}

impl<'a, G: Grammar> Context<'a, G> {
    pub(crate) fn new(workspace: &'a Workspace<G>, wire: &'a mut Wire) -> Self {
        Self { workspace, wire }
    }

    /// Returns the open document with this URI, if the client has one open.
    ///
    /// URIs match exactly, as the client sent them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::Context;
    ///
    /// fn line_count<G: Grammar>(cx: &Context<'_, G>, uri: &str) -> Option<u32> {
    ///     let doc = cx.document(uri)?;
    ///     Some(doc.position(doc.text().len() as u32).line + 1)
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn document(&self, uri: &str) -> Option<&'a Document<G>> {
        self.workspace.get(uri)
    }

    /// Returns every open document, ordered by URI.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::Context;
    ///
    /// // Total bytes the client has open, for a status message, say.
    /// fn open_bytes<G: Grammar>(cx: &Context<'_, G>) -> usize {
    ///     cx.documents().map(|doc| doc.text().len()).sum()
    /// }
    /// ```
    pub fn documents(&self) -> impl ExactSizeIterator<Item = &'a Document<G>> + 'a {
        self.workspace.values()
    }

    /// Sends a notification to the client.
    ///
    /// `params` is a [`Value`](crate::Value) built with [`json!`](crate::json),
    /// or a pre-serialized [`Raw`](crate::rpc_io::Raw): an array, an object, or
    /// `()` for none.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidParams`] if `params` is a scalar, [`Error::Closed`] if
    /// the connection has ended. Nothing is sent in either case, and the
    /// session carries on.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Error};
    ///
    /// // Pop up a message in the editor (`MessageType` 3 is "info").
    /// fn announce<G: Grammar>(cx: &mut Context<'_, G>) -> Result<(), Error> {
    ///     cx.notify("window/showMessage", json!({ "type": 3, "message": "Indexing done" }))
    /// }
    /// ```
    pub fn notify<'p>(&mut self, method: &str, params: impl Into<Json<'p>>) -> Result<(), Error> {
        self.wire.peer.notify(method, params).map_err(Error::from)
    }

    /// Sends a request to the client and returns the id it was sent with.
    ///
    /// The client's answer arrives later, as a call to
    /// [`Handler::response`](crate::Handler::response) with the same id; keep
    /// the id to tell answers apart. Ids count up from 1 for each session. The
    /// server delivers exactly one outcome for each request.
    ///
    /// At most 1024 requests await an answer at once. Sending one more cancels
    /// the oldest outstanding request: the server sends the client
    /// `$/cancelRequest` for it and delivers
    /// [`REQUEST_CANCELLED`](crate::ResponseError::REQUEST_CANCELLED) as its
    /// answer once the current handler call returns. An answer the client
    /// sends for it afterwards is dropped.
    ///
    /// During `initialize` a request is held and sent right after the
    /// `initialize` response, in order. If `initialize` fails, the request is
    /// never sent and its outcome is `REQUEST_CANCELLED`.
    ///
    /// # Errors
    ///
    /// As [`notify`](Context::notify). Nothing is sent, and no id is used up.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Error};
    ///
    /// // Ask for the user's settings for this language.
    /// fn fetch_settings<G: Grammar>(cx: &mut Context<'_, G>) -> Result<u64, Error> {
    ///     cx.request("workspace/configuration", json!({ "items": [{ "section": "lisp" }] }))
    /// }
    /// ```
    pub fn request<'p>(&mut self, method: &str, params: impl Into<Json<'p>>) -> Result<u64, Error> {
        let call = self.wire.peer.request(method, params)?;
        let id = call.id();
        self.wire.calls.push(call);
        Ok(id)
    }

    /// Sends `window/logMessage`: a line for the client's output log.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, MessageType};
    ///
    /// fn note<G: Grammar>(cx: &mut Context<'_, G>) {
    ///     cx.log(MessageType::Info, "index rebuilt");
    /// }
    /// ```
    pub fn log(&mut self, kind: MessageType, message: &str) {
        self.wire.log(kind, message);
    }

    /// Publishes `diagnostics` for `document`, replacing whatever was
    /// published for it before.
    ///
    /// Each [`Diagnostic`]'s spans are byte offsets into the document's text,
    /// the same space as the syntax tree's spans and the space a
    /// `diag_lang::SourceMap` gives a single file added first. They are
    /// converted to ranges in the session encoding (clamped to the text), the
    /// message is joined with the primary label, notes, and help lines, and
    /// secondary labels become related information; the full mapping is in the
    /// crate documentation. The notification carries the document's version, so
    /// the client can drop results for text it has since changed.
    ///
    /// The JSON is written directly into the outgoing message, with no
    /// intermediate allocation per diagnostic, and all spans are converted in
    /// one ascending pass, so many diagnostics on one long line cost one scan
    /// of it. Publishing an empty slice clears the document's diagnostics.
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
    /// use lsp_lang::{Context, Diagnostic, Document, Handler};
    /// use lsp_lang::diag_lang::{Label, Severity};
    ///
    /// /// Flags every tab character.
    /// struct NoTabs;
    ///
    /// impl Handler<Plain> for NoTabs {
    ///     fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
    ///         let found: Vec<Diagnostic> = doc
    ///             .text()
    ///             .match_indices('\t')
    ///             .map(|(at, _)| {
    ///                 let at = at as u32;
    ///                 Diagnostic::new(
    ///                     Severity::Warning,
    ///                     "tab character",
    ///                     Label::new(Span::new(at, at + 1), "indent with spaces"),
    ///                 )
    ///             })
    ///             .collect();
    ///         cx.publish_diagnostics(doc, &found);
    ///     }
    /// }
    /// ```
    pub fn publish_diagnostics(&mut self, document: &Document<G>, diagnostics: &[Diagnostic]) {
        let wire = &mut *self.wire;
        wire.text.clear();
        write_publish(&mut wire.text, &mut wire.scratch, document, diagnostics);
        wire.send_text("textDocument/publishDiagnostics");
    }

    /// Clears the diagnostics published for `uri`.
    ///
    /// Works whether or not the document is open; the usual call is on close,
    /// which is what the default
    /// [`Handler::closed`](crate::Handler::closed) does.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::Context;
    ///
    /// // Forget everything reported, e.g. when the project is reloaded.
    /// fn clear_all<G: Grammar>(cx: &mut Context<'_, G>) {
    ///     let uris: Vec<&str> = cx.documents().map(|doc| doc.uri()).collect();
    ///     for uri in uris {
    ///         cx.clear_diagnostics(uri);
    ///     }
    /// }
    /// ```
    pub fn clear_diagnostics(&mut self, uri: &str) {
        let wire = &mut *self.wire;
        wire.text.clear();
        write_clear(&mut wire.text, uri);
        wire.send_text("textDocument/publishDiagnostics");
    }
}

impl<G: Grammar> fmt::Debug for Context<'_, G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("documents", &self.workspace.len())
            .field("outstanding_requests", &self.wire.calls.len())
            .finish_non_exhaustive()
    }
}
