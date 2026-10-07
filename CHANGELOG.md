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

[Unreleased]: https://github.com/jamesgober/lsp-lang/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/lsp-lang/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/lsp-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/lsp-lang/releases/tag/v0.1.0
