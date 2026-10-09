//! Properties of document synchronization that must hold for every edit
//! sequence a client can send:
//!
//! - after every `didChange`, the server's text equals the client's;
//! - the incrementally maintained tree equals a fresh parse of that text;
//! - converting any character boundary to a position and back is exact
//!   (except between the `\r` and `\n` of one line break, which is not a caret
//!   position);
//! - published diagnostics are well-formed JSON with ranges inside the text;
//! - the server never reports a text the client did not have at that
//!   version: a change it cannot place (a line the document lacks, a version
//!   that does not increase) takes the document out of step, and nothing is
//!   reported until a whole-text change brings it back;
//! - a grammar whose fragment reparse always panics still tracks the client
//!   exactly, through the full-reparse fallback.
//!
//! The client side is an independent model: it applies edits to a `String` by
//! byte offset and computes the protocol positions it sends with a naive
//! line-splitting algorithm written straight from the specification.

#[path = "../examples/common/mod.rs"]
mod common;

use std::sync::Once;

use common::{Client, Kind, Lisp, LispServer, messages};
use lsp_lang::incremental_lang::{Builder, Document as Incremental, Grammar, Token};
use lsp_lang::{Context, Document, Handler, Position, Server, Value, json};
use proptest::prelude::*;

const URI: &str = "file:///p.lisp";

/// The protocol position of byte `offset`, computed the slow, obvious way.
fn model_position(text: &str, offset: usize, utf8: bool) -> Position {
    let bytes = text.as_bytes();
    let (mut line, mut start) = (0u32, 0usize);
    let mut i = 0;
    while i < offset {
        let b = bytes[i];
        let brk = if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            if i + 1 < offset { 2 } else { 0 }
        } else if b == b'\n' || b == b'\r' {
            1
        } else {
            0
        };
        if brk > 0 {
            line += 1;
            i += brk;
            start = i;
        } else {
            i += 1;
        }
    }
    let prefix = &text[start..offset];
    let character = if utf8 {
        prefix.len()
    } else {
        prefix.encode_utf16().count()
    };
    Position::new(line, character as u32)
}

/// Snaps `at` down to a caret position: a character boundary that is not
/// between the two bytes of a CRLF line break. No protocol position names
/// the inside of a CRLF, so a real client never edits there.
fn floor(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) || inside_crlf(text, at) {
        at -= 1;
    }
    at
}

fn inside_crlf(text: &str, offset: usize) -> bool {
    let b = text.as_bytes();
    offset > 0 && b[offset - 1] == b'\r' && b.get(offset) == Some(&b'\n')
}

/// Checks every invariant on every change and records what it saw.
#[derive(Default)]
struct Checker {
    texts: Vec<String>,
    versions: Vec<i32>,
    failures: Vec<String>,
}

impl<G: Grammar<Kind = Kind>> Handler<G> for Checker {
    fn changed(&mut self, doc: &Document<G>, _: &mut Context<'_, G>) {
        let text = doc.text();
        match Incremental::new(Lisp, text) {
            Ok(fresh) if fresh.tree() == doc.tree() => {}
            _ => self
                .failures
                .push(format!("tree differs from a fresh parse of {text:?}")),
        }
        for offset in (0..=text.len()).filter(|&o| text.is_char_boundary(o)) {
            let pos = doc.position(offset as u32);
            let model = model_position(text, offset, doc.encoding() == lsp_lang::Encoding::Utf8);
            if pos != model && !inside_crlf(text, offset) {
                self.failures.push(format!(
                    "position({offset}) = {pos:?}, model {model:?} in {text:?}"
                ));
            }
            if !inside_crlf(text, offset) && doc.offset(pos) as usize != offset {
                self.failures.push(format!(
                    "offset(position({offset})) != {offset} in {text:?}"
                ));
            }
        }
        self.texts.push(text.to_owned());
        self.versions.push(doc.version());
    }
}

/// The Lisp grammar with a bug in its fragment path: reparsing one list on
/// its own panics, so every incremental reparse fails and the server must
/// fall back to parsing the whole text.
#[derive(Clone, Copy)]
struct FragileLisp;

const FRAGILE: &str = "fragile-lisp: fragment reparse is broken";

impl Grammar for FragileLisp {
    type Kind = Kind;
    const ROOT: Kind = Lisp::ROOT;
    fn lex(&self, text: &str, tokens: &mut Vec<Token<Kind>>) {
        Lisp.lex(text, tokens);
    }
    fn parse(&self, kind: Kind, b: &mut Builder<'_, Kind>) {
        assert!(kind == Self::ROOT, "{FRAGILE}");
        Lisp.parse(kind, b);
    }
    fn is_reparsable(&self, kind: Kind) -> bool {
        Lisp.is_reparsable(kind)
    }
}

/// Keeps the expected `FragileLisp` panics out of the test output; every other
/// panic still reaches the default hook.
fn quiet_fragile_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let message = info
                .payload()
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| info.payload().downcast_ref::<&str>().copied());
            if message != Some(FRAGILE) {
                default(info);
            }
        }));
    });
}

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "(", ")", "(a", "b)", "define", " ", "\n", "\r\n", "\r", ";c", "é", "😀", "中", "1",
        ]),
        0..24,
    )
    .prop_map(|parts| parts.concat())
}

/// One `didChange`: up to three edits `(from, to, insert)` by byte offset into
/// the text as it stands when each edit applies, or `None` for a whole-text
/// replacement.
fn change_strategy() -> impl Strategy<Value = Vec<Option<(usize, usize, String)>>> {
    prop::collection::vec(
        prop_oneof![
            8 => (0usize..80, 0usize..80, text_strategy()).prop_map(Some),
            1 => Just(None),
        ],
        1..4,
    )
}

/// Applies `changes` to the model and turns them into protocol content changes.
fn encode(
    model: &mut String,
    changes: &[Option<(usize, usize, String)>],
    replacement: &str,
    utf8: bool,
) -> Vec<Value> {
    changes
        .iter()
        .map(|change| match change {
            Some((a, b, insert)) => {
                let (a, b) = (floor(model, *a.min(b)), floor(model, *a.max(b)));
                let (start, end) = (
                    model_position(model, a, utf8),
                    model_position(model, b, utf8),
                );
                model.replace_range(a..b, insert);
                json!({ "range": { "start": start, "end": end }, "text": insert })
            }
            None => {
                *model = replacement.to_owned();
                json!({ "text": replacement })
            }
        })
        .collect()
}

/// One step of a change in the out-of-step property.
#[derive(Clone, Debug)]
enum Step {
    /// A ranged edit `(from, to, insert)` by byte offset into the client text.
    Edit(usize, usize, String),
    /// A client bug: an insertion at a line two past the client's last line.
    /// The client puts `insert` at its end.
    Misplaced(String),
    /// A whole-text replacement.
    Whole,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => (0usize..80, 0usize..80, text_strategy()).prop_map(|(a, b, t)| Step::Edit(a, b, t)),
        1 => text_strategy().prop_map(Step::Misplaced),
        1 => Just(Step::Whole),
    ]
}

/// Number of lines in `text`, as the protocol counts them.
fn line_count(text: &str) -> u32 {
    model_position(text, text.len(), true).line + 1
}

proptest! {
    #![proptest_config(ProptestConfig::default())]

    #[test]
    fn prop_server_never_reports_a_text_the_client_did_not_have(
        initial in text_strategy(),
        changes in prop::collection::vec(
            (prop::collection::vec(step_strategy(), 0..4), prop::bool::weighted(0.1)),
            1..10,
        ),
        replacement in text_strategy(),
    ) {
        let mut client = Client::new();
        client.initialize(&[]).open(URI, &initial);
        let mut text = initial.clone();
        let mut version = 1;
        let mut in_step = true;
        let mut expected = vec![(1, initial)];
        for (steps, repeat) in &changes {
            if !repeat {
                version += 1;
            }
            // Edits before the last whole-text change are superseded.
            let whole = steps.iter().rposition(|s| matches!(s, Step::Whole));
            let mut placed = !*repeat || whole.is_some();
            placed &= in_step || whole.is_some();
            let mut content = Vec::new();
            for (at, step) in steps.iter().enumerate() {
                let applies = whole.is_none_or(|w| at >= w);
                match step {
                    Step::Edit(a, b, insert) => {
                        let (a, b) = (floor(&text, *a.min(b)), floor(&text, *a.max(b)));
                        let (start, end) = (model_position(&text, a, false), model_position(&text, b, false));
                        text.replace_range(a..b, insert);
                        content.push(json!({ "range": { "start": start, "end": end }, "text": insert }));
                    }
                    Step::Misplaced(insert) => {
                        let line = line_count(&text) + 1;
                        let at = Position::new(line, 0);
                        text.push_str(insert);
                        content.push(json!({ "range": { "start": at, "end": at }, "text": insert }));
                        if applies {
                            placed = false;
                        }
                    }
                    Step::Whole => {
                        text.clone_from(&replacement);
                        content.push(json!({ "text": replacement }));
                    }
                }
            }
            client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": URI, "version": version }, "contentChanges": content }),
            );
            in_step = placed;
            if placed {
                expected.push((version, text.clone()));
            }
        }
        // A final whole-text change always brings the document back.
        client.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": URI, "version": version + 1 }, "contentChanges": [{ "text": replacement }] }),
        );
        expected.push((version + 1, replacement.clone()));
        client.shutdown();

        let mut server = Server::new(Lisp, Checker::default());
        prop_assert!(server.run(client.bytes(), Vec::new()).is_ok());
        let checker = server.handler();
        prop_assert_eq!(&checker.failures, &Vec::<String>::new());
        let seen: Vec<(i32, String)> = checker
            .versions
            .iter()
            .copied()
            .zip(checker.texts.iter().cloned())
            .collect();
        prop_assert_eq!(seen, expected);
    }

    #[test]
    fn prop_full_reparse_fallback_tracks_client_text(
        initial in text_strategy(),
        changes in prop::collection::vec(change_strategy(), 1..8),
        replacement in text_strategy(),
        utf8 in any::<bool>(),
    ) {
        quiet_fragile_panics();
        let mut client = Client::new();
        client.initialize(if utf8 { &["utf-8"] } else { &["utf-16"] }).open(URI, &initial);
        let mut model = initial.clone();
        let mut expected = vec![initial];
        for (version, change) in changes.iter().enumerate() {
            let content = encode(&mut model, change, &replacement, utf8);
            client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": URI, "version": version + 2 }, "contentChanges": content }),
            );
            expected.push(model.clone());
        }
        client.shutdown();

        let mut server = Server::new(FragileLisp, Checker::default());
        prop_assert!(server.run(client.bytes(), Vec::new()).is_ok());
        let checker = server.handler();
        prop_assert_eq!(&checker.failures, &Vec::<String>::new());
        prop_assert_eq!(&checker.texts, &expected);
    }

    #[test]
    fn prop_server_text_tracks_client_text(
        initial in text_strategy(),
        changes in prop::collection::vec(change_strategy(), 1..10),
        replacement in text_strategy(),
        utf8 in any::<bool>(),
    ) {
        let mut client = Client::new();
        client.initialize(if utf8 { &["utf-8"] } else { &["utf-16"] }).open(URI, &initial);
        let mut model = initial.clone();
        let mut expected = vec![initial];
        for (version, change) in changes.iter().enumerate() {
            let content = encode(&mut model, change, &replacement, utf8);
            client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": URI, "version": version + 2 }, "contentChanges": content }),
            );
            expected.push(model.clone());
        }
        client.shutdown();

        let mut server = Server::new(Lisp, Checker::default());
        prop_assert!(server.run(client.bytes(), Vec::new()).is_ok());
        let checker = server.handler();
        prop_assert_eq!(&checker.failures, &Vec::<String>::new());
        prop_assert_eq!(&checker.texts, &expected);
    }

    #[test]
    fn prop_published_diagnostics_are_well_formed(
        initial in text_strategy(),
        changes in prop::collection::vec(change_strategy(), 0..6),
        replacement in text_strategy(),
    ) {
        let mut client = Client::new();
        client.initialize(&[]).open(URI, &initial);
        let mut model = initial;
        let mut finals = Vec::new();
        for (version, change) in changes.iter().enumerate() {
            let content = encode(&mut model, change, &replacement, false);
            client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": URI, "version": version + 2 }, "contentChanges": content }),
            );
            finals.push(model.clone());
        }
        client.shutdown();

        let mut server = Server::new(Lisp, LispServer::default());
        let mut output = Vec::new();
        prop_assert!(server.run(client.bytes(), &mut output).is_ok());
        let out = messages(&output);
        prop_assert!(out.iter().all(|m| !m.is_null()), "a message was not valid JSON");
        let published: Vec<&Value> =
            out.iter().filter(|m| m["method"] == "textDocument/publishDiagnostics").collect();
        prop_assert_eq!(published.len(), changes.len() + 1);
        for p in published {
            for d in p["params"]["diagnostics"].as_array().into_iter().flatten() {
                let (s, e) = (&d["range"]["start"], &d["range"]["end"]);
                let start = (s["line"].as_u64(), s["character"].as_u64());
                let end = (e["line"].as_u64(), e["character"].as_u64());
                prop_assert!(start <= end, "reversed range {}", d);
                prop_assert!(d["message"].as_str().is_some_and(|m| !m.is_empty()));
                prop_assert!((1..=4).contains(&d["severity"].as_u64().unwrap_or(0)));
            }
        }
    }
}
