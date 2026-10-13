//! [`Handler`]: the trait a language server implements.

use incremental_lang::Grammar;
use jsonx_lib::Value;

use crate::{
    completion::CompletionItem,
    context::Context,
    document::Document,
    features::Features,
    position::Position,
    response::ResponseError,
    semantic::SemanticTokens,
    symbols::DocumentSymbol,
    types::{Hover, Location},
};

/// A language server's behaviour: the part of a server that is about the
/// language, as opposed to the protocol.
///
/// The [`Server`](crate::Server) owns the protocol (framing, the
/// `initialize`/`shutdown`/`exit` lifecycle, document synchronization,
/// cancellation) and calls a handler for everything else. Every method has a
/// default, so a handler implements only what its language supports: a
/// diagnostics-only server needs [`changed`](Handler::changed) and nothing
/// more.
///
/// There are three groups of methods.
///
/// **Lifecycle and documents.** [`initialize`](Handler::initialize),
/// [`features`](Handler::features), [`changed`](Handler::changed),
/// [`closed`](Handler::closed).
///
/// **Typed features.** [`hover`](Handler::hover),
/// [`definition`](Handler::definition), [`completion`](Handler::completion),
/// [`document_symbols`](Handler::document_symbols),
/// [`semantic_tokens`](Handler::semantic_tokens). Override the method and list
/// the feature in [`features`](Handler::features); the server advertises
/// exactly what is listed, parses the request, finds the document, and writes
/// the answer.
///
/// **Everything else.** [`request`](Handler::request),
/// [`notification`](Handler::notification), and [`response`](Handler::response)
/// carry other methods as parsed JSON.
///
/// | Method | Called when | Default |
/// |---|---|---|
/// | [`initialize`](Handler::initialize) | the client sends `initialize` | accepts |
/// | [`features`](Handler::features) | after `initialize`, to build capabilities | none |
/// | [`changed`](Handler::changed) | a document opens or changes | nothing |
/// | [`closed`](Handler::closed) | a document closes | clears its diagnostics |
/// | [`hover`](Handler::hover), [`definition`](Handler::definition), [`completion`](Handler::completion), [`document_symbols`](Handler::document_symbols), [`semantic_tokens`](Handler::semantic_tokens) | the matching request | `MethodNotFound` |
/// | [`request`](Handler::request) | a custom request registered on the server | `MethodNotFound` |
/// | [`notification`](Handler::notification) | `initialized`, `didSave`, and other notifications | ignored |
/// | [`response`](Handler::response) | the client answers a [`Context::request`] | ignored |
///
/// Calls are made one at a time, in message order, on the thread running
/// [`Server::run`](crate::Server::run). A request is answered with whatever its
/// method returns, so its result is computed before the next message is
/// handled; a handler with slow work can snapshot the tree ([`Document::tree`]
/// clones in O(1)) and hand it to a worker, reporting back through
/// [`Context::notify`] on a later call.
///
/// A panic in any method is contained where panics unwind: a request is
/// answered with [`INTERNAL_ERROR`](ResponseError::INTERNAL_ERROR) carrying the
/// panic's message, a panic anywhere else is logged to the client
/// (`window/logMessage`), and the session carries on with the same handler.
/// Messages the handler sent before panicking are still sent. The handler's own
/// state is whatever the panic left. (With `panic = "abort"` the process ends
/// instead, as that setting asks.)
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
/// use lsp_lang::{Context, Document, Features, Handler, Hover, Position, ResponseError};
///
/// struct LineHover;
///
/// impl Handler<Plain> for LineHover {
///     fn features(&self) -> Features {
///         Features::new().hover()
///     }
///
///     fn hover(
///         &mut self,
///         doc: &Document<Plain>,
///         at: Position,
///         _: &mut Context<'_, Plain>,
///     ) -> Result<Option<Hover>, ResponseError> {
///         let line = doc.text().lines().nth(at.line as usize).unwrap_or("");
///         Ok(Some(Hover::markdown(line)))
///     }
/// }
/// ```
pub trait Handler<G: Grammar> {
    /// Called when the client sends `initialize`, before the server answers it.
    ///
    /// `params` is the client's `InitializeParams`, for anything the handler
    /// needs from it: `rootUri`, `initializationOptions`, client capabilities.
    /// It is `null` when the client sent none.
    ///
    /// Returning an error refuses the `initialize` request; the session stays
    /// uninitialized and the client may try again.
    ///
    /// Anything sent through `cx` here is held back and reaches the client
    /// right after the `initialize` response, in the order sent: the protocol
    /// lets a server send nothing else before it has answered. If `initialize`
    /// fails (an error, or a panic), what was sent is discarded, never written;
    /// a request among it is never sent and is answered with
    /// [`REQUEST_CANCELLED`](ResponseError::REQUEST_CANCELLED) through
    /// [`response`](Handler::response). Even so, wait for the `initialized`
    /// notification (see [`notification`](Handler::notification)) before
    /// registering capabilities or asking for configuration, as the protocol
    /// advises.
    ///
    /// The capabilities come from [`features`](Handler::features), which the
    /// server calls once this method has returned.
    ///
    /// The default accepts.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Handler, ResponseError, Value};
    ///
    /// struct Project { root: Option<String> }
    ///
    /// impl<G: Grammar> Handler<G> for Project {
    ///     fn initialize(&mut self, params: &Value<'_>, _: &mut Context<'_, G>) -> Result<(), ResponseError> {
    ///         self.root = params["rootUri"].as_str().map(str::to_owned);
    ///         Ok(())
    ///     }
    /// }
    /// ```
    fn initialize(
        &mut self,
        params: &Value<'_>,
        cx: &mut Context<'_, G>,
    ) -> Result<(), ResponseError> {
        let _unused = (params, cx);
        Ok(())
    }

    /// The typed features this handler implements; the server advertises these
    /// capabilities and answers only these requests.
    ///
    /// Called once per session, after [`initialize`](Handler::initialize)
    /// returned, so what it returns may depend on what `initialize` learned.
    /// See [`Features`] for how a feature, its method, and its capability fit
    /// together.
    ///
    /// The default is no features: a server with document synchronization
    /// and nothing else.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Features, Handler};
    ///
    /// struct Navigator;
    ///
    /// impl<G: Grammar> Handler<G> for Navigator {
    ///     fn features(&self) -> Features {
    ///         Features::new().hover().definition().document_symbols()
    ///     }
    /// }
    /// ```
    fn features(&self) -> Features {
        Features::new()
    }

    /// Called after a document is opened and after each change to it, once
    /// the text and syntax tree are current.
    ///
    /// The usual body analyses the document and publishes what it finds with
    /// [`Context::publish_diagnostics`]. A `didChange` carrying several edits
    /// produces one call, after all of them are applied.
    ///
    /// If an edit's incremental reparse fails (the grammar panicked, or its
    /// lexer broke its contract on the reparsed fragment) the server applies
    /// the edit again by parsing the whole new text, logs a warning, and calls
    /// this method as usual. If the change cannot be applied at all (an edit
    /// names a line the document does not have, the version does not
    /// increase, the grammar fails on the whole text too, or the text would
    /// outgrow 4 GiB) the document is kept at its last good state (text,
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

    /// Answers `textDocument/hover`: what to show for the symbol at `position`.
    ///
    /// `position` is the client's, in the session [`Encoding`](crate::Encoding);
    /// turn it into a byte offset with [`Document::offset`]. Return `Ok(None)`
    /// for "nothing here". Requested only when [`Features::hover`] is listed;
    /// a request for a document the client does not have open is answered
    /// `null` without calling this method.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Document, Handler, Hover, Position, ResponseError};
    ///
    /// struct WordHover;
    ///
    /// impl<G: Grammar> Handler<G> for WordHover {
    ///     fn hover(
    ///         &mut self,
    ///         doc: &Document<G>,
    ///         at: Position,
    ///         _: &mut Context<'_, G>,
    ///     ) -> Result<Option<Hover>, ResponseError> {
    ///         let offset = doc.offset(at) as usize;
    ///         let word = doc.text()[offset..].split_whitespace().next();
    ///         Ok(word.map(|w| Hover::markdown(format!("`{w}`"))))
    ///     }
    /// }
    /// ```
    fn hover(
        &mut self,
        document: &Document<G>,
        position: Position,
        cx: &mut Context<'_, G>,
    ) -> Result<Option<Hover>, ResponseError> {
        let _unused = (document, position, cx);
        Err(ResponseError::unsupported("textDocument/hover"))
    }

    /// Answers `textDocument/definition`: where the symbol at `position` is
    /// defined. An empty list means "nowhere" and is sent as `null`.
    ///
    /// Locations may be in other documents; build ranges with the
    /// [`Document::range`] of the document they lie in. Requested only when
    /// [`Features::definition`] is listed.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Document, Handler, Location, Position, ResponseError};
    ///
    /// struct StartOfFile;
    ///
    /// impl<G: Grammar> Handler<G> for StartOfFile {
    ///     fn definition(
    ///         &mut self,
    ///         doc: &Document<G>,
    ///         _: Position,
    ///         _: &mut Context<'_, G>,
    ///     ) -> Result<Vec<Location>, ResponseError> {
    ///         let first = doc.range(lsp_lang::incremental_lang::Span::new(0, 0));
    ///         Ok(vec![Location::new(doc.uri(), first)])
    ///     }
    /// }
    /// ```
    fn definition(
        &mut self,
        document: &Document<G>,
        position: Position,
        cx: &mut Context<'_, G>,
    ) -> Result<Vec<Location>, ResponseError> {
        let _unused = (document, position, cx);
        Err(ResponseError::unsupported("textDocument/definition"))
    }

    /// Answers `textDocument/completion`: the suggestions at `position`.
    ///
    /// The server sends them as a complete list (`isIncomplete: false`).
    /// Requested only when [`Features::completion`] is listed.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{CompletionItem, CompletionItemKind, Context, Document, Handler, Position, ResponseError};
    ///
    /// struct Keywords;
    ///
    /// impl<G: Grammar> Handler<G> for Keywords {
    ///     fn completion(
    ///         &mut self,
    ///         _: &Document<G>,
    ///         _: Position,
    ///         _: &mut Context<'_, G>,
    ///     ) -> Result<Vec<CompletionItem>, ResponseError> {
    ///         Ok(["define", "if"]
    ///             .into_iter()
    ///             .map(|k| CompletionItem::new(k).with_kind(CompletionItemKind::Keyword))
    ///             .collect())
    ///     }
    /// }
    /// ```
    fn completion(
        &mut self,
        document: &Document<G>,
        position: Position,
        cx: &mut Context<'_, G>,
    ) -> Result<Vec<CompletionItem>, ResponseError> {
        let _unused = (document, position, cx);
        Err(ResponseError::unsupported("textDocument/completion"))
    }

    /// Answers `textDocument/documentSymbol`: the document's outline, as a
    /// hierarchy. Requested only when [`Features::document_symbols`] is listed.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Document, DocumentSymbol, Handler, ResponseError, SymbolKind};
    /// use lsp_lang::incremental_lang::Span;
    ///
    /// struct Outline;
    ///
    /// impl<G: Grammar> Handler<G> for Outline {
    ///     fn document_symbols(
    ///         &mut self,
    ///         doc: &Document<G>,
    ///         _: &mut Context<'_, G>,
    ///     ) -> Result<Vec<DocumentSymbol>, ResponseError> {
    ///         let whole = doc.range(Span::new(0, doc.text().len() as u32));
    ///         Ok(vec![DocumentSymbol::new(doc.uri(), SymbolKind::File, whole, whole)])
    ///     }
    /// }
    /// ```
    fn document_symbols(
        &mut self,
        document: &Document<G>,
        cx: &mut Context<'_, G>,
    ) -> Result<Vec<DocumentSymbol>, ResponseError> {
        let _unused = (document, cx);
        Err(ResponseError::unsupported("textDocument/documentSymbol"))
    }

    /// Answers `textDocument/semanticTokens/full`, and the delta form: the
    /// document's highlighting, built with a
    /// [`SemanticTokensBuilder`](crate::SemanticTokensBuilder). Requested only
    /// when [`Features::semantic_tokens`] is listed.
    ///
    /// Return every token; the server keeps the last answer per document and
    /// turns the next one into a small edit when the client asks for a delta.
    ///
    /// The default answers [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    fn semantic_tokens(
        &mut self,
        document: &Document<G>,
        cx: &mut Context<'_, G>,
    ) -> Result<SemanticTokens, ResponseError> {
        let _unused = (document, cx);
        Err(ResponseError::unsupported(
            "textDocument/semanticTokens/full",
        ))
    }

    /// Answers a custom request: a method registered with
    /// [`Server::custom_request`](crate::Server::custom_request). `params` is
    /// the request's parameters (`null` when absent).
    ///
    /// Requests only reach the handler between `initialize` and `shutdown`;
    /// the server answers anything outside that window with the error the
    /// protocol prescribes. The default answers
    /// [`METHOD_NOT_FOUND`](ResponseError::METHOD_NOT_FOUND).
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Handler, ResponseError, Value};
    ///
    /// struct Echo;
    ///
    /// impl<G: Grammar> Handler<G> for Echo {
    ///     fn request(&mut self, method: &str, params: &Value<'_>, _: &mut Context<'_, G>) -> Result<Value<'static>, ResponseError> {
    ///         match method {
    ///             // Answer with the parameters, unchanged.
    ///             "echo/params" => Ok(params.clone().into_owned()),
    ///             _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
    ///         }
    ///     }
    /// }
    /// ```
    fn request(
        &mut self,
        method: &str,
        params: &Value<'_>,
        cx: &mut Context<'_, G>,
    ) -> Result<Value<'static>, ResponseError> {
        let _unused = (params, cx);
        Err(ResponseError::unsupported(method))
    }

    /// Receives a notification the server does not handle itself: everything
    /// except `exit` and the document-synchronization ones. That includes
    /// `initialized`, `workspace/didChangeConfiguration`,
    /// `textDocument/didSave`, and the custom notifications registered with
    /// [`Server::custom_notification`](crate::Server::custom_notification).
    ///
    /// Notifications have no response. Notifications that arrive before
    /// `initialize` or after `shutdown` are dropped without reaching the
    /// handler, as the protocol prescribes. `$/cancelRequest` is the server's
    /// own business and never arrives here.
    ///
    /// The default ignores them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Handler, Value};
    ///
    /// struct Hello;
    ///
    /// impl<G: Grammar> Handler<G> for Hello {
    ///     fn notification(&mut self, method: &str, _: &Value<'_>, cx: &mut Context<'_, G>) {
    ///         if method == "initialized" {
    ///             // A failed send here would only skip the greeting.
    ///             let _sent = cx.notify("window/logMessage", json!({ "type": 3, "message": "ready" }));
    ///         }
    ///     }
    /// }
    /// ```
    fn notification(&mut self, method: &str, params: &Value<'_>, cx: &mut Context<'_, G>) {
        let _unused = (method, params, cx);
    }

    /// Receives the outcome of a request sent with [`Context::request`].
    ///
    /// `id` is the id `Context::request` returned. `result` is the client's
    /// result, or the error it answered with. An error without a message has
    /// an empty one; an answer that is not a valid response arrives as an
    /// error too, with the code the transport gave it.
    ///
    /// Each request has one outcome. A request cancelled for want of room (see
    /// [`Context::request`]) is delivered as
    /// [`REQUEST_CANCELLED`](ResponseError::REQUEST_CANCELLED) once the call
    /// that sent the new request returns. An answer that matches no
    /// outstanding request (a second answer, an id the server never sent) is
    /// dropped. The server never replies to an answer.
    ///
    /// The default ignores them.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{Context, Handler, ResponseError, Value};
    ///
    /// #[derive(Default)]
    /// struct Settings { pending: Option<u64>, tab_width: u64 }
    ///
    /// impl<G: Grammar> Handler<G> for Settings {
    ///     fn response(&mut self, id: u64, result: Result<Value<'_>, ResponseError>, _: &mut Context<'_, G>) {
    ///         if self.pending == Some(id) {
    ///             self.pending = None;
    ///             if let Ok(items) = result {
    ///                 self.tab_width = items[0]["tabWidth"].as_u64().unwrap_or(4);
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    fn response(
        &mut self,
        id: u64,
        result: Result<Value<'_>, ResponseError>,
        cx: &mut Context<'_, G>,
    ) {
        let _unused = (id, result, cx);
    }
}
