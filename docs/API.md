# lsp-lang &mdash; API Reference

> Complete reference for every public item in `lsp-lang`, with examples.
> **Status: stable (1.0).** The surface below is the `1.0` contract; it follows
> [Semantic Versioning](#stability) and will not change in a breaking way before
> `2.0`. See [`../dev/ROADMAP.md`](../dev/ROADMAP.md).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Concepts](#concepts)
  - [The session lifecycle](#the-session-lifecycle)
  - [Document synchronization](#document-synchronization)
  - [Positions and encodings](#positions-and-encodings)
  - [Diagnostics](#diagnostics-mapping)
  - [Three kinds of failure](#three-kinds-of-failure)
  - [Threading and performance](#threading-and-performance)
  - [About the examples](#about-the-examples)
- [`Server`](#server)
  - [`Server::new`](#servernew)
  - [`Server::with_message_limit`](#serverwith_message_limit)
  - [`Server::handler`](#serverhandler)
  - [`Server::run`](#serverrun)
- [`Handler`](#handler)
  - [`Handler::initialize`](#handlerinitialize)
  - [`Handler::changed`](#handlerchanged)
  - [`Handler::closed`](#handlerclosed)
  - [`Handler::request`](#handlerrequest)
  - [`Handler::notification`](#handlernotification)
  - [`Handler::response`](#handlerresponse)
- [`Context`](#context)
  - [`Context::document`](#contextdocument)
  - [`Context::documents`](#contextdocuments)
  - [`Context::notify`](#contextnotify)
  - [`Context::request`](#contextrequest)
  - [`Context::publish_diagnostics`](#contextpublish_diagnostics)
  - [`Context::clear_diagnostics`](#contextclear_diagnostics)
- [`Document`](#document)
  - [`Document::uri`](#documenturi)
  - [`Document::version`](#documentversion)
  - [`Document::text`](#documenttext)
  - [`Document::tree`](#documenttree)
  - [`Document::incremental`](#documentincremental)
  - [`Document::encoding`](#documentencoding)
  - [`Document::position`](#documentposition)
  - [`Document::offset`](#documentoffset)
  - [`Document::range`](#documentrange)
  - [`Document::span`](#documentspan)
- [`Json`](#json)
  - [`Json::from_raw`](#jsonfrom_raw)
  - [`Json::parse`](#jsonparse)
  - [`Json::as_str`](#jsonas_str)
- [`Reply`](#reply)
  - [`Reply::new`](#replynew)
  - [`Reply::null`](#replynull)
  - [`Reply::as_str`](#replyas_str)
- [`ResponseError`](#responseerror)
- [`Position`](#position)
- [`Range`](#range)
- [`Encoding`](#encoding)
- [`Error`](#error)
- [Re-exports](#re-exports)
- [Stability](#stability)

## Overview

lsp-lang is the Language Server Protocol layer of the `-lang` family. A
language server built on it supplies two things:

1. an [`incremental_lang::Grammar`](https://docs.rs/incremental-lang), which
   parses each open document and says which nodes may be reparsed on their own;
2. a [`Handler`](#handler), which answers the requests the language supports
   and publishes diagnostics as documents change.

The [`Server`](#server) does everything else: it frames and parses JSON-RPC
messages, runs the `initialize`/`shutdown`/`exit` lifecycle, keeps every open
document in step with the editor (reparsing incrementally on each edit), and
converts between protocol positions and byte offsets. Handlers work in byte
offsets and byte spans, like the rest of the family, and convert at the edge
with a [`Document`](#document).

| Item | Kind | Role |
|---|---|---|
| [`Server`](#server) | struct | The protocol engine: reads, dispatches, writes. |
| [`Handler`](#handler) | trait | The language's behaviour; every method has a default. |
| [`Context`](#context) | struct | A handler's view of the session: documents and a channel to the client. |
| [`Document`](#document) | struct | An open file: text, syntax tree, position conversion. |
| [`Json`](#json) | struct | Incoming parameters or results, parsed on demand. |
| [`Reply`](#reply) | struct | A request's result, pre-serialized. |
| [`ResponseError`](#responseerror) | struct | A request's failure, with a JSON-RPC error code. |
| [`Position`](#position), [`Range`](#range) | structs | The protocol's coordinates. |
| [`Encoding`](#encoding) | enum | The unit columns are counted in. |
| [`Error`](#error) | enum | What ends a session, or fails a write. |

## Installation

```toml
[dependencies]
lsp-lang = "1"
```

`incremental_lang`, `diag_lang`, and `serde_json` are [re-exported](#re-exports).
To derive `Deserialize` for parameter types, add `serde` with the `derive`
feature. MSRV: Rust 1.85 (Rust 2024 edition).

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
use std::io;
use std::process::ExitCode;
use lsp_lang::{Context, Document, Handler, Server};

struct MyServer;

impl Handler<MyGrammar> for MyServer {
    fn changed(&mut self, doc: &Document<MyGrammar>, cx: &mut Context<'_, MyGrammar>) {
        // Analyse `doc.text()` / `doc.tree()`; publish what you find.
        cx.publish_diagnostics(doc, &[]);
    }
}

fn main() -> ExitCode {
    let mut server = Server::new(MyGrammar, MyServer);
    match server.run(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("language server stopped: {err}");
            ExitCode::FAILURE
        }
    }
}
```

The [README](../README.md#quick-start) has a complete, runnable session, and
the [examples](../examples) a full server with diagnostics, hover, and
go-to-definition.

## Concepts

### The session lifecycle

[`Server::run`](#serverrun) serves one session. The protocol's lifecycle rules
are enforced by the server, so requests and notifications reach a handler only
inside the session window:

| Phase | Requests | Notifications |
|---|---|---|
| Before `initialize` | `initialize` is handled; anything else is refused with `ServerNotInitialized` (`-32002`). | Dropped, except `exit`. |
| Running | `initialize` again is refused with `InvalidRequest`; `shutdown` moves to the next phase; everything else goes to [`Handler::request`](#handlerrequest). | Document sync is handled by the server; `exit` ends the session; everything else goes to [`Handler::notification`](#handlernotification). |
| After `shutdown` | Refused with `InvalidRequest` (`-32600`). | Dropped, except `exit`. |

`exit` ends [`run`](#serverrun): with `Ok(())` after `shutdown`, or
[`Error::ExitWithoutShutdown`](#error) before it, mirroring the exit codes 0
and 1 the protocol asks for.

A message is classified by its members. With a `method` and an `id` it is a
request; the id must be a string or an integer, and a request with any other
id (`null`, a fraction, an object) is refused with `InvalidRequest` under a
`null` id, without being dispatched. With a `method` and no `id` it is a
notification. Without a `method` it is the client's answer to one of the
server's own requests ([`Context::request`](#contextrequest)): it reaches
[`Handler::response`](#handlerresponse) if, and only if, it answers a request
still awaiting an answer, whatever the phase; anything else that looks like an
answer is logged and dropped. The server never replies to an answer.

During `initialize`, the server negotiates the [position encoding](#encoding),
asks [`Handler::initialize`](#handlerinitialize) for the language's
capabilities, and adds the two it owns: `positionEncoding` and
`textDocumentSync` (open/close notifications, incremental changes).

### Document synchronization

The server handles `textDocument/didOpen`, `didChange`, and `didClose` itself.
Each open document is a [`Document`](#document): an
[`incremental_lang::Document`](https://docs.rs/incremental-lang) plus a line
index. A `didChange` may carry several edits; each is converted from its range
to a byte span, applied to the text, reparsed incrementally, and folded into
the line index, and then [`Handler::changed`](#handlerchanged) is called once.
A whole-text change (no range) replaces the text.

If an edit cannot be applied — the grammar's lexer broke its contract on the
new text, the grammar panicked, or the text outgrew 4 GiB — the server cannot
keep the document in step with the client. It drops the document, logs why to
the client with `window/logMessage`, and calls
[`Handler::closed`](#handlerclosed). A `didOpen` that fails the same way is
logged too, and if a document was already open under that URI it is dropped
(and `closed` called), since it no longer matches the client's text either.
Changes to a document that is not open are logged and ignored.

A grammar panic costs the document, not the session: the server catches it
where panics unwind. (A binary built with `panic = "abort"` ends instead, as
that setting asks.) A panic in the handler's own code is not caught; it
propagates out of [`run`](#serverrun).

The parsing work of an edit is proportional to the edit. What stays linear in
the size of the document is cheap: moving the text after the edit (one
`memmove`) and shifting the line starts after it (one vectorized pass).

### Positions and encodings

The protocol's `Position` is a zero-based line and a column; lines end at
`\n`, `\r\n`, or a lone `\r`. The column's unit is negotiated: the client lists
the encodings it supports in `general.positionEncodings`, and the server picks
**UTF-8** whenever it is offered (a byte column converts to a byte offset
without scanning) and otherwise the protocol's mandatory **UTF-16**. A
[`Document`](#document) converts in the negotiated encoding, so a handler never
counts columns itself.

Conversions are total. Input the protocol considers out of range is clamped as
the protocol prescribes: a line past the last maps to the end of the text, a
column past the end of its line maps to the end of that line (before the line
break). A column inside a character — between the halves of a UTF-16 surrogate
pair, or inside a UTF-8 sequence — maps to that character's start. Byte offsets
past the end clamp to the end, and offsets inside a character map to it.

Every byte offset that is a caret position round-trips exactly through
[`position`](#documentposition) and [`offset`](#documentoffset). The one
offset that is not — between the `\r` and `\n` of one line break — maps to the
column just past the `\r`, which converts back to the `\r` itself.

### Diagnostics mapping

[`Context::publish_diagnostics`](#contextpublish_diagnostics) converts
[`diag_lang::Diagnostic`](https://docs.rs/diag-lang)s whose spans are byte
offsets into the document — the space the syntax tree's spans are in — and
writes `textDocument/publishDiagnostics` with the document's URI and version:

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

### Three kinds of failure

| Kind | Type | Who sees it |
|---|---|---|
| A request failed | [`ResponseError`](#responseerror) | The client, as the request's error response. |
| A notification could not be processed (malformed parameters, an edit that could not be applied, a grammar panic, a change to a document that is not open), or an answer from the client matched no outstanding request | `window/logMessage` | The client's log. Notifications and answers have no response, so this is the only channel. |
| The session cannot continue (the transport failed or closed, the framing broke, a message was too large, `exit` without `shutdown`) | [`Error`](#error) | The caller of [`run`](#serverrun), which should exit non-zero. |

Malformed JSON and messages of the wrong shape are answered with `ParseError`
(`-32700`) and `InvalidRequest` (`-32600`) and the session continues. Nothing in
the crate panics on client input.

### Threading and performance

The server is synchronous and single-threaded: it reads one message, handles
it to completion, writes everything the handler sent with one `write_all` and
`flush`, then reads the next. For an editor talking to one server over a pipe,
this is the lowest-latency design — nothing stands between the client and the
handler. A handler with slow work can clone a document's
[tree](#documenttree) (one reference-count increment; `Send + Sync`) for a
worker thread and report back on a later call.

On the hot path, nothing is built that is not needed. The JSON-RPC envelope is
parsed in place, borrowing the method name and URIs from the message and
keeping parameters as raw JSON until the handler reads them; results are
serialized straight to text; diagnostics are written field by field into the
outgoing buffer. Message buffers are allocated once per session. An edit
costs a binary search to convert its range, an incremental reparse, and a
patch of the line index.

### About the examples

Examples that run a whole session start with a few hidden lines (shown as
`# …` on GitHub): a grammar that reads a whole document as one token, and two
helpers. `session(text, rest)` builds the input an editor would send —
`initialize` (id 0), `didOpen` of `file:///a` with `text`, the raw messages in
`rest`, then `shutdown` (id 99) and `exit`. `replies(output)` splits the
server's output back into JSON values.

## `Server`

```rust,ignore
pub struct Server<G, H> { /* private */ }

impl<G: Grammar + Clone, H: Handler<G>> Server<G, H>
```

The protocol engine around a [`Handler`](#handler). It speaks the base
protocol (`Content-Length`-framed JSON-RPC) over any `BufRead` and `Write`,
runs the [lifecycle](#the-session-lifecycle), keeps [documents in
sync](#document-synchronization), and calls the handler for everything else.

`G` is the grammar every document is parsed with; `H` the handler. `Debug`
shows the message limit.

### `Server::new`

```rust,ignore
pub fn new(grammar: G, handler: H) -> Server<G, H>
```

Creates a server.

| Parameter | Meaning |
|---|---|
| `grammar` | The language's grammar. Each opened document gets its own clone; a grammar is typically a unit struct, so cloning costs nothing. |
| `handler` | The language server's behaviour. The server owns it; read it back with [`handler`](#serverhandler). |

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

// The smallest possible server: document sync, nothing else.
struct SyncOnly;
impl Handler<Plain> for SyncOnly {}

let server = Server::new(Plain, SyncOnly);
```

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

// A handler with state of its own, configured before the session starts.
struct Linter { max_line: usize }
impl Handler<Plain> for Linter {}

let server = Server::new(Plain, Linter { max_line: 100 });
assert_eq!(server.handler().max_line, 100);
```

### `Server::with_message_limit`

```rust,ignore
pub fn with_message_limit(self, bytes: usize) -> Server<G, H>
```

Sets the largest message body accepted, in bytes. The default is 256 MiB.

| Parameter | Meaning |
|---|---|
| `bytes` | The limit. A message that announces a larger `Content-Length` ends the session with [`Error::TooLarge`](#error) before any of its body is read. |

The limit bounds the memory one message can claim. Opening a file sends its
whole text in one message (JSON-escaped, so somewhat larger than the file), so
it also bounds the size of document the server accepts.

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

// Refuse anything over 64 MiB.
let mut server = Server::new(Plain, SyncOnly).with_message_limit(64 << 20);

let huge = "Content-Length: 99999999999\r\n\r\n";
let result = server.run(huge.as_bytes(), Vec::new());
assert!(matches!(result, Err(Error::TooLarge { limit, .. }) if limit == 64 << 20));
```

### `Server::handler`

```rust,ignore
pub fn handler(&self) -> &H
```

Returns the handler — to inspect what it recorded after [`run`](#serverrun)
returns, in a test for instance.

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
use lsp_lang::{Context, Document, Handler, Server};

#[derive(Default)]
struct Versions(Vec<i32>);

impl Handler<Plain> for Versions {
    fn changed(&mut self, doc: &Document<Plain>, _: &mut Context<'_, Plain>) {
        self.0.push(doc.version());
    }
}

let input = session("hi", &[r#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":
    {"textDocument":{"uri":"file:///a","version":7},"contentChanges":[{"text":"bye"}]}}"#]);
let mut server = Server::new(Plain, Versions::default());
server.run(input.as_bytes(), Vec::new())?;
assert_eq!(server.handler().0, [1, 7]);
# Ok::<(), lsp_lang::Error>(())
```

### `Server::run`

```rust,ignore
pub fn run<R: BufRead, W: Write>(&mut self, input: R, output: W) -> Result<(), Error>
```

Serves one session: reads messages from `input` and writes responses and
notifications to `output` until the client sends `exit`.

| Parameter | Meaning |
|---|---|
| `input` | The client's messages. Should be buffered; `StdinLock` is. |
| `output` | Where the server's messages go. Each turn's output is written with one `write_all` and flushed, so it need not be buffered. |

Each call is a fresh session — uninitialized, no documents open — while the
grammar and handler carry over.

**Errors.** `Ok(())` is a clean stop: `shutdown`, then `exit`. Otherwise one of
[`Error`](#error)'s session-ending variants: `ExitWithoutShutdown`,
`Disconnected`, `Io`, `InvalidHeader`, or `TooLarge`. The protocol expects the
process to exit non-zero in every error case.

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
use lsp_lang::Server;

// A clean session: initialize, shutdown, exit.
let input = [
    frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":
        {"capabilities":{"general":{"positionEncodings":["utf-8","utf-16"]}}}}"#),
    frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
    frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
]
.concat();

let mut output = Vec::new();
Server::new(Plain, SyncOnly).run(input.as_bytes(), &mut output)?;
let output = String::from_utf8(output).unwrap();
assert!(output.contains(r#""positionEncoding":"utf-8""#));
assert!(output.contains(r#""textDocumentSync":{"change":2,"openClose":true}"#));
# Ok::<(), lsp_lang::Error>(())
```

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
use lsp_lang::{Error, Server};

let mut server = Server::new(Plain, SyncOnly);

// The editor went away without saying goodbye.
let input = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
assert!(matches!(server.run(input.as_bytes(), Vec::new()), Err(Error::Disconnected)));

// `exit` without `shutdown`: the protocol's "exit with status 1".
let input = frame(r#"{"jsonrpc":"2.0","method":"exit"}"#);
assert!(matches!(server.run(input.as_bytes(), Vec::new()), Err(Error::ExitWithoutShutdown)));
```

## `Handler`

```rust,ignore
pub trait Handler<G: Grammar> {
    fn initialize(&mut self, params: Json<'_>, cx: &mut Context<'_, G>) -> Result<Value, ResponseError>;
    fn changed(&mut self, document: &Document<G>, cx: &mut Context<'_, G>);
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>);
    fn request(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>) -> Result<Reply, ResponseError>;
    fn notification(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>);
    fn response(&mut self, id: u64, result: Result<Json<'_>, ResponseError>, cx: &mut Context<'_, G>);
}
```

A language server's behaviour. Every method has a default, so a handler
implements only what its language supports:

| Method | Called when | Default |
|---|---|---|
| [`initialize`](#handlerinitialize) | the client sends `initialize` | no capabilities beyond document sync |
| [`changed`](#handlerchanged) | a document opens or changes | nothing |
| [`closed`](#handlerclosed) | a document closes | clears its diagnostics |
| [`request`](#handlerrequest) | any request but `initialize` and `shutdown` | `MethodNotFound` |
| [`notification`](#handlernotification) | any notification but `exit` and document sync | ignored |
| [`response`](#handlerresponse) | the client answers a [`Context::request`](#contextrequest) | ignored |

Calls are made one at a time, in message order, on the thread running
[`Server::run`](#serverrun). Every method receives a [`Context`](#context).

A handler that does not depend on the language can be written for every
grammar at once — `impl<G: Grammar> Handler<G> for MyHandler` — as several
examples below are.

### `Handler::initialize`

```rust,ignore
fn initialize(&mut self, params: Json<'_>, cx: &mut Context<'_, G>) -> Result<Value, ResponseError>
```

Answers `initialize` with the server's capabilities.

| Parameter | Meaning |
|---|---|
| `params` | The client's `InitializeParams`: `rootUri`, `initializationOptions`, client capabilities. |
| `cx` | The session; no documents are open yet. |

**Returns** the `ServerCapabilities` object for what the handler supports. The
server adds `positionEncoding` and `textDocumentSync` before replying; other
members of a `textDocumentSync` object the handler returns (such as `save`) are
kept, while `openClose` and `change` are set by the server. A value that is not
a JSON object is treated as `{}`. An error refuses `initialize`; the session
stays uninitialized.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Handler, Json, ResponseError, Value};

struct Navigator;

impl<G: Grammar> Handler<G> for Navigator {
    fn initialize(&mut self, _: Json<'_>, _: &mut Context<'_, G>) -> Result<Value, ResponseError> {
        Ok(json!({
            "hoverProvider": true,
            "definitionProvider": true,
            // Merged with the server's own sync settings.
            "textDocumentSync": { "save": { "includeText": false } },
        }))
    }
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Handler, Json, ResponseError, Value};

// Read the workspace root and the client's options.
#[derive(Default)]
struct Project { root: Option<String>, strict: bool }

impl<G: Grammar> Handler<G> for Project {
    fn initialize(&mut self, params: Json<'_>, _: &mut Context<'_, G>) -> Result<Value, ResponseError> {
        let params: Value = params.parse()?;
        self.root = params["rootUri"].as_str().map(str::to_owned);
        self.strict = params["initializationOptions"]["strict"].as_bool().unwrap_or(false);
        if self.root.is_none() {
            return Err(ResponseError::new(ResponseError::REQUEST_FAILED, "open a folder first"));
        }
        Ok(json!({}))
    }
}
```

### `Handler::changed`

```rust,ignore
fn changed(&mut self, document: &Document<G>, cx: &mut Context<'_, G>)
```

Called after a document opens and after each `didChange`, once its text and
tree are current. The usual body analyses the document and publishes
diagnostics. A `didChange` carrying several edits produces one call.

| Parameter | Meaning |
|---|---|
| `document` | The document, up to date. |
| `cx` | The session; publish through it. |

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::{Context, Diagnostic, Document, Handler, Server};

/// Flags lines longer than 20 characters.
struct LongLines;

impl Handler<Plain> for LongLines {
    fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
        let mut found = Vec::new();
        let mut at = 0;
        for line in doc.text().split_inclusive('\n') {
            let content = line.trim_end_matches(['\r', '\n']);
            if content.chars().count() > 20 {
                let span = Span::new(at as u32, (at + content.len()) as u32);
                found.push(Diagnostic::new(Severity::Warning, "line too long", Label::new(span, "")));
            }
            at += line.len();
        }
        cx.publish_diagnostics(doc, &found);
    }
}

let input = session("short\nthis line is far too long to fit\nok", &[]);
let mut output = Vec::new();
Server::new(Plain, LongLines).run(input.as_bytes(), &mut output)?;
let output = String::from_utf8_lossy(&output);
assert!(output.contains(r#""range":{"start":{"line":1,"character":0},"end":{"line":1,"character":32}}"#));
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Document, Handler};

// Hand the tree to a worker: cloning it is O(1), and the snapshot never changes.
struct Background { queue: Vec<(String, i32, String)> }

impl<G: Grammar> Handler<G> for Background {
    fn changed(&mut self, doc: &Document<G>, _: &mut Context<'_, G>) {
        let _snapshot = doc.tree().clone();
        self.queue.push((doc.uri().to_owned(), doc.version(), doc.text().to_owned()));
    }
}
```

### `Handler::closed`

```rust,ignore
fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>)
```

Called after the document `uri` was closed and removed — by `didClose`, or
because an edit to it could not be applied. The default clears its
diagnostics, as the protocol recommends; an override that keeps per-document
state should drop it here and call
[`Context::clear_diagnostics`](#contextclear_diagnostics) itself.

| Parameter | Meaning |
|---|---|
| `uri` | The closed document's URI. It is no longer in [`Context::document`](#contextdocument). |
| `cx` | The session. |

```rust
use std::collections::HashMap;
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler};

#[derive(Default)]
struct Index { symbols: HashMap<String, Vec<String>> }

impl<G: Grammar> Handler<G> for Index {
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, G>) {
        self.symbols.remove(uri);
        cx.clear_diagnostics(uri); // keep the default's behaviour
    }
}
```

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
use lsp_lang::{Handler, Server};

// The default clears the document's diagnostics on close.
struct Defaults;
impl Handler<Plain> for Defaults {}

let input = session("text", &[r#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":
    {"textDocument":{"uri":"file:///a"}}}"#]);
let mut output = Vec::new();
Server::new(Plain, Defaults).run(input.as_bytes(), &mut output)?;
let output = String::from_utf8_lossy(&output);
assert!(output.contains(r#""params":{"uri":"file:///a","diagnostics":[]}"#));
# Ok::<(), lsp_lang::Error>(())
```

### `Handler::request`

```rust,ignore
fn request(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>) -> Result<Reply, ResponseError>
```

Answers a request — every request but `initialize` and `shutdown`, and only
between them.

| Parameter | Meaning |
|---|---|
| `method` | The request's method, such as `"textDocument/hover"`. |
| `params` | Its parameters, to [`parse`](#jsonparse) into the handler's own type. `null` if absent. |
| `cx` | The session. |

**Returns** the response: a [`Reply`](#reply) (use [`Reply::null`](#replynull)
for "nothing here") or a [`ResponseError`](#responseerror). `?` works on both
[`Json::parse`](#jsonparse) and [`Reply::new`](#replynew). The default returns
`MethodNotFound`; a handler with its own `match` should fall back to the same.

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
# fn replies(out: &[u8]) -> Vec<lsp_lang::Value> {
#     String::from_utf8_lossy(out).split("Content-Length: ")
#         .filter_map(|m| m.split_once("\r\n\r\n"))
#         .filter_map(|(_, body)| lsp_lang::serde_json::from_str(body).ok()).collect()
# }
use lsp_lang::{json, Context, Handler, Json, Position, Range, Reply, ResponseError, Server};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Params<'a> {
    #[serde(borrow)]
    text_document: Doc<'a>,
    position: Position,
}

#[derive(serde::Deserialize)]
struct Doc<'a> { uri: &'a str }

/// Hover: the word under the cursor and its range.
struct Words;

impl Handler<Plain> for Words {
    fn request(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, Plain>) -> Result<Reply, ResponseError> {
        match method {
            "textDocument/hover" => {
                let p: Params<'_> = params.parse()?;
                let Some(doc) = cx.document(p.text_document.uri) else { return Ok(Reply::null()) };
                let text = doc.text();
                let at = doc.offset(p.position) as usize;
                let start = text[..at].rfind(' ').map_or(0, |i| i + 1);
                let end = text[at..].find(' ').map_or(text.len(), |i| at + i);
                let span = Span::new(start as u32, end as u32);
                Ok(Reply::new(&json!({ "contents": &text[start..end], "range": doc.range(span) }))?)
            }
            _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
        }
    }
}

let input = session("héllo wörld", &[
    r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":
        {"textDocument":{"uri":"file:///a"},"position":{"line":0,"character":8}}}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{}}"#,
]);
let mut output = Vec::new();
Server::new(Plain, Words).run(input.as_bytes(), &mut output)?;
let replies = replies(&output);
let hover = replies.iter().find(|m| m["id"] == 1).unwrap();
assert_eq!(hover["result"]["contents"], "wörld");
assert_eq!(hover["result"]["range"]["start"]["character"], 6);
let rename = replies.iter().find(|m| m["id"] == 2).unwrap();
assert_eq!(rename["error"]["code"], ResponseError::METHOD_NOT_FOUND);
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler, Json, Reply, ResponseError};

// Refuse work on a document that changed underneath the request.
struct Careful { busy: bool }

impl<G: Grammar> Handler<G> for Careful {
    fn request(&mut self, method: &str, _: Json<'_>, _: &mut Context<'_, G>) -> Result<Reply, ResponseError> {
        if self.busy {
            return Err(ResponseError::new(ResponseError::CONTENT_MODIFIED, "document changed; retry"));
        }
        Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method))
    }
}
```

### `Handler::notification`

```rust,ignore
fn notification(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>)
```

Receives a notification the server does not handle itself: everything but
`exit` and document synchronization. That includes `initialized`,
`workspace/didChangeConfiguration`, `textDocument/didSave`, and protocol
notifications such as `$/cancelRequest` and `$/setTrace`, which a handler may
ignore. Notifications before `initialize` or after `shutdown` never arrive.

| Parameter | Meaning |
|---|---|
| `method` | The notification's method. |
| `params` | Its parameters; `null` if absent. |
| `cx` | The session. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Handler, Json};

// Say hello once the client is ready.
struct Greeter;

impl<G: Grammar> Handler<G> for Greeter {
    fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, G>) {
        if method == "initialized" {
            let hello = json!({ "type": 3, "message": "Lisp server ready" });
            if let Err(err) = cx.notify("window/logMessage", &hello) {
                eprintln!("{err}");
            }
        }
    }
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Handler, Json};

// React to saves: the client sends `didSave` when `save` is in the
// `textDocumentSync` capability.
#[derive(Default)]
struct SaveCounter { saves: u32 }

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidSave<'a> { #[serde(borrow)] text_document: Uri<'a> }
#[derive(serde::Deserialize)]
struct Uri<'a> { uri: &'a str }

impl<G: Grammar> Handler<G> for SaveCounter {
    fn notification(&mut self, method: &str, params: Json<'_>, _: &mut Context<'_, G>) {
        if method == "textDocument/didSave" && params.parse::<DidSave<'_>>().is_ok() {
            self.saves += 1;
        }
    }
}
```

### `Handler::response`

```rust,ignore
fn response(&mut self, id: u64, result: Result<Json<'_>, ResponseError>, cx: &mut Context<'_, G>)
```

Receives the client's answer to a request sent with
[`Context::request`](#contextrequest). Each request is answered at most once:
an answer that matches no request still awaiting one — a second answer, an id
the server never sent, an answer with no id — is logged and dropped, and the
server never replies to an answer.

| Parameter | Meaning |
|---|---|
| `id` | The id [`Context::request`](#contextrequest) returned. |
| `result` | The response's result, or the error the client answered with, read leniently: a missing message reads as empty, and a missing or out-of-range code — or an `error` that is not an object — as `INTERNAL_ERROR`. |
| `cx` | The session. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Handler, Json, ResponseError, Value};

// Ask for configuration once ready; apply the answer when it comes.
#[derive(Default)]
struct Configured { pending: Option<u64>, tab_width: u64 }

impl<G: Grammar> Handler<G> for Configured {
    fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, G>) {
        if method == "initialized" {
            let items = json!({ "items": [{ "section": "editor" }] });
            self.pending = cx.request("workspace/configuration", &items).ok();
        }
    }

    fn response(&mut self, id: u64, result: Result<Json<'_>, ResponseError>, _: &mut Context<'_, G>) {
        if self.pending.take() != Some(id) {
            return;
        }
        let settings: Vec<Value> = result.and_then(|json| json.parse()).unwrap_or_default();
        self.tab_width = settings.first().and_then(|s| s["tabWidth"].as_u64()).unwrap_or(4);
    }
}
```

## `Context`

```rust,ignore
pub struct Context<'a, G: Grammar> { /* private */ }
```

A handler's view of the session for one call: the open documents, and a
channel back to the client. Messages sent through a context are buffered and
written in order as soon as the handler returns, with any response to the
current request after them.

Documents borrowed from a context live for the whole call (`'a`), not just the
borrow of the context, so a handler can hold one and send messages at once.
`Debug` shows the number of open documents and the next request id.

### `Context::document`

```rust,ignore
pub fn document(&self, uri: &str) -> Option<&'a Document<G>>
```

Returns the open document with this URI. URIs match exactly, as the client
sent them.

| Parameter | Meaning |
|---|---|
| `uri` | The document's URI. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Diagnostic};

// Look a document up and publish to it in the same breath.
fn report<G: Grammar>(cx: &mut Context<'_, G>, uri: &str, found: &[Diagnostic]) -> bool {
    let Some(doc) = cx.document(uri) else { return false };
    cx.publish_diagnostics(doc, found);
    true
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Context;

fn line_count<G: Grammar>(cx: &Context<'_, G>, uri: &str) -> Option<u32> {
    let doc = cx.document(uri)?;
    Some(doc.position(doc.text().len() as u32).line + 1)
}
```

### `Context::documents`

```rust,ignore
pub fn documents(&self) -> impl ExactSizeIterator<Item = &'a Document<G>> + 'a
```

Returns every open document, ordered by URI (so anything published while
iterating goes out in the same order every run).

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Diagnostic};

// Re-check everything, e.g. after a configuration change.
fn recheck_all<G: Grammar>(cx: &mut Context<'_, G>, check: impl Fn(&str) -> Vec<Diagnostic>) {
    for doc in cx.documents() {
        let found = check(doc.text());
        cx.publish_diagnostics(doc, &found);
    }
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Context;

fn open_bytes<G: Grammar>(cx: &Context<'_, G>) -> usize {
    cx.documents().map(|doc| doc.text().len()).sum()
}
```

### `Context::notify`

```rust,ignore
pub fn notify<P: Serialize + ?Sized>(&mut self, method: &str, params: &P) -> Result<(), Error>
```

Sends a notification to the client. `params` is serialized straight into the
outgoing message.

| Parameter | Meaning |
|---|---|
| `method` | The notification's method. |
| `params` | Any `Serialize` value, including a [`Value`](#re-exports) from [`json!`](#re-exports). |

**Errors.** [`Error::Json`](#error) if `params` fails to serialize. Nothing is
sent, and the session continues.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Error};

// `MessageType` 1 = error, 2 = warning, 3 = info, 4 = log.
fn warn<G: Grammar>(cx: &mut Context<'_, G>, text: &str) -> Result<(), Error> {
    cx.notify("window/showMessage", &json!({ "type": 2, "message": text }))
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Error};

// A typed payload, serialized without an intermediate value.
#[derive(serde::Serialize)]
struct Progress<'a> { token: &'a str, value: Report }
#[derive(serde::Serialize)]
struct Report { kind: &'static str, percentage: u32 }

fn progress<G: Grammar>(cx: &mut Context<'_, G>, percent: u32) -> Result<(), Error> {
    let value = Report { kind: "report", percentage: percent };
    cx.notify("$/progress", &Progress { token: "indexing", value })
}
```

### `Context::request`

```rust,ignore
pub fn request<P: Serialize + ?Sized>(&mut self, method: &str, params: &P) -> Result<u64, Error>
```

Sends a request to the client and returns the id it was sent with; the answer
arrives later through [`Handler::response`](#handlerresponse) with the same
id, exactly once. Ids count up from 0 in each session.

| Parameter | Meaning |
|---|---|
| `method` | The request's method, such as `"workspace/configuration"`. |
| `params` | Any `Serialize` value. |

**Errors.** [`Error::Json`](#error) if `params` fails to serialize. Nothing is
sent and no id is used.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Error};

fn ask_settings<G: Grammar>(cx: &mut Context<'_, G>) -> Result<u64, Error> {
    cx.request("workspace/configuration", &json!({ "items": [{ "section": "lisp" }] }))
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Error};

// Register for file-watch events at runtime.
fn watch_lisp_files<G: Grammar>(cx: &mut Context<'_, G>) -> Result<u64, Error> {
    cx.request("client/registerCapability", &json!({
        "registrations": [{
            "id": "watch",
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": [{ "globPattern": "**/*.lisp" }] },
        }],
    }))
}
```

### `Context::publish_diagnostics`

```rust,ignore
pub fn publish_diagnostics(&mut self, document: &Document<G>, diagnostics: &[Diagnostic])
```

Publishes `diagnostics` for `document`, replacing what was published for it
before. Spans are byte offsets into the document's text; they are converted to
ranges in the session encoding (clamped to the text) and the rest is mapped as
[described above](#diagnostics-mapping). The notification carries the
document's version. An empty slice clears the document's diagnostics.

All spans are converted in one ascending pass before anything is written, so
many diagnostics on one long line cost one scan of that line, in whatever
order they come.

| Parameter | Meaning |
|---|---|
| `document` | The document the diagnostics are about. |
| `diagnostics` | The diagnostics, spans in the document's byte offsets. |

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
# fn replies(out: &[u8]) -> Vec<lsp_lang::Value> {
#     String::from_utf8_lossy(out).split("Content-Length: ")
#         .filter_map(|m| m.split_once("\r\n\r\n"))
#         .filter_map(|(_, body)| lsp_lang::serde_json::from_str(body).ok()).collect()
# }
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::{json, Context, Diagnostic, Document, Handler, Server};

/// Reports a duplicated word, linking back to its first occurrence.
struct Doubles;

impl Handler<Plain> for Doubles {
    fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
        let text = doc.text();
        let (first, second) = (text.find("the").unwrap(), text.rfind("the").unwrap());
        let span = |at: usize| Span::new(at as u32, at as u32 + 3);
        let found = Diagnostic::new(Severity::Error, "word repeated", Label::new(span(second), "again here"))
            .with_secondary(Label::new(span(first), "first here"))
            .with_help("delete one");
        cx.publish_diagnostics(doc, &[found]);
    }
}

let input = session("😀 the\nthe end", &[]);
let mut output = Vec::new();
Server::new(Plain, Doubles).run(input.as_bytes(), &mut output)?;
let replies = replies(&output);
let params = &replies.iter().find(|m| m["method"] == "textDocument/publishDiagnostics").unwrap()["params"];
assert_eq!(params["version"], 1);
let d = &params["diagnostics"][0];
assert_eq!(d["severity"], 1);
assert_eq!(d["message"], "word repeated\nagain here\nhelp: delete one");
assert_eq!(d["range"]["start"], json!({ "line": 1, "character": 0 }));
// `😀` is two UTF-16 units, so the first `the` starts at column 3.
assert_eq!(d["relatedInformation"][0]["location"]["range"]["start"], json!({ "line": 0, "character": 3 }));
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Diagnostic, Document};

// Publish diagnostics produced by any stage built on diag-lang.
fn publish_all<G: Grammar>(cx: &mut Context<'_, G>, doc: &Document<G>, stages: &[Vec<Diagnostic>]) {
    let all: Vec<Diagnostic> = stages.iter().flatten().cloned().collect();
    cx.publish_diagnostics(doc, &all);
}
```

### `Context::clear_diagnostics`

```rust,ignore
pub fn clear_diagnostics(&mut self, uri: &str)
```

Clears the diagnostics published for `uri`, whether or not the document is
open. The default [`Handler::closed`](#handlerclosed) calls it.

| Parameter | Meaning |
|---|---|
| `uri` | The document whose diagnostics to clear. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Context;

// Forget everything reported, e.g. when the project is reloaded.
fn clear_all<G: Grammar>(cx: &mut Context<'_, G>) {
    let uris: Vec<&str> = cx.documents().map(|doc| doc.uri()).collect();
    for uri in uris {
        cx.clear_diagnostics(uri);
    }
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Context;

// Clear a file the client never opened, after deleting it on disk.
fn forget<G: Grammar>(cx: &mut Context<'_, G>) {
    cx.clear_diagnostics("file:///project/removed.lisp");
}
```

## `Document`

```rust,ignore
pub struct Document<G: Grammar> { /* private */ }
```

A document the client has open: its text, its syntax tree, and the
conversions between protocol positions and byte offsets in the session's
[encoding](#encoding). Created by the server on `didOpen`, updated on every
`didChange`, dropped on `didClose`; handlers see it read-only.

Each document pairs an [`incremental_lang::Document`](#documentincremental)
with a line index. An edit is converted from its range to a byte span,
reparsed incrementally — only the innermost node around it that can be
reparsed on its own is rebuilt — and folded into the line index in place.
`Debug` shows the URI, version, encoding, and length, not the text.

### `Document::uri`

```rust,ignore
pub fn uri(&self) -> &str
```

The document's URI, exactly as the client sent it.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Document;

fn is_test<G: Grammar>(doc: &Document<G>) -> bool {
    doc.uri().ends_with("_test.lisp")
}
```

### `Document::version`

```rust,ignore
pub fn version(&self) -> i32
```

The version the client gave the current text; it increases with every change.
Published diagnostics carry it, so the client can discard results for text it
has since replaced.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Document;

// Skip work if this version was already analysed.
fn is_new<G: Grammar>(doc: &Document<G>, last_seen: &mut i32) -> bool {
    let new = doc.version() != *last_seen;
    *last_seen = doc.version();
    new
}
```

### `Document::text`

```rust,ignore
pub fn text(&self) -> &str
```

The current text.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Document;

fn word_count<G: Grammar>(doc: &Document<G>) -> usize {
    doc.text().split_whitespace().count()
}
```

### `Document::tree`

```rust,ignore
pub fn tree(&self) -> &Tree<G::Kind>
```

The current syntax tree — the grammar's lossless concrete syntax tree of the
text, with spans in byte offsets. Clone it (one reference-count increment) for
a snapshot that later edits leave untouched.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Range};

// Ranges of the top-level nodes, for an outline or folding.
fn top_level<G: Grammar>(doc: &Document<G>) -> Vec<Range> {
    doc.tree().root().children().map(|child| doc.range(child.span())).collect()
}
```

```rust
use lsp_lang::incremental_lang::{Grammar, Tree};
use lsp_lang::Document;

// A snapshot for another thread.
fn snapshot<G: Grammar>(doc: &Document<G>) -> Tree<G::Kind> {
    doc.tree().clone()
}
```

### `Document::incremental`

```rust,ignore
pub fn incremental(&self) -> &incremental_lang::Document<G>
```

The underlying incremental document, for its grammar and its reparse
statistics.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::Document;

// Are this grammar's reparsable kinds paying off?
fn incremental_share<G: Grammar>(doc: &Document<G>) -> f64 {
    let stats = doc.incremental().stats();
    stats.partial as f64 / stats.edits.max(1) as f64
}
```

### `Document::encoding`

```rust,ignore
pub fn encoding(&self) -> Encoding
```

The [encoding](#encoding) negotiated for the session. The conversion methods
apply it already; it matters only to a handler counting columns itself.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Encoding};

fn columns_are_bytes<G: Grammar>(doc: &Document<G>) -> bool {
    doc.encoding() == Encoding::Utf8
}
```

### `Document::position`

```rust,ignore
pub fn position(&self, offset: u32) -> Position
```

Converts a byte offset into the text to a protocol position. Past the end maps
to the end; inside a character maps to that character; between the `\r` and
`\n` of one line break, to the column just past the `\r`. Cost: a binary search
over line starts, plus — in a UTF-16 session — a vectorized count over the
line's text before the offset.

| Parameter | Meaning |
|---|---|
| `offset` | A byte offset into [`text`](#documenttext). |

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
use lsp_lang::{Context, Document, Handler, Position, Server};

#[derive(Default)]
struct Probe(Vec<Position>);

impl Handler<Plain> for Probe {
    fn changed(&mut self, doc: &Document<Plain>, _: &mut Context<'_, Plain>) {
        // "a😀b\r\nc": `b` is byte 5, `c` byte 8; 99 is past the end.
        self.0 = [5, 8, 99, 2].iter().map(|&at| doc.position(at)).collect();
    }
}

let mut server = Server::new(Plain, Probe::default());
server.run(session("a😀b\r\nc", &[]).as_bytes(), Vec::new())?;
assert_eq!(server.handler().0, [
    Position::new(0, 3), // UTF-16: `😀` counts two
    Position::new(1, 0),
    Position::new(1, 1), // clamped to the end
    Position::new(0, 1), // inside `😀`: its start
]);
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Position};

fn find<G: Grammar>(doc: &Document<G>, needle: &str) -> Option<Position> {
    doc.text().find(needle).map(|at| doc.position(at as u32))
}
```

### `Document::offset`

```rust,ignore
pub fn offset(&self, position: Position) -> u32
```

Converts a protocol position to a byte offset into the text. A line past the
last maps to the end of the text; a column past the end of its line maps to
the end of the line, before its break; a column inside a character maps to its
start. The result is always a character boundary.

| Parameter | Meaning |
|---|---|
| `position` | A position in the session's encoding — typically a request's cursor. |

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
# fn frame(json: &str) -> String { format!("Content-Length: {}\r\n\r\n{json}", json.len()) }
# fn session(text: &str, rest: &[&str]) -> String {
#     let open = lsp_lang::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":
#         {"textDocument":{"uri":"file:///a","languageId":"x","version":1,"text":text}}});
#     let mut s = frame(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}"#);
#     s += &frame(&open.to_string());
#     for m in rest { s += &frame(m); }
#     s += &frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
#     s + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#)
# }
use lsp_lang::{Context, Document, Handler, Position, Server};

#[derive(Default)]
struct Probe(Vec<u32>);

impl Handler<Plain> for Probe {
    fn changed(&mut self, doc: &Document<Plain>, _: &mut Context<'_, Plain>) {
        let at = [(0, 3), (0, 2), (0, 50), (1, 1), (9, 0)];
        self.0 = at.iter().map(|&(l, c)| doc.offset(Position::new(l, c))).collect();
    }
}

let mut server = Server::new(Plain, Probe::default());
server.run(session("a😀b\r\nc", &[]).as_bytes(), Vec::new())?;
assert_eq!(server.handler().0, [
    5, // after `😀`, on `b`
    1, // between the halves of `😀`: its start
    6, // past the line's end: before `\r\n`
    9, // end of line 1
    9, // past the last line: end of text
]);
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Position};

// The word under a cursor.
fn word_at<G: Grammar>(doc: &Document<G>, cursor: Position) -> &str {
    let text = doc.text();
    let at = doc.offset(cursor) as usize;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at].rfind(|c| !is_word(c)).map_or(0, |i| i + 1);
    let end = text[at..].find(|c| !is_word(c)).map_or(text.len(), |i| at + i);
    &text[start..end]
}
```

### `Document::range`

```rust,ignore
pub fn range(&self, span: Span) -> Range
```

Converts a byte span — from the tree or a diagnostic — to a protocol range,
each end as by [`position`](#documentposition).

| Parameter | Meaning |
|---|---|
| `span` | A byte span of the text. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Range};

fn whole<G: Grammar>(doc: &Document<G>) -> Range {
    doc.range(doc.tree().root().span())
}
```

```rust
use lsp_lang::incremental_lang::{Grammar, Span};
use lsp_lang::{json, Document, Value};

// A `Location` for a definition result.
fn location<G: Grammar>(doc: &Document<G>, span: Span) -> Value {
    json!({ "uri": doc.uri(), "range": doc.range(span) })
}
```

### `Document::span`

```rust,ignore
pub fn span(&self, range: Range) -> Span
```

Converts a protocol range to a byte span, each end as by
[`offset`](#documentoffset). A range whose ends arrive reversed yields the span
between them.

| Parameter | Meaning |
|---|---|
| `range` | A range in the session's encoding — typically a request's selection. |

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Range};

fn selected<G: Grammar>(doc: &Document<G>, range: Range) -> &str {
    let span = doc.span(range);
    &doc.text()[span.start().to_usize()..span.end().to_usize()]
}
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Range};

// The tree nodes a selection overlaps.
fn overlapping<G: Grammar>(doc: &Document<G>, range: Range) -> usize {
    let span = doc.span(range);
    doc.tree()
        .root()
        .descendants()
        .filter(|n| n.span().start() < span.end() && span.start() < n.span().end())
        .count()
}
```

## `Json`

```rust,ignore
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Json<'a> { /* private */ }
```

A JSON value from an incoming message — a request's or notification's
parameters, or a response's result — kept as the raw text the client sent.
Nothing is parsed until [`parse`](#jsonparse) is called, and then straight into
the requested type, borrowing strings from the message where they have no
escapes. `Debug` prints the raw JSON.

### `Json::from_raw`

```rust,ignore
pub const fn from_raw(raw: &'a str) -> Json<'a>
```

Wraps raw JSON text. The server builds every `Json` a handler receives; this is
for calling a handler's logic directly, as in its unit tests. The text is not
checked until [`parse`](#jsonparse).

```rust
use lsp_lang::{Json, Position};

let params = Json::from_raw(r#"{"line":4,"character":2}"#);
let pos: Position = params.parse()?;
assert_eq!(pos, Position::new(4, 2));
# Ok::<(), lsp_lang::ResponseError>(())
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Json, ResponseError};

// Test the parsing half of a handler without a server.
fn parse_query(params: Json<'_>) -> Result<String, ResponseError> {
    let v: lsp_lang::Value = params.parse()?;
    v["query"].as_str().map(str::to_owned).ok_or_else(|| {
        ResponseError::new(ResponseError::INVALID_PARAMS, "missing `query`")
    })
}

assert_eq!(parse_query(Json::from_raw(r#"{"query":"fn"}"#)).unwrap(), "fn");
assert!(parse_query(Json::from_raw("{}")).is_err());
```

### `Json::parse`

```rust,ignore
pub fn parse<T: Deserialize<'a>>(self) -> Result<T, ResponseError>
```

Deserializes the value into `T` — a handler's own struct (which may borrow
`&'a str` fields from the message), a [`Position`](#position), or a
[`Value`](#re-exports).

**Errors.** A [`ResponseError`](#responseerror) with code `INVALID_PARAMS`
carrying `serde_json`'s description of the mismatch, ready to return with `?`.

```rust
use lsp_lang::Json;

#[derive(serde::Deserialize)]
struct Rename<'a> { #[serde(rename = "newName")] new_name: &'a str }

let params = Json::from_raw(r#"{"newName":"total","extra":true}"#);
let rename: Rename<'_> = params.parse()?; // unknown fields are ignored
assert_eq!(rename.new_name, "total");
# Ok::<(), lsp_lang::ResponseError>(())
```

```rust
use lsp_lang::{Json, ResponseError};

let err = Json::from_raw(r#"{"line":"three"}"#).parse::<lsp_lang::Position>().unwrap_err();
assert_eq!(err.code(), ResponseError::INVALID_PARAMS);
assert!(err.message().starts_with("invalid params"));
```

### `Json::as_str`

```rust,ignore
pub const fn as_str(self) -> &'a str
```

The raw JSON text, exactly as the client sent it — to log it, or to forward it
without parsing.

```rust
use lsp_lang::Json;

let params = Json::from_raw(r#"{ "a": [1, 2] }"#);
assert_eq!(params.as_str(), r#"{ "a": [1, 2] }"#);
```

## `Reply`

```rust,ignore
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply { /* private */ }
```

A request's successful result, already serialized. Serializing directly to
text, rather than building a [`Value`](#re-exports) first, keeps a large result
to one pass and one allocation; the server copies the text into the response
verbatim.

### `Reply::new`

```rust,ignore
pub fn new<T: Serialize + ?Sized>(value: &T) -> Result<Reply, Error>
```

Serializes `value` as the result.

| Parameter | Meaning |
|---|---|
| `value` | Any `Serialize` value. |

**Errors.** [`Error::Json`](#error) if serialization fails (a map with
non-string keys, a custom `Serialize` that errors). Inside
[`Handler::request`](#handlerrequest), `?` turns it into an `INTERNAL_ERROR`
response.

```rust
use lsp_lang::{json, Reply};

let reply = Reply::new(&json!({ "contents": "a number" }))?;
assert_eq!(reply.as_str(), r#"{"contents":"a number"}"#);
# Ok::<(), lsp_lang::Error>(())
```

```rust
use lsp_lang::{Position, Range, Reply};

// Typed results serialize without a `Value` in between.
#[derive(serde::Serialize)]
struct TextEdit { range: Range, #[serde(rename = "newText")] new_text: String }

let edits = vec![TextEdit {
    range: Range::new(Position::new(0, 0), Position::new(0, 3)),
    new_text: "let".into(),
}];
let reply = Reply::new(&edits)?;
assert!(reply.as_str().starts_with(r#"[{"range":{"start":{"line":0"#));
# Ok::<(), lsp_lang::Error>(())
```

### `Reply::null`

```rust,ignore
pub const fn null() -> Reply
```

The `null` result, without allocating — the protocol's "nothing here": no
hover at the position, no definition found.

```rust
use lsp_lang::Reply;

assert_eq!(Reply::null().as_str(), "null");
assert_eq!(Reply::null(), Reply::new(&None::<u32>).unwrap());
```

### `Reply::as_str`

```rust,ignore
pub fn as_str(&self) -> &str
```

The serialized JSON.

```rust
use lsp_lang::Reply;

assert_eq!(Reply::new(&[1, 2, 3])?.as_str(), "[1,2,3]");
# Ok::<(), lsp_lang::Error>(())
```

## `ResponseError`

```rust,ignore
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResponseError { /* private */ }

impl ResponseError {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    pub const SERVER_NOT_INITIALIZED: i32 = -32002;
    pub const REQUEST_FAILED: i32 = -32803;
    pub const SERVER_CANCELLED: i32 = -32802;
    pub const CONTENT_MODIFIED: i32 = -32801;
    pub const REQUEST_CANCELLED: i32 = -32800;

    pub fn new(code: i32, message: impl Into<String>) -> ResponseError;
    pub const fn code(&self) -> i32;
    pub fn message(&self) -> &str;
}

impl From<Error> for ResponseError;      // INTERNAL_ERROR
impl Display for ResponseError;          // "message (code N)"
impl std::error::Error for ResponseError;
```

A JSON-RPC error: a request's failure, sent to the client in place of a result.

| Code | Constant | Meaning — and who sends it |
|---|---|---|
| -32700 | `PARSE_ERROR` | The message was not valid JSON. The server. |
| -32600 | `INVALID_REQUEST` | Not a valid request (wrong shape, `initialize` twice, a request after `shutdown`). The server. |
| -32601 | `METHOD_NOT_FOUND` | The method is not supported. The default [`Handler::request`](#handlerrequest). |
| -32602 | `INVALID_PARAMS` | The parameters are invalid. [`Json::parse`](#jsonparse). |
| -32603 | `INTERNAL_ERROR` | Something failed inside the server, such as serializing the result. |
| -32002 | `SERVER_NOT_INITIALIZED` | A request arrived before `initialize`. The server. |
| -32803 | `REQUEST_FAILED` | The request was understood but could not be served; the message says why. |
| -32802 | `SERVER_CANCELLED` | The server cancelled the request; the client may retry. |
| -32801 | `CONTENT_MODIFIED` | The document changed during the request; the client should retry against the new text. |
| -32800 | `REQUEST_CANCELLED` | The client cancelled the request. |

| Method | Meaning |
|---|---|
| `new(code, message)` | Builds an error. Some clients show the message to the user: say what failed and what to do. |
| `code()` | The error code. |
| `message()` | The message. |

```rust
use lsp_lang::ResponseError;

let err = ResponseError::new(ResponseError::REQUEST_FAILED, "no project loaded");
assert_eq!(err.code(), -32803);
assert_eq!(err.message(), "no project loaded");
assert_eq!(err.to_string(), "no project loaded (code -32803)");
```

```rust
use lsp_lang::{Error, ResponseError};

// A library error becomes an internal error response.
let err: ResponseError = Error::Disconnected.into();
assert_eq!(err.code(), ResponseError::INTERNAL_ERROR);
```

## `Position`

```rust,ignore
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub const fn new(line: u32, character: u32) -> Position;
}
```

A place in a document: a zero-based line and a zero-based column in the
session's [encoding](#encoding). Serializes as the protocol's
`{"line":…,"character":…}`, so it can sit directly in parameter and result
types. Orders by line, then column.

| Field | Meaning |
|---|---|
| `line` | Zero-based line. Lines end at `\n`, `\r\n`, or a lone `\r`. |
| `character` | Zero-based column, in the session's encoding. |

```rust
use lsp_lang::Position;

let a = Position::new(2, 4);
assert!(a < Position::new(3, 0));
assert_eq!(lsp_lang::serde_json::to_string(&a).unwrap(), r#"{"line":2,"character":4}"#);
```

```rust
use lsp_lang::{Json, Position};

#[derive(serde::Deserialize)]
struct Cursor { position: Position }

let c: Cursor = Json::from_raw(r#"{"position":{"line":1,"character":7}}"#).parse()?;
assert_eq!(c.position, Position::new(1, 7));
# Ok::<(), lsp_lang::ResponseError>(())
```

## `Range`

```rust,ignore
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Range;
}
```

A stretch of a document: `start` inclusive, `end` exclusive. An empty range
marks a point. Serializes as `{"start":…,"end":…}`.

| Field | Meaning |
|---|---|
| `start` | Where the range begins. |
| `end` | Where it ends (exclusive). |

```rust
use lsp_lang::{Position, Range};

let word = Range::new(Position::new(0, 4), Position::new(0, 9));
assert_eq!(
    lsp_lang::serde_json::to_string(&word).unwrap(),
    r#"{"start":{"line":0,"character":4},"end":{"line":0,"character":9}}"#
);
```

```rust
use lsp_lang::{Position, Range};

let cursor = Range::new(Position::new(3, 1), Position::new(3, 1));
assert_eq!(cursor.start, cursor.end); // a point
assert_eq!(Range::default(), Range::new(Position::default(), Position::default()));
```

## `Encoding`

```rust,ignore
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
}
```

The unit [`Position`](#position) columns are counted in, negotiated at
`initialize` (see [Positions and encodings](#positions-and-encodings)).

| Variant | Protocol name | Unit | `é` | `😀` |
|---|---|---|---|---|
| `Utf8` | `"utf-8"` | bytes | 2 | 4 |
| `Utf16` | `"utf-16"` | UTF-16 code units (the default) | 1 | 2 |

`#[non_exhaustive]`: match it with a wildcard arm.

```rust
use lsp_lang::Encoding;

assert_eq!(Encoding::default(), Encoding::Utf16);
```

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Document, Encoding};

fn describe<G: Grammar>(doc: &Document<G>) -> &'static str {
    match doc.encoding() {
        Encoding::Utf8 => "byte columns",
        Encoding::Utf16 => "UTF-16 columns",
        _ => "other",
    }
}
```

## `Error`

```rust,ignore
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),
    Disconnected,
    InvalidHeader { reason: &'static str },
    TooLarge { len: usize, limit: usize },
    ExitWithoutShutdown,
    Json(serde_json::Error),
}

impl Display for Error;
impl std::error::Error for Error;   // source(): the wrapped Io or Json error
impl From<std::io::Error> for Error;
```

Something went wrong in the server itself, as opposed to a request that failed
(a [`ResponseError`](#responseerror)).

| Variant | Meaning | What to do |
|---|---|---|
| `Io(e)` | Reading from or writing to the client failed. | Exit; the connection is broken. |
| `Disconnected` | The input ended before `exit`, or partway through a message. | Exit; the editor is gone. |
| `InvalidHeader { reason }` | A header was malformed (no `Content-Length`, a bad length, an over-long or excessive header). The stream cannot be split into messages any more. | Exit. |
| `TooLarge { len, limit }` | A message announced a body over the [limit](#serverwith_message_limit). Its body was not read. | Exit, or raise the limit. |
| `ExitWithoutShutdown` | `exit` came without `shutdown`. | Exit with status 1, as the protocol asks. |
| `Json(e)` | A handler's value failed to serialize ([`Reply::new`](#replynew), [`Context::notify`](#contextnotify), [`Context::request`](#contextrequest)). Nothing was sent; the session goes on. | Fix the value's `Serialize`; in a request, `?` reports it as `INTERNAL_ERROR`. |

`#[non_exhaustive]`: match it with a wildcard arm.

```rust
use lsp_lang::Error;

fn exit_code(result: Result<(), Error>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(Error::ExitWithoutShutdown) => 1,
        Err(err) => {
            eprintln!("language server stopped: {err}");
            1
        }
    }
}
assert_eq!(exit_code(Ok(())), 0);
assert_eq!(exit_code(Err(Error::Disconnected)), 1);
```

```rust
use std::error::Error as _;
use lsp_lang::Error;

let err = Error::from(std::io::Error::other("pipe closed"));
assert!(err.to_string().contains("pipe closed"));
assert!(err.source().is_some());
```

## Re-exports

| Item | From | Why |
|---|---|---|
| `lsp_lang::incremental_lang` | [`incremental-lang`](https://docs.rs/incremental-lang) | The `Grammar`, `Builder`, `Token`, `Span`, and `Tree` a server's grammar is written with. |
| `lsp_lang::diag_lang` | [`diag-lang`](https://docs.rs/diag-lang) | `Label` and `Severity`, to build diagnostics. |
| `lsp_lang::Diagnostic` | `diag-lang` | What [`publish_diagnostics`](#contextpublish_diagnostics) takes. |
| `lsp_lang::serde_json` | [`serde_json`](https://docs.rs/serde_json) | JSON for capabilities, parameters, and results. |
| `lsp_lang::Value`, `lsp_lang::json!` | `serde_json` | What [`Handler::initialize`](#handlerinitialize) returns; a quick way to build any JSON. |

Re-exporting means a server needs only `lsp-lang` (plus `serde`, to derive)
and always gets the versions lsp-lang was built against.

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

As of `1.0.0` the public API is frozen. lsp-lang follows
[Semantic Versioning](https://semver.org/); within the `1.x` series:

- The **surface** will not change in a breaking way:
  [`Server`](#server) (`new`, `with_message_limit`, `handler`, `run`),
  [`Handler`](#handler) (`initialize`, `changed`, `closed`, `request`,
  `notification`, `response`, each with its default),
  [`Context`](#context) (`document`, `documents`, `notify`, `request`,
  `publish_diagnostics`, `clear_diagnostics`),
  [`Document`](#document) (`uri`, `version`, `text`, `tree`, `incremental`,
  `encoding`, `position`, `offset`, `range`, `span`),
  [`Json`](#json) (`from_raw`, `parse`, `as_str`), [`Reply`](#reply) (`new`,
  `null`, `as_str`), [`ResponseError`](#responseerror) (its constants, `new`,
  `code`, `message`, `From<Error>`), [`Position`](#position) and
  [`Range`](#range) (fields, `new`, serde shape), [`Encoding`](#encoding),
  [`Error`](#error), and the [re-exports](#re-exports). New items, new
  `Handler` methods (always with a default), and new methods on existing types
  are minor additions; a breaking change means a new major version.
- The **lifecycle** is fixed as described in [The session
  lifecycle](#the-session-lifecycle): which requests and notifications reach the
  handler in each phase, the error codes the server answers with, and the
  outcome of [`run`](#serverrun) for each way a session ends.
- The **capabilities the server owns** are fixed: it adds `positionEncoding`
  and sets `textDocumentSync.openClose` to `true` and `textDocumentSync.change`
  to incremental, keeping every other member the handler returns.
- **Encoding negotiation** is fixed: UTF-8 when the client offers it,
  otherwise UTF-16. Offering more encodings in a later release would only ever
  apply to clients that offer neither.
- **Position conversion** is fixed: the line breaks (`\n`, `\r\n`, lone `\r`),
  the clamping rules, and the snapping of a column inside a character to its
  start, as described in [Positions and encodings](#positions-and-encodings).
- The **diagnostics mapping** in [Diagnostics](#diagnostics-mapping) is fixed:
  which part of a `Diagnostic` becomes which protocol field, the severity
  mapping, and the order of the message's lines.
- **Document synchronization** is fixed: every edit of a `didChange` is applied
  in order before one [`changed`](#handlerchanged) call; an edit that cannot be
  applied drops the document, logs to the client, and calls
  [`closed`](#handlerclosed).
- **Ordering** is fixed: messages a handler sends go out in the order sent,
  before the response to the request being handled, all before the next
  incoming message is read. Ids from [`Context::request`](#contextrequest)
  count up from 0 in each session.
- `Encoding` and `Error` are `#[non_exhaustive]`, so variants can be added in a
  minor release; match them with a wildcard arm.
- `incremental-lang`, `diag-lang`, `serde_json`, and `serde` are **public
  dependencies**: their types appear in this API (`Grammar`, `Tree`, `Span`,
  `Diagnostic`, `Value`, the `Serialize`/`Deserialize` bounds). Moving to a new
  major version of any of them would be a major release here.
- MSRV (Rust 1.85) is a compatibility surface: raising it is a documented minor
  change, never a patch.

What is **not** promised: the wording of error messages, `window/logMessage`
text, and `Display` output; the `Debug` output of any type; the order of keys
and the whitespace in the JSON the server writes (it is valid JSON, but compare
values, not text); the default message limit; and performance figures — the
benchmarks are tracked, but they are measurements, not guarantees.

See [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and
[`../CHANGELOG.md`](../CHANGELOG.md).
