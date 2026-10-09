//! [`Context`]: what a handler can reach while it runs — the open documents,
//! and a channel back to the client.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use diag_lang::Diagnostic;
use incremental_lang::Grammar;
use serde::Serialize;

use crate::{
    diagnostics::{Scratch, write_clear, write_publish},
    document::Document,
    error::Error,
    transport::Outbox,
};

/// The open documents, keyed by URI. Ordered, so iteration — and anything a
/// handler publishes while iterating — is the same from run to run.
pub(crate) type Workspace<G> = BTreeMap<Box<str>, Document<G>>;

/// A handler's view of the session for the duration of one call.
///
/// Every [`Handler`](crate::Handler) method receives one. Through it a handler
/// reads the open documents and talks back to the client: notifications
/// ([`notify`](Context::notify)), server-to-client requests
/// ([`request`](Context::request)), and diagnostics
/// ([`publish_diagnostics`](Context::publish_diagnostics)).
///
/// Messages sent through a context are buffered and written to the client in
/// one write, in the order they were sent, as soon as the handler returns —
/// before the server reads the next message. A response to the request being
/// handled, if there is one, is written after them.
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

/// Everything a session sends with: the outgoing messages, the bookkeeping for
/// the server's own requests, and scratch space reused across diagnostics
/// batches.
#[derive(Debug, Default)]
pub(crate) struct Wire {
    pub(crate) outbox: Outbox,
    pub(crate) requests: Requests,
    scratch: Scratch,
}

/// Most of the server's own requests that may await an answer at once. A
/// client answers each in turn, so a real session keeps a handful
/// outstanding; the cap only bounds a handler that sends requests the client
/// never answers. Issuing one more cancels the oldest (see [`Requests::issue`]).
pub(crate) const MAX_PENDING_REQUESTS: usize = 1024;

/// The server's own requests to the client: the next id to use, the ids still
/// awaiting an answer, and the ids cancelled for want of room, whose handler
/// has not been told yet.
///
/// Ids count up from 0, so the smallest pending id is the oldest request (ids
/// would wrap only after 2^64 requests). A `BTreeSet` gives the O(log n)
/// settle and the O(log n) "remove the oldest" the cap needs.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    next: u64,
    pending: BTreeSet<u64>,
    cancelled: Vec<u64>,
}

impl Requests {
    /// Records `id` as awaiting an answer. At the cap, the oldest outstanding
    /// request is dropped to make room, and its id is returned so the caller
    /// can tell the client; the server tells the handler once the current call
    /// returns (see [`take_cancelled`](Requests::take_cancelled)).
    fn issue(&mut self, id: u64) -> Option<u64> {
        let evicted = if self.pending.len() >= MAX_PENDING_REQUESTS {
            self.pending.pop_first()
        } else {
            None
        };
        let _fresh = self.pending.insert(id);
        if let Some(oldest) = evicted {
            self.cancelled.push(oldest);
        }
        evicted
    }

    /// Marks `id` answered. Returns `false` if no request with that id is
    /// outstanding — a forged, duplicate, or stray answer, or an answer to a
    /// request cancelled to stay under the cap.
    pub(crate) fn settle(&mut self, id: u64) -> bool {
        self.pending.remove(&id)
    }

    /// Takes the ids cancelled since the last call, oldest first.
    pub(crate) fn take_cancelled(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.cancelled)
    }

    /// Forgets every outstanding and cancelled request. Used when `initialize`
    /// fails: what the handler sent during it is never written, so nothing
    /// will answer it.
    pub(crate) fn forget(&mut self) {
        self.pending.clear();
        self.cancelled.clear();
    }

    /// How many requests await an answer, for tests.
    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> usize {
        self.pending.len()
    }
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
    /// use lsp_lang::{Context, Position};
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
    /// // Total bytes the client has open — for a status message, say.
    /// fn open_bytes<G: Grammar>(cx: &Context<'_, G>) -> usize {
    ///     cx.documents().map(|doc| doc.text().len()).sum()
    /// }
    /// ```
    pub fn documents(&self) -> impl ExactSizeIterator<Item = &'a Document<G>> + 'a {
        self.workspace.values()
    }

    /// Sends a notification to the client.
    ///
    /// `params` is serialized straight into the outgoing message: any
    /// `Serialize` type, including a [`Value`](crate::Value) built with
    /// [`json!`](crate::json).
    ///
    /// # Errors
    ///
    /// [`Error::Json`] if `params` fails to serialize. Nothing is sent in that
    /// case, and the session carries on.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Error};
    ///
    /// // Pop up a message in the editor (`MessageType` 3 is "info").
    /// fn announce<G: Grammar>(cx: &mut Context<'_, G>) -> Result<(), Error> {
    ///     cx.notify("window/showMessage", &json!({ "type": 3, "message": "Indexing done" }))
    /// }
    /// ```
    pub fn notify<P: Serialize + ?Sized>(&mut self, method: &str, params: &P) -> Result<(), Error> {
        self.wire.outbox.notify(method, params)
    }

    /// Sends a request to the client and returns the id it was sent with.
    ///
    /// The client's answer arrives later, as a call to
    /// [`Handler::response`](crate::Handler::response) with the same id; keep
    /// the id to tell answers apart. Ids are assigned from a counter starting at
    /// 0 for each session. The server remembers which ids are awaiting an
    /// answer and delivers exactly one answer for each; anything else that
    /// looks like a response is logged and dropped.
    ///
    /// At most 1024 requests await an answer at once. Sending one more cancels
    /// the oldest outstanding request: the server sends the client
    /// `$/cancelRequest` for it and, once the current handler call returns,
    /// delivers [`REQUEST_CANCELLED`](crate::ResponseError::REQUEST_CANCELLED)
    /// as its answer. An answer the client sends for it afterwards is logged
    /// and dropped, so each request is still answered exactly once.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] if `params` fails to serialize. Nothing is sent, and no
    /// id is used up.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsp_lang::incremental_lang::Grammar;
    /// use lsp_lang::{json, Context, Error};
    ///
    /// // Ask for the user's settings for this language.
    /// fn fetch_settings<G: Grammar>(cx: &mut Context<'_, G>) -> Result<u64, Error> {
    ///     cx.request("workspace/configuration", &json!({ "items": [{ "section": "lisp" }] }))
    /// }
    /// ```
    pub fn request<P: Serialize + ?Sized>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<u64, Error> {
        let requests = &mut self.wire.requests;
        let id = requests.next;
        self.wire.outbox.request(id, method, params)?;
        requests.next = id.wrapping_add(1);
        if let Some(oldest) = requests.issue(id) {
            // Tell the client to stop working on it; any answer it still
            // sends is logged and dropped, since the id is no longer pending.
            self.wire.outbox.cancel(oldest);
        }
        Ok(id)
    }

    /// Publishes `diagnostics` for `document`, replacing whatever was
    /// published for it before.
    ///
    /// Each [`Diagnostic`]'s spans are byte offsets into the document's text —
    /// the same space as the syntax tree's spans, and the space a
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
        let Wire {
            outbox, scratch, ..
        } = &mut *self.wire;
        outbox.notify_with("textDocument/publishDiagnostics", |out| {
            write_publish(out, scratch, document, diagnostics);
        });
    }

    /// Clears the diagnostics published for `uri`.
    ///
    /// Works whether or not the document is open — the usual call is on
    /// close, which is what the default
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
        self.wire
            .outbox
            .notify_with("textDocument/publishDiagnostics", |out| {
                write_clear(out, uri)
            });
    }
}

impl<G: Grammar> fmt::Debug for Context<'_, G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("documents", &self.workspace.len())
            .field("next_id", &self.wire.requests.next)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_requests_settle_each_id_once() {
        let mut requests = Requests::default();
        for id in 0..3 {
            assert_eq!(requests.issue(id), None);
        }
        assert!(requests.settle(1));
        assert!(!requests.settle(1), "a second answer is refused");
        assert!(!requests.settle(99), "an id never issued is refused");
        assert_eq!(requests.outstanding(), 2);
    }

    #[test]
    fn test_requests_at_the_cap_cancel_the_oldest() {
        let mut requests = Requests::default();
        let cap = MAX_PENDING_REQUESTS as u64;
        for id in 0..cap {
            assert_eq!(requests.issue(id), None);
        }
        // Answering one makes room without cancelling anything.
        assert!(requests.settle(5));
        assert_eq!(requests.issue(cap), None);
        // Now full again: each new request cancels the oldest still waiting.
        assert_eq!(requests.issue(cap + 1), Some(0));
        assert_eq!(requests.issue(cap + 2), Some(1));
        assert_eq!(requests.outstanding(), MAX_PENDING_REQUESTS);
        assert_eq!(requests.take_cancelled(), [0, 1]);
        assert!(requests.take_cancelled().is_empty());
        assert!(!requests.settle(0), "a cancelled request takes no answer");
        assert!(requests.settle(cap + 2));
    }

    #[test]
    fn test_requests_forget_clears_everything_but_the_counter() {
        let mut requests = Requests::default();
        for id in 0..=MAX_PENDING_REQUESTS as u64 {
            let _cancelled = requests.issue(id);
        }
        requests.forget();
        assert_eq!(requests.outstanding(), 0);
        assert!(requests.take_cancelled().is_empty());
    }
}
