<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>lsp-lang</b>
    <br>
    <sub><sup>LSP SCAFFOLDING</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/lsp-lang"><img alt="Crates.io" src="https://img.shields.io/crates/v/lsp-lang"></a>
    <a href="https://crates.io/crates/lsp-lang"><img alt="Downloads" src="https://img.shields.io/crates/d/lsp-lang?color=%230099ff"></a>
    <a href="https://docs.rs/lsp-lang"><img alt="docs.rs" src="https://img.shields.io/docsrs/lsp-lang"></a>
    <a href="https://github.com/jamesgober/lsp-lang/actions"><img alt="CI" src="https://github.com/jamesgober/lsp-lang/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>lsp-lang</strong> is the Language Server Protocol layer of the <code>-lang</code> language-construction family. It owns everything a language server needs that is not about the language itself &mdash; JSON-RPC over the base protocol's framing on any byte stream, the <code>initialize</code>/<code>shutdown</code>/<code>exit</code> lifecycle, a copy of every open file kept in step with the editor, position conversion between the protocol's UTF-16 columns and byte offsets, <code>$/cancelRequest</code>, and the translation of compiler diagnostics into protocol diagnostics &mdash; so a server built on it is a grammar and a handler. Typed helpers cover the features a language needs first: semantic tokens (full and delta), hierarchical document symbols, go to definition, hover, and completion.
    </p>
    <p>
        Every open document is an <a href="https://crates.io/crates/incremental-lang"><code>incremental-lang</code></a> document: as the user types, only the syntax-tree node around each edit is reparsed, and the line index is patched rather than rebuilt, so the parsing work of a keystroke is proportional to the edit rather than the file. Diagnostics are <a href="https://crates.io/crates/diag-lang"><code>diag-lang</code></a> diagnostics, so whatever a lexer, parser, resolver, or type checker in the family reports can be published to the editor as-is. The transport is <a href="https://crates.io/crates/rpc-io"><code>rpc-io</code></a> and the JSON is <a href="https://crates.io/crates/jsonx-lib"><code>jsonx-lib</code></a>: there are no third-party dependencies.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>#![forbid(unsafe_code)]</code>. Zero third-party runtime dependencies: <a href="https://crates.io/crates/incremental-lang"><code>incremental-lang</code></a>, <a href="https://crates.io/crates/diag-lang"><code>diag-lang</code></a>, <a href="https://crates.io/crates/rpc-io"><code>rpc-io</code></a>, and <a href="https://crates.io/crates/jsonx-lib"><code>jsonx-lib</code></a>.
    </p>
    <blockquote>
        <strong>2.0.0 is a major release.</strong> 1.x depended on <code>serde</code>, <code>serde_json</code> (re-exported in its API), and <code>memchr</code>; 2.0 is rebuilt on the family's own stack. The API changed accordingly: see the <a href="./CHANGELOG.md#migrating-from-1x">migration table</a>. The 2.0 surface is frozen and follows Semantic Versioning (<a href="./docs/API.md#stability"><code>docs/API.md</code></a>), with the limits of that promise stated there: <code>rpc-io</code> and <code>jsonx-lib</code> are still at 0.2, and their types appear in this API.
    </blockquote>
</div>

<hr>
<br>

## The model

A language plugs in twice; the server does the rest.

- A **[`Grammar`](https://docs.rs/incremental-lang)** (from `incremental-lang`) parses each open document, and declares which nodes may be reparsed on their own.
- A **[`Handler`](./docs/API.md#handler)** is the language server's behaviour. Every method has a default; a server implements only what its language supports, and lists what it offers in **[`Features`](./docs/API.md#features)**, from which the server's capabilities are advertised.
- The **[`Server`](./docs/API.md#server)** runs the protocol over any `Read` and `Write` &mdash; stdin and stdout for an editor (`serve_stdio`), a buffer in a test &mdash; and calls the handler.
- A **[`Context`](./docs/API.md#context)** is what the handler reaches while it runs: the open **[`Document`](./docs/API.md#document)**s, and a channel back to the client for notifications, requests, log messages, and diagnostics.
- Parameters are **[`Value`](./docs/API.md#re-exports)**s from `jsonx-lib`; results are typed (`Hover`, `Location`, `CompletionItem`, `DocumentSymbol`, `SemanticTokens`) or a `Value`, and a failure is a **[`ResponseError`](./docs/API.md#responseerror)**.

<br>

What the server owns, and what it hands to the handler:

| Message | Server | Handler |
|---|---|---|
| `initialize` | negotiates the position encoding; advertises document sync and the listed features | [`initialize`](./docs/API.md#handlerinitialize) reads the client's options; [`features`](./docs/API.md#handlerfeatures) says what is offered |
| `textDocument/didOpen`, `didChange` | applies the text, reparses incrementally (falling back to a full parse), patches the line index; never drops a document on a failed change | [`changed`](./docs/API.md#handlerchanged) analyses and publishes diagnostics |
| `textDocument/didClose` | drops the document | [`closed`](./docs/API.md#handlerclosed) (default: clears its diagnostics) |
| `hover`, `definition`, `completion`, `documentSymbol`, `semanticTokens/full` and `/delta` | reads the request, finds the document, writes the answer; the delta is computed from the previous answer | [`hover`](./docs/API.md#handlerhover), [`definition`](./docs/API.md#handlerdefinition), [`completion`](./docs/API.md#handlercompletion), [`document_symbols`](./docs/API.md#handlerdocument_symbols), [`semantic_tokens`](./docs/API.md#handlersemantic_tokens) return typed results |
| `$/cancelRequest` | answers a request that has not started with `RequestCancelled`; the handler never sees it | &mdash; |
| a registered custom request | answers protocol errors (before `initialize`, after `shutdown`) | [`request`](./docs/API.md#handlerrequest) returns the result |
| any other notification | drops it outside the session window | [`notification`](./docs/API.md#handlernotification) |
| an answer from the client | delivers it if it answers an outstanding request, once; logs anything else; cancels the oldest beyond 1024 outstanding | [`response`](./docs/API.md#handlerresponse) |
| `shutdown`, `exit` | ends the session | &mdash; |

<hr>
<br>

## Installation

```toml
[dependencies]
lsp-lang = "2"
```

Or from the terminal:

```bash
cargo add lsp-lang
```

`incremental_lang`, `diag_lang`, `jsonx_lib`, and `rpc_io` are re-exported, so a server needs no other dependency to write its grammar, build diagnostics, read parameters, or name a transport type.

<hr>
<br>

## Quick start

A complete server that warns about every `TODO` in a document. The grammar treats a whole document as one token; a real language supplies its own.

```rust
use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::{Context, Diagnostic, Document, Handler, Server};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K { Root, Text }
impl TokenKind for K {}

#[derive(Clone)]
struct Plain;

impl Grammar for Plain {
    type Kind = K;
    const ROOT: K = K::Root;
    fn lex(&self, text: &str, tokens: &mut Vec<Token<K>>) {
        if !text.is_empty() {
            tokens.push(Token::new(K::Text, Span::new(0, text.len() as u32)));
        }
    }
    fn parse(&self, _: K, b: &mut Builder<'_, K>) {
        b.start(K::Root);
        while !b.at_end() { b.bump(); }
        b.finish();
    }
}

struct Todos;

impl Handler<Plain> for Todos {
    // Called after a document opens and after every change to it.
    fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
        let found: Vec<Diagnostic> = doc
            .text()
            .match_indices("TODO")
            .map(|(at, _)| {
                let span = Span::new(at as u32, at as u32 + 4);
                Diagnostic::new(Severity::Warning, "unfinished work", Label::new(span, "here"))
            })
            .collect();
        cx.publish_diagnostics(doc, &found);
    }
}

fn frame(json: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{json}", json.len())
}

fn main() -> Result<(), lsp_lang::Error> {
    // A real server binary serves the editor with one call:
    //     lsp_lang::serve_stdio(Plain, Todos)
    // Here, a scripted session plays the editor's part.
    let input = [
        frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
        frame(r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":
            {"uri":"file:///notes.txt","languageId":"text","version":1,"text":"ship it\nTODO: tests"}}}"#),
        frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
        frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    ]
    .concat();

    let mut output = Vec::new();
    Server::new(Plain, Todos).run(input.as_bytes(), &mut output)?;

    let output = String::from_utf8_lossy(&output);
    assert!(output.contains(r#""range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}}"#));
    Ok(())
}
```

<br>

### Offering features

List what the handler offers in `features`, and override the matching methods. The server advertises exactly the listed capabilities, reads each request, finds the document, calls the method, and writes the answer; positions arrive in the session encoding, and `Document::offset` turns one into a byte offset.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{Context, Document, Features, Handler, Hover, Position, ResponseError};

/// Hover shows the character under the cursor and its byte offset.
struct CharHover;

impl<G: Grammar> Handler<G> for CharHover {
    fn features(&self) -> Features {
        Features::new().hover()
    }

    fn hover(&mut self, doc: &Document<G>, at: Position, _: &mut Context<'_, G>)
        -> Result<Option<Hover>, ResponseError>
    {
        let offset = doc.offset(at);
        let ch = doc.text()[offset as usize..].chars().next();
        Ok(ch.map(|ch| Hover::markdown(format!("`{ch}` at byte {offset}"))))
    }
}
```

### Semantic tokens

A `SemanticTokensBuilder` turns byte spans from the syntax tree into the protocol's delta-encoded integers: it sorts, drops overlaps, converts to lines and columns in the session encoding (a surrogate pair counts as two in UTF-16), and splits a token that crosses a line break. The server keeps the last answer per document and answers a `full/delta` request with a single token-aligned edit.

```rust
use lsp_lang::incremental_lang::{Grammar, Span};
use lsp_lang::{Context, Document, Features, Handler, ResponseError};
use lsp_lang::{SemanticTokens, SemanticTokensBuilder, SemanticTokensLegend};

struct Keywords(SemanticTokensLegend);

impl<G: Grammar> Handler<G> for Keywords {
    fn features(&self) -> Features {
        Features::new().semantic_tokens(self.0.clone())
    }

    fn semantic_tokens(&mut self, doc: &Document<G>, _: &mut Context<'_, G>)
        -> Result<SemanticTokens, ResponseError>
    {
        let keyword = self.0.token_type("keyword").unwrap_or(0);
        let mut tokens = SemanticTokensBuilder::new(doc);
        for (at, word) in doc.text().match_indices("fn") {
            tokens.push(Span::new(at as u32, (at + word.len()) as u32), keyword, 0);
        }
        Ok(tokens.build())
    }
}
```

<hr>
<br>

## Positions

The protocol counts columns in UTF-16 code units unless the client offers something else; the family counts byte offsets. A `Document` converts between them in the encoding negotiated at `initialize` &mdash; UTF-8 whenever the client offers it, because byte columns convert without scanning, and UTF-16 otherwise.

| Method | Converts | Use it for |
|---|---|---|
| [`Document::offset`](./docs/API.md#documentoffset) | `Position` → byte offset | a request's cursor, to look up in the tree |
| [`Document::span`](./docs/API.md#document) | `Range` → `Span` | a request's selection |
| [`Document::position`](./docs/API.md#documentposition) | byte offset → `Position` | a result's location |
| [`Document::range`](./docs/API.md#document) | `Span` → `Range` | a tree node or a diagnostic |

Line breaks are `\n`, `\r\n`, and a lone `\r`, as the protocol specifies. Out-of-range input is clamped the way the protocol prescribes &mdash; a column past the end of its line means the end of the line &mdash; and a column inside a character (between the halves of a surrogate pair) snaps to that character's start. Conversions never fail and never panic.

<hr>
<br>

## Diagnostics

`Context::publish_diagnostics` takes `diag_lang::Diagnostic`s whose spans are byte offsets into the document &mdash; what the syntax tree's spans already are &mdash; and writes `textDocument/publishDiagnostics` directly, with the document's version:

| diag-lang | Protocol |
|---|---|
| primary label's span | `range` |
| `Error`, `Warning`, `Note`, `Help` | `severity` 1 (Error), 2 (Warning), 3 (Information), 4 (Hint) |
| message, primary label, notes, help | `message`: the headline first, then one line each (`note: …`, `help: …`) |
| secondary labels | `relatedInformation`, located in the same document |

Diagnostics from `symbol-lang`'s `unresolved_name` and `duplicate_definition`, or from any other family crate that reports through `diag-lang`, publish with no adapter; the `server` example does exactly that.

<hr>
<br>

## Cancellation

`$/cancelRequest` cancels a request that has been received and not started. The server queues what it receives and runs the queue when its input has nothing more buffered, so a cancellation that arrives with the request it names, or behind it in the same read (the usual case for a request waiting behind slow work), is applied first: the client gets `RequestCancelled` once, and the handler is never called. A handler that is already running is not interrupted &mdash; the server is single-threaded &mdash; so slow work should be kept in short calls or moved to a worker thread from a snapshot of the tree. [`docs/API.md`](./docs/API.md#cancellation) states the rules.

<hr>
<br>

## Examples

Each example plays a scripted editor session in memory and prints what the server sent back. They share a complete language server for a tiny Lisp &mdash; grammar, analysis, and handler &mdash; in [`examples/common`](./examples/common/mod.rs), written as a template for wiring in a real language.

| Example | Shows |
|---|---|
| [`server`](./examples/server.rs) | Live diagnostics (syntax errors, plus undefined and duplicate names resolved with `symbol-lang`), hover, go-to-definition, completion, the outline, and semantic tokens. Run with `-- --stdio` to serve a real editor. |
| [`encodings`](./examples/encodings.rs) | The same hover asked by a UTF-16 client and a UTF-8 client, and a column inside a surrogate pair snapping to its character. |
| [`client_requests`](./examples/client_requests.rs) | A server-to-client request (`workspace/configuration`) answered by the editor over in-memory channels, its answer through `Handler::response`, and republishing for every open document. |

```bash
cargo run --example server
cargo run --example server -- --stdio
cargo run --example encodings
cargo run --example client_requests
```

<hr>
<br>

## Performance

Measured end to end through `Server::run` on in-memory streams, with only the measured messages timed (`cargo bench`; Windows 11, x86_64, release profile, one run). Documents are generated Lisp, about 34 bytes per line, with every tenth line a non-ASCII comment. The feature benchmarks run the example Lisp server over a 10,000-line document.

| Benchmark | What one iteration is | Time |
|---|---|---:|
| `keystroke/1000` | one `didChange` typing or deleting a character, 1,000-line document | ~2.8 µs |
| `keystroke/10000` | the same, 10,000 lines (340 KB) | ~8.2 µs |
| `keystroke/100000` | the same, 100,000 lines (3.4 MB) | ~116 µs |
| `request/hover` | a request round trip: queue, read, two position conversions, reply | ~1.1 µs |
| `publish/100` | a keystroke whose handler publishes 100 diagnostics | ~22 µs |
| `publish/long_line_10000` | a keystroke whose handler publishes 10,000 diagnostics, in scattered order, on one 39 KB non-ASCII line | ~1.9 ms |
| `open/10000` | `didOpen` of a 340 KB document: JSON, line index, full parse, analysis not included | ~3.0 ms |
| `request/semantic_tokens` | `semanticTokens/full` for 10,000 lines (about 40,000 tokens): build, encode, write | ~5.8 ms |
| `request/document_symbols` | `documentSymbol` for 10,000 lines (about 9,000 symbols) | ~6.2 ms |
| `request/completion` | `completion` with about 9,000 items | ~1.9 ms |

There is no controlled comparison against 1.x on one machine in this table; the 1.x README quoted a different run. A keystroke's cost is dominated by `incremental-lang`'s reparse and its text splice, which grows with the size of the file because the text is one contiguous string; lsp-lang's own share is a small fraction of it. What keeps it small:

- **No value trees.** The JSON-RPC envelope is decoded in place by `rpc-io` and `jsonx-lib`; parameters stay slices until read. Results are written straight to text: diagnostics, semantic tokens, outlines, and completion lists are never a `Value`.
- **One write per turn.** Everything sent during one message or one queued job is written with a single `write_all` and `flush`, and a turn that sends nothing costs no system call. Notifications the crate writes itself skip the transport's validate-and-copy (debug builds still validate every one).
- **An incremental line index.** An edit drops and rescans only the line starts whose two deciding bytes it touched, then shifts the rest by a constant in one pass. Line breaks are found eight bytes at a time in plain safe Rust. Converting a position is a binary search over line starts plus, for UTF-16, a branch-free count over the line prefix (or nothing at all for an ASCII prefix, or in a UTF-8 session).
- **Batched diagnostics.** All the spans of a `publish_diagnostics` call are converted in one ascending pass, so ten thousand diagnostics on one long line cost one scan of it, not ten thousand.

<hr>
<br>

## Design notes

- **Synchronous and single-threaded.** The server decodes every complete message it holds, runs the work they queued in arrival order, and only then reads again. For an editor talking to one server over a pipe this is the lowest-latency design: no locks or executor stand between the client and the handler, and the gap between receiving a request and running it is what lets a cancellation arrive first. The cost is that a running handler cannot be interrupted. A handler with slow work can snapshot a document's tree (`O(1)`, `Send + Sync`) for a worker thread.
- **The server owns document sync.** It advertises incremental sync with open/close notifications, applies every change, and falls back to a full parse when an incremental reparse fails. A change it cannot apply at all (a line the document lacks, a version that does not increase, a grammar that fails on the whole text) never loses the document: it keeps its last good state, is marked out of step, and comes back on the next whole-text change or reopen, and the client is told why with `window/logMessage`.
- **Problems are reported, never panicked on.** Malformed JSON and invalid requests are answered with JSON-RPC errors; malformed notifications and stray answers from the client, which cannot be answered, are logged to the client. A panic in the grammar or the handler is contained where panics unwind: a request whose handler panicked is answered with `InternalError`, any other panic is logged, and the session carries on. Only a broken transport, broken framing, or the end of the session stops `run`.
- **Capabilities are a declaration.** `Handler::features` lists what the handler offers, and the capabilities follow from it. Rust cannot tell which trait methods an implementation overrode, so the list is explicit: a feature listed without its method answers `MethodNotFound`, and a method overridden but not listed is never called.
- **Hand-written JSON, no derives.** Every protocol type writes itself with straight-line code and reads itself with checked accessors that say what was wrong; there is no reflection and no macro.
- **Types the protocol needs, no more.** The five features and their results are typed; everything else is a `Value` through `Handler::request`, so the crate does not pin the server to one version of the full protocol type set.

<hr>
<br>

## Testing

```bash
cargo test
```

Unit tests cover each module. Integration tests drive the server as an editor would: [`tests/server.rs`](./tests/server.rs) through byte streams (the lifecycle, framing, every error response, document synchronization, diagnostics, server-to-client requests, panics), [`tests/conformance.rs`](./tests/conformance.rs) as a conversation over in-memory pipes (messages split at every byte, pipelined requests, hang-ups), [`tests/features.rs`](./tests/features.rs) for the typed features and cancellation, and [`tests/hostile.rs`](./tests/hostile.rs) for malformed JSON, wrong types, huge and deeply nested messages, floods, and broken framing (nothing panics; every request is answered once). Property tests in [`tests/proptests.rs`](./tests/proptests.rs) check, for arbitrary edit sequences in both encodings, that the server's text always equals the client's, that the tree always equals a fresh parse, that positions round-trip with surrogate pairs, that semantic tokens decode to a reference encoding, and that published diagnostics are well-formed. Every `rust` example in this README and in [`docs/API.md`](./docs/API.md) is compiled and run as a doctest.

<hr>
<br>

## Cross-platform support

The crate is pure Rust with no platform-specific code and no `unsafe`. It is tested on Linux, macOS, and Windows in CI, on stable and on the 1.85 MSRV.

<hr>
<br>

## Contributing

See [`REPS.md`](./REPS.md) for the engineering standards every change is held to, and [`dev/ROADMAP.md`](./dev/ROADMAP.md) for the roadmap. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober <me@jamesgober.com>.</strong></sup>
</div>
