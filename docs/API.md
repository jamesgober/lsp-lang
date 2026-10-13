# lsp-lang &mdash; API Reference

> Complete reference for every public item in `lsp-lang`, with examples.
> **Status: 2.0.** The surface below is the `2.0` contract; it follows
> [Semantic Versioning](#stability) and will not change in a breaking way before
> `3.0`. See [`../dev/ROADMAP.md`](../dev/ROADMAP.md), and the
> [migration table](../CHANGELOG.md#migrating-from-1x) for code written
> against `1.x`.

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Concepts](#concepts)
  - [The session lifecycle](#the-session-lifecycle)
  - [Features and capabilities](#features-and-capabilities)
  - [Document synchronization](#document-synchronization)
  - [Positions and encodings](#positions-and-encodings)
  - [Diagnostics mapping](#diagnostics-mapping)
  - [Cancellation](#cancellation)
  - [Three kinds of failure](#three-kinds-of-failure)
  - [Threading and performance](#threading-and-performance)
  - [About the examples](#about-the-examples)
- [`Server`](#server)
  - [`Server::new`](#servernew)
  - [`Server::with_message_limit`](#serverwith_message_limit)
  - [`Server::with_info`](#serverwith_info)
  - [`Server::custom_request`](#servercustom_request), [`Server::custom_notification`](#servercustom_notification)
  - [`Server::handler`](#serverhandler)
  - [`Server::run`](#serverrun)
- [`serve_stdio`](#serve_stdio)
- [`Handler`](#handler)
  - [`Handler::initialize`](#handlerinitialize)
  - [`Handler::features`](#handlerfeatures)
  - [`Handler::changed`](#handlerchanged)
  - [`Handler::closed`](#handlerclosed)
  - [`Handler::hover`](#handlerhover)
  - [`Handler::definition`](#handlerdefinition)
  - [`Handler::completion`](#handlercompletion)
  - [`Handler::document_symbols`](#handlerdocument_symbols)
  - [`Handler::semantic_tokens`](#handlersemantic_tokens)
  - [`Handler::request`](#handlerrequest)
  - [`Handler::notification`](#handlernotification)
  - [`Handler::response`](#handlerresponse)
- [`Features`](#features)
- [`Context`](#context)
  - [`Context::document`](#contextdocument), [`Context::documents`](#contextdocuments)
  - [`Context::notify`](#contextnotify), [`Context::request`](#contextrequest)
  - [`Context::log`](#contextlog), [`MessageType`](#messagetype)
  - [`Context::publish_diagnostics`](#contextpublish_diagnostics), [`Context::clear_diagnostics`](#contextclear_diagnostics)
- [`Document`](#document)
- [Positions: `Position`, `Range`, `Encoding`](#positions-position-range-encoding)
- [Results: `Location`, `Hover`, `MarkupKind`, `TextEdit`](#results-location-hover-markupkind-textedit)
- [`CompletionItem`, `CompletionItemKind`](#completionitem-completionitemkind)
- [`DocumentSymbol`, `SymbolKind`](#documentsymbol-symbolkind)
- [Semantic tokens](#semantic-tokens)
  - [`SemanticTokensLegend`](#semantictokenslegend)
  - [`SemanticTokens`](#semantictokens)
  - [`SemanticTokensBuilder`](#semantictokensbuilder)
- [`ResponseError`](#responseerror)
- [`Error`](#error)
- [Re-exports](#re-exports)
- [Stability](#stability)

## Overview

lsp-lang is the Language Server Protocol layer of the `-lang` family. A
language server built on it supplies two things:

1. an [`incremental_lang::Grammar`](https://docs.rs/incremental-lang), which
   parses each open document and says which nodes may be reparsed on their own;
2. a [`Handler`](#handler), which publishes diagnostics as documents change and
   answers the requests the language supports.

The [`Server`](#server) does everything else: it frames and parses JSON-RPC
messages ([`rpc-io`](https://docs.rs/rpc-io), over [`jsonx-lib`](https://docs.rs/jsonx-lib)),
runs the `initialize`/`shutdown`/`exit` lifecycle, keeps every open document in
step with the editor (reparsing incrementally on each edit), cancels requests
the client no longer wants, and converts between protocol positions and byte
offsets. Handlers work in byte offsets and byte spans, like the rest of the
family, and convert at the edge with a [`Document`](#document). There are no
third-party dependencies.

| Item | Kind | Role |
|---|---|---|
| [`Server`](#server), [`serve_stdio`](#serve_stdio) | struct, fn | The protocol engine: reads, dispatches, writes. |
| [`Handler`](#handler) | trait | The language's behaviour; every method has a default. |
| [`Features`](#features) | struct | What a handler offers; capabilities are advertised from it. |
| [`Context`](#context), [`MessageType`](#messagetype) | struct, enum | A handler's view of the session: documents and a channel to the client. |
| [`Document`](#document) | struct | An open file: text, syntax tree, position conversion. |
| [`Position`, `Range`, `Encoding`](#positions-position-range-encoding) | structs, enum | The protocol's coordinates and the unit of a column. |
| [`Location`, `Hover`, `MarkupKind`, `TextEdit`](#results-location-hover-markupkind-textedit) | structs, enum | Results the typed features return. |
| [`CompletionItem`, `CompletionItemKind`](#completionitem-completionitemkind) | struct, enum | Completion. |
| [`DocumentSymbol`, `SymbolKind`](#documentsymbol-symbolkind) | struct, enum | The outline, as a tree. |
| [`SemanticTokensLegend`, `SemanticTokens`, `SemanticTokensBuilder`](#semantic-tokens) | structs | Highlighting, full and delta. |
| [`ResponseError`](#responseerror) | struct | A request's failure, with a JSON-RPC error code. |
| [`Error`](#error) | enum | What ends a session, or fails a write. |

## Installation

```toml
[dependencies]
lsp-lang = "2"
```

`incremental_lang`, `diag_lang`, `jsonx_lib`, and `rpc_io` are
[re-exported](#re-exports), so a server needs no other dependency to write its
grammar, build diagnostics, read parameters, or name a transport type. MSRV:
Rust 1.85 (Rust 2024 edition).

## Quick start

A server binary is a grammar, a handler, and one call:

```rust,no_run
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct MyGrammar;
# impl Grammar for MyGrammar {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
use std::process::ExitCode;
use lsp_lang::{Context, Document, Handler};

struct MyServer;

impl Handler<MyGrammar> for MyServer {
    fn changed(&mut self, doc: &Document<MyGrammar>, cx: &mut Context<'_, MyGrammar>) {
        // Analyse `doc.text()` / `doc.tree()`; publish what you find.
        cx.publish_diagnostics(doc, &[]);
    }
}

fn main() -> ExitCode {
    match lsp_lang::serve_stdio(MyGrammar, MyServer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("language server stopped: {err}");
            ExitCode::FAILURE
        }
    }
}
```

Add features by listing them and overriding their methods:

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Document, Features, Handler, Hover, Position, ResponseError};

struct Lines;

impl<G: Grammar> Handler<G> for Lines {
    fn features(&self) -> Features {
        Features::new().hover()
    }

    fn hover(&mut self, doc: &Document<G>, at: Position, _: &mut Context<'_, G>)
        -> Result<Option<Hover>, ResponseError>
    {
        let line = doc.text().lines().nth(at.line as usize).unwrap_or("");
        Ok(Some(Hover::markdown(format!("`{line}`"))))
    }
}
```

The [README](../README.md#quick-start) has a complete, runnable session, and
the [examples](../examples) a full server with diagnostics, hover,
go-to-definition, completion, an outline, and semantic tokens.

## Concepts

### The session lifecycle

[`Server::run`](#serverrun) serves one session. The protocol's lifecycle rules
are enforced by the server, so requests and notifications reach a handler only
inside the session window:

| Phase | Requests | Notifications |
|---|---|---|
| Before `initialize` | `initialize` is handled; anything else is refused with `ServerNotInitialized` (`-32002`). | Dropped, except `exit`. |
| Running | `initialize` again is refused with `InvalidRequest`; `shutdown` moves to the next phase; the listed [features](#features-and-capabilities) and registered [custom requests](#servercustom_request) go to the handler; any other method is `MethodNotFound`. | Document sync is handled by the server; `exit` ends the session; the standard and [registered](#servercustom_notification) notifications go to [`Handler::notification`](#handlernotification); any other is ignored. |
| After `shutdown` | Refused with `InvalidRequest` (`-32600`); a method the server does not know is still `MethodNotFound`. | Dropped, except `exit`. |

`exit` ends [`run`](#serverrun): with `Ok(())` after `shutdown`, or
[`Error::ExitWithoutShutdown`](#error) before it, mirroring the exit codes 0
and 1 the protocol asks for. A failed `initialize` leaves the session
uninitialized, and the client may send it again.

A message is classified by its members. With a `method` and an `id` it is a
request; the id must be a string or an integer, and a request with any other id
(`null`, a fraction, an object) is refused with `InvalidRequest` under a `null`
id, without being dispatched. A request whose envelope cannot be read (a
`method` that is not a string, say) but whose `id` is a string or an integer is
refused with `InvalidRequest` under that id. With a `method` and no `id` it is
a notification. Without a `method` it is the client's answer to one of the
server's own requests ([`Context::request`](#contextrequest)): it reaches
[`Handler::response`](#handlerresponse) if, and only if, it answers a request
still awaiting an answer; anything else that looks like an answer is logged
and dropped. The server never replies to an answer. `params` must be an array
or an object (or absent, or `null`), as JSON-RPC requires; a batch of messages
is answered with one array of answers.

During `initialize`, the server negotiates the [position encoding](#encoding),
calls [`Handler::initialize`](#handlerinitialize), asks the handler for its
[`features`](#handlerfeatures), and answers with the capabilities they imply
plus the two it owns: `positionEncoding` and `textDocumentSync`. The
`initialize` response is the first message the client receives: whatever the
handler sends while answering is held back and follows it. If `initialize`
fails, that held-back output is discarded, and a request among it is never sent
(its outcome is `REQUEST_CANCELLED`, delivered to
[`Handler::response`](#handlerresponse)).

### Features and capabilities

The typed requests are [`hover`](#handlerhover), [`definition`](#handlerdefinition),
[`completion`](#handlercompletion), [`document_symbols`](#handlerdocument_symbols),
and [`semantic_tokens`](#handlersemantic_tokens). A handler offers one in two
steps: override the method, and list the feature in
[`Handler::features`](#handlerfeatures) with the [`Features`](#features)
builder. The server then

- advertises exactly the listed features as capabilities in the `initialize`
  result (hover is `hoverProvider`, completion carries its trigger characters,
  semantic tokens carry their legend and `full: { delta: true }`);
- reads the request's parameters, finds the document, calls the method, and
  writes the answer;
- answers a request for a feature that is not listed with `MethodNotFound`,
  even if the method is overridden, because the client was never told it exists;
- answers a listed feature whose method was not overridden with `MethodNotFound`
  from the method's default.

A request for a document the client does not have open is answered `null`
without calling the handler. Malformed parameters are answered
`InvalidParams`. Features that depend on what the client announced can be
decided in [`initialize`](#handlerinitialize), since `features` is called after
it.

### Document synchronization

The server handles `textDocument/didOpen`, `didChange`, and `didClose` itself.
Each open document is a [`Document`](#document): an
[`incremental_lang::Document`](https://docs.rs/incremental-lang) plus a line
index. A `didChange` may carry several edits; each is converted from its range
to a byte span, applied to the text, reparsed incrementally, and folded into
the line index, and then [`Handler::changed`](#handlerchanged) is called once.
A whole-text change (no range) replaces the text.

A failed change never loses the document. What happens depends on why it
failed:

| Failure | What the server does |
|---|---|
| The incremental reparse of an edit failed: the grammar panicked, or its lexer broke its contract, on the reparsed fragment | Applies the same edit again by splicing it into the text and parsing the whole result, logs a warning naming the fault, and carries on: `changed` is called as usual. The document's incremental statistics start over. |
| The grammar also fails on the whole new text, or the text would outgrow 4 GiB | Keeps the document at its last good state and marks it **out of step**; logs an error (grammar) or warning (size). |
| An edit's range names a line the document does not have: more than one past its last line | Out of step, logged as a warning. The line just past the last is accepted and means the end of the text; a column past the end of its line is clamped, as the protocol prescribes. |
| A change with only ranged edits carries a version no greater than the document's | Out of step, logged as a warning: versions only increase, so the change repeats one already applied or follows one that went missing. |

A `didChange` is all or nothing: if any of its edits fails, the document goes
back to the text it had before the first (its last good state), and its version
stays the last one applied.

A document out of step stays open, with the last good text, tree, and version,
for handlers to read; `changed` is not called and `closed` is not called. The
protocol has no request for the client's current text, so the server recovers
when the client next sends it:

- **a whole-text change** (a content change without a range) is applied
  whatever state the document is in, whatever its version, and brings it back
  in step; edits after it in the same `didChange` apply on top of it, and edits
  before it are superseded;
- **a reopen** (`didOpen` for the same URI) replaces it.

Ranged changes to an out-of-step document are logged as warnings and ignored.
A `didOpen` that fails (the grammar fails on the text, or it is over 4 GiB) is
logged as an error; a document already open under that URI is kept, out of
step, and otherwise the URI is remembered so a whole-text change can still open
it. Changes to any other document that is not open are logged and ignored.

A grammar panic costs at most the document's freshness, never the session: the
server catches it where panics unwind. (A binary built with `panic = "abort"`
ends instead, as that setting asks.)

The parsing work of an edit is proportional to the edit. What stays linear in
the size of the document is cheap: moving the text after the edit (one
`memmove`) and shifting the line starts after it (one pass over integers).

### Positions and encodings

The protocol's `Position` is a zero-based line and a column; lines end at
`\n`, `\r\n`, or a lone `\r`. The column's unit is negotiated: the client lists
the encodings it supports in `general.positionEncodings`, and the server picks
**UTF-8** whenever it is offered (a byte column converts to a byte offset
without scanning) and otherwise the protocol's mandatory **UTF-16**. UTF-32 is
not negotiated. A [`Document`](#document) converts in the negotiated encoding,
so a handler never counts columns itself.

Conversions are total. Input the protocol considers out of range is clamped as
the protocol prescribes: a line past the last maps to the end of the text, a
column past the end of its line maps to the end of that line (before the line
break). A column inside a character (between the halves of a UTF-16 surrogate
pair, or inside a UTF-8 sequence) maps to that character's start. Byte offsets
past the end clamp to the end, and offsets inside a character map to it.

Every byte offset that is a caret position round-trips exactly through
[`position`](#documentposition) and [`offset`](#documentoffset). The one offset
that is not, between the `\r` and `\n` of one line break, maps to the column
just past the `\r`, which converts back to the `\r` itself.

### Diagnostics mapping

[`Context::publish_diagnostics`](#contextpublish_diagnostics) converts
[`diag_lang::Diagnostic`](https://docs.rs/diag-lang)s whose spans are byte
offsets into the document (the space the syntax tree's spans are in) and writes
`textDocument/publishDiagnostics` with the document's URI and version:

| diag-lang | Protocol |
|---|---|
| primary label's span | `range` |
| `Severity::Error` | `severity: 1` (Error) |
| `Severity::Warning` | `severity: 2` (Warning) |
| `Severity::Note` | `severity: 3` (Information) |
| `Severity::Help` | `severity: 4` (Hint) |
| message | `message`, first line |
| primary label's text | `message`, next line (if not empty) |
| notes | `message`, one line each: `note: …` |
| help lines | `message`, one line each: `help: …` |
| secondary labels | `relatedInformation`: one entry each, located in the same document, with the label's text (or the headline, if the label has none) |

### Cancellation

`$/cancelRequest` cancels a request that has been received and has not started.
The transport answers a cancelled request with `RequestCancelled` (`-32800`) the
moment the cancellation is read, exactly once, and the handler is never called
for it; a cancellation for a request that already finished, or for an id nobody
sent, is ignored.

The server queues every request and notification it receives and runs the queue
when its input has nothing more buffered, in arrival order. A cancellation that
arrives with the request it names, or behind it in the same read (the usual
case for a request queued behind a slow one), is therefore applied before the
request runs. The one exception to queueing is `initialize`, which is run at
once, because the client may pipeline messages behind it.

A handler that is already running is **not interrupted**: the server is
single-threaded, so it cannot read the cancellation until the handler returns.
A handler with slow work should keep each call short, or do the work on a worker
thread from a snapshot of the tree (which is `O(1)` to take), and report back on
a later call.

Server-to-client requests ([`Context::request`](#contextrequest)) are the other
direction: when more than 1024 are outstanding, the oldest is cancelled with
`$/cancelRequest` and its outcome is `REQUEST_CANCELLED`.

### Three kinds of failure

| Kind | Type | Who sees it |
|---|---|---|
| A request failed | [`ResponseError`](#responseerror) | The client, as the request's error response. |
| A notification could not be processed (malformed parameters, an edit that could not be applied, a grammar panic, a change to a document that is not open or is out of step, a handler panic outside a request), or an answer from the client matched no outstanding request | `window/logMessage` | The client's log. Notifications and answers have no response, so this is the only channel. |
| The session cannot continue (the transport failed or closed, the framing broke, `exit` without `shutdown`) | [`Error`](#error) | The caller of [`run`](#serverrun), which should exit non-zero. |

Malformed JSON and messages of the wrong shape are answered with `ParseError`
(`-32700`) and `InvalidRequest` (`-32600`) and the session continues, as does a
message over the [size limit](#serverwith_message_limit). Nothing in the crate
panics on client input; the limits on message size, nesting depth, string
length, and values per message (see `rpc-io` and `jsonx-lib`) bound what a
message can cost.

A panic in the handler is contained the same way as one in the grammar, where
panics unwind. A request whose handler panicked is answered with
`InternalError` (`-32603`), its message carrying the panic's (clipped to 256
bytes); a panic in any other handler method (`changed`, `closed`,
`notification`, `response`) is logged as an error. Either way the session
carries on with the same handler, and messages the handler sent before
panicking are still sent; the handler's own state is whatever the panic left. A
panic in `initialize` or `features` refuses `initialize` with `InternalError`,
like an error return.

### Threading and performance

The server is synchronous and single-threaded: it reads, handles each complete
message, runs the queued work, writes what it produced with one `write_all` and
`flush` per message or job, then reads again. For an editor talking to one
server over a pipe this is the lowest-latency design: nothing stands between the
client and the handler. A handler with slow work can clone a document's
[tree](#documenttree) (one reference-count increment; `Send + Sync`) for a
worker thread and report back on a later call.

On the hot path, nothing is built that is not needed. The JSON-RPC envelope is
decoded in place, borrowing the method name and URIs from the message and
keeping parameters as slices until they are read; results are written straight
to text (semantic tokens, outlines, and diagnostics never become value trees);
the output buffer is reused for the whole session. An edit costs a binary
search to convert its range, an incremental reparse, and a patch of the line
index.

### About the examples

Examples that run a whole session start with a few hidden lines (shown as
`# …` on GitHub): a grammar that reads a whole document as one token. Sessions
are driven from memory: build the framed bytes an editor would send, pass them
to [`Server::run`](#serverrun) as the input, and read the framed bytes written
to the output.

## `Server`

```rust,ignore
pub struct Server<G, H> { /* private */ }
impl<G: Grammar + Clone, H: Handler<G>> Server<G, H> { /* ... */ }
```

The protocol engine around a [`Handler`](#handler): transport, lifecycle,
document synchronization, the typed features, and cancellation. `G` is the
grammar every document is parsed with (cloned per document; a unit struct costs
nothing). `Debug` shows the message limit.

### `Server::new`

```rust,ignore
pub fn new(grammar: G, handler: H) -> Server<G, H>
```

Creates a server. The default message limit is 256 MiB.

```rust
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct Plain;
# impl Grammar for Plain {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
use lsp_lang::{Handler, Server};

struct SyncOnly;
impl Handler<Plain> for SyncOnly {}

let server = Server::new(Plain, SyncOnly);
```

### `Server::with_message_limit`

```rust,ignore
pub fn with_message_limit(self, bytes: usize) -> Server<G, H>
```

Sets the largest message body accepted, and the longest string in a message,
in bytes. A message announcing a larger body is skipped as it arrives, without
being buffered, and answered `InvalidRequest` under a `null` id; the session
goes on. Opening a file sends its whole text in one message (JSON-escaped), so
the limit caps the size of document the server accepts.

```rust
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct Plain;
# impl Grammar for Plain {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
# struct SyncOnly;
# impl lsp_lang::Handler<Plain> for SyncOnly {}
use lsp_lang::{Error, Server};

let mut server = Server::new(Plain, SyncOnly).with_message_limit(16);
let input = "Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}";
let mut output = Vec::new();
assert!(matches!(server.run(input.as_bytes(), &mut output), Err(Error::Disconnected)));
assert!(String::from_utf8_lossy(&output).contains("too large"));
```

### `Server::with_info`

```rust,ignore
pub fn with_info(self, name: impl Into<String>, version: impl Into<String>) -> Server<G, H>
```

Sends `serverInfo` (`name`, `version`) in the `initialize` result.

### `Server::custom_request`

```rust,ignore
pub fn custom_request(self, method: &str) -> Server<G, H>
```

Registers a request method the server forwards to
[`Handler::request`](#handlerrequest), for methods outside the protocol's own
and the typed features. A request for an unregistered method is
`MethodNotFound`. A method that is one of the server's own is not affected.

### `Server::custom_notification`

```rust,ignore
pub fn custom_notification(self, method: &str) -> Server<G, H>
```

Registers a notification method the server forwards to
[`Handler::notification`](#handlernotification), beyond the standard ones it
forwards already: `initialized`, `textDocument/didSave`,
`textDocument/willSave`, `workspace/didChangeConfiguration`,
`workspace/didChangeWatchedFiles`, `workspace/didChangeWorkspaceFolders`,
`$/setTrace`, and `window/workDoneProgress/cancel`.

### `Server::handler`

```rust,ignore
pub fn handler(&self) -> &H
```

The handler, for inspecting what it recorded after [`run`](#serverrun)
returns, in a test, say.

### `Server::run`

```rust,ignore
pub fn run<R: Read, W: Write>(&mut self, input: R, output: W) -> Result<(), Error>
```

Serves one session: reads messages from `input` and writes responses and
notifications to `output` until the client sends `exit`. `input` need not be
buffered, and neither `input` nor `output` needs to be `Send` or `'static`: a
locked stdin and stdout, sockets, or `&[u8]` and `&mut Vec<u8>`. Everything
sent during one message or one queued job is written with one `write_all` and
flushed at once; a turn that sends nothing writes nothing.

Each call is a fresh session (uninitialized, no documents open) while the
grammar and handler carry over. Nothing the client sends and no panic in the
grammar or handler ends the session; only the transport and the lifecycle do.

**Errors:** `Ok(())` means a clean stop: `shutdown`, then `exit`. Otherwise
[`Error::ExitWithoutShutdown`](#error), [`Error::Disconnected`](#error) (the
input ended before `exit`, possibly mid-message), [`Error::Io`](#error), or
[`Error::Frame`](#error). Requests received before the failure have been
answered first.

```rust
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct Plain;
# impl Grammar for Plain {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
# struct SyncOnly;
# impl lsp_lang::Handler<Plain> for SyncOnly {}
use lsp_lang::Server;

fn frame(json: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{json}", json.len())
}

let input = [
    frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
    frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
    frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
]
.concat();

let mut output = Vec::new();
Server::new(Plain, SyncOnly).run(input.as_bytes(), &mut output)?;

let output = String::from_utf8(output).unwrap();
assert!(output.contains(r#""positionEncoding":"utf-16""#));
assert!(output.contains(r#"{"jsonrpc":"2.0","id":2,"result":null}"#));
# Ok::<(), lsp_lang::Error>(())
```

## `serve_stdio`

```rust,ignore
pub fn serve_stdio<G: Grammar + Clone, H: Handler<G>>(grammar: G, handler: H) -> Result<(), Error>
```

The one-call entry point: `Server::new(grammar, handler)` run on the process's
standard input and output. **Errors:** as [`Server::run`](#serverrun).

## `Handler`

```rust,ignore
pub trait Handler<G: Grammar> { /* every method has a default */ }
```

The language's behaviour. Every method has a default, so a handler implements
only what its language supports: a diagnostics-only server needs
[`changed`](#handlerchanged) and nothing more.

| Method | Called when | Default |
|---|---|---|
| [`initialize`](#handlerinitialize) | the client sends `initialize` | accepts |
| [`features`](#handlerfeatures) | after `initialize`, to build the capabilities | none |
| [`changed`](#handlerchanged) | a document opens or changes | nothing |
| [`closed`](#handlerclosed) | a document closes | clears its diagnostics |
| [`hover`](#handlerhover), [`definition`](#handlerdefinition), [`completion`](#handlercompletion), [`document_symbols`](#handlerdocument_symbols), [`semantic_tokens`](#handlersemantic_tokens) | the matching request, if listed in `features` | `MethodNotFound` |
| [`request`](#handlerrequest) | a [custom request](#servercustom_request) | `MethodNotFound` |
| [`notification`](#handlernotification) | `initialized`, `didSave`, and other notifications | ignored |
| [`response`](#handlerresponse) | the client answers a [`Context::request`](#contextrequest) | ignored |

Calls are made one at a time, in message order, on the thread running
[`Server::run`](#serverrun). A panic in any method is contained where panics
unwind (see [Three kinds of failure](#three-kinds-of-failure)).

### `Handler::initialize`

```rust,ignore
fn initialize(&mut self, params: &Value<'_>, cx: &mut Context<'_, G>) -> Result<(), ResponseError>
```

Called when the client sends `initialize`, before it is answered. `params` is
the client's `InitializeParams` (`rootUri`, `initializationOptions`, client
capabilities), `null` if it sent none. An error refuses the request and leaves
the session uninitialized. Anything sent through `cx` is held until after the
`initialize` response, and discarded if `initialize` fails.

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler, ResponseError, Value};

struct Project { root: Option<String> }

impl<G: Grammar> Handler<G> for Project {
    fn initialize(&mut self, params: &Value<'_>, _: &mut Context<'_, G>) -> Result<(), ResponseError> {
        self.root = params["rootUri"].as_str().map(str::to_owned);
        Ok(())
    }
}
```

### `Handler::features`

```rust,ignore
fn features(&self) -> Features
```

The [`Features`](#features) the handler implements. Called once per session,
after `initialize` returned. Default: none.

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Features, Handler};

struct Navigator;

impl<G: Grammar> Handler<G> for Navigator {
    fn features(&self) -> Features {
        Features::new().hover().definition().document_symbols()
    }
}
```

### `Handler::changed`

```rust,ignore
fn changed(&mut self, document: &Document<G>, cx: &mut Context<'_, G>)
```

Called after a document is opened and after each change to it, once the text
and syntax tree are current. A `didChange` carrying several edits produces one
call. The usual body analyses the document and publishes diagnostics. It is
not called for a change that left the document out of step (see
[Document synchronization](#document-synchronization)).

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Document, Handler};

#[derive(Default)]
struct Watch { edits: u64 }

impl<G: Grammar> Handler<G> for Watch {
    fn changed(&mut self, doc: &Document<G>, _: &mut Context<'_, G>) {
        self.edits = doc.incremental().stats().edits;
    }
}
```

### `Handler::closed`

```rust,ignore
fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>)
```

Called after the document `uri` is closed and removed. The default clears the
diagnostics published for it, as the protocol recommends; override it to drop
per-document state, and call `cx.clear_diagnostics(uri)` to keep that.

### `Handler::hover`

```rust,ignore
fn hover(&mut self, document: &Document<G>, position: Position, cx: &mut Context<'_, G>)
    -> Result<Option<Hover>, ResponseError>
```

Answers `textDocument/hover`. `position` is the client's, in the session
encoding; turn it into a byte offset with [`Document::offset`](#documentoffset).
`Ok(None)` is "nothing here" (`null`). Requested only when
[`Features::hover`](#features) is listed. Default: `MethodNotFound`.

### `Handler::definition`

```rust,ignore
fn definition(&mut self, document: &Document<G>, position: Position, cx: &mut Context<'_, G>)
    -> Result<Vec<Location>, ResponseError>
```

Answers `textDocument/definition`. An empty list is "nowhere" and is sent as
`null`. Locations may be in other documents. Requested only when
[`Features::definition`](#features) is listed. Default: `MethodNotFound`.

### `Handler::completion`

```rust,ignore
fn completion(&mut self, document: &Document<G>, position: Position, cx: &mut Context<'_, G>)
    -> Result<Vec<CompletionItem>, ResponseError>
```

Answers `textDocument/completion`. The items are sent as a complete list
(`isIncomplete: false`). Requested only when [`Features::completion`](#features)
is listed. Default: `MethodNotFound`.

### `Handler::document_symbols`

```rust,ignore
fn document_symbols(&mut self, document: &Document<G>, cx: &mut Context<'_, G>)
    -> Result<Vec<DocumentSymbol>, ResponseError>
```

Answers `textDocument/documentSymbol` with the outline as a hierarchy.
Requested only when [`Features::document_symbols`](#features) is listed.
Default: `MethodNotFound`.

### `Handler::semantic_tokens`

```rust,ignore
fn semantic_tokens(&mut self, document: &Document<G>, cx: &mut Context<'_, G>)
    -> Result<SemanticTokens, ResponseError>
```

Answers `textDocument/semanticTokens/full` and its delta form. Return every
token, built with a [`SemanticTokensBuilder`](#semantictokensbuilder); the
server assigns the `resultId`, keeps the last answer per document, and turns
the next one into a single token-aligned edit when the client asks for a
delta. A `previousResultId` the server does not know is answered in full.
Requested only when [`Features::semantic_tokens`](#features) is listed.
Default: `MethodNotFound`.

### `Handler::request`

```rust,ignore
fn request(&mut self, method: &str, params: &Value<'_>, cx: &mut Context<'_, G>)
    -> Result<Value<'static>, ResponseError>
```

Answers a [custom request](#servercustom_request). `params` is `null` when
absent. Requests reach it only between `initialize` and `shutdown`. Default:
`MethodNotFound`.

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler, ResponseError, Value};

struct Echo;

impl<G: Grammar> Handler<G> for Echo {
    fn request(&mut self, method: &str, params: &Value<'_>, _: &mut Context<'_, G>)
        -> Result<Value<'static>, ResponseError>
    {
        match method {
            "echo/params" => Ok(params.clone().into_owned()),
            _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
        }
    }
}
```

### `Handler::notification`

```rust,ignore
fn notification(&mut self, method: &str, params: &Value<'_>, cx: &mut Context<'_, G>)
```

Receives the notifications the server does not handle itself (see
[`custom_notification`](#servercustom_notification) for the list). Notifications
before `initialize` or after `shutdown` are dropped. `$/cancelRequest` is the
server's own and never arrives here. Default: ignored.

### `Handler::response`

```rust,ignore
fn response(&mut self, id: u64, result: Result<Value<'_>, ResponseError>, cx: &mut Context<'_, G>)
```

Receives the outcome of a request sent with [`Context::request`](#contextrequest):
the client's result or error. An answer that is not a valid response (an error
without a message, a non-integer code) arrives as an error with the code
`INVALID_REQUEST`. A request cancelled for want of room, or never sent because
`initialize` failed, arrives as `REQUEST_CANCELLED`. Each request has one
outcome; an answer that matches no outstanding request is logged to the client
and dropped. Default: ignored.

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler, ResponseError, Value};

#[derive(Default)]
struct Settings { pending: Option<u64>, tab_width: u64 }

impl<G: Grammar> Handler<G> for Settings {
    fn response(&mut self, id: u64, result: Result<Value<'_>, ResponseError>, _: &mut Context<'_, G>) {
        if self.pending == Some(id) {
            self.pending = None;
            if let Ok(items) = result {
                self.tab_width = items[0]["tabWidth"].as_u64().unwrap_or(4);
            }
        }
    }
}
```

## `Features`

```rust,ignore
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Features { /* private */ }
```

What a handler offers, from which the server's capabilities are derived.

| Method | Capability | Handler method |
|---|---|---|
| `Features::new()` | none | none |
| `.hover()` | `hoverProvider` | [`hover`](#handlerhover) |
| `.definition()` | `definitionProvider` | [`definition`](#handlerdefinition) |
| `.completion(&[&str])` | `completionProvider` with the trigger characters | [`completion`](#handlercompletion) |
| `.document_symbols()` | `documentSymbolProvider` | [`document_symbols`](#handlerdocument_symbols) |
| `.semantic_tokens(legend)` | `semanticTokensProvider` with the legend and `full: { delta: true }` | [`semantic_tokens`](#handlersemantic_tokens) |
| `.capability(name, value)` | any other `ServerCapabilities` member | [`request`](#handlerrequest) |
| `.semantic_tokens_legend()` | | the legend listed, if any |
| `.capabilities_json()` | | the capabilities these features produce, before the server's own |

`capability` overrides a derived capability of the same name. It cannot
override `positionEncoding`, and a `textDocumentSync` object is merged into the
server's own: `openClose` and `change` stay the server's, other members such as
`save` are kept.

```rust
use lsp_lang::{json, Features, SemanticTokensLegend};

let features = Features::new()
    .hover()
    .completion(&["(", " "])
    .semantic_tokens(SemanticTokensLegend::standard())
    .capability("renameProvider", json!(true));
let caps = features.capabilities_json();
assert_eq!(caps["hoverProvider"], true);
assert_eq!(caps["completionProvider"]["triggerCharacters"][0], "(");
assert_eq!(caps["renameProvider"], true);
assert!(caps.get("definitionProvider").is_none());
```

## `Context`

```rust,ignore
pub struct Context<'a, G: Grammar> { /* private */ }
```

A handler's view of the session for one call: the open documents, and a channel
to the client. Messages sent through it are written in the order sent, in one
write, as soon as the handler returns, before the response to the request being
handled. Documents borrowed from a context live for the whole call, so a handler
can hold a document and send messages at the same time.

### `Context::document`

```rust,ignore
pub fn document(&self, uri: &str) -> Option<&'a Document<G>>
```

The open document with this URI, if any. URIs match exactly, as the client sent
them.

### `Context::documents`

```rust,ignore
pub fn documents(&self) -> impl ExactSizeIterator<Item = &'a Document<G>> + 'a
```

Every open document, ordered by URI.

### `Context::notify`

```rust,ignore
pub fn notify<'p>(&mut self, method: &str, params: impl Into<rpc_io::Json<'p>>) -> Result<(), Error>
```

Sends a notification. `params` is a [`Value`](#re-exports) built with `json!`,
a pre-serialized `rpc_io::Raw`, or `()` for none: an array or an object.
**Errors:** [`Error::InvalidParams`](#error) for a scalar,
[`Error::Closed`](#error) when the connection has ended. Nothing is sent in
either case and the session carries on.

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Error};

fn announce<G: Grammar>(cx: &mut Context<'_, G>) -> Result<(), Error> {
    cx.notify("window/showMessage", json!({ "type": 3, "message": "Indexing done" }))
}
```

### `Context::request`

```rust,ignore
pub fn request<'p>(&mut self, method: &str, params: impl Into<rpc_io::Json<'p>>) -> Result<u64, Error>
```

Sends a request to the client and returns its id (counting up from 1 in each
session). The outcome arrives later through
[`Handler::response`](#handlerresponse), exactly once. At most 1024 requests
await an answer at once; one more cancels the oldest. **Errors:** as
[`notify`](#contextnotify).

### `Context::log`

```rust,ignore
pub fn log(&mut self, kind: MessageType, message: &str)
```

Sends `window/logMessage`: a line for the client's output log.

#### `MessageType`

```rust,ignore
#[non_exhaustive]
pub enum MessageType { Error = 1, Warning = 2, Info = 3, Log = 4 }
```

`as_u32(self) -> u32` gives the protocol's number.

### `Context::publish_diagnostics`

```rust,ignore
pub fn publish_diagnostics(&mut self, document: &Document<G>, diagnostics: &[Diagnostic])
```

Publishes `diagnostics` for `document`, replacing what was published before.
Spans are byte offsets into the document's text; they are converted to ranges
in the session encoding (clamped to the text), all in one ascending pass, so
many diagnostics on one long line cost one scan of it. The message is joined
from the headline, the primary label, notes, and help, and secondary labels
become related information ([the mapping](#diagnostics-mapping)). The
notification carries the document's version. An empty slice clears the
document's diagnostics.

```rust
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct Plain;
# impl Grammar for Plain {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::{Context, Diagnostic, Document, Handler};

struct NoTabs;

impl Handler<Plain> for NoTabs {
    fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
        let found: Vec<Diagnostic> = doc
            .text()
            .match_indices('\t')
            .map(|(at, _)| {
                let at = at as u32;
                Diagnostic::new(Severity::Warning, "tab character",
                    Label::new(Span::new(at, at + 1), "indent with spaces"))
            })
            .collect();
        cx.publish_diagnostics(doc, &found);
    }
}
```

### `Context::clear_diagnostics`

```rust,ignore
pub fn clear_diagnostics(&mut self, uri: &str)
```

Clears the diagnostics published for `uri`, whether or not the document is
open. The default [`Handler::closed`](#handlerclosed) calls it.

## `Document`

```rust,ignore
pub struct Document<G: Grammar> { /* private */ }
```

A document the client has open: its text, its syntax tree, and the conversions
between protocol positions and byte offsets in the session's
[encoding](#positions-position-range-encoding). Created by the server on
`didOpen`, updated on every `didChange`, dropped on `didClose`; handlers see it
read-only. `Debug` shows the URI, version, encoding, and length, not the text.

| Method | Returns |
|---|---|
| `uri(&self) -> &str` | The URI, exactly as the client sent it. |
| `version(&self) -> i32` | The version the client gave the current text. Diagnostics carry it. |
| `text(&self) -> &str` | The current text. |
| `tree(&self) -> &Tree<G::Kind>` | The syntax tree; clone it (one reference-count increment) to keep a snapshot. |
| `incremental(&self) -> &incremental_lang::Document<G>` | The underlying incremental document, for its grammar and reparse statistics. |
| `encoding(&self) -> Encoding` | The session's position encoding. |
| `position(&self, offset: u32) -> Position` | A byte offset as a position. Past the end clamps; inside a character rounds down. |
| `offset(&self, position: Position) -> u32` | A position as a byte offset. Always a character boundary within the text. |
| `range(&self, span: Span) -> Range` | A byte span as a range. |
| `span(&self, range: Range) -> Span` | A range as a byte span; a reversed range yields the span between its ends. |

### `Document::tree`

The tree is the grammar's lossless concrete syntax tree of `text`; its spans are
byte offsets into that text, ready for `range`.

### `Document::position`

Cost: a binary search over the line starts, plus, for UTF-16, a count over the
line's text before the offset.

### `Document::offset`

Out-of-range input is clamped as the protocol prescribes (see [Positions and
encodings](#positions-and-encodings)).

```rust
# use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Position, Range};

// The word under the cursor, for a hover request.
fn word_at<G: Grammar>(doc: &Document<G>, cursor: Position) -> &str {
    let text = doc.text();
    let at = doc.offset(cursor) as usize;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at].rfind(|c| !is_word(c)).map_or(0, |i| i + 1);
    let end = text[at..].find(|c| !is_word(c)).map_or(text.len(), |i| at + i);
    &text[start..end]
}

// The range covering the whole document.
fn everything<G: Grammar>(doc: &Document<G>) -> Range {
    doc.range(doc.tree().root().span())
}
```

## Positions: `Position`, `Range`, `Encoding`

```rust,ignore
pub struct Position { pub line: u32, pub character: u32 }
pub struct Range { pub start: Position, pub end: Position }
#[non_exhaustive] pub enum Encoding { Utf8, Utf16 }
```

`Position` is a zero-based line and a column in the session encoding; it orders
by line, then column. `Range` is `start` inclusive to `end` exclusive; an empty
range marks a point. `Encoding::default()` is `Utf16`.

| Method | |
|---|---|
| `Position::new(line, character)`, `Range::new(start, end)` | `const` constructors. |
| `to_json(&self) -> Value<'static>` | The protocol's JSON (`{"line":…,"character":…}`, `{"start":…,"end":…}`). |
| `from_json(&Value) -> Result<Self, ResponseError>` | Reads the JSON. `INVALID_PARAMS` for a missing member or one that is not an unsigned 32-bit integer. |

```rust
use lsp_lang::{json, Position, Range};

let range = Range::new(Position::new(3, 4), Position::new(3, 9));
assert_eq!(range.to_json().to_string(),
    r#"{"start":{"line":3,"character":4},"end":{"line":3,"character":9}}"#);
assert_eq!(Range::from_json(&range.to_json())?, range);
assert!(Position::from_json(&json!({ "line": -1, "character": 0 })).is_err());
# Ok::<(), lsp_lang::ResponseError>(())
```

## Results: `Location`, `Hover`, `MarkupKind`, `TextEdit`

```rust,ignore
pub struct Location { pub uri: String, pub range: Range }
#[non_exhaustive] pub struct Hover { pub contents: String, pub kind: MarkupKind, pub range: Option<Range> }
#[non_exhaustive] pub enum MarkupKind { PlainText, Markdown }
pub struct TextEdit { pub range: Range, pub new_text: String }
```

| Item | Methods |
|---|---|
| `Location` | `new(uri, range)`, `to_json`, `from_json` |
| `Hover` | `markdown(text)`, `plain_text(text)`, `with_range(range)`, `to_json` |
| `MarkupKind` | `as_str()` (`"plaintext"`, `"markdown"`); default `Markdown` |
| `TextEdit` | `new(range, new_text)`, `to_json`, `from_json` |

```rust
use lsp_lang::{Hover, Location, Position, Range};

let range = Range::new(Position::new(0, 8), Position::new(0, 13));
let hover = Hover::markdown("`width`: defined on line 1").with_range(range);
assert_eq!(hover.to_json()["contents"]["kind"], "markdown");
assert_eq!(Location::new("file:///a", range).to_json()["uri"], "file:///a");
```

## `CompletionItem`, `CompletionItemKind`

```rust,ignore
#[non_exhaustive] pub struct CompletionItem {
    pub label: String, pub kind: Option<CompletionItemKind>, pub detail: Option<String>,
    pub documentation: Option<String>, pub sort_text: Option<String>,
    pub filter_text: Option<String>, pub insert_text: Option<String>,
    pub text_edit: Option<TextEdit>,
}
#[non_exhaustive] pub enum CompletionItemKind { Text = 1, Method, Function, /* ... */ TypeParameter = 25 }
```

Only `label` is required. Build an item with `CompletionItem::new(label)` and
chain `with_kind`, `with_detail`, `with_documentation` (Markdown),
`with_sort_text`, `with_filter_text`, `with_insert_text`, `with_text_edit`;
`to_json` gives the protocol's JSON. `CompletionItemKind::as_u32` gives the
protocol's number.

```rust
use lsp_lang::{CompletionItem, CompletionItemKind};

let item = CompletionItem::new("define")
    .with_kind(CompletionItemKind::Keyword)
    .with_detail("(define name value)");
assert_eq!(item.to_json()["kind"], 14);
```

## `DocumentSymbol`, `SymbolKind`

```rust,ignore
#[non_exhaustive] pub struct DocumentSymbol {
    pub name: String, pub detail: Option<String>, pub kind: SymbolKind,
    pub range: Range, pub selection_range: Range, pub children: Vec<DocumentSymbol>,
}
#[non_exhaustive] pub enum SymbolKind { File = 1, Module, /* ... */ TypeParameter = 26 }
```

One entry of the outline, with the entries nested inside it. `range` covers the
whole symbol and `selection_range` the part to highlight; the protocol requires
the selection to lie inside the range, and clients reject the whole answer
otherwise, so the server clamps `selection_range` into `range` when it writes
the symbol. `DocumentSymbol::new(name, kind, range, selection_range)`,
`with_detail`, `with_child`, and `to_json` build and convert; `SymbolKind::as_u32`
gives the protocol's number.

A symbol tree can be as deep as the source. Writing, cloning, and dropping it
never recurse: each walks an explicit stack, so depth costs memory and not call
stack. `Debug` and `PartialEq` follow the nesting and are meant for realistic
depths.

```rust
use lsp_lang::{DocumentSymbol, Position, Range, SymbolKind};

let line = |n| Range::new(Position::new(n, 0), Position::new(n, 20));
let class = DocumentSymbol::new("Shape", SymbolKind::Class, line(0), line(0))
    .with_child(DocumentSymbol::new("area", SymbolKind::Method, line(1), line(1)));
assert_eq!(class.to_json()["children"][0]["name"], "area");
```

## Semantic tokens

### `SemanticTokensLegend`

```rust,ignore
pub struct SemanticTokensLegend { pub token_types: Vec<String>, pub token_modifiers: Vec<String> }
```

The token types and modifiers a server uses; a token's type is an index into
`token_types` and its modifiers a bit mask over `token_modifiers`.

| Method | |
|---|---|
| `new(&[&str], &[&str])` | A legend of these types and modifiers. |
| `standard()` | LSP 3.17's standard type names (`namespace` … `decorator`) and modifier names (`declaration` … `defaultLibrary`), in the protocol's order. |
| `token_type(name) -> Option<u32>` | The index of a type, to put in a token. |
| `modifier(name) -> Option<u32>` | The bit of a modifier, to OR into a token's modifiers; `None` for an unknown name or one past 32. |
| `to_json()` | The protocol's JSON. |

### `SemanticTokens`

```rust,ignore
pub struct SemanticTokens { /* private */ }
```

A document's tokens, delta-encoded: five integers per token (line delta, column
delta, length, type, modifiers). `from_data(Vec<u32>)` wraps already-encoded
data (the server refuses a length that is not a multiple of five, answering the
request with `InternalError`); `data()`, `len()` (tokens), `is_empty()`,
`result_id()` (assigned by the server), and `to_json()` read it.

### `SemanticTokensBuilder`

```rust,ignore
pub struct SemanticTokensBuilder<'d, G: Grammar> { /* private */ }
```

Collects tokens by byte span and encodes them: `new(&Document<G>)`,
`push(Span, token_type: u32, modifiers: u32)`, `len()`, `is_empty()`, and
`build() -> SemanticTokens`. Tokens may be pushed in any order. `build` sorts
them, drops a token that overlaps an earlier one (the protocol allows no
overlaps) or is empty, clamps spans to the text and to character boundaries,
converts to lines and columns in the session encoding (a UTF-16 session counts
a surrogate pair as two), and splits a token that crosses a line break into one
token per line, which every client accepts. Cost: `O(n log n)` in the number of
tokens, plus the text they cover.

```rust
# use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
# #[derive(Clone, Copy, Debug, PartialEq, Eq)]
# enum K { Root, Text }
# impl TokenKind for K {}
# #[derive(Clone)]
# struct Plain;
# impl Grammar for Plain {
#     type Kind = K;
#     const ROOT: K = K::Root;
#     fn lex(&self, t: &str, out: &mut Vec<Token<K>>) {
#         if !t.is_empty() { out.push(Token::new(K::Text, Span::new(0, t.len() as u32))); }
#     }
#     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
#         b.start(K::Root);
#         while !b.at_end() { b.bump(); }
#         b.finish();
#     }
# }
use lsp_lang::{Context, Document, Features, Handler, ResponseError};
use lsp_lang::{SemanticTokens, SemanticTokensBuilder, SemanticTokensLegend};

/// Highlights every `fn` as a keyword.
struct Keywords(SemanticTokensLegend);

impl Handler<Plain> for Keywords {
    fn features(&self) -> Features {
        Features::new().semantic_tokens(self.0.clone())
    }

    fn semantic_tokens(&mut self, doc: &Document<Plain>, _: &mut Context<'_, Plain>)
        -> Result<SemanticTokens, ResponseError>
    {
        let keyword = self.0.token_type("keyword").unwrap_or(0);
        let mut tokens = SemanticTokensBuilder::new(doc);
        for (at, _) in doc.text().match_indices("fn") {
            tokens.push(Span::new(at as u32, at as u32 + 2), keyword, 0);
        }
        Ok(tokens.build())
    }
}
```

## `ResponseError`

```rust,ignore
pub struct ResponseError { /* code: i32, message, data */ }
```

A JSON-RPC error, the failed outcome of a request. `ResponseError::new(code,
message)`, `with_data(Value<'static>)`, `code()`, `message()`, `data()`.
`Display` is `"<message> (code <code>)"`. The associated constants are the
codes JSON-RPC and the protocol define:

| Constant | Code | Meaning |
|---|---:|---|
| `PARSE_ERROR` | -32700 | Invalid JSON. Sent by the server. |
| `INVALID_REQUEST` | -32600 | Not a valid request object. |
| `METHOD_NOT_FOUND` | -32601 | The default answer of unimplemented handler methods. |
| `INVALID_PARAMS` | -32602 | What the `from_json` conversions return on a mismatch. |
| `INTERNAL_ERROR` | -32603 | A handler panicked, or the server could not write a result. |
| `SERVER_NOT_INITIALIZED` | -32002 | A request arrived before `initialize`. |
| `REQUEST_FAILED` | -32803 | Understood but could not be served. |
| `SERVER_CANCELLED` | -32802 | The server cancelled the request; retry. |
| `CONTENT_MODIFIED` | -32801 | The document changed while serving; retry against the new content. |
| `REQUEST_CANCELLED` | -32800 | The client cancelled the request. |

Conversions: `From<Error>` (an `INTERNAL_ERROR`), `From<jsonx_lib::Error>` (an
`INVALID_PARAMS`), and to and from `rpc_io::ErrorObject`.

## `Error`

```rust,ignore
#[non_exhaustive]
pub enum Error { Io(io::Error), Disconnected, Frame(FrameError), ExitWithoutShutdown, Closed, InvalidParams(Kind) }
```

Something went wrong in the server itself, as opposed to a request that failed.
`Io`, `Disconnected`, `Frame`, and `ExitWithoutShutdown` end
[`Server::run`](#serverrun); in every case the right response is to exit non-zero.
`Closed` and `InvalidParams` describe a single send from a handler
([`notify`](#contextnotify), [`request`](#contextrequest)) and never end the
session. `FrameError` is [`rpc_io::FrameError`](https://docs.rs/rpc-io); `Kind`
is [`jsonx_lib::Kind`](https://docs.rs/jsonx-lib). `Error` implements
`std::error::Error`, with `source()` for `Io` and `Frame`.

```rust
use lsp_lang::Error;

fn exit_code(result: Result<(), Error>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
assert_eq!(exit_code(Err(Error::ExitWithoutShutdown)), 1);
```

## Re-exports

| Item | From | Why |
|---|---|---|
| `lsp_lang::incremental_lang` | [`incremental-lang`](https://docs.rs/incremental-lang) | The `Grammar`, `Builder`, `Token`, `Span`, and `Tree` a server's grammar is written with. |
| `lsp_lang::diag_lang` | [`diag-lang`](https://docs.rs/diag-lang) | `Label` and `Severity`, to build diagnostics. |
| `lsp_lang::Diagnostic` | `diag-lang` | What [`publish_diagnostics`](#contextpublish_diagnostics) takes. |
| `lsp_lang::jsonx_lib` | [`jsonx-lib`](https://docs.rs/jsonx-lib) | The JSON: parsing, `Value`, and the writer. |
| `lsp_lang::Value`, `lsp_lang::json!` | `jsonx-lib` | What handlers read parameters as and answer custom requests with; a quick way to build any JSON. |
| `lsp_lang::rpc_io` | [`rpc-io`](https://docs.rs/rpc-io) | `Json`, `Raw`, and `FrameError`, the transport types in this API. |

Re-exporting means a server needs only `lsp-lang` and always gets the versions
lsp-lang was built against.

```rust
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::incremental_lang::Span;
use lsp_lang::{json, Diagnostic, Value};

let d = Diagnostic::new(Severity::Error, "boom", Label::unlabelled(Span::new(0, 1)));
let caps: Value = json!({ "hoverProvider": true });
assert_eq!(d.message(), "boom");
assert_eq!(caps["hoverProvider"], true);
```

## Stability

As of `2.0.0` the public API is frozen, on the family's rule that a major
release freezes the surface it ships. lsp-lang follows
[Semantic Versioning](https://semver.org/); within the `2.x` series:

- The **surface** will not change in a breaking way: every item and method in
  this document, with the defaults of the `Handler` methods. New items, new
  `Handler` methods (always with a default), new `Features` builders, and new
  methods on existing types are minor additions; a breaking change means a new
  major version. `Encoding`, `Error`, `MessageType`, `MarkupKind`,
  `CompletionItemKind`, `SymbolKind`, `Hover`, `CompletionItem`, and
  `DocumentSymbol` are `#[non_exhaustive]`, so variants and fields can be added
  in a minor release; match enums with a wildcard arm.
- The **lifecycle** is fixed as described in [The session
  lifecycle](#the-session-lifecycle): which requests and notifications reach the
  handler in each phase, the error codes the server answers with, and the
  outcome of [`run`](#serverrun) for each way a session ends.
- The **capabilities** are fixed: derived from [`Features`](#features) as
  described there, plus the server's own `positionEncoding` and
  `textDocumentSync` (`openClose: true`, `change: 2`).
- **Encoding negotiation** is fixed: UTF-8 when the client offers it, otherwise
  UTF-16. Offering more encodings in a later release would only ever apply to
  clients that offer neither.
- **Position conversion** is fixed: the line breaks (`\n`, `\r\n`, lone `\r`),
  the clamping rules, and the snapping of a column inside a character to its
  start.
- The **diagnostics mapping** is fixed: which part of a `Diagnostic` becomes
  which protocol field, the severity mapping, and the order of the message's
  lines.
- **Document synchronization** is fixed: every edit of a `didChange` is applied
  in order before one [`changed`](#handlerchanged) call; a change that cannot be
  applied keeps the document at its last good state, out of step, until a
  whole-text change or a reopen.
- **Semantic tokens** are fixed: the builder's rules (sorting, dropping overlaps
  and empty tokens, splitting at line breaks, the session encoding's lengths),
  the single token-aligned edit of a delta, and the answer in full for an unknown
  `previousResultId`. **Symbols**: the selection range is clamped into the range.
- **Cancellation** is fixed as described in [Cancellation](#cancellation): a
  request received and not yet started is cancelled with `REQUEST_CANCELLED` and
  never reaches the handler; a running handler is not interrupted.
- **Ordering** is fixed: messages a handler sends go out in the order sent,
  before the response to the request being handled, all before the next incoming
  message is read, except during `initialize`, whose response goes first. Ids
  from [`Context::request`](#contextrequest) count up from 1 in each session.
- **Containment** is fixed: a panic in the grammar or the handler never ends the
  session where panics unwind; at most 1024 of the server's requests are
  outstanding, the oldest cancelled first.
- `incremental-lang`, `diag-lang`, `jsonx-lib`, and `rpc-io` are **public
  dependencies**: their types appear in this API (`Grammar`, `Tree`, `Span`,
  `Diagnostic`, `Value`, `Json`, `Raw`, `FrameError`, `Kind`). Moving to a new
  major version of any of them would be a major release here.
- MSRV (Rust 1.85) is a compatibility surface: raising it is a documented minor
  change, never a patch.

**Honest limits of this freeze.** `jsonx-lib` and `rpc-io` are at `0.2` as of
this release, not `1.0`, and their types are in this API: the freeze here
promises lsp-lang's own surface, and a breaking change in either dependency
before it reaches `1.0` would be absorbed by a lsp-lang major release rather
than hidden. The typed JSON conversions are inherent `to_json`/`from_json`
methods because `jsonx-lib` does not yet have `ToJson`/`FromJson` traits; if
it adds them, lsp-lang will implement them in a minor release alongside the
methods, which stay. `Handler::features` is an explicit declaration, not
something inferred from which methods were overridden (Rust cannot see that).

What is **not** promised: the wording of error messages, `window/logMessage`
text, and `Display` output; the `Debug` output of any type; the order of keys and
the whitespace in the JSON the server writes (it is valid JSON, but compare
values, not text); the default message limit; and performance figures: the
benchmarks are tracked, but they are measurements, not guarantees.

See [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and
[`../CHANGELOG.md`](../CHANGELOG.md).
