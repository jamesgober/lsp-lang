//! Hostile input: nothing a client sends may panic the server, overflow its
//! stack, or make it allocate without bound, and what it sends must get the
//! JSON-RPC answer the specification prescribes: exactly one response per
//! request, under the request's own id when that can be read and `null`
//! otherwise, and never a response to a notification or to a response.

#[path = "../examples/common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::{Client, Lisp, LispServer, messages};
use lsp_lang::rpc_io::FrameError;
use lsp_lang::{
    CompletionItem, Context, Document, DocumentSymbol, Error, Features, Handler, Hover, Location,
    Position, ResponseError, SemanticTokens, Server, Value, json,
};
use proptest::prelude::*;

const URI: &str = "file:///h.lisp";

/// The Lisp server, plus one custom request that echoes its parameters.
#[derive(Default)]
struct Echoing(LispServer);

impl Handler<Lisp> for Echoing {
    fn features(&self) -> Features {
        self.0.features()
    }
    fn changed(&mut self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
        self.0.changed(doc, cx);
    }
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Lisp>) {
        self.0.closed(uri, cx);
    }
    fn hover(
        &mut self,
        doc: &Document<Lisp>,
        at: Position,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Option<Hover>, ResponseError> {
        self.0.hover(doc, at, cx)
    }
    fn definition(
        &mut self,
        doc: &Document<Lisp>,
        at: Position,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Vec<Location>, ResponseError> {
        self.0.definition(doc, at, cx)
    }
    fn completion(
        &mut self,
        doc: &Document<Lisp>,
        at: Position,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Vec<CompletionItem>, ResponseError> {
        self.0.completion(doc, at, cx)
    }
    fn document_symbols(
        &mut self,
        doc: &Document<Lisp>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Vec<DocumentSymbol>, ResponseError> {
        self.0.document_symbols(doc, cx)
    }
    fn semantic_tokens(
        &mut self,
        doc: &Document<Lisp>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<SemanticTokens, ResponseError> {
        self.0.semantic_tokens(doc, cx)
    }
    fn request(
        &mut self,
        _: &str,
        params: &Value<'_>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Value<'static>, ResponseError> {
        Ok(params.clone().into_owned())
    }
}

fn serve() -> Server<Lisp, Echoing> {
    Server::new(Lisp, Echoing::default()).custom_request("test/echo")
}

/// Runs `input` as a whole session; the outcome and the messages written.
fn run(input: &[u8]) -> (Result<(), Error>, Vec<Value<'static>>) {
    let mut output = Vec::new();
    let result = serve().run(input, &mut output);
    (result, messages(&output))
}

fn answer_to(out: &[Value<'static>], id: &Value<'_>) -> usize {
    out.iter()
        .filter(|m| &m["id"] == id && m.get("method").is_none())
        .count()
}

fn code(message: &Value<'_>) -> Option<i64> {
    message["error"]["code"].as_i64()
}

// ---------------------------------------------------------------------------
// Malformed JSON and wrong types
// ---------------------------------------------------------------------------

/// Bodies the client can send instead of a message, the error each earns, and
/// the id it is answered under: its own when the message has a `method` and a
/// valid, unrepeated id, otherwise `null`.
fn bad_bodies() -> Vec<(&'static [u8], i32, Option<i64>)> {
    let parse = ResponseError::PARSE_ERROR;
    let invalid = ResponseError::INVALID_REQUEST;
    vec![
        (b"", parse, None),
        (b"   ", parse, None),
        (b"{", parse, None),
        (b"}", parse, None),
        (b"nul", parse, None),
        (b"{\"jsonrpc\":\"2.0\",", parse, None),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"x\"}}",
            parse,
            None,
        ),
        (b"{'jsonrpc':'2.0'}", parse, None),
        (b"{\"a\":1,}", parse, None),
        (b"[1,]", parse, None),
        (b"// comment\n{}", parse, None),
        (
            b"\xEF\xBB\xBF{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}",
            parse,
            None,
        ),
        (b"{\"jsonrpc\":\"2.0\",\"method\":\"\xFF\"}", parse, None),
        (b"{\"s\":\"\\ud800\"}", parse, None),
        (b"{\"s\":\"tab\there\"}", parse, None),
        (b"NaN", parse, None),
        (b"-", parse, None),
        (b"01", parse, None),
        (b"\"unterminated", parse, None),
        (
            b"{\"jsonrpc\":\"2.0\",\"method\":\"x\",\"params\":[1,2,],\"id\":1}",
            parse,
            None,
        ),
        (b"null", invalid, None),
        (b"true", invalid, None),
        (b"5", invalid, None),
        (b"\"hello\"", invalid, None),
        (b"{}", invalid, None),
        (b"[]", invalid, None),
        (b"{\"jsonrpc\":\"2.0\"}", invalid, None),
        (
            b"{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"initialize\"}",
            invalid,
            Some(1),
        ),
        (b"{\"id\":1,\"method\":\"initialize\"}", invalid, Some(1)),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":5}",
            invalid,
            Some(1),
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"m\",\"params\":\"s\"}",
            invalid,
            Some(1),
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"m\",\"params\":5}",
            invalid,
            Some(1),
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"m\",\"method\":\"n\"}",
            invalid,
            Some(1),
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"id\":2,\"method\":\"m\"}",
            invalid,
            None,
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"m\"}",
            invalid,
            None,
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":1e400,\"method\":\"m\"}",
            invalid,
            None,
        ),
        (
            b"{\"jsonrpc\":\"2.0\",\"id\":99999999999999999999,\"method\":\"m\"}",
            invalid,
            None,
        ),
    ]
}

#[test]
fn test_bad_bodies_get_the_prescribed_error_and_the_session_survives() {
    for (body, expected, id) in bad_bodies() {
        let shown = String::from_utf8_lossy(body).into_owned();
        // The body travels as it is, even when it is not UTF-8.
        let mut script = Client::new().initialize(&[]).bytes().to_vec();
        script.extend(format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes());
        script.extend_from_slice(body);
        let mut tail = Client::new();
        tail.request("test/echo", json!(["alive"])).shutdown();
        script.extend_from_slice(tail.bytes());

        let (result, out) = run(&script);
        assert!(result.is_ok(), "{shown:?}: the session ended: {result:?}");
        let wanted = id.map_or(Value::Null, |n| json!(n));
        let errors: Vec<&Value> = out
            .iter()
            .filter(|m| m["id"] == wanted && m.get("error").is_some())
            .collect();
        assert_eq!(errors.len(), 1, "{shown:?}: {out:?}");
        assert_eq!(code(errors[0]), Some(i64::from(expected)), "{shown:?}");
        assert!(
            out.iter().any(|m| m["result"] == json!(["alive"])),
            "{shown:?}: the next request was not answered"
        );
    }
}

#[test]
fn test_every_request_is_answered_exactly_once_whatever_its_params() {
    let params = [
        json!(null),
        json!([]),
        json!({}),
        json!([null]),
        json!({ "textDocument": null }),
        json!({ "textDocument": [] }),
        json!({ "textDocument": { "uri": null }, "position": null }),
        json!({ "textDocument": { "uri": URI }, "position": { "line": 1e30, "character": 0 } }),
        json!({ "textDocument": { "uri": URI }, "position": { "line": 4294967295_u64, "character": 4294967295_u64 } }),
        json!({ "textDocument": { "uri": URI }, "previousResultId": 5 }),
        json!({ "capabilities": [] }),
        json!([[[[[[[[[[1]]]]]]]]]]),
    ];
    let methods = [
        "textDocument/hover",
        "textDocument/definition",
        "textDocument/completion",
        "textDocument/documentSymbol",
        "textDocument/semanticTokens/full",
        "textDocument/semanticTokens/full/delta",
        "test/echo",
        "initialize",
        "shutdown",
        "no/such/method",
    ];
    let mut client = Client::new();
    let mut ids = Vec::new();
    client.initialize(&[]).open(URI, "(define x 1)");
    for method in methods {
        if method == "shutdown" || method == "initialize" {
            continue;
        }
        for p in &params {
            let _ = client.request(method, p.clone());
            ids.push(client.last_id());
        }
    }
    client.shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok(), "{result:?}");
    for id in ids {
        assert_eq!(answer_to(&out, &json!(id)), 1, "request {id}");
    }
}

#[test]
fn test_sync_notifications_with_absurd_content_never_panic_and_never_lose_the_document() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(define x 1)\n");
    let edits = [
        json!({ "range": { "start": { "line": 4294967295_u64, "character": 4294967295_u64 }, "end": { "line": 4294967295_u64, "character": 0 } }, "text": "a" }),
        json!({ "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 0, "character": 0 } }, "text": "reversed" }),
        json!({ "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 4 } }, "text": "😀\u{0}\n\r\r\n" }),
        json!({ "range": null, "text": "whole" }),
        json!({ "range": { "start": { "line": 0 } }, "text": "partial" }),
        json!({ "text": 5 }),
        json!({}),
        json!(7),
    ];
    for (version, edit) in edits.into_iter().enumerate() {
        client.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": URI, "version": version + 2 }, "contentChanges": [edit] }),
        );
    }
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 2147483647 }, "contentChanges": [] }),
    );
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": -4 }, "contentChanges": [{ "text": "neg" }] }),
    );
    client.of("textDocument/documentSymbol", URI);
    let probe = client.last_id();
    client.shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok(), "{result:?}");
    // The document is still there to answer for.
    assert_eq!(answer_to(&out, &json!(probe)), 1);
    assert!(
        out.iter()
            .any(|m| m["id"] == probe && m.get("result").is_some())
    );
}

// ---------------------------------------------------------------------------
// Size and depth
// ---------------------------------------------------------------------------

#[test]
fn test_a_message_over_the_limit_is_skipped_not_buffered() {
    let mut client = Client::new();
    client.initialize(&[]);
    let big = "x".repeat(200_000);
    client.send_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":50,"method":"test/echo","params":["{big}"]}}"#
    ));
    client.request("test/echo", json!(["after"])).shutdown();
    let mut server = serve().with_message_limit(100_000);
    let mut output = Vec::new();
    let result = server.run(client.bytes(), &mut output);
    assert!(result.is_ok(), "{result:?}");
    let out = messages(&output);
    // Refused under `null` (its id was never read), and the next one is fine.
    assert!(
        out.iter().any(
            |m| m["id"].is_null() && code(m) == Some(i64::from(ResponseError::INVALID_REQUEST))
        )
    );
    assert_eq!(answer_to(&out, &json!(50)), 0);
    assert!(out.iter().any(|m| m["result"] == json!(["after"])));
}

#[test]
fn test_an_announced_length_that_is_absurd_is_refused_without_reading_it() {
    // 2^62 bytes announced, 20 sent, then the line goes dead.
    let mut input = Vec::new();
    input.extend_from_slice(b"Content-Length: 4611686018427387904\r\n\r\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    let (result, out) = run(&input);
    assert!(matches!(result, Err(Error::Disconnected)), "{result:?}");
    assert_eq!(out.len(), 1);
    assert_eq!(
        code(&out[0]),
        Some(i64::from(ResponseError::INVALID_REQUEST))
    );

    // A length too big for 64 bits is not a length at all.
    let (result, _) = run(b"Content-Length: 99999999999999999999999\r\n\r\n{}");
    assert!(matches!(
        result,
        Err(Error::Frame(FrameError::InvalidContentLength))
    ));
}

#[test]
fn test_deeply_nested_json_is_refused_without_recursion() {
    // A recursive parser would overflow this thread's small stack.
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let deep = format!("{}1{}", "[".repeat(1_000_000), "]".repeat(1_000_000));
            let mut client = Client::new();
            client.initialize(&[]);
            client.send_raw(&format!(
                r#"{{"jsonrpc":"2.0","id":9,"method":"test/echo","params":{deep}}}"#
            ));
            // Nested in a notification the server reads itself.
            client.send_raw(&format!(
                r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"u","version":1}},"contentChanges":{deep}}}}}"#
            ));
            client.request("test/echo", json!(["alive"])).shutdown();
            let mut server = serve().with_message_limit(8 * 1024 * 1024);
            let mut output = Vec::new();
            let result = server.run(client.bytes(), &mut output);
            (result.is_ok(), messages(&output))
        });
    let outcome = handle.map(|h| h.join());
    let Ok(Ok((ok, out))) = outcome else {
        assert!(
            outcome.is_ok_and(|r| r.is_ok()),
            "the server overflowed its stack"
        );
        return;
    };
    assert!(ok);
    // The deep request was refused (a budget, not a crash), under its own id.
    assert!(out.iter().any(|m| m["id"] == 9 && m.get("error").is_some()));
    assert!(out.iter().any(|m| m["result"] == json!(["alive"])));
}

#[test]
fn test_deep_but_allowed_nesting_round_trips() {
    // 100 levels is inside the budget and must work.
    let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
    let mut client = Client::new();
    client.initialize(&[]);
    client.send_raw(&format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"test/echo","params":[{deep}]}}"#
    ));
    client.shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok());
    assert!(
        out.iter()
            .any(|m| m["id"] == 3 && m.get("result").is_some())
    );
}

#[test]
fn test_a_flood_of_requests_is_answered_once_each() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(define x 1)");
    let mut ids = Vec::new();
    for _ in 0..5_000 {
        let _ = client.at("textDocument/hover", URI, 0, 9);
        ids.push(client.last_id());
    }
    client.shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok(), "{result:?}");
    for id in ids {
        assert_eq!(answer_to(&out, &json!(id)), 1, "request {id}");
    }
}

#[test]
fn test_a_flood_of_notifications_costs_nothing_lasting() {
    let mut client = Client::new();
    client.initialize(&[]);
    for i in 0..20_000 {
        client.send_raw(r#"{"jsonrpc":"2.0","method":"$/unknown"}"#);
        if i % 4 == 0 {
            client.send_raw(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":123}}"#);
        }
        if i % 10 == 0 {
            client.send_raw(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
        }
    }
    client.request("test/echo", json!(["alive"])).shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok());
    assert!(out.iter().any(|m| m["result"] == json!(["alive"])));
    // None of it is worth a word to the client.
    assert!(out.iter().all(|m| m["method"] != "window/logMessage"));
}

#[test]
fn test_many_documents_open_and_close() {
    let mut client = Client::new();
    client.initialize(&[]);
    for i in 0..2_000 {
        client.open(&format!("file:///many/{i}.lisp"), "(a)");
    }
    for i in 0..2_000 {
        client.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": format!("file:///many/{i}.lisp") } }),
        );
    }
    client.shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok());
    assert_eq!(
        out.iter()
            .filter(|m| m["method"] == "textDocument/publishDiagnostics")
            .count(),
        4_000
    );
}

#[test]
fn test_a_document_with_every_awkward_character_round_trips() {
    let text = "\u{0}\u{1f}\u{7f}\u{80}\u{2028}\u{2029}\u{feff}\u{fffd}\r\r\n\n\r😀😀\u{10ffff}(define a 1)";
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, text)
        .of("textDocument/semanticTokens/full", URI)
        .of("textDocument/documentSymbol", URI)
        .shutdown();
    let (result, out) = run(client.bytes());
    assert!(result.is_ok(), "{result:?}");
    assert!(
        out.iter().all(|m| !m.is_null()),
        "every message is valid JSON"
    );
    assert!(
        out.iter()
            .any(|m| m["id"] == 2 && m.get("result").is_some())
    );
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

#[test]
fn test_broken_framing_ends_the_session_with_an_error_not_a_panic() {
    type Expect = Box<dyn Fn(&Error) -> bool>;
    let cases: Vec<(&[u8], Expect)> = vec![
        (
            b"Content-Length: x\r\n\r\n",
            Box::new(|e| matches!(e, Error::Frame(FrameError::InvalidContentLength))),
        ),
        (
            b"Content-Length: -1\r\n\r\n",
            Box::new(|e| matches!(e, Error::Frame(FrameError::InvalidContentLength))),
        ),
        (
            b"Content-Length: 1\r\nContent-Length: 1\r\n\r\n1",
            Box::new(|e| matches!(e, Error::Frame(FrameError::DuplicateContentLength))),
        ),
        (
            b"X: y\r\n\r\n{}",
            Box::new(|e| matches!(e, Error::Frame(FrameError::MissingContentLength))),
        ),
        (
            b"no colon here\r\n\r\n",
            Box::new(|e| matches!(e, Error::Frame(FrameError::InvalidHeader))),
        ),
        (
            b"Content-Length : 2\r\n\r\n{}",
            Box::new(|e| matches!(e, Error::Frame(_))),
        ),
        (
            b"{\"jsonrpc\":\"2.0\"}",
            Box::new(|e| matches!(e, Error::Frame(_) | Error::Disconnected)),
        ),
        (
            b"\x00\x00\x00\x00\r\n\r\n",
            Box::new(|e| matches!(e, Error::Frame(_))),
        ),
    ];
    for (input, expected) in cases {
        let (result, _) = run(input);
        assert!(
            result.as_ref().err().is_some_and(&expected),
            "{:?}: {result:?}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn test_header_floods_are_refused() {
    let many = "X-Header: v\r\n".repeat(10_000);
    let (result, _) = run(format!("{many}Content-Length: 2\r\n\r\n{{}}").as_bytes());
    assert!(
        matches!(result, Err(Error::Frame(FrameError::TooManyHeaders { .. }))),
        "{result:?}"
    );
    let long = format!("X: {}\r\n\r\n", "a".repeat(100_000));
    let (result, _) = run(long.as_bytes());
    assert!(
        matches!(result, Err(Error::Frame(FrameError::HeaderTooLarge { .. }))),
        "{result:?}"
    );
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

fn method_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "initialize",
        "initialized",
        "shutdown",
        "exit",
        "textDocument/didOpen",
        "textDocument/didChange",
        "textDocument/didClose",
        "textDocument/hover",
        "textDocument/definition",
        "textDocument/completion",
        "textDocument/documentSymbol",
        "textDocument/semanticTokens/full",
        "textDocument/semanticTokens/full/delta",
        "$/cancelRequest",
        "test/echo",
        "unknown/method",
    ])
}

/// A small JSON value, nested a few levels, from a fixed vocabulary that
/// includes the shapes the server looks for.
fn json_strategy() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        Just("null".to_owned()),
        Just("true".to_owned()),
        Just("0".to_owned()),
        Just("-1".to_owned()),
        Just("4294967296".to_owned()),
        Just("1.5".to_owned()),
        Just("\"\"".to_owned()),
        Just("\"file:///h.lisp\"".to_owned()),
        Just("\"(a b)\"".to_owned()),
        Just("[]".to_owned()),
        Just("{}".to_owned()),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(|v| format!("[{}]", v.join(","))),
            prop::collection::vec(
                (
                    prop::sample::select(vec![
                        "textDocument",
                        "uri",
                        "version",
                        "text",
                        "position",
                        "line",
                        "character",
                        "range",
                        "start",
                        "end",
                        "contentChanges",
                        "previousResultId",
                        "id",
                        "capabilities",
                        "general",
                        "positionEncodings",
                        "x",
                    ]),
                    inner,
                ),
                0..5,
            )
            .prop_map(|members| {
                // Keys are unique, as the transport requires.
                let mut seen = BTreeSet::new();
                let members: Vec<String> = members
                    .into_iter()
                    .filter(|(k, _)| seen.insert(*k))
                    .map(|(k, v)| format!("\"{k}\":{v}"))
                    .collect();
                format!("{{{}}}", members.join(","))
            }),
        ]
    })
}

fn message_strategy() -> impl Strategy<Value = (bool, &'static str, String)> {
    (any::<bool>(), method_strategy(), json_strategy())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_random_bytes_never_panic_and_everything_written_is_well_formed(
        bytes in prop::collection::vec(any::<u8>(), 0..600),
        framed in any::<bool>(),
    ) {
        let input = if framed {
            let mut input = format!("Content-Length: {}\r\n\r\n", bytes.len()).into_bytes();
            input.extend_from_slice(&bytes);
            input
        } else {
            bytes
        };
        let mut output = Vec::new();
        let _ended = serve().run(&input[..], &mut output);
        // Whatever came out is a sequence of well-framed JSON messages.
        let mut rest = &output[..];
        while !rest.is_empty() {
            let split = rest.windows(4).position(|w| w == b"\r\n\r\n");
            prop_assert!(split.is_some(), "output is not framed");
            let split = split.unwrap_or(0);
            let header = String::from_utf8_lossy(&rest[..split]).into_owned();
            let len = header
                .strip_prefix("Content-Length: ")
                .and_then(|n| n.trim().parse::<usize>().ok());
            prop_assert!(len.is_some(), "bad header {header:?}");
            let len = len.unwrap_or(0);
            let body = &rest[split + 4..split + 4 + len];
            prop_assert!(lsp_lang::jsonx_lib::parse_bytes(body).is_ok(), "bad body");
            rest = &rest[split + 4 + len..];
        }
    }

    #[test]
    fn prop_every_request_gets_one_response_and_nothing_else_does(
        script in prop::collection::vec(message_strategy(), 0..30),
    ) {
        let mut client = Client::new();
        client.initialize(&[]).open(URI, "(define x 1)\n(+ x 2)");
        let mut requests = Vec::new();
        for (is_request, method, params) in &script {
            // The lifecycle messages would end the session early; they have
            // their own tests.
            if matches!(*method, "shutdown" | "exit" | "initialize") {
                continue;
            }
            if *is_request {
                let id = 1000 + requests.len() as u64;
                requests.push(id);
                client.send_raw(&format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#
                ));
            } else {
                client.send_raw(&format!(
                    r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#
                ));
            }
        }
        client.of("textDocument/documentSymbol", URI);
        let probe = client.last_id();
        client.shutdown();
        let (result, out) = run(client.bytes());
        prop_assert!(result.is_ok(), "{result:?}");
        for id in &requests {
            prop_assert_eq!(answer_to(&out, &json!(*id)), 1, "request {}", id);
        }
        prop_assert_eq!(answer_to(&out, &json!(probe)), 1);
        // Every response answers a request that was sent.
        let sent: BTreeSet<u64> = requests.iter().copied().chain(1..=probe + 1).collect();
        for message in &out {
            if message.get("method").is_none() {
                let id = &message["id"];
                prop_assert!(id.is_null() || id.as_u64().is_some_and(|n| sent.contains(&n)), "{}", message);
            }
        }
    }

    #[test]
    fn prop_random_edits_with_random_positions_keep_the_session_alive(
        edits in prop::collection::vec(
            (any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>(), "[a-z() \n😀]{0,6}"),
            0..12,
        ),
    ) {
        let mut client = Client::new();
        client.initialize(&[]).open(URI, "(define x 1)\n(+ x 2)\n");
        for (version, (a, b, c, d, text)) in edits.iter().enumerate() {
            client.notify(
                "textDocument/didChange",
                json!({ "textDocument": { "uri": URI, "version": version + 2 }, "contentChanges": [{
                    "range": { "start": { "line": *a % 6, "character": *b % 20 },
                               "end": { "line": *c % 6, "character": *d % 20 } },
                    "text": text.clone() }] }),
            );
        }
        client.of("textDocument/semanticTokens/full", URI);
        let probe = client.last_id();
        client.shutdown();
        let (result, out) = run(client.bytes());
        prop_assert!(result.is_ok(), "{result:?}");
        prop_assert_eq!(answer_to(&out, &json!(probe)), 1);
        prop_assert!(out.iter().any(|m| m["id"] == probe && m.get("result").is_some()));
    }
}
