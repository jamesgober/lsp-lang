//! Benchmarks, measured end to end through `Server::run` on in-memory streams,
//! as an editor would drive it:
//!
//! - `open/*` — `didOpen` of a document of the given number of lines: JSON,
//!   line index, full parse.
//! - `keystroke/*` — one `didChange` typing or deleting a character in the
//!   middle of a document of the given number of lines: JSON, position
//!   conversion, incremental reparse, line-index patch.
//! - `publish/*` — a keystroke whose handler publishes the given number of
//!   diagnostics, measuring the diag-lang to protocol conversion.
//! - `request/hover` — a request round trip: parameters parsed into a
//!   borrowing struct, positions converted to offsets and back, a range
//!   replied.
//!
//! Only the measured messages are timed. The input stream is split in three —
//! setup (`initialize`, and `didOpen` for the per-message benchmarks), the
//! measured messages, and `shutdown`/`exit` — and notes the instant the server
//! first asks for each part's bytes. The server reads a message's header only
//! after it has handled the previous message, so those instants bracket the
//! measured messages exactly, leaving out setup and the session's teardown.

#[path = "../examples/common/mod.rs"]
mod common;

use std::hint::black_box;
use std::io::{self, BufRead, Read};
use std::time::{Duration, Instant};

use common::{Client, Lisp, PositionParams};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lsp_lang::diag_lang::{Label, Severity};
use lsp_lang::incremental_lang::Span;
use lsp_lang::{
    Context, Diagnostic, Document, Handler, Json, Position, Reply, ResponseError, Server,
};

const URI: &str = "file:///bench.lisp";

/// A Lisp document of `lines` lines, about 34 bytes each, every tenth a
/// comment with non-ASCII text.
fn document(lines: usize) -> String {
    let mut text = String::with_capacity(lines * 36);
    for i in 0..lines {
        if i % 10 == 9 {
            text.push_str("; ünïcode 😀 commentary\n");
        } else {
            text.push_str(&format!(
                "(define n{i} (+ {i} (* n{} 2)))\n",
                i.saturating_sub(1)
            ));
        }
    }
    text
}

/// Framed bytes of `initialize`, then of opening `text` (when given).
fn setup(text: Option<&str>) -> Vec<u8> {
    let mut client = Client::new();
    client.initialize(&["utf-16"]);
    if let Some(text) = text {
        client.open(URI, text);
    }
    client.bytes().to_vec()
}

/// Framed bytes of the measured messages.
fn timed(messages: impl FnOnce(&mut Client)) -> Vec<u8> {
    let mut client = Client::new();
    messages(&mut client);
    client.bytes().to_vec()
}

/// Framed bytes of `shutdown` and `exit`.
fn teardown() -> Vec<u8> {
    let mut client = Client::new();
    client.shutdown();
    client.bytes().to_vec()
}

/// Appends `n` keystrokes in the middle of the document, alternately typing
/// `x` into a name and deleting it again, so the text never drifts.
fn keystrokes(client: &mut Client, lines: usize, n: u64) {
    let line = (lines / 2 / 10 * 10) as u32; // a `define` line, not a comment
    for k in 0..n {
        let version = k as i32 + 2;
        if k % 2 == 0 {
            client.edit(URI, version, (line, 9), (line, 9), "x");
        } else {
            client.edit(URI, version, (line, 9), (line, 10), "");
        }
    }
}

/// An input stream in three parts that notes when the server first reads
/// from the second and from the third.
struct Split<'a> {
    parts: [&'a [u8]; 3],
    current: usize,
    entered: [Option<Instant>; 3],
}

impl BufRead for Split<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        while self.parts[self.current].is_empty() && self.current < 2 {
            self.current += 1;
        }
        let entered = &mut self.entered[self.current];
        if entered.is_none() {
            *entered = Some(Instant::now());
        }
        Ok(self.parts[self.current])
    }

    fn consume(&mut self, n: usize) {
        let part = &mut self.parts[self.current];
        *part = &part[n..];
    }
}

impl Read for Split<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.consume(n);
        Ok(n)
    }
}

/// Runs `setup`, `measured`, and a teardown through a fresh server, timing
/// only `measured`.
fn measure<H: Handler<Lisp>>(handler: H, setup: &[u8], measured: &[u8]) -> Duration {
    let teardown = teardown();
    let mut server = Server::new(Lisp, handler);
    let mut input = Split {
        parts: [setup, measured, &teardown],
        current: 0,
        entered: [None; 3],
    };
    let result = server.run(black_box(&mut input), io::sink());
    assert!(result.is_ok());
    match input.entered {
        [_, Some(start), Some(end)] => end - start,
        _ => Duration::ZERO,
    }
}

/// Handles document sync only.
struct SyncOnly;
impl Handler<Lisp> for SyncOnly {}

/// Publishes the same diagnostics on every change.
struct Publisher(Vec<Diagnostic>);
impl Handler<Lisp> for Publisher {
    fn changed(&mut self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
        cx.publish_diagnostics(doc, &self.0);
    }
}

/// Answers hover with the range from the start of the line to the cursor.
struct Hover;
impl Handler<Lisp> for Hover {
    fn request(
        &mut self,
        _: &str,
        params: Json<'_>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Reply, ResponseError> {
        let p: PositionParams<'_> = params.parse()?;
        let Some(doc) = cx.document(p.text_document.uri) else {
            return Ok(Reply::null());
        };
        let at = doc.offset(p.position);
        let line_start = doc.offset(Position::new(p.position.line, 0));
        Ok(Reply::new(&doc.range(Span::new(line_start, at)))?)
    }
}

fn diagnostics(text: &str, count: usize) -> Vec<Diagnostic> {
    let len = text.len() as u32;
    (0..count)
        .map(|i| {
            let at = (i as u32 * 7919) % len.saturating_sub(4).max(1);
            Diagnostic::new(
                Severity::Warning,
                "cannot find `value` in this scope",
                Label::new(Span::new(at, at + 3), "not found"),
            )
            .with_secondary(Label::new(Span::new(0, 3), "first defined here"))
            .with_note("names are resolved globally")
        })
        .collect()
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("open");
    group.sample_size(20);
    let setup = setup(None);
    for lines in [1_000, 10_000, 100_000] {
        let text = document(lines);
        let measured = timed(|c| {
            c.open(URI, &text);
        });
        group.throughput(Throughput::Bytes(text.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(lines),
            &measured,
            |b, measured| {
                b.iter_custom(|iters| {
                    (0..iters)
                        .map(|_| measure(SyncOnly, &setup, measured))
                        .sum()
                });
            },
        );
    }
    group.finish();
}

fn bench_keystroke(c: &mut Criterion) {
    let mut group = c.benchmark_group("keystroke");
    for lines in [1_000, 10_000, 100_000] {
        let setup = setup(Some(&document(lines)));
        group.bench_with_input(BenchmarkId::from_parameter(lines), &setup, |b, setup| {
            b.iter_custom(|iters| {
                measure(SyncOnly, setup, &timed(|c| keystrokes(c, lines, iters)))
            });
        });
    }
    group.finish();
}

fn bench_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("publish");
    let lines = 10_000;
    let text = document(lines);
    let setup = setup(Some(&text));
    for count in [10, 100] {
        let found = diagnostics(&text, count);
        group.bench_with_input(BenchmarkId::from_parameter(count), &found, |b, found| {
            b.iter_custom(|iters| {
                let measured = timed(|c| keystrokes(c, lines, iters));
                measure(Publisher(found.clone()), &setup, &measured)
            });
        });
    }

    // Many diagnostics, in scattered order, on one 39 KB line of non-ASCII
    // text (a minified file, say), in a UTF-16 session: the batch conversion
    // counts the line once instead of once per diagnostic.
    let line = "é ".repeat(13_000);
    let long_setup = crate::setup(Some(&line));
    let found = diagnostics(&line, 10_000);
    group.bench_function("long_line_10000", |b| {
        b.iter_custom(|iters| {
            let measured = timed(|c| keystrokes(c, 1, iters));
            measure(Publisher(found.clone()), &long_setup, &measured)
        });
    });
    group.finish();
}

fn bench_request(c: &mut Criterion) {
    let setup = setup(Some(&document(10_000)));
    c.bench_function("request/hover", |b| {
        b.iter_custom(|iters| {
            let measured = timed(|c| {
                for k in 0..iters {
                    c.at("textDocument/hover", URI, (k % 9_000) as u32, 21);
                }
            });
            measure(Hover, &setup, &measured)
        });
    });
}

criterion_group!(
    benches,
    bench_open,
    bench_keystroke,
    bench_publish,
    bench_request
);
criterion_main!(benches);
