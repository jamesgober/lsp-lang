//! The back half of a session: the state of one connection (lifecycle,
//! documents, the semantic-token cache) and the code that runs a queued
//! [`Job`] against the language's [`Handler`].
//!
//! Everything the language is asked to do happens here, one job at a time, in
//! the order the messages arrived, and every call into code the server does not
//! own (the handler and the grammar) is contained, so a bug there costs one
//! request or one document, never the session.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    fmt,
    panic::{self, AssertUnwindSafe},
};

use incremental_lang::Grammar;
use jsonx_lib::Value;
use rpc_io::{CallError, Json, Responder};

use crate::{
    completion,
    context::{Context, MessageType, Wire, Workspace},
    document::Document,
    error::Error,
    features::Features,
    handler::Handler,
    jobs::{Edit, Feature, Job},
    json::{trusted_raw, write_str},
    position::Encoding,
    response::ResponseError,
    semantic::{self, SemanticTokens},
    symbols,
};

/// Where a session is in the protocol's lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Before a successful `initialize`.
    Uninitialized,
    /// Between `initialize` and `shutdown`.
    Running,
    /// After `shutdown`, waiting for `exit`.
    ShuttingDown,
}

/// The last semantic tokens sent for a document, so the next request can be
/// answered with the difference.
struct Cached {
    id: String,
    data: Vec<u32>,
}

/// The state of one [`Server::run`](crate::Server::run).
pub(crate) struct Session<'h, G: Grammar, H> {
    grammar: G,
    handler: &'h mut H,
    pub(crate) phase: Phase,
    encoding: Encoding,
    features: Features,
    info: Option<(String, String)>,
    workspace: Workspace<G>,
    /// URIs the client opened but the grammar could not parse: not in the
    /// workspace, but a whole-text change to one opens it. Bounded, like the
    /// workspace, by the documents the client holds open.
    unopened: BTreeSet<Box<str>>,
    semantic: BTreeMap<Box<str>, Cached>,
    next_result: u64,
    pub(crate) wire: Wire,
    /// Set by `exit`: how the session ended.
    pub(crate) finished: Option<Result<(), Error>>,
}

impl<'h, G: Grammar + Clone, H: Handler<G>> Session<'h, G, H> {
    pub(crate) fn new(
        grammar: G,
        handler: &'h mut H,
        wire: Wire,
        info: Option<(String, String)>,
    ) -> Self {
        Self {
            grammar,
            handler,
            phase: Phase::Uninitialized,
            encoding: Encoding::default(),
            features: Features::new(),
            info,
            workspace: Workspace::new(),
            unopened: BTreeSet::new(),
            semantic: BTreeMap::new(),
            next_result: 0,
            wire,
            finished: None,
        }
    }

    /// Runs one job.
    pub(crate) fn run(&mut self, job: Job) {
        match job {
            Job::Initialize {
                params,
                encoding,
                responder,
            } => self.initialize(&params, encoding, responder),
            Job::Shutdown { responder } => self.shutdown(responder),
            Job::Exit => {
                self.finished = Some(if self.phase == Phase::ShuttingDown {
                    Ok(())
                } else {
                    Err(Error::ExitWithoutShutdown)
                });
            }
            Job::Request {
                method,
                params,
                responder,
            } => self.custom_request(&method, &params, responder),
            Job::Feature {
                method,
                request,
                responder,
            } => self.feature(method, request, responder),
            Job::Log { kind, message } => {
                // Before `initialize` (and after a failed one) the client is
                // not ready for server messages, and the transport would hold
                // this until a later `initialize` succeeded.
                if self.phase == Phase::Running {
                    self.wire.log(kind, &message);
                }
            }
            Job::Malformed { method, why } => {
                if self.phase == Phase::Running {
                    let message = format!("ignored `{method}`: {why}");
                    self.wire.log(MessageType::Error, &message);
                }
            }
            // Notifications reach the handler only inside the session window.
            other if self.phase != Phase::Running => drop(other),
            Job::Open { uri, version, text } => self.did_open(uri, version, text),
            Job::Change {
                uri,
                version,
                edits,
            } => self.did_change(&uri, version, &edits),
            Job::Close { uri } => self.did_close(&uri),
            Job::Notification { method, params } => self.contained(
                || format!("`{method}`"),
                |handler, cx| handler.notification(&method, &params, cx),
            ),
        }
    }

    // ----- Lifecycle ------------------------------------------------------

    fn initialize(&mut self, params: &Value<'static>, encoding: Encoding, responder: Responder) {
        if responder.is_cancelled() {
            return;
        }
        let result = match self.phase {
            Phase::Uninitialized => self.negotiate(params, encoding),
            _ => Err(ResponseError::new(
                ResponseError::INVALID_REQUEST,
                "`initialize` may only be sent once",
            )),
        };
        self.answer(responder, result);
    }

    /// Asks the handler to initialize and for its features, and builds the
    /// `initialize` result. The session moves on only if all of it works.
    fn negotiate(
        &mut self,
        params: &Value<'static>,
        encoding: Encoding,
    ) -> Result<Json<'static>, ResponseError> {
        let Self {
            handler,
            workspace,
            wire,
            ..
        } = self;
        let mut cx = Context::new(workspace, wire);
        caught(|| handler.initialize(params, &mut cx))
            .unwrap_or_else(|why| Err(handler_panicked("initialize", &why)))?;
        let features =
            caught(|| handler.features()).map_err(|why| handler_panicked("features", &why))?;

        let mut text = String::from("{\"capabilities\":");
        features.write_capabilities(&mut text, Some(encoding));
        if let Some((name, version)) = &self.info {
            text.push_str(",\"serverInfo\":{\"name\":");
            write_str(&mut text, name);
            text.push_str(",\"version\":");
            write_str(&mut text, version);
            text.push('}');
        }
        text.push('}');
        let reply = raw(&text)?;

        self.encoding = encoding;
        self.features = features;
        self.phase = Phase::Running;
        self.wire.go_direct(true);
        Ok(reply)
    }

    fn shutdown(&mut self, responder: Responder) {
        if responder.is_cancelled() {
            return;
        }
        let result = match self.phase {
            Phase::Running => {
                self.phase = Phase::ShuttingDown;
                Ok(Json::from(()))
            }
            _ => self.lifecycle("shutdown").map(|()| Json::from(())),
        };
        self.answer(responder, result);
    }

    /// `Ok` when requests are allowed; otherwise the error the protocol
    /// prescribes for `method` in the session's phase.
    fn lifecycle(&self, method: &str) -> Result<(), ResponseError> {
        match self.phase {
            Phase::Running => Ok(()),
            Phase::Uninitialized => Err(ResponseError::new(
                ResponseError::SERVER_NOT_INITIALIZED,
                format!("`{method}` received before `initialize`"),
            )),
            Phase::ShuttingDown => Err(ResponseError::new(
                ResponseError::INVALID_REQUEST,
                format!("`{method}` received after `shutdown`"),
            )),
        }
    }

    /// Sends the response to a request. A request that was cancelled while it
    /// ran was already answered by the transport, and its reply is dropped.
    fn answer(&mut self, responder: Responder, result: Result<Json<'static>, ResponseError>) {
        let sent = responder.respond(result.map_err(rpc_io::ErrorObject::from));
        match sent {
            Ok(()) | Err(rpc_io::Error::AlreadyAnswered) => {}
            Err(error) => self.wire.remember_failure(Error::from(error)),
        }
    }

    // ----- Requests -------------------------------------------------------

    fn custom_request(&mut self, method: &str, params: &Value<'static>, responder: Responder) {
        if responder.is_cancelled() {
            return;
        }
        let result = self.lifecycle(method).and_then(|()| {
            let Self {
                handler,
                workspace,
                wire,
                ..
            } = self;
            let mut cx = Context::new(workspace, wire);
            let value = caught(|| handler.request(method, params, &mut cx))
                .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
            Ok(Json::Value(value))
        });
        self.answer(responder, result);
    }

    fn feature(
        &mut self,
        method: &'static str,
        request: Result<Feature, ResponseError>,
        responder: Responder,
    ) {
        if responder.is_cancelled() {
            return;
        }
        let result = match self.lifecycle(method) {
            Err(error) => Err(error),
            Ok(()) => request.and_then(|feature| self.run_feature(method, feature)),
        };
        self.answer(responder, result);
    }

    fn run_feature(
        &mut self,
        method: &'static str,
        feature: Feature,
    ) -> Result<Json<'static>, ResponseError> {
        let Self {
            handler,
            workspace,
            wire,
            features,
            semantic,
            next_result,
            ..
        } = self;
        let null = || Ok(Json::from(()));
        match feature {
            Feature::Hover(uri, at) if features.hover => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let hover = caught(|| handler.hover(document, at, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                match hover {
                    Some(hover) => written(|out| hover.write(out)),
                    None => null(),
                }
            }
            Feature::Definition(uri, at) if features.definition => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let found = caught(|| handler.definition(document, at, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                if found.is_empty() {
                    return null();
                }
                written(|out| {
                    out.push('[');
                    for (i, location) in found.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        location.write(out);
                    }
                    out.push(']');
                })
            }
            Feature::Completion(uri, at) if features.completion.is_some() => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let items = caught(|| handler.completion(document, at, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                written(|out| completion::write_list(out, &items))
            }
            Feature::Symbols(uri) if features.document_symbols => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let symbols = caught(|| handler.document_symbols(document, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                written(|out| symbols::write_symbols(out, &symbols))
            }
            Feature::SemanticFull(uri) if features.semantic_tokens.is_some() => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let tokens = caught(|| handler.semantic_tokens(document, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                send_full(semantic, next_result, &uri, tokens)
            }
            Feature::SemanticDelta { uri, previous } if features.semantic_tokens.is_some() => {
                let Some(document) = workspace.get(&*uri) else {
                    return null();
                };
                let mut cx = Context::new(workspace, wire);
                let tokens = caught(|| handler.semantic_tokens(document, &mut cx))
                    .unwrap_or_else(|why| Err(handler_panicked(method, &why)))?;
                match semantic.get(&*uri) {
                    Some(cached) if cached.id == previous => {
                        send_delta(semantic, next_result, &uri, tokens)
                    }
                    _ => send_full(semantic, next_result, &uri, tokens),
                }
            }
            // A feature the handler did not list: not offered, so not found.
            _ => Err(ResponseError::unsupported(method)),
        }
    }

    // ----- Document synchronization ---------------------------------------

    fn did_open(&mut self, uri: String, version: i32, text: String) {
        let grammar = self.grammar.clone();
        let encoding = self.encoding;
        match guarded(|| Document::open(grammar, &uri, version, text, encoding)) {
            Ok(document) => {
                let uri: Box<str> = uri.into();
                let _was_unopened = self.unopened.remove(&uri);
                let _replaced = self.workspace.insert(uri.clone(), document);
                let _stale = self.semantic.remove(&uri);
                self.changed(&uri);
            }
            Err(why) => {
                // Keep what there is: a document already open under this URI
                // stays, at its last good state, marked out of step; a new one
                // is remembered so a whole-text change can still open it.
                let message = match self.workspace.get_mut(&*uri) {
                    Some(document) => {
                        document.mark_stale();
                        format!(
                            "could not reopen `{uri}`: {why}; keeping version {} until {RESYNC}",
                            document.version()
                        )
                    }
                    None => {
                        let _fresh = self.unopened.insert(uri.as_str().into());
                        format!("could not open `{uri}`: {why}; it stays closed until {RESYNC}")
                    }
                };
                self.wire.log(MessageType::Error, &message);
            }
        }
    }

    fn did_change(&mut self, uri: &str, version: i32, edits: &[Edit]) {
        // A whole-text change supersedes every edit before it and applies
        // whatever state this copy is in: it is how a document out of step
        // with the client gets back in step.
        let mut from = edits.iter().rposition(|edit| edit.range.is_none());
        if !self.workspace.contains_key(uri) {
            if !self.open_from_change(uri, version, edits, from) {
                return;
            }
            // Opened from the whole-text change; the edits after it remain.
            from = from.map(|at| at + 1);
        }
        let Some(document) = self.workspace.get_mut(uri) else {
            return;
        };
        let edits = match from {
            Some(at) => edits.get(at..).unwrap_or_default(),
            None if document.is_stale() => {
                let message = format!(
                    "ignored a change to `{uri}`, which is out of step; waiting for {RESYNC}"
                );
                return self.wire.log(MessageType::Warning, &message);
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
                return self.wire.log(MessageType::Warning, &message);
            }
            None => edits,
        };
        match apply(document, edits) {
            Ok(recovered) => {
                document.synced(version);
                if let Some(fault) = recovered {
                    // In step, but the grammar has a bug worth knowing about.
                    let message = format!(
                        "reparsing an edit to `{uri}` incrementally failed ({fault}); \
                         the whole text was parsed instead"
                    );
                    self.wire.log(MessageType::Warning, &message);
                }
                self.changed(uri);
            }
            Err(failure) => {
                document.mark_stale();
                let message = format!(
                    "`{uri}` is out of step: {failure}; keeping version {} until {RESYNC}",
                    document.version()
                );
                self.wire.log(failure.level(), &message);
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
        edits: &[Edit],
        whole: Option<usize>,
    ) -> bool {
        let failed_open = self.unopened.contains(uri);
        let text = whole
            .filter(|_| failed_open)
            .and_then(|at| edits.get(at))
            .map(|edit| edit.text.clone());
        let Some(text) = text else {
            let message = if failed_open {
                format!(
                    "ignored a change to `{uri}`, which could not be opened; waiting for {RESYNC}"
                )
            } else {
                format!("ignored a change to `{uri}`, which is not open")
            };
            self.wire.log(MessageType::Warning, &message);
            return false;
        };
        let (grammar, encoding) = (self.grammar.clone(), self.encoding);
        match guarded(|| Document::open(grammar, uri, version, text, encoding)) {
            Ok(document) => {
                let _was_unopened = self.unopened.remove(uri);
                let _fresh = self.workspace.insert(uri.into(), document);
                true
            }
            Err(why) => {
                let message = format!("could not open `{uri}` from a whole-text change: {why}");
                self.wire.log(MessageType::Error, &message);
                false
            }
        }
    }

    fn did_close(&mut self, uri: &str) {
        let _was_unopened = self.unopened.remove(uri);
        let _stale = self.semantic.remove(uri);
        if self.workspace.remove(uri).is_some() {
            self.contained(
                || format!("`textDocument/didClose` for `{uri}`"),
                |handler, cx| handler.closed(uri, cx),
            );
        }
    }

    /// Calls [`Handler::changed`] for the open document `uri`.
    fn changed(&mut self, uri: &str) {
        let Self {
            workspace,
            wire,
            handler,
            ..
        } = self;
        let Some(document) = workspace.get(uri) else {
            return;
        };
        let mut cx = Context::new(workspace, wire);
        let outcome = caught(|| handler.changed(document, &mut cx));
        if let Err(why) = outcome {
            let message = format!("the handler panicked in a change to `{uri}`: {why}");
            wire.log(MessageType::Error, &message);
        }
    }

    /// Calls a handler method that has no result. A panic in it is contained:
    /// it is logged to the client as having happened during `during()`, and
    /// the session carries on.
    fn contained(
        &mut self,
        during: impl FnOnce() -> String,
        call: impl FnOnce(&mut H, &mut Context<'_, G>),
    ) {
        let Self {
            handler,
            workspace,
            wire,
            ..
        } = self;
        let mut cx = Context::new(workspace, wire);
        let outcome = caught(|| call(handler, &mut cx));
        if let Err(why) = outcome {
            let message = format!("the handler panicked in {}: {why}", during());
            wire.log(MessageType::Error, &message);
        }
    }

    // ----- The server's own requests --------------------------------------

    /// Delivers the outcome of every request of the server's own that has
    /// one. Requests a handler sends while hearing of these wait for the next
    /// poll, so a handler that answers every outcome with a new request cannot
    /// keep this going.
    pub(crate) fn poll_calls(&mut self) {
        if self.wire.calls.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.wire.calls);
        let mut ready = Vec::new();
        for call in waiting {
            let id = call.id();
            match call.try_wait() {
                Ok(outcome) => ready.push((id, outcome)),
                Err(call) => self.wire.calls.push(call),
            }
        }
        for (id, outcome) in ready {
            self.deliver(id, outcome);
        }
    }

    fn deliver(&mut self, id: u64, outcome: Result<rpc_io::Raw<'static>, CallError>) {
        if matches!(outcome, Err(CallError::Closed)) {
            return;
        }
        let Self {
            handler,
            workspace,
            wire,
            ..
        } = self;
        let mut cx = Context::new(workspace, wire);
        let called = caught(|| match &outcome {
            Ok(raw) => match raw.parse() {
                Ok(value) => handler.response(id, Ok(value), &mut cx),
                Err(error) => handler.response(id, Err(ResponseError::from(error)), &mut cx),
            },
            Err(error) => handler.response(id, Err(call_error(error)), &mut cx),
        });
        if let Err(why) = called {
            let message = format!("the handler panicked in the answer to request {id}: {why}");
            wire.log(MessageType::Error, &message);
        }
    }
}

/// How a document out of step with the client is recovered, for log messages.
const RESYNC: &str = "a whole-text change or a reopen";

/// A request of the server's own that ended without a result, as the error the
/// handler is told about.
fn call_error(error: &CallError) -> ResponseError {
    match error {
        CallError::Rejected(object) | CallError::InvalidResponse(object) => {
            ResponseError::from(object.clone())
        }
        CallError::Evicted | CallError::Cancelled | CallError::TimedOut => ResponseError::new(
            ResponseError::REQUEST_CANCELLED,
            format!("cancelled without an answer: {error}"),
        ),
        other => ResponseError::new(ResponseError::INTERNAL_ERROR, other.to_string()),
    }
}

/// Wraps text the server wrote as a response.
fn raw(text: &str) -> Result<Json<'static>, ResponseError> {
    trusted_raw(text).map(Json::Raw)
}

/// Writes a response with `write` and wraps it.
fn written(write: impl FnOnce(&mut String)) -> Result<Json<'static>, ResponseError> {
    let mut text = String::new();
    write(&mut text);
    raw(&text)
}

/// A full semantic-token answer; remembers it for the next delta.
fn send_full(
    cache: &mut BTreeMap<Box<str>, Cached>,
    counter: &mut u64,
    uri: &str,
    tokens: SemanticTokens,
) -> Result<Json<'static>, ResponseError> {
    let data = checked(tokens)?;
    *counter += 1;
    let id = counter.to_string();
    let answer = SemanticTokens::from_data(data).with_result_id(id.clone());
    let reply = written(|out| answer.write(out));
    let _previous = cache.insert(
        uri.into(),
        Cached {
            id,
            data: answer.into_data(),
        },
    );
    reply
}

/// A delta semantic-token answer against the cached tokens of `uri`.
fn send_delta(
    cache: &mut BTreeMap<Box<str>, Cached>,
    counter: &mut u64,
    uri: &str,
    tokens: SemanticTokens,
) -> Result<Json<'static>, ResponseError> {
    let data = checked(tokens)?;
    *counter += 1;
    let id = counter.to_string();
    let edit = match cache.get(uri) {
        Some(previous) => semantic::diff(&previous.data, &data),
        None => semantic::diff(&[], &data),
    };
    let reply = written(|out| semantic::write_delta(out, &id, edit));
    let _previous = cache.insert(uri.into(), Cached { id, data });
    reply
}

/// The tokens' data, if it is whole tokens.
fn checked(tokens: SemanticTokens) -> Result<Vec<u32>, ResponseError> {
    let data = tokens.into_data();
    if data.len() % 5 == 0 {
        Ok(data)
    } else {
        Err(ResponseError::new(
            ResponseError::INTERNAL_ERROR,
            format!(
                "the handler returned {} integers of semantic token data, which is not a multiple of five",
                data.len()
            ),
        ))
    }
}

/// Applies a `didChange`'s edits, in order, to `document`.
///
/// Each edit is placed (see [`Document::edit_span`]) and reparsed
/// incrementally. If the incremental reparse fails (the grammar panicked or
/// broke its contract on the fragment) the same edit is applied again by
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
    edits: &[Edit],
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
    edit: &Edit,
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
    fn level(&self) -> MessageType {
        match self {
            Failure::Misplaced | Failure::Refused(_) => MessageType::Warning,
            Failure::Unparsable(_) => MessageType::Error,
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
/// Used for every call into code the server does not own (the grammar and
/// the handler), so a bug there costs one request, notification, or document,
/// not the session. The server's own state stays consistent across a caught
/// panic: the handler sees the workspace only through a shared reference, and
/// the outgoing messages are committed whole by the transport. The handler's
/// own state is whatever its panic left.
pub(crate) fn caught<T>(work: impl FnOnce() -> T) -> Result<T, String> {
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

/// At most the first `max` bytes of `text`, on a character boundary, with an
/// ellipsis when cut.
pub(crate) fn clip_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_short_text_is_unchanged() {
        assert_eq!(clip_to("\"abc\"", 64), "\"abc\"");
        assert_eq!(clip_to(&"x".repeat(64), 64), "x".repeat(64));
    }

    #[test]
    fn test_clip_long_text_is_cut_on_a_char_boundary() {
        let clipped = clip_to(&"é".repeat(100), 64);
        assert!(clipped.ends_with('…'));
        assert!(clipped.len() <= 64 + '…'.len_utf8());
        assert_eq!(
            clip_to(&"[".repeat(2_000_000), 64).len(),
            64 + '…'.len_utf8()
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
        assert_eq!(Failure::Misplaced.level(), MessageType::Warning);
        let too_large = incremental_lang::Error::TooLarge { len: 1 << 33 };
        assert_eq!(Failure::Refused(too_large).level(), MessageType::Warning);
        let panicked = Failure::Unparsable(Fault::Panic("bug".to_owned()));
        assert_eq!(panicked.level(), MessageType::Error);
        assert!(panicked.to_string().contains("the grammar panicked: bug"));
    }

    #[test]
    fn test_call_errors_map_to_response_errors() {
        let rejected = CallError::Rejected(rpc_io::ErrorObject::new(rpc_io::ErrorCode(-5), "no"));
        assert_eq!(call_error(&rejected).code(), -5);
        for ended in [
            CallError::Evicted,
            CallError::Cancelled,
            CallError::TimedOut,
        ] {
            assert_eq!(call_error(&ended).code(), ResponseError::REQUEST_CANCELLED);
        }
        assert_eq!(
            call_error(&CallError::WouldDeadlock).code(),
            ResponseError::INTERNAL_ERROR
        );
    }

    #[test]
    fn test_checked_refuses_partial_tokens() {
        assert!(checked(SemanticTokens::from_data(vec![0; 10])).is_ok());
        let err = checked(SemanticTokens::from_data(vec![0; 7])).err();
        assert_eq!(err.map(|e| e.code()), Some(ResponseError::INTERNAL_ERROR));
    }
}
