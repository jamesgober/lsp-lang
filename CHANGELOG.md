<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>lsp-lang</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

### Added

### Changed

### Fixed

### Security

---

## [2.0.0] - 2026-10-13

A major release. lsp-lang is rebuilt on the family's own stack: the transport is
[`rpc-io`](https://crates.io/crates/rpc-io) (JSON-RPC 2.0, the base protocol's
`Content-Length` framing, id correlation, `$/cancelRequest`) and the JSON is
[`jsonx-lib`](https://crates.io/crates/jsonx-lib). `serde`, `serde_json` (which
1.x re-exported in its public API), and `memchr` are gone: the crate now has no
third-party dependencies. Everything 1.x did is kept and ported (the
lifecycle, document synchronization, incremental sync through
`incremental-lang`, `diag-lang` diagnostics, the robustness fixes of 1.0.1),
and the typed features a language needs first are added: semantic tokens (full
and delta), hierarchical document symbols, go to definition, hover, and
completion, with `$/cancelRequest` that cancels.

### Breaking

- `serde_json` is no longer re-exported. `lsp_lang::serde_json`, and the
  `Value` and `json!` that came from it, are replaced by `lsp_lang::jsonx_lib`
  and its `Value<'a>` and `json!`. A `jsonx_lib::Value` borrows from the text it
  was parsed from; call `into_owned()` for a `Value<'static>`.
- `Handler::initialize` returns `Result<(), ResponseError>` instead of the
  capabilities, and takes `&Value<'_>` instead of `Json<'_>`. Capabilities are
  derived from the new `Handler::features`.
- `Handler::request`, `notification`, and `response` take and return
  `jsonx_lib` values: `request(method, &Value<'_>, cx) -> Result<Value<'static>,
  ResponseError>`, `notification(method, &Value<'_>, cx)`, and
  `response(id, Result<Value<'_>, ResponseError>, cx)`.
- `Handler::request` no longer receives every request. The typed requests go to
  their own methods (`hover`, `definition`, `completion`, `document_symbols`,
  `semantic_tokens`), and any other method must be registered with
  `Server::custom_request`; an unregistered method is `MethodNotFound`.
  `Handler::notification` receives the standard notifications and those
  registered with `Server::custom_notification`; other notifications are
  ignored.
- `Json` and `Reply` are removed. Parameters are read from a `&Value`, and
  results are typed values or a `Value<'static>`.
- `Position` and `Range` no longer implement `Serialize`/`Deserialize`; use
  `to_json()` and `from_json()`.
- `Context::notify` and `Context::request` take `impl Into<rpc_io::Json>` (a
  `json!(...)` value, or a `Raw`) instead of `&impl Serialize`. Params must be an
  array, an object, or `()`, as JSON-RPC requires.
- Ids of the server's own requests count from 1 (they counted from 0).
- `Server::run` takes `R: Read` instead of `R: BufRead`. A `BufRead` is a `Read`,
  so existing calls compile.
- `Error` loses `InvalidHeader`, `TooLarge`, and `Json`, and gains `Frame`,
  `Closed`, and `InvalidParams`. A message over the size limit no longer ends the
  session with `TooLarge`: it is skipped as it arrives and refused with
  `InvalidRequest` under a `null` id.
- JSON-RPC is applied as the specification has it. A batch is answered with an
  array (1.x refused every batch with `InvalidRequest`); scalar `params` is an
  `InvalidRequest`; an error answer from the client with no message or a
  non-integer code is an invalid response (`Handler::response` receives it as an
  `INVALID_REQUEST` error; 1.x read it leniently).
- Work is queued and run when the input has nothing more buffered, in order. A
  client's answer to a server request can no longer be handled before the
  `Handler` call that sent the request, which no real client can cause but a
  script that pipelines an answer can.
- After `shutdown`, a request for a method the server does not know is
  `MethodNotFound`, not `InvalidRequest`.

### Added

- `Handler::features`, `Features`: what a handler offers (`hover`, `definition`,
  `completion`, `document_symbols`, `semantic_tokens`, `capability`), from which
  the `initialize` result is built. Features may depend on what `initialize`
  learned.
- Typed handler methods `hover`, `definition`, `completion`, `document_symbols`,
  `semantic_tokens`, and the types they speak: `Hover`, `MarkupKind`, `Location`,
  `TextEdit`, `CompletionItem`, `CompletionItemKind`, `DocumentSymbol`,
  `SymbolKind`, `SemanticTokens`, `SemanticTokensLegend`, and
  `SemanticTokensBuilder`. Every type has hand-written `to_json` (and
  `from_json` where a handler reads it); there are no derives.
- Semantic tokens: the builder sorts, drops overlaps and empty tokens, converts
  byte spans to lines and columns in the session encoding (surrogate pairs
  count as two in UTF-16), and splits multi-line tokens; the server assigns a
  `resultId`, keeps the last answer per document, and answers
  `semanticTokens/full/delta` with a single token-aligned edit (a full answer for
  an unknown id).
- Document symbols are written and dropped iteratively, so an outline as deep as
  the source cannot overflow the stack; a selection range outside its range is
  clamped into it.
- `$/cancelRequest` cancels: a request received and not started is answered
  `RequestCancelled` once and never reaches the handler.
- `serve_stdio`, the one-call entry point; `Server::with_info` (`serverInfo`);
  `Server::custom_request` and `Server::custom_notification`.
- `Context::log` and `MessageType` (`window/logMessage`).
- `ResponseError::with_data` and `ResponseError::data`; conversions to and from
  `rpc_io::ErrorObject` and from `jsonx_lib::Error`.
- `Position::to_json`/`from_json` and `Range::to_json`/`from_json`.
- Re-exports of `jsonx_lib` and `rpc_io`.
- The server tells the client (`window/logMessage`) about an answer that is not
  a valid response, and about the client reporting that it could not read
  something the server sent.
- Tests: an in-memory-pipe conformance suite (messages split at every byte,
  pipelined requests, hang-ups, a conversation with server-to-client requests),
  feature tests (capabilities, outlines, semantic-token delta that applies back
  to the full answer, cancellation including a request cancelled while another
  runs), hostile-input tests (malformed JSON, wrong types, huge and 1,000,000-deep
  messages, floods, broken framing, random bytes), and properties (UTF-16 and
  UTF-8 position conversion including surrogate pairs and positions inside them,
  semantic tokens against a reference encoding, the whole-session text and
  tree invariants). Benchmarks for the typed features.

### Removed

- The dependencies `serde`, `serde_json`, and `memchr`. Line breaks are found by
  an eight-bytes-at-a-time scan in safe Rust (`src/lines.rs`) instead of
  `memchr`; the transport is `rpc-io`'s decoder instead of the hand-written
  framing reader.
- `Json`, `Reply`, `Error::InvalidHeader`, `Error::TooLarge`, `Error::Json`, and
  the `serde_json` re-exports.

### Changed

- Everything written in one turn (a message or a queued job) still reaches the
  client in one `write`; notifications the crate writes itself (diagnostics, log
  lines) are framed directly into the output buffer once the session is running,
  skipping the transport's validate-and-copy. Debug builds validate each one.
- Cargo description and keywords describe the typed features.
- `Handler` documentation lists the three groups of methods and when each is
  called.

- A request a handler sent during an `initialize` that then failed is no longer
  forgotten: it is never sent, and its outcome is delivered to
  `Handler::response` as `REQUEST_CANCELLED`, so a handler that tracks its
  requests is not left waiting.

### Migrating from 1.x

| 1.x | 2.0 |
|---|---|
| `use lsp_lang::{serde_json, Value, json}` | `use lsp_lang::{jsonx_lib, Value, json}`; `Value<'a>` borrows, `into_owned()` for `'static`; `value["a"]["b"].as_str()` reads, never panics |
| `fn initialize(&mut self, p: Json<'_>, cx) -> Result<Value, ResponseError>` returning `json!({ "hoverProvider": true })` | `fn initialize(&mut self, p: &Value<'_>, cx) -> Result<(), ResponseError>` plus `fn features(&self) -> Features { Features::new().hover() }`; other capabilities with `.capability("renameProvider", json!(true))` |
| `fn request(&mut self, method: &str, p: Json<'_>, cx)` with `match method { "textDocument/hover" => … }` | override `hover(&mut self, doc, position, cx) -> Result<Option<Hover>, ResponseError>` and list `.hover()`; the same for `definition`, `completion`, `document_symbols`, `semantic_tokens` |
| custom requests such as `"lsk/explain"` in `Handler::request` | `Server::new(g, h).custom_request("lsk/explain")`, answered by `request(&mut self, method, &Value<'_>, cx) -> Result<Value<'static>, ResponseError>` |
| `let p: MyParams = params.parse()?` with `#[derive(Deserialize)]` | `params["textDocument"]["uri"].as_str()`, or `Position::from_json(&params["position"])?`; a missing member is `Value::Null`, so check with `ok_or_else` |
| `Reply::new(&json!({ … }))?`, `Reply::null()` | return `Ok(json!({ … }))` / `Ok(Value::Null)` from `request`, or `Hover`, `Vec<Location>`, … from a typed method |
| `cx.notify("m", &json!({ … }))?` | `cx.notify("m", json!({ … }))?` |
| `cx.request("m", &json!({ … }))?` returning an id counted from 0 | `cx.request("m", json!({ … }))?`, ids count from 1 |
| `fn response(&mut self, id: u64, r: Result<Json<'_>, ResponseError>, cx)` | `fn response(&mut self, id: u64, r: Result<Value<'_>, ResponseError>, cx)`; `r.map(\|v\| v.to_string())` for the text |
| `serde_json::to_value(&position)`, `#[derive(Serialize)]` on a result containing `Position` | `position.to_json()`; build results with `json!` or the typed result types |
| `Error::TooLarge { len, limit }` to detect an oversize message | not an error any more: the message is refused with `InvalidRequest`; match `Error::Frame(_)` for broken framing |
| `Error::InvalidHeader { reason }` | `Error::Frame(rpc_io::FrameError)` |
| `Error::Json(e)` from `Reply::new` or `notify` | gone; `notify` and `request` return `Error::InvalidParams(kind)` for scalar params, `Error::Closed` after the session ended |
| `server.run(input, &mut out)` with `R: BufRead` | unchanged; `R` needs only `Read` |
| `ResponseError::new(code, message)` | unchanged; `.with_data(value)` is new |
| a client script that pipelines `initialize`, `initialized`, and an answer to a request the handler sends on `initialized` | send the answer after the request arrives (a real client always does); in a one-shot test script use the in-memory pipe pattern of `tests/support` |

---

## [1.0.1] - 2026-10-08

A robustness patch (ISSUES M74, M75). A failed edit no longer drops the
document, and a panic in a handler no longer ends the session. The public API
is unchanged; the behaviour below changes only where 1.0.0 was wrong.

### Changed

- A `didChange` that cannot be applied keeps the document instead of dropping
  it. If an edit's incremental reparse fails (a grammar panic or contract
  break in the reparsed fragment), the edit is applied again by parsing the
  whole new text, and a warning is logged. If the change cannot be applied at
  all (an edit names a line the document does not have, a ranged change's
  version does not increase, the grammar fails on the whole text, or the text
  would outgrow 4 GiB), the document keeps its last good text, tree, and
  version, is marked out of step, and the reason is logged; `changed` and
  `closed` are not called. A whole-text change or a reopen brings it back in
  step. A `didChange` is all or nothing: a failure partway restores the text
  it started from.
- A failed reopen keeps the open document (out of step) instead of dropping
  it and calling `Handler::closed`. A failed first open is remembered, so a
  whole-text change can still open the document.
- An edit whose range names a line more than one past the document's last is
  no longer clamped to the end of the text: it takes the document out of step.
  The line just past the last still means the end of the text, and columns
  past the end of a line are still clamped, as the protocol prescribes.
- Everything a handler sends during `initialize` is held back and written
  after the `initialize` response. If `initialize` fails, it is discarded, and
  requests among it are forgotten.
- At most 1024 of the server's own requests await an answer at once. Sending
  one more cancels the oldest: the server sends `$/cancelRequest` for it and
  delivers `RequestCancelled` to `Handler::response`. Settling an answer is
  O(log n) (was O(n) over an unbounded list).
- A grammar panic's message is included in the log line.

### Fixed

- A panic in `Handler::request`, `notification`, `changed`, `closed`,
  `response`, or `initialize` unwound out of `Server::run` and ended the
  session. Every handler call is now contained (where panics unwind): a
  request is answered with `InternalError` carrying the panic's message
  (clipped to 256 bytes), any other panic is logged as an error, and the
  session carries on.
- A failed edit dropped the document, so every later `didChange` was logged
  as "not open" and language features stopped for that file until the editor
  reopened it.
- A handler could send requests and notifications before the `initialize`
  response, which the protocol forbids.
- The server's outstanding requests were an unbounded list with a linear
  scan per answer.
- A request whose envelope could not be read (a `method` that is not a
  string, say) was answered under a `null` id even when its `id` was valid;
  it is now answered under that id.

---

## [1.0.0] - 2026-10-07

The API freeze. The public surface is unchanged from 0.2.0 and is now the
stable `1.x` contract, together with the protocol behaviour recorded in
`docs/API.md#stability`. A pre-freeze adversarial review found no panic on
client input and no way to desync a document; the defects it did find are
fixed below.

### Added

- `docs/API.md#stability`: the frozen surface, the protocol behaviour that is
  part of the contract (lifecycle, message classification, capabilities the
  server owns, encoding negotiation, position conversion, diagnostics mapping,
  document synchronization, ordering), the public dependencies, the MSRV, and
  what is not promised.
- The server tracks its own outstanding requests: each is answered at most
  once through `Handler::response`.
- A `publish/long_line_10000` benchmark, and tests for every fix below.

### Changed

- `Context::publish_diagnostics` converts all spans in one ascending pass
  before writing, so many diagnostics on one long line cost one scan of it.
- A panic in the grammar while opening or editing a document now drops that
  document (logged, `Handler::closed` called) instead of ending the session,
  where panics unwind.
- `null` members anywhere in `InitializeParams` read as absent.
- A message body's buffer reserves at most 1 MiB before the body arrives.
- Documentation states the cost of an edit precisely: its parsing work is
  proportional to the edit; the text `memmove` and line-start shift are
  linear and cheap.

### Fixed

- Publishing diagnostics on a long line in a UTF-16 session was quadratic in
  the number of diagnostics.
- A request with `"id": null` was delivered as a notification. Requests whose
  id is not a string or an integer are now refused with `InvalidRequest` under
  a `null` id and never dispatched or echoed.
- Malformed answers from the client (an `error` of `null`, an error without a
  message or with a non-integer code, an id with neither `result` nor `error`)
  were answered with error responses. The server no longer replies to answers:
  errors are read leniently and delivered, and anything unusable is logged.
- Answers reached `Handler::response` before `initialize`, and duplicate or
  unsolicited answers were delivered. Only answers to outstanding requests are
  delivered now.
- Client-supplied text quoted in log messages (ids, malformed errors) is
  clipped to 64 bytes.

---

## [0.2.0] - 2026-10-07

The core, and the hard part of the roadmap: the scaffold becomes a working
language-server framework. A server built on lsp-lang is a grammar and a
handler; the crate owns the protocol — framing, the lifecycle, document
synchronization with incremental reparsing, position conversion, and
diagnostics publishing.

### Added

- `Server` — the protocol engine: `new`, `with_message_limit`, `handler`, and
  `run`, which serves one session over any `BufRead` and `Write`. Reads
  `Content-Length`-framed JSON-RPC, enforces the `initialize`/`shutdown`/`exit`
  lifecycle (`ServerNotInitialized` before `initialize`, `InvalidRequest` after
  `shutdown`), and answers malformed JSON and shapeless messages with
  `ParseError` and `InvalidRequest` without ending the session.
- `Handler` — the trait a language implements, every method defaulted:
  `initialize` (capabilities, merged with the server's own), `changed` (after a
  document opens or changes), `closed` (default clears its diagnostics),
  `request` (default `MethodNotFound`), `notification`, and `response`.
- `Context` — a handler's view of the session: `document`, `documents`,
  `notify`, `request` (server-to-client requests with numbered ids, answers
  routed to `Handler::response`), `publish_diagnostics`, and
  `clear_diagnostics`.
- `Document` — an open file: `uri`, `version`, `text`, `tree`, `incremental`,
  `encoding`, and the conversions `position`, `offset`, `range`, and `span`.
  Each edit is applied to an `incremental_lang::Document`, reparsing only the
  node around it, and folded into a line index patched in place.
- Position encoding negotiation: UTF-8 when the client offers it, otherwise
  UTF-16. Line breaks are `\n`, `\r\n`, and lone `\r`; out-of-range positions
  are clamped as the protocol prescribes, and columns inside a character snap
  to its start.
- Diagnostics publishing from `diag_lang::Diagnostic`: primary label to range,
  severity mapped (Error, Warning, Information, Hint), primary label, notes,
  and help folded into the message, secondary labels to related information,
  and the document version attached — written to the wire directly.
- `Json` — incoming parameters and results kept as raw JSON and parsed on
  demand into the handler's own types (`from_raw`, `parse`, `as_str`), with
  mismatches reported as `InvalidParams`.
- `Reply` — pre-serialized request results (`new`, `null`, `as_str`).
- `ResponseError` — JSON-RPC errors with the protocol's error codes as
  constants, and a conversion from `Error` to `InternalError`.
- `Position`, `Range` — the protocol's coordinates, serializable.
- `Encoding` — `Utf8` and `Utf16`.
- `Error` — `Io`, `Disconnected`, `InvalidHeader`, `TooLarge`,
  `ExitWithoutShutdown`, and `Json`.
- Problems with notifications, which cannot be answered (malformed parameters,
  an edit that cannot be applied, a change to a document that is not open), are
  reported to the client with `window/logMessage`.
- Re-exports of `incremental_lang`, `diag_lang`, `serde_json`, `Diagnostic`,
  `Value`, and `json!`.
- Examples: `server` (a Lisp language server with live diagnostics, hover, and
  go-to-definition, names resolved with `symbol-lang`; `--stdio` serves a real
  editor), `encodings` (UTF-16 and UTF-8 clients side by side), and
  `client_requests` (`workspace/configuration` and its answer), sharing a
  complete language server in `examples/common`.
- Integration tests driving the server through byte streams, property tests
  holding the server's text, tree, and positions to an independent client-side
  model across arbitrary edit sequences in both encodings, and Criterion
  benchmarks for opening, keystrokes, diagnostics publishing, and request
  round trips.
- `README.md` and `docs/API.md` rust examples run as doctests.

### Changed

- Wired `incremental-lang` 1 and `diag-lang` 1; added `serde_json` (with
  `raw_value`), `serde`, and `memchr`. `symbol-lang` is deliberately not wired:
  its diagnostics publish through `Context::publish_diagnostics` with no
  adapter (see `dev/ROADMAP.md`).
- The crate is `std`-only. Removed the scaffold's `std` and `serde` features
  (serialization is the wire format, not an option) and the `loom`
  dev-dependency (the server is single-threaded by design).
- `Cargo.toml` description, keywords, and categories describe the crate.

### Fixed

- `Cargo.toml` did not parse: `keywords` and `categories` were unquoted.
- `clippy.toml` declared MSRV 1.87 against the crate's 1.85.
- `deny.toml` named another project.
- Byte-order marks removed from `dev/ROADMAP.md` and `docs/API.md`.
- The README linked a `dev/DIRECTIVES.md` that does not exist; standards are
  in `REPS.md`.

---

## [0.1.0] - 2026-06-18

Initial scaffold and repository bootstrap. No domain logic yet &mdash; this release establishes the structure, tooling, and quality gates the implementation will be built on.

### Added

- `Cargo.toml` with crate metadata, Rust 2024 edition, MSRV 1.85.
- Dual `Apache-2.0 OR MIT` license files.
- `README.md`, `CHANGELOG.md`, and a documentation skeleton.
- `REPS.md` compliance baseline.
- `.github/workflows/ci.yml` CI matrix; `deny.toml`, `clippy.toml`, `rustfmt.toml`.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md` (committed engineering standards + plan).

[Unreleased]: https://github.com/jamesgober/lsp-lang/compare/v2.0.0...HEAD
[2.0.0]: https://github.com/jamesgober/lsp-lang/compare/v1.0.1...v2.0.0
[1.0.1]: https://github.com/jamesgober/lsp-lang/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/jamesgober/lsp-lang/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/lsp-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/lsp-lang/releases/tag/v0.1.0
