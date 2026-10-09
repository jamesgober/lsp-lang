//! [`Handler`]: the trait a language server implements.

use incremental_lang::Grammar;
use serde_json::{Map, Value};

use crate::{
    context::Context,
    document::Document,
    json::{Json, Reply, ResponseError},
};

/// A language server's behaviour: the part of a server that is about the
/// language, as opposed to the protocol.
///
/// The [`Server`](crate::Server) owns the protocol — framing, the
/// `initialize`/`shutdown`/`exit` lifecycle, and document synchronization —
/// and calls a handler for everything else. Every method has a default, so a
/// handler implements only what its language supports: a diagnostics-only
/// server needs [`changed`](Handler::changed) and nothing more.
///
/// | Method | Called when | Default |
/// |---|---|---|
/// | [`initialize`](Handler::initialize) | the client sends `initialize` | no extra capabilities |
/// | [`changed`](Handler::changed) | a document opens or changes | nothing |
/// | [`closed`](Handler::closed) | a document closes | clears its diagnostics |
/// | [`request`](Handler::request) | any other request arrives | `MethodNotFound` |
/// | [`notification`](Handler::notification) | any other notification arrives | ignored |
/// | [`response`](Handler::response) | the client answers a [`Context::request`] | ignored |
///
/// Calls are made one at a time, in message order, on the thread running
/// [`Server::run`](crate::Server::run). A request is answered with whatever its
/// method returns, so its result is computed before the next message is read;
/// a handler with slow work can snapshot the tree ([`Document::tree`] clones in
/// O(1)) and hand it to a worker, reporting back through
/// [`Context::notify`] on a later call.
///
/// A panic in any method is contained where panics unwind: a request is
/// answered with [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR) carrying
/// the panic's message, a panic anywhere else is logged to the client
/// (`window/logMessage`), and the session carries on with the same handler.
/// Messages the handler sent before panicking are still sent. The handler's
/// own state is whatever the panic left. (With `panic = "abort"` the process
/// ends instead, as that setting asks.)
///
/// `G` is the grammar every document is parsed with; see
/// [`incremental_lang::Grammar`].
///
/// # Examples
///
/// A handler that offers hover, answering with the text of the line under the
/// cursor:
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
/// use lsp_lang::{json, Context, Handler, Json, Position, Reply, ResponseError, Value};
///
/// struct LineHover;
///
/// #[derive(serde::Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// struct HoverParams<'a> {
///     #[serde(borrow)]
///     text_document: TextDocument<'a>,
///     position: Position,
/// }
///
/// #[derive(serde::Deserialize)]
/// struct TextDocument<'a> {
///     uri: &'a str,
/// }
///
/// impl Handler<Plain> for LineHover {
///     fn initialize(&mut self, _: Json<'_>, _: &mut Context<'_, Plain>) -> Result<Value, ResponseError> {
///         Ok(json!({ "hoverProvider": true }))
///     }
///
///     fn request(
///         &mut self,
///         method: &str,
///         params: Json<'_>,
///         cx: &mut Context<'_, Plain>,
///     ) -> Result<Reply, ResponseError> {
///         match method {
///             "textDocument/hover" => {
///                 let p: HoverParams<'_> = params.parse()?;
///                 let Some(doc) = cx.document(p.text_document.uri) else {
///                     return Ok(Reply::null());
///                 };
///                 let line = doc.text().lines().nth(p.position.line as usize).unwrap_or("");
///                 Ok(Reply::new(&json!({ "contents": line }))?)
///             }
///             _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
///         }
///     }
/// }
/// ```
pub trait Handler<G: Grammar> {
    /// Answers `initialize` with the server's capabilities.
    ///
    /// Return the `ServerCapabilities` object for everything the handler
    /// supports — `hoverProvider`, `completionProvider`, and so on. The server
    /// adds the two capabilities it owns before replying: `positionEncoding`
    /// (the negotiated [`Encoding`](crate::Encoding)) and `textDocumentSync`
    /// (open/close notifications and incremental changes). Other members of a
    /// `textDocumentSync` object the handler returns, such as `save`, are kept.
    /// A value that is not a JSON object is treated as `{}`.
    ///
    /// `params` is the client's `InitializeParams`, for anything the handler
    /// needs from it: `rootUri`, `initializationOptions`, client capabilities.
    ///
    /// Returning an error refuses the `initialize` request; the session stays
    /// uninitialized and the client may try again.
    ///
    /// Anything sent through `cx` here is held back and reaches the client
    /// right after the `initialize` response, in the order sent — the
    /// protocol lets a server send nothing else before it has answered. If
    /// `initialize` fails (an error, or a panic), the session stays
    /// uninitialized and what was sent is discarded, never written; a request
    /// among it is forgotten and gets no [`response`](Handler::response).
    /// Even so, wait for the `initialized` notification (see
    /// [`notification`](Handler::notification)) before registering
    /// capabilities or asking for configuration, as the protocol advises.
    ///
    /// The default returns `{}`: a server with document sync and nothing else.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Handler, Json, ResponseError, Value};
    ///
    /// struct Project { root: Option<String> }
    ///
    /// impl<G: Grammar> Handler<G> for Project {
    ///     fn initialize(&mut self, params: Json<'_>, _: &mut Context<'_, G>) -> Result<Value, ResponseError> {
    ///         let params: Value = params.parse()?;
    ///         self.root = params["rootUri"].as_str().map(str::to_owned);
    ///         Ok(json!({
    ///             "hoverProvider": true,
    ///             "definitionProvider": true,
    ///             "textDocumentSync": { "save": true },
    ///         }))
    ///     }
    /// }
    /// ```
    fn initialize(
        &mut self,
        params: Json<'_>,
        cx: &mut Context<'_, G>,
    ) -> Result<Value, ResponseError> {
        let _unused = (params, cx);
        Ok(Value::Object(Map::new()))
    }

    /// Called after a document is opened and after each change to it, once
    /// the text and syntax tree are current.
    ///
    /// The usual body analyses the document and publishes what it finds with
    /// [`Context::publish_diagnostics`]. A `didChange` carrying several edits
    /// produces one call, after all of them are applied.
    ///
    /// If an edit's incremental reparse fails — the grammar panicked or its
    /// lexer broke its contract on the reparsed fragment — the server applies
    /// the edit again by parsing the whole new text, logs a warning, and calls
    /// this method as usual. If the change cannot be applied at all — an edit
    /// names a line the document does not have, the version does not
    /// increase, the grammar fails on the whole text too, or the text would
    /// outgrow 4 GiB — the document is kept at its last good state (text,
    /// tree, and version), marked out of step with the client, and the reason
    /// is logged (`window/logMessage`); this method is not called. Ranged
    /// changes to it are then ignored (and logged) until a whole-text change,
    /// or a reopen, brings it back in step, which calls this method again. The
    /// document is never dropped on a failed change. (A grammar panic is
    /// contained only where panics unwind; with `panic = "abort"` the process
    /// ends.)
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Document, Handler};
    ///
    /// // Count reparses served incrementally, across all documents.
    /// #[derive(Default)]
    /// struct Watch { edits: u64 }
    ///
    /// impl<G: Grammar> Handler<G> for Watch {
    ///     fn changed(&mut self, doc: &Document<G>, _: &mut Context<'_, G>) {
    ///         self.edits = doc.incremental().stats().edits;
    ///     }
    /// }
    /// ```
    fn changed(&mut self, document: &Document<G>, cx: &mut Context<'_, G>) {
        let _unused = (document, cx);
    }

    /// Called after the document `uri` is closed (and removed).
    ///
    /// The default clears the diagnostics published for it, as the protocol
    /// recommends, since the client may otherwise keep showing them for a file
    /// that is no longer being analysed. Override it to drop per-document
    /// state as well; call [`Context::clear_diagnostics`] to keep that
    /// behaviour.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use std::collections::HashMap;
    /// use lsp_lang::{Context, Handler};
    ///
    /// #[derive(Default)]
    /// struct Cache { per_file: HashMap<String, Vec<u32>> }
    ///
    /// impl<G: Grammar> Handler<G> for Cache {
    ///     fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>) {
    ///         self.per_file.remove(uri);
    ///         cx.clear_diagnostics(uri);
    ///     }
    /// }
    /// ```
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>) {
        cx.clear_diagnostics(uri);
    }

    /// Answers a request the server does not handle itself — every request
    /// except `initialize` and `shutdown`.
    ///
    /// `method` names the request (`"textDocument/hover"`); `params` are its
    /// parameters, to [`parse`](Json::parse) into the type the handler wants.
    /// The returned [`Reply`] or [`ResponseError`] is sent to the client as the
    /// response. Return `Ok(Reply::null())` for "nothing here".
    ///
    /// Requests only reach the handler between `initialize` and `shutdown`;
    /// the server answers anything outside that window with the error the
    /// protocol prescribes.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    /// A handler with its own `match` should fall back to the same.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Handler, Json, Reply, ResponseError};
    ///
    /// struct Echo;
    ///
    /// impl<G: Grammar> Handler<G> for Echo {
    ///     fn request(&mut self, method: &str, params: Json<'_>, _: &mut Context<'_, G>) -> Result<Reply, ResponseError> {
    ///         match method {
    ///             // A custom request: answer with the parameters, unchanged.
    ///             "echo/params" => Ok(Reply::new(&params.parse::<lsp_lang::Value>()?)?),
    ///             _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
    ///         }
    ///     }
    /// }
    /// ```
    fn request(
        &mut self,
        method: &str,
        params: Json<'_>,
        cx: &mut Context<'_, G>,
    ) -> Result<Reply, ResponseError> {
        let _unused = (params, cx);
        Err(ResponseError::new(
            ResponseError::METHOD_NOT_FOUND,
            format!("unhandled method `{method}`"),
        ))
    }

    /// Receives a notification the server does not handle itself — every
    /// notification except `exit` and the three document-synchronization
    /// ones. That includes `initialized`, `workspace/didChangeConfiguration`,
    /// `textDocument/didSave`, and protocol-internal ones such as
    /// `$/cancelRequest` and `$/setTrace`, which may be ignored.
    ///
    /// Notifications have no response. Notifications that arrive before
    /// `initialize` or after `shutdown` are dropped without reaching the
    /// handler, as the protocol prescribes.
    ///
    /// The default ignores them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Handler, Json};
    ///
    /// struct Hello;
    ///
    /// impl<G: Grammar> Handler<G> for Hello {
    ///     fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, G>) {
    ///         if method == "initialized" {
    ///             // A serialization failure here would only skip the greeting.
    ///             let _sent = cx.notify("window/logMessage", &json!({ "type": 3, "message": "ready" }));
    ///         }
    ///     }
    /// }
    /// ```
    fn notification(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>) {
        let _unused = (method, params, cx);
    }

    /// Receives the client's answer to a request sent with
    /// [`Context::request`].
    ///
    /// `id` is the id `Context::request` returned. `result` is the response's
    /// result, or the error the client answered with — read leniently: an
    /// error without a message has an empty one, and an error whose code is
    /// missing or out of range, or that is not an object at all, arrives as
    /// [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR).
    ///
    /// Each request is answered at most once. An answer whose id matches no
    /// request still awaiting one — a second answer, an id the server never
    /// sent, or an answer with no id — is logged to the client and dropped. The
    /// server never replies to an answer.
    ///
    /// At most 1024 requests await an answer at once (see
    /// [`Context::request`]). When a new request would exceed that, the oldest
    /// is cancelled: this method receives
    /// [`REQUEST_CANCELLED`](ResponseError::REQUEST_CANCELLED) for it, after
    /// the call that sent the new request returns, and a late answer from the
    /// client is dropped.
    ///
    /// The default ignores them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Handler, Json, ResponseError, Value};
    ///
    /// #[derive(Default)]
    /// struct Settings { pending: Option<u64>, tab_width: u64 }
    ///
    /// impl<G: Grammar> Handler<G> for Settings {
    ///     fn response(&mut self, id: u64, result: Result<Json<'_>, ResponseError>, _: &mut Context<'_, G>) {
    ///         if self.pending == Some(id) {
    ///             self.pending = None;
    ///             if let Ok(Ok(Value::Array(items))) = result.map(|json| json.parse::<Value>()) {
    ///                 self.tab_width = items.first().and_then(|s| s["tabWidth"].as_u64()).unwrap_or(4);
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    fn response(
        &mut self,
        id: u64,
        result: Result<Json<'_>, ResponseError>,
        cx: &mut Context<'_, G>,
    ) {
        let _unused = (id, result, cx);
    }
}
