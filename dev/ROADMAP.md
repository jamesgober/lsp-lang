# lsp-lang - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Master plan: ../../_strategy/LANG_COLLECTION.md
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt.

## v0.2.0 - Core (THE HARD PART, NOT DEFERRED) (DONE)
Language Server Protocol scaffolding for editor integration.
Dependencies (wires incremental, symbol, diag) are wired here, when first used.
Exit criteria:
- [x] Every public item has rustdoc + a runnable example.
- [x] Core invariants property-tested (full DIRECTIVES + API authored at this stage).

Delivered 2026-10-07: `Server`, `Handler`, `Context`, `Document`, `Json`,
`Reply`, `ResponseError`, `Position`, `Range`, `Encoding`, `Error`. The server
speaks the base protocol over any `BufRead`/`Write`, runs the
`initialize`/`shutdown`/`exit` lifecycle (refusing traffic outside the session
window with the codes the protocol prescribes), negotiates the position
encoding (UTF-8 when offered, else UTF-16), and owns document synchronization:
every open file is an incremental document with a line index patched in place
on each edit. Handlers see parameters as raw JSON parsed on demand into their
own types, answer with pre-serialized replies, and publish `diag-lang`
diagnostics, which are written to the wire field by field. The invariants —
the server's text always equals the client's after any edit sequence in
either encoding, the tree always equals a fresh parse, positions round-trip,
published diagnostics are well-formed — are property-tested against an
independent client-side model.

Scaffold defects fixed on the way in: unquoted `keywords` / `categories` in
`Cargo.toml` (the manifest did not parse), clippy MSRV `1.87` → `1.85`, a
stale project name in `deny.toml`, byte-order marks in `dev/ROADMAP.md` and
`docs/API.md`, and a README link to a `dev/DIRECTIVES.md` that does not exist.
The engineering standards are `REPS.md`; the API is authored in `docs/API.md`.

Dependency wiring, recorded under the anti-deferral rule:

- **`incremental-lang` — wired.** Every open document is an
  `incremental_lang::Document`, so a keystroke reparses only the node around
  it, and its `Grammar` is how a language plugs in. Re-exported so versions
  always match.
- **`diag-lang` — wired.** `Context::publish_diagnostics` takes
  `diag_lang::Diagnostic` and writes protocol diagnostics directly: primary
  label to range, severity mapped, notes and help folded into the message,
  secondary labels to related information. Any family stage that reports
  through `diag-lang` publishes as-is.
- **`symbol-lang` — not wired, by design.** Its `SymbolTable` is a scope
  stack a resolver threads through its own walk of the tree; it keeps no
  record a language server could query afterwards, and the protocol layer
  works in positions, ranges, and diagnostics rather than scopes. Where the
  two meet is the diagnostics `symbol-lang` produces (`unresolved_name`,
  `duplicate_definition`), which are `diag-lang` diagnostics and publish
  through `Context::publish_diagnostics` with no adapter — the `server`
  example resolves names with `symbol-lang` and does exactly that (it is a
  dev-dependency). Wiring it would add `symbol-lang` and `intern-lang` to every
  server for no API. This is a design decision, not a deferral.
- **`serde_json`, `serde`, `memchr` — added.** The protocol is JSON-RPC.
  `serde_json`'s `RawValue` lets the envelope be parsed in place with
  parameters left as raw JSON until a handler deserializes them into its own
  types, so no value tree is ever built; `serde` derives the private borrowing
  deserializers for the messages the server reads itself; `memchr` finds line
  breaks with SIMD and is already in the graph through `serde_json`. All three
  are mature, widely audited, and MSRV-compatible.
- **The scaffold's `std` and `serde` features and the `loom` dev-dependency
  — removed.** The crate's job is I/O over `std::io` streams, so it is
  `std`-only; serialization is the wire format, not an option. The server is
  single-threaded by design (one message at a time, the lowest-latency model
  for one client on a pipe), so there is no concurrent code for `loom` to
  check.

## v1.0.0 - API freeze (DONE)
Public surface stable and frozen until 2.0.
- [x] docs/API.md marked stable; SemVer promise recorded.
- [x] Full test + benchmark suite green on all three platforms.

Shipped 2026-10-07. Before freezing, an adversarial review attacked the server
with hostile framing, deeply nested and malformed JSON, invalid ids, edit
sequences built to desync the line index, panicking grammars, and large
inputs. It found no panic on client input and no way to desync a document,
and these defects, all fixed:

- Publishing many diagnostics on one long line in a UTF-16 session was
  quadratic: each span counted its line from the start. Spans are now
  converted in one ascending batch, so a line is counted once (10,000
  diagnostics on a 39 KB line: ~2.5 ms per keystroke).
- A request with `"id": null` was treated as a notification — a `shutdown`
  with a null id was delivered to the handler. Requests whose id is not a
  string or an integer are now refused with `InvalidRequest` under a `null`
  id, never dispatched or echoed.
- Malformed answers from the client (an `error` of `null`, an error object
  without a message or with a non-integer code, an id with neither `result`
  nor `error`) were answered with error responses, which the client would read
  as errors for its own requests. The server now never replies to an answer:
  it reads the error leniently and delivers it, or logs and drops it.
- Answers reached `Handler::response` outside the session window, including
  before `initialize`. The server now tracks which of its requests are
  outstanding and delivers exactly one answer to each; anything else is logged.
- A panicking grammar ended the session. The server now catches the panic
  (where panics unwind), drops the document, logs, and calls `closed`, as
  `incremental-lang`'s transactional edits allow.
- Smaller: `null` members in `InitializeParams` were rejected; a client that
  announced a large body made the server reserve all of it up front (now
  capped at 1 MiB until the bytes arrive); and the docs overstated a keystroke
  as proportional to the edit — its parsing is, but the text `memmove` and the
  line-start shift are linear (and cheap).

The public API is unchanged from 0.2.0. The surface, the lifecycle, the
encoding negotiation, the position-conversion and diagnostics-mapping rules,
document synchronization, message ordering, the public dependencies, and MSRV
1.85 are recorded as the contract in `docs/API.md#stability`. Tests and
benchmarks green on Windows and Linux (WSL2) locally; macOS through the CI
matrix.

## v1.0.1 - Robustness patch (DONE)
Fixes ISSUES M74 and M75 in `lang-collection/_lexersketch/ISSUES.md`. No
public API added or removed.
- [x] A failed `didChange` never loses the document; recovery documented.
- [x] Every handler call contained; `initialize` output ordered; outstanding
      requests bounded; malformed requests answered under their own id.
- [x] Tests for each, including a handler that panics in each entry point.

Delivered 2026-10-08:

- **M74, a failed edit dropped the document.** Edits now take the
  incremental path first and, if the grammar fails there, are applied again
  by splicing the text and parsing it whole (`Document::rebuild`, private).
  A change that cannot be applied at all leaves the document at its last
  good state, marked out of step (`Document::is_stale`, private), and logged:
  a warning for drift (a line the document lacks; a ranged change whose
  version does not increase; a 4 GiB overflow), an error for a grammar that
  fails on the whole text. A `didChange` is all or nothing (the starting text
  is kept when it has several edits, and restored on failure). LSP has no
  request for the client's text, so recovery is the next whole-text change
  (applied in any state, any version) or a reopen; ranged changes to an
  out-of-step document are ignored and logged meanwhile. A failed reopen keeps
  the open document; a failed first open is remembered so a whole-text change
  can open it.
- **M75, handler panics killed the session.** Every handler call runs under
  `catch_unwind`: a request whose handler panicked is answered with
  `InternalError` and the panic message; any other panic is logged. Output a
  handler sends during `initialize` is held back until after the response, or
  discarded if it fails. The server's outstanding requests are a `BTreeSet`
  capped at 1024: the oldest is cancelled (`$/cancelRequest` to the client,
  `RequestCancelled` to `Handler::response` after the call returns), and
  settling is O(log n). A request whose envelope does not parse is answered
  under its own id when it has a `method` member and a valid id.
- **Behaviour revised in the contract.** `docs/API.md#stability` recorded the
  drop-on-failure rule and handler-first ordering during `initialize`; both
  were the defects fixed here, and the stability section now records the
  1.0.1 behaviour. Line numbers more than one past the last no longer clamp in
  edits (they cannot be placed); position conversion through the public
  `Document` methods is unchanged.

Dependency wiring: unchanged. `incremental-lang` 1 (published, 1.0.0 on
crates.io), `diag-lang` 1, `serde_json` 1, `serde` 1, `memchr` 2.

Additive 1.x candidates (not commitments):

- `Server::with_info(name, version)`, to send `serverInfo` in the `initialize`
  result.
- `data` on `ResponseError`, for clients that act on structured error details.
- Documents for files the client has not opened (a constructor taking a URI and
  text), so cross-file results — a definition in another file — carry correct
  ranges.
- Deferred replies: answering a request after its handler returns (from a
  worker thread), which would make `$/cancelRequest` meaningful.
- UTF-32 position encoding, for clients that offer it without UTF-8.
- `Document::is_in_step()` (public), so a handler can tell a document kept at
  its last good state from one that matches the client, and say so in hover
  or diagnostics. 1.0.1 tracks this privately; exposing it is additive.
