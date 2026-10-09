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
        <strong>lsp-lang</strong> is the Language Server Protocol layer of the <code>-lang</code> language-construction family. It owns everything a language server needs that is not about the language itself &mdash; JSON-RPC framing over any byte stream, the <code>initialize</code>/<code>shutdown</code>/<code>exit</code> lifecycle, a copy of every open file kept in step with the editor, position conversion between the protocol's UTF-16 columns and byte offsets, and the translation of compiler diagnostics into protocol diagnostics &mdash; so a server built on it is a grammar and a handler.
    </p>
    <p>
        Every open document is an <a href="https://crates.io/crates/incremental-lang"><code>incremental-lang</code></a> document: as the user types, only the syntax-tree node around each edit is reparsed, and the line index is patched rather than rebuilt, so the parsing work of a keystroke is proportional to the edit rather than the file. Diagnostics are <a href="https://crates.io/crates/diag-lang"><code>diag-lang</code></a> diagnostics, so whatever a lexer, parser, resolver, or type checker in the family reports can be published to the editor as-is. Messages are parsed in place and written straight into reused buffers; parameters stay raw JSON until the handler reads them into its own types.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>#![forbid(unsafe_code)]</code>. Built on <a href="https://crates.io/crates/incremental-lang"><code>incremental-lang</code></a>, <a href="https://crates.io/crates/diag-lang"><code>diag-lang</code></a>, and <a href="https://crates.io/crates/serde_json"><code>serde_json</code></a>.
    </p>
    <blockquote>
        <strong>1.0.0 is the API freeze.</strong> The public surface is stable and follows Semantic Versioning &mdash; no breaking changes before <code>2.0</code>. See <a href="./docs/API.md#stability"><code>docs/API.md</code></a> for the frozen-surface list, the protocol behaviour that is part of the contract, and the SemVer promise, and <a href="./CHANGELOG.md"><code>CHANGELOG.md</code></a>.
    </blockquote>
</div>

<hr>
<br>

## The model

A language plugs in twice; the server does the rest.

- A **[`Grammar`](https://docs.rs/incremental-lang)** (from `incremental-lang`) parses each open document, and declares which nodes may be reparsed on their own.
- A **[`Handler`](./docs/API.md#handler)** is the language server's behaviour. Every method has a default; a server implements only what its language supports.
- The **[`Server`](./docs/API.md#server)** runs the protocol over any `BufRead` and `Write` &mdash; stdin and stdout for an editor, a buffer in a test &mdash; and calls the handler.
- A **[`Context`](./docs/API.md#context)** is what the handler reaches while it runs: the open **[`Document`](./docs/API.md#document)**s, and a channel back to the client for notifications, requests, and diagnostics.
- **[`Json`](./docs/API.md#json)** parameters are parsed on demand into the handler's own types; results go back as a **[`Reply`](./docs/API.md#reply)** or a **[`ResponseError`](./docs/API.md#responseerror)**.

<br>

What the server owns, and what it hands to the handler:

| Message | Server | Handler |
|---|---|---|
| `initialize` | negotiates the position encoding; advertises document sync | [`initialize`](./docs/API.md#handlerinitialize) returns the language's capabilities |
| `textDocument/didOpen`, `didChange` | applies the text, reparses incrementally (falling back to a full parse), patches the line index; never drops a document on a failed change | [`changed`](./docs/API.md#handlerchanged) analyses and publishes diagnostics |
| `textDocument/didClose` | drops the document | [`closed`](./docs/API.md#handlerclosed) (default: clears its diagnostics) |
| any other request | answers protocol errors (before `initialize`, after `shutdown`) | [`request`](./docs/API.md#handlerrequest) returns the result |
| any other notification | drops it outside the session window | [`notification`](./docs/API.md#handlernotification) |
| an answer from the client | delivers it if it answers an outstanding request, once; logs anything else; cancels the oldest beyond 1024 outstanding | [`response`](./docs/API.md#handlerresponse) |
| `shutdown`, `exit` | ends the session | &mdash; |

<hr>
<br>

## Installation

```toml
[dependencies]
lsp-lang = "1"
```

Or from the terminal:

```bash
cargo add lsp-lang
```

`incremental_lang`, `diag_lang`, and `serde_json` are re-exported, so a server needs no other dependency to write its grammar, build diagnostics, or read parameters. Deriving `Deserialize` for parameter types needs `serde` with its `derive` feature.

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
    // A real server binary serves the editor:
    //     Server::new(Plain, Todos).run(std::io::stdin().lock(), std::io::stdout().lock())
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

### Answering requests

Requests arrive with their method name and raw parameters. Parse the parameters into a borrowing struct, convert positions with the document, and reply with any `Serialize` value. A `?` on a parse error returns `InvalidParams` to the client.

```rust
use lsp_lang::incremental_lang::Grammar;
use lsp_lang::{json, Context, Handler, Json, Position, Reply, ResponseError, Value};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HoverParams<'a> {
    #[serde(borrow)]
    text_document: TextDocument<'a>,
    position: Position,
}

#[derive(serde::Deserialize)]
struct TextDocument<'a> {
    uri: &'a str,
}

/// Hover shows the character under the cursor and its byte offset.
struct CharHover;

impl<G: Grammar> Handler<G> for CharHover {
    fn initialize(&mut self, _: Json<'_>, _: &mut Context<'_, G>) -> Result<Value, ResponseError> {
        Ok(json!({ "hoverProvider": true }))
    }

    fn request(&mut self, method: &str, params: Json<'_>, cx: &mut Context<'_, G>) -> Result<Reply, ResponseError> {
        match method {
            "textDocument/hover" => {
                let p: HoverParams<'_> = params.parse()?;
                let Some(doc) = cx.document(p.text_document.uri) else {
                    return Ok(Reply::null());
                };
                let at = doc.offset(p.position);
                let ch = doc.text()[at as usize..].chars().next();
                Ok(Reply::new(&json!({ "contents": format!("{ch:?} at byte {at}") }))?)
            }
            _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
        }
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
| [`Document::span`](./docs/API.md#documentspan) | `Range` → `Span` | a request's selection |
| [`Document::position`](./docs/API.md#documentposition) | byte offset → `Position` | a result's location |
| [`Document::range`](./docs/API.md#documentrange) | `Span` → `Range` | a tree node or a diagnostic |

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

## Examples

Each example plays a scripted editor session in memory and prints what the server sent back. They share a complete language server for a tiny Lisp &mdash; grammar, analysis, and handler &mdash; in [`examples/common`](./examples/common/mod.rs), written as a template for wiring in a real language.

| Example | Shows |
|---|---|
| [`server`](./examples/server.rs) | Live diagnostics (syntax errors, plus undefined and duplicate names resolved with `symbol-lang`), hover, and go-to-definition. Run with `-- --stdio` to serve a real editor. |
| [`encodings`](./examples/encodings.rs) | The same hover asked by a UTF-16 client and a UTF-8 client, and a column inside a surrogate pair snapping to its character. |
| [`client_requests`](./examples/client_requests.rs) | A server-to-client request (`workspace/configuration`), its answer through `Handler::response`, and republishing for every open document. |

```bash
cargo run --example server
cargo run --example server -- --stdio
cargo run --example encodings
cargo run --example client_requests
```

<hr>
<br>

## Performance

Measured end to end through `Server::run` on in-memory streams, with only the measured messages timed (`cargo bench`; x86_64, release profile). Documents are generated Lisp, about 34 bytes per line, with every tenth line a non-ASCII comment.

| Benchmark | What one iteration is | Windows | Linux (WSL2) |
|---|---|---:|---:|
| `keystroke/1000` | one `didChange` typing or deleting a character, 1,000-line document | ~2.3 µs | ~1.4 µs |
| `keystroke/10000` | the same, 10,000 lines (340 KB) | ~10 µs | ~6.4 µs |
| `keystroke/100000` | the same, 100,000 lines (3.4 MB) | ~137 µs | ~61 µs |
| `request/hover` | a request round trip: parse, two position conversions, reply | ~0.74 µs | ~0.44 µs |
| `publish/100` | a keystroke whose handler publishes 100 diagnostics | ~34 µs | ~26 µs |
| `publish/long_line_10000` | a keystroke whose handler publishes 10,000 diagnostics, in scattered order, on one 39 KB non-ASCII line | ~2.5 ms | ~1.7 ms |
| `open/10000` | `didOpen` of a 340 KB document: JSON, line index, full parse | ~3.1 ms | ~2.1 ms |

A keystroke's cost is dominated by `incremental-lang`'s reparse and its text splice, which grows with the size of the file because the text is one contiguous string; lsp-lang's own share &mdash; reading the message, converting the range, patching the line index &mdash; is a small fraction of it. What makes that so:

- **No value trees.** The JSON-RPC envelope is parsed in place: method names and URIs are borrowed from the message, and parameters stay raw JSON (`serde_json`'s `RawValue`) until the handler deserializes them into its own types. Results are serialized directly to text, and diagnostics are written field by field into the outgoing buffer.
- **Pooled buffers.** The incoming message buffer, the header scratch, and the outgoing body and frame buffers are allocated once per session and reused. All output for one incoming message goes out in a single `write_all` and `flush`, and a message that produces no output costs no system call.
- **An incremental line index.** An edit drops and rescans only the line starts whose two deciding bytes it touched, then shifts the rest by a constant in one vectorized pass. Converting a position is a binary search over line starts plus, for UTF-16, a branch-free count over the line prefix (or nothing at all for an ASCII prefix, or in a UTF-8 session).
- **Batched diagnostics.** All the spans of a `publish_diagnostics` call are converted in one ascending pass, so ten thousand diagnostics on one long line cost one scan of it, not ten thousand.
- **SIMD line scanning.** Line breaks are found with `memchr`.

<hr>
<br>

## Design notes

- **Synchronous and single-threaded.** One message at a time, handled to completion before the next is read. For an editor talking to one server over a pipe, this is the lowest-latency design: no locks, queues, or executor stand between the client and the handler. A handler with slow work can snapshot a document's tree (`O(1)`, `Send + Sync`) for a worker thread.
- **The server owns document sync.** It advertises incremental sync with open/close notifications, applies every change, and falls back to a full parse when an incremental reparse fails. A change it cannot apply at all (a line the document lacks, a version that does not increase, a grammar that fails on the whole text) never loses the document: it keeps its last good state, is marked out of step, and comes back on the next whole-text change or reopen, and the client is told why with `window/logMessage`.
- **Problems are reported, never panicked on.** Malformed JSON and invalid requests are answered with JSON-RPC errors; malformed notifications and stray answers from the client, which cannot be answered, are logged to the client. A panic in the grammar or the handler is contained where panics unwind: a request whose handler panicked is answered with `InternalError`, any other panic is logged, and the session carries on. Only a broken transport, broken framing, or the end of the session stops `run`.
- **Types the protocol needs, no more.** `Position` and `Range` are the crate's own; everything else a server sends or receives is the handler's own `Deserialize`/`Serialize` types or a `Value`, so the crate does not pin the server to one version of the full protocol type set.

<hr>
<br>

## Testing

```bash
cargo test
```

Unit tests cover each module; integration tests in [`tests/server.rs`](./tests/server.rs) drive the server through byte streams exactly as an editor would, covering the lifecycle, framing, every error response, document synchronization, diagnostics, and server-to-client requests. Property tests in [`tests/proptests.rs`](./tests/proptests.rs) check, for arbitrary edit sequences in both encodings, that the server's text always equals the client's, that the tree always equals a fresh parse, that positions round-trip, and that published diagnostics are well-formed. Every `rust` example in this README and in [`docs/API.md`](./docs/API.md) is compiled and run as a doctest.

<hr>
<br>

## Cross-platform support

The crate is pure Rust with no platform-specific code and no `unsafe`. It is tested on Linux, macOS, and Windows in CI, on stable and on the 1.85 MSRV.

<hr>
<br>

## Contributing

See [`REPS.md`](./REPS.md) for the engineering standards every change is held to, and [`dev/ROADMAP.md`](./dev/ROADMAP.md) for the plan to 1.0. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

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
