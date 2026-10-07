//! End-to-end behaviour of `Server`: lifecycle, framing, error responses,
//! document synchronization, diagnostics, and server-to-client requests —
//! driven through byte streams exactly as an editor would.

#[path = "../examples/common/mod.rs"]
mod common;

use std::io::{self, Write};

use common::{Client, Lisp, LispServer, messages};
use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
use lsp_lang::{
    Context, Document, Error, Handler, Json, Position, Reply, ResponseError, Server, Value, json,
};

const URI: &str = "file:///t.lisp";

/// Records what the server tells it, and answers as configured.
#[derive(Default)]
struct Recorder {
    /// `(uri, version, text)` at every `changed` call.
    changes: Vec<(String, i32, String)>,
    closed: Vec<String>,
    notifications: Vec<String>,
    responses: Vec<(u64, Result<String, i32>)>,
    /// What `initialize` returns; `None` for an error.
    capabilities: Option<Value>,
    /// Requests to send on `initialized`.
    ask: usize,
}

impl Handler<Lisp> for Recorder {
    fn initialize(
        &mut self,
        _: Json<'_>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Value, ResponseError> {
        self.capabilities
            .clone()
            .ok_or_else(|| ResponseError::new(ResponseError::REQUEST_FAILED, "not today"))
    }

    fn changed(&mut self, doc: &Document<Lisp>, _: &mut Context<'_, Lisp>) {
        self.changes
            .push((doc.uri().to_owned(), doc.version(), doc.text().to_owned()));
    }

    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Lisp>) {
        self.closed.push(uri.to_owned());
        cx.clear_diagnostics(uri);
    }

    fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, Lisp>) {
        self.notifications.push(method.to_owned());
        if method == "initialized" {
            for _ in 0..self.ask {
                assert!(
                    cx.request("workspace/configuration", &json!({ "items": [] }))
                        .is_ok()
                );
            }
        }
    }

    fn response(
        &mut self,
        id: u64,
        result: Result<Json<'_>, ResponseError>,
        _: &mut Context<'_, Lisp>,
    ) {
        self.responses.push((
            id,
            result.map(|j| j.as_str().to_owned()).map_err(|e| e.code()),
        ));
    }

    fn request(
        &mut self,
        method: &str,
        params: Json<'_>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Reply, ResponseError> {
        match method {
            "test/echo" => Ok(Reply::new(&params.parse::<Value>()?)?),
            "test/position" => {
                let p: (String, u32) = params.parse()?;
                let doc = cx
                    .document(&p.0)
                    .ok_or_else(|| ResponseError::new(ResponseError::REQUEST_FAILED, "not open"))?;
                Ok(Reply::new(&doc.position(p.1))?)
            }
            "test/unserializable" => {
                let mut map = std::collections::BTreeMap::new();
                let _previous = map.insert((1, 2), 3);
                Ok(Reply::new(&map)?)
            }
            "test/notify-unserializable" => {
                let mut map = std::collections::BTreeMap::new();
                let _previous = map.insert((1, 2), 3);
                match cx.notify("x", &map) {
                    Err(Error::Json(_)) => Ok(Reply::null()),
                    other => Err(ResponseError::new(-1, format!("{other:?}"))),
                }
            }
            _ => Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method)),
        }
    }
}

fn recorder() -> Recorder {
    Recorder {
        capabilities: Some(json!({})),
        ..Recorder::default()
    }
}

/// Runs one session; returns the server (for its handler), the outcome, and
/// the messages the server sent.
fn run<H: Handler<Lisp>>(
    handler: H,
    client: &Client,
) -> (Server<Lisp, H>, Result<(), Error>, Vec<Value>) {
    let mut server = Server::new(Lisp, handler);
    let mut output = Vec::new();
    let result = server.run(client.bytes(), &mut output);
    (server, result, messages(&output))
}

fn response(messages: &[Value], id: u64) -> &Value {
    messages
        .iter()
        .find(|m| m["id"] == id && m.get("method").is_none())
        .unwrap_or(&Value::Null)
}

fn error_code(messages: &[Value], id: u64) -> Option<i64> {
    response(messages, id)["error"]["code"].as_i64()
}

fn logs(messages: &[Value]) -> Vec<(i64, String)> {
    messages
        .iter()
        .filter(|m| m["method"] == "window/logMessage")
        .map(|m| {
            (
                m["params"]["type"].as_i64().unwrap_or(0),
                m["params"]["message"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

fn published(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .map(|m| &m["params"])
        .collect()
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn test_run_clean_shutdown_returns_ok() {
    let mut client = Client::new();
    client.initialize(&[]).shutdown();
    let (_, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert_eq!(response(&out, 2)["result"], Value::Null);
    assert!(response(&out, 2).get("result").is_some());
}

#[test]
fn test_run_exit_without_shutdown_returns_exit_without_shutdown() {
    let mut client = Client::new();
    client.initialize(&[]).notify("exit", Value::Null);
    let (_, result, _) = run(recorder(), &client);
    assert!(matches!(result, Err(Error::ExitWithoutShutdown)));

    // Before initialize, too.
    let mut client = Client::new();
    client.notify("exit", Value::Null);
    assert!(matches!(
        run(recorder(), &client).1,
        Err(Error::ExitWithoutShutdown)
    ));
}

#[test]
fn test_run_input_ends_before_exit_returns_disconnected() {
    let mut client = Client::new();
    client.initialize(&[]);
    assert!(matches!(
        run(recorder(), &client).1,
        Err(Error::Disconnected)
    ));
    assert!(matches!(
        run(recorder(), &Client::new()).1,
        Err(Error::Disconnected)
    ));
}

#[test]
fn test_request_before_initialize_returns_server_not_initialized() {
    let mut client = Client::new();
    client
        .request("test/echo", json!(1))
        .initialize(&[])
        .shutdown();
    let (_, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert_eq!(
        error_code(&out, 1),
        Some(i64::from(ResponseError::SERVER_NOT_INITIALIZED))
    );
}

#[test]
fn test_notification_before_initialize_is_dropped() {
    let mut client = Client::new();
    client
        .open(URI, "(a)")
        .notify("custom/early", json!({}))
        .initialize(&[])
        .shutdown();
    let (server, result, _) = run(recorder(), &client);
    assert!(result.is_ok());
    assert!(server.handler().changes.is_empty());
    assert_eq!(server.handler().notifications, ["initialized"]);
}

#[test]
fn test_initialize_twice_returns_invalid_request() {
    let mut client = Client::new();
    client.initialize(&[]).initialize(&[]).shutdown();
    let (_, _, out) = run(recorder(), &client);
    assert!(response(&out, 1)["result"]["capabilities"].is_object());
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::INVALID_REQUEST))
    );
}

#[test]
fn test_request_after_shutdown_returns_invalid_request() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("shutdown", Value::Null)
        .request("test/echo", json!(1))
        .open(URI, "(a)") // ignored after shutdown
        .notify("exit", Value::Null);
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert_eq!(
        error_code(&out, 3),
        Some(i64::from(ResponseError::INVALID_REQUEST))
    );
    assert!(server.handler().changes.is_empty());
}

#[test]
fn test_initialize_negotiates_utf8_only_when_offered() {
    let encoding = |offered: &[&str]| {
        let mut client = Client::new();
        client.initialize(offered).shutdown();
        let (_, _, out) = run(recorder(), &client);
        response(&out, 1)["result"]["capabilities"]["positionEncoding"].clone()
    };
    assert_eq!(encoding(&["utf-32", "utf-8"]), "utf-8");
    assert_eq!(encoding(&["utf-32"]), "utf-16");
    assert_eq!(encoding(&[]), "utf-16");
}

#[test]
fn test_initialize_accepts_null_or_absent_params() {
    for params in [Value::Null, json!({})] {
        let mut client = Client::new();
        client.request("initialize", params).shutdown();
        let (_, result, out) = run(recorder(), &client);
        assert!(result.is_ok());
        assert_eq!(
            response(&out, 1)["result"]["capabilities"]["positionEncoding"],
            "utf-16"
        );
    }
}

#[test]
fn test_initialize_rejects_malformed_params() {
    let mut client = Client::new();
    client.request(
        "initialize",
        json!({ "capabilities": { "general": { "positionEncodings": 7 } } }),
    );
    let (_, _, out) = run(recorder(), &client);
    assert_eq!(
        error_code(&out, 1),
        Some(i64::from(ResponseError::INVALID_PARAMS))
    );
}

#[test]
fn test_initialize_merges_handler_capabilities_with_server_owned_ones() {
    let mut handler = recorder();
    handler.capabilities = Some(json!({
        "hoverProvider": true,
        "positionEncoding": "utf-32",
        "textDocumentSync": { "save": true, "change": 1 },
    }));
    let mut client = Client::new();
    client.initialize(&[]).shutdown();
    let (_, _, out) = run(handler, &client);
    let caps = &response(&out, 1)["result"]["capabilities"];
    assert_eq!(caps["hoverProvider"], true);
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(
        caps["textDocumentSync"],
        json!({ "save": true, "change": 2, "openClose": true })
    );
}

#[test]
fn test_initialize_non_object_capabilities_are_treated_as_empty() {
    for weird in [json!(null), json!([1]), json!({ "textDocumentSync": 1 })] {
        let mut handler = recorder();
        handler.capabilities = Some(weird);
        let mut client = Client::new();
        client.initialize(&[]).shutdown();
        let (_, _, out) = run(handler, &client);
        let caps = &response(&out, 1)["result"]["capabilities"];
        assert_eq!(
            caps["textDocumentSync"],
            json!({ "change": 2, "openClose": true })
        );
    }
}

#[test]
fn test_initialize_handler_error_keeps_session_uninitialized() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("test/echo", json!(1))
        .shutdown();
    let mut handler = recorder();
    handler.capabilities = None;
    let (_, result, out) = run(handler, &client);
    assert_eq!(
        error_code(&out, 1),
        Some(i64::from(ResponseError::REQUEST_FAILED))
    );
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::SERVER_NOT_INITIALIZED))
    );
    // `shutdown` is refused too, so `exit` is unclean.
    assert_eq!(
        error_code(&out, 3),
        Some(i64::from(ResponseError::SERVER_NOT_INITIALIZED))
    );
    assert!(matches!(result, Err(Error::ExitWithoutShutdown)));
}

#[test]
fn test_each_run_is_a_fresh_session_with_the_same_handler() {
    let mut server = Server::new(Lisp, recorder());
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)").shutdown();
    assert!(server.run(client.bytes(), Vec::new()).is_ok());

    // The second session never opens the document, so it is not there.
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("test/position", json!([URI, 0]))
        .shutdown();
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    let out = messages(&output);
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::REQUEST_FAILED))
    );
    assert_eq!(server.handler().changes.len(), 1);
}

// ---------------------------------------------------------------------------
// Messages and errors
// ---------------------------------------------------------------------------

#[test]
fn test_malformed_json_returns_parse_error_and_session_continues() {
    let mut client = Client::new();
    client.initialize(&[]);
    client.send_raw("{\"jsonrpc\": \"2.0\", \"id\": 9, \"method\": ");
    client.request("test/echo", json!("still here")).shutdown();
    let (_, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let parse_error = out
        .iter()
        .find(|m| m["error"]["code"] == ResponseError::PARSE_ERROR);
    assert_eq!(parse_error.map(|m| m["id"].clone()), Some(Value::Null));
    assert_eq!(response(&out, 2)["result"], "still here");
}

#[test]
fn test_batch_and_shapeless_messages_return_invalid_request_with_null_id() {
    let mut client = Client::new();
    client.initialize(&[]);
    client.send_raw("[1, 2]");
    client.send_raw(r#"{"jsonrpc":"2.0"}"#);
    // An id without a method is an answer to the server; it is never answered.
    client.send_raw(r#"{"jsonrpc":"2.0","id":"x"}"#);
    client.shutdown();
    let (_, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let invalid: Vec<&Value> = out
        .iter()
        .filter(|m| m["error"]["code"] == ResponseError::INVALID_REQUEST)
        .map(|m| &m["id"])
        .collect();
    assert_eq!(invalid, [&Value::Null, &Value::Null]);
    assert!(out.iter().all(|m| m["id"] != "x"));
    let logs = logs(&out);
    assert_eq!(logs.len(), 1);
    assert!(logs[0].1.contains("\"x\""));
}

#[test]
fn test_request_with_invalid_id_is_refused_not_dispatched() {
    let mut client = Client::new();
    client.initialize(&[]);
    for id in ["null", "1.5", "{\"a\":1}", "true", "[1]"] {
        client.send_raw(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"test/echo","params":"leaked"}}"#
        ));
    }
    // A `shutdown` with a null id must not shut the session down.
    client.send_raw(r#"{"jsonrpc":"2.0","id":null,"method":"shutdown"}"#);
    client.request("test/echo", json!("still running"));
    client.shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let refused: Vec<&Value> = out
        .iter()
        .filter(|m| m["error"]["code"] == ResponseError::INVALID_REQUEST)
        .map(|m| &m["id"])
        .collect();
    assert_eq!(refused.len(), 6);
    assert!(refused.iter().all(|id| id.is_null()));
    assert!(out.iter().all(|m| m["result"] != "leaked"));
    assert_eq!(response(&out, 2)["result"], "still running");
    assert_eq!(server.handler().notifications, ["initialized"]);
}

#[test]
fn test_unknown_request_returns_method_not_found() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("textDocument/rename", json!({}))
        .shutdown();
    let (_, _, out) = run(LispServer::default(), &client);
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::METHOD_NOT_FOUND))
    );
}

#[test]
fn test_request_id_is_echoed_verbatim() {
    let mut client = Client::new();
    client.initialize(&[]);
    client
        .send(&json!({ "jsonrpc": "2.0", "id": "abc-1", "method": "test/echo", "params": [true] }));
    client.send(&json!({ "jsonrpc": "2.0", "id": -5, "method": "test/echo" }));
    client.shutdown();
    let (_, _, out) = run(recorder(), &client);
    let by_id = |id: Value| {
        out.iter()
            .find(|m| m["id"] == id)
            .map(|m| m["result"].clone())
    };
    assert_eq!(by_id(json!("abc-1")), Some(json!([true])));
    assert_eq!(by_id(json!(-5)), Some(Value::Null)); // absent params read as null
}

#[test]
fn test_invalid_params_returns_invalid_params() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("textDocument/hover", json!({ "position": "nowhere" }))
        .shutdown();
    let (_, _, out) = run(LispServer::default(), &client);
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::INVALID_PARAMS))
    );
}

#[test]
fn test_unserializable_reply_returns_internal_error() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("test/unserializable", Value::Null)
        .shutdown();
    let (_, _, out) = run(recorder(), &client);
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::INTERNAL_ERROR))
    );
}

#[test]
fn test_unserializable_notification_sends_nothing() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("test/notify-unserializable", Value::Null)
        .shutdown();
    let (_, _, out) = run(recorder(), &client);
    assert!(response(&out, 2)["result"].is_null());
    assert!(out.iter().all(|m| m["method"] != "x"));
}

#[test]
fn test_error_message_is_escaped_json() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("weird \"name\"\n", Value::Null)
        .shutdown();
    let (_, _, out) = run(recorder(), &client);
    assert_eq!(response(&out, 2)["error"]["message"], "weird \"name\"\n");
}

// ---------------------------------------------------------------------------
// Framing and transport
// ---------------------------------------------------------------------------

#[test]
fn test_invalid_header_ends_the_session() {
    let mut server = Server::new(Lisp, recorder());
    let input = b"Content-Length: twelve\r\n\r\n{}";
    assert!(matches!(
        server.run(&input[..], Vec::new()),
        Err(Error::InvalidHeader { .. })
    ));
    let input = b"Content-Type: x\r\n\r\n{}";
    assert!(matches!(
        server.run(&input[..], Vec::new()),
        Err(Error::InvalidHeader { .. })
    ));
}

#[test]
fn test_message_over_limit_returns_too_large_and_reads_no_body() {
    let mut client = Client::new();
    client.initialize(&[]);
    let mut server = Server::new(Lisp, recorder()).with_message_limit(32);
    let result = server.run(client.bytes(), Vec::new());
    assert!(matches!(result, Err(Error::TooLarge { limit: 32, .. })));
}

#[test]
fn test_failing_output_returns_io_error() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut client = Client::new();
    client.initialize(&[]).shutdown();
    let mut server = Server::new(Lisp, recorder());
    assert!(matches!(
        server.run(client.bytes(), Broken),
        Err(Error::Io(_))
    ));
}

#[test]
fn test_turns_without_output_write_nothing() {
    // Counts writes: one per turn that produced output, none otherwise.
    struct Counting(usize);
    impl Write for Counting {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 += 1;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)");
    for v in 2..12 {
        client.edit(URI, v, (0, 1), (0, 1), "x");
    }
    client.shutdown();
    let mut sink = Counting(0);
    let mut server = Server::new(Lisp, recorder());
    assert!(server.run(client.bytes(), &mut sink).is_ok());
    // initialize and shutdown responses only; the recorder publishes nothing.
    assert_eq!(sink.0, 2);
}

// ---------------------------------------------------------------------------
// Document synchronization
// ---------------------------------------------------------------------------

#[test]
fn test_did_change_ranged_edits_track_the_client_text() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(define 😀 1)\n(+ 😀 2)")
        .edit(URI, 2, (1, 3), (1, 5), "x") // replace the emoji (2 UTF-16 units)
        .edit(URI, 3, (0, 0), (0, 0), "; hi\n")
        .shutdown();
    let (server, result, _) = run(recorder(), &client);
    assert!(result.is_ok());
    let changes = &server.handler().changes;
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes[1],
        (URI.to_owned(), 2, "(define 😀 1)\n(+ x 2)".to_owned())
    );
    assert_eq!(changes[2].2, "; hi\n(define 😀 1)\n(+ x 2)");
    assert_eq!(changes[2].1, 3);
}

#[test]
fn test_did_change_utf8_session_counts_bytes() {
    let mut client = Client::new();
    client
        .initialize(&["utf-8"])
        .open(URI, "(é é)")
        .edit(URI, 2, (0, 4), (0, 6), "e") // the second `é`: bytes 4..6
        .shutdown();
    let (server, _, _) = run(recorder(), &client);
    assert_eq!(server.handler().changes[1].2, "(é e)");
}

#[test]
fn test_did_change_without_range_replaces_the_text() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(old)");
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 9 }, "contentChanges": [{ "text": "(new)\n" }] }),
    );
    client.shutdown();
    let (server, _, _) = run(recorder(), &client);
    assert_eq!(
        server.handler().changes[1],
        (URI.to_owned(), 9, "(new)\n".to_owned())
    );
}

#[test]
fn test_did_change_several_edits_apply_in_order_with_one_call() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "abc");
    let edit = |line: u32, from: u32, to: u32, text: &str| json!({ "range": { "start": { "line": line, "character": from }, "end": { "line": line, "character": to } }, "text": text });
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": URI, "version": 2 },
            "contentChanges": [edit(0, 3, 3, "\nd"), edit(1, 0, 1, "D"), edit(0, 0, 1, "")],
        }),
    );
    client.shutdown();
    let (server, _, _) = run(recorder(), &client);
    let changes = &server.handler().changes;
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].2, "bc\nD");
}

#[test]
fn test_did_change_out_of_range_positions_are_clamped() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "ab\ncd")
        .edit(URI, 2, (0, 99), (0, 99), "!") // past the end of line 0
        .edit(URI, 3, (50, 0), (60, 0), "?") // past the last line
        .shutdown();
    let (server, _, _) = run(recorder(), &client);
    assert_eq!(server.handler().changes[2].2, "ab!\ncd?");
}

#[test]
fn test_did_change_unknown_document_logs_a_warning() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .edit("file:///nope", 2, (0, 0), (0, 0), "x")
        .shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert!(server.handler().changes.is_empty());
    let logs = logs(&out);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].0, 2);
    assert!(logs[0].1.contains("file:///nope"));
}

#[test]
fn test_malformed_sync_notifications_log_errors() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": URI } }),
        )
        .notify("textDocument/didChange", json!([]))
        .notify("textDocument/didClose", json!({}))
        .shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert!(server.handler().changes.is_empty());
    let logs = logs(&out);
    assert_eq!(logs.len(), 3);
    assert!(logs.iter().all(|(kind, _)| *kind == 1));
    assert!(logs[0].1.contains("textDocument/didOpen"));
}

#[test]
fn test_did_close_removes_the_document_and_clears_diagnostics() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(+ x 1)")
        .notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": URI } }),
        )
        .at("textDocument/hover", URI, 0, 3)
        // Closing a document that is not open is a no-op.
        .notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": URI } }),
        )
        .shutdown();
    let (_, result, out) = run(LispServer::default(), &client);
    assert!(result.is_ok());
    let published = published(&out);
    assert_eq!(published.len(), 2);
    assert_eq!(
        published[0]["diagnostics"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(published[1], &json!({ "uri": URI, "diagnostics": [] }));
    assert!(response(&out, 2)["result"].is_null());
}

#[test]
fn test_did_open_again_replaces_the_document() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(first)")
        .open(URI, "(second)")
        .shutdown();
    let (server, _, _) = run(recorder(), &client);
    let texts: Vec<&str> = server
        .handler()
        .changes
        .iter()
        .map(|c| c.2.as_str())
        .collect();
    assert_eq!(texts, ["(first)", "(second)"]);
}

#[test]
fn test_documents_are_independent() {
    let other = "file:///other.lisp";
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .open(other, "(b)")
        .edit(other, 2, (0, 1), (0, 2), "c")
        .request("test/position", json!([URI, 2]))
        .request("test/position", json!([other, 99]))
        .shutdown();
    let (server, _, out) = run(recorder(), &client);
    assert_eq!(
        server.handler().changes[2],
        (other.to_owned(), 2, "(c)".to_owned())
    );
    assert_eq!(response(&out, 2)["result"], json!(Position::new(0, 2)));
    assert_eq!(response(&out, 3)["result"], json!(Position::new(0, 3)));
}

/// A grammar whose lexer breaks the contract on `!`: it leaves the character
/// out of every token, so the tokens no longer tile the text.
#[derive(Clone, Copy)]
struct Gappy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum G {
    Root,
    Char,
}

impl TokenKind for G {}

impl Grammar for Gappy {
    type Kind = G;
    const ROOT: G = G::Root;
    fn lex(&self, text: &str, tokens: &mut Vec<Token<G>>) {
        for (i, c) in text.char_indices().filter(|&(_, c)| c != '!') {
            tokens.push(Token::new(
                G::Char,
                Span::new(i as u32, (i + c.len_utf8()) as u32),
            ));
        }
    }
    fn parse(&self, _: G, b: &mut Builder<'_, G>) {
        b.start(G::Root);
        while !b.at_end() {
            b.bump();
        }
        b.finish();
    }
}

#[derive(Default)]
struct GappyRecorder {
    changed: usize,
    closed: Vec<String>,
}

impl Handler<Gappy> for GappyRecorder {
    fn changed(&mut self, _: &Document<Gappy>, _: &mut Context<'_, Gappy>) {
        self.changed += 1;
    }
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Gappy>) {
        self.closed.push(uri.to_owned());
        cx.clear_diagnostics(uri);
    }
}

#[test]
fn test_unapplicable_edit_drops_the_document_logs_and_calls_closed() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "abc")
        .edit(URI, 2, (0, 1), (0, 1), "!")
        .edit(URI, 3, (0, 0), (0, 0), "z") // the document is gone now
        .shutdown();
    let mut server = Server::new(Gappy, GappyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    assert_eq!(server.handler().changed, 1);
    assert_eq!(server.handler().closed, [URI]);
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].0, 1);
    assert!(logs[0].1.contains("could not be applied"));
    assert_eq!(logs[1].0, 2); // the later change targets a closed document
}

#[test]
fn test_failed_reopen_is_logged_and_drops_the_open_version() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "ok")
        .open(URI, "not ok!")
        .shutdown();
    let mut server = Server::new(Gappy, GappyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    // The first version opened; the failed reopen dropped it and said so.
    assert_eq!(server.handler().changed, 1);
    assert_eq!(server.handler().closed, [URI]);
    let out = messages(&output);
    assert!(logs(&out)[0].1.contains("could not open"));
}

/// A grammar whose lexer panics on `#` — a bug in a language's grammar.
#[derive(Clone, Copy)]
struct Panicky;

impl Grammar for Panicky {
    type Kind = G;
    const ROOT: G = G::Root;
    fn lex(&self, text: &str, tokens: &mut Vec<Token<G>>) {
        assert!(!text.contains('#'), "the lexer cannot handle `#`");
        Gappy.lex(text, tokens);
    }
    fn parse(&self, kind: G, b: &mut Builder<'_, G>) {
        Gappy.parse(kind, b);
    }
}

#[derive(Default)]
struct PanickyRecorder {
    changed: usize,
    closed: Vec<String>,
}

impl Handler<Panicky> for PanickyRecorder {
    fn changed(&mut self, _: &Document<Panicky>, _: &mut Context<'_, Panicky>) {
        self.changed += 1;
    }
    fn closed(&mut self, uri: &str, _: &mut Context<'_, Panicky>) {
        self.closed.push(uri.to_owned());
    }
}

#[test]
fn test_grammar_panic_costs_the_document_not_the_session() {
    let other = "file:///other.lisp";
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "abc")
        .edit(URI, 2, (0, 1), (0, 1), "#") // panics while reparsing
        .open(other, "a # b") // panics while opening
        .open(other, "fine")
        .request("test/echo", json!(1))
        .shutdown();
    let mut server = Server::new(Panicky, PanickyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    assert_eq!(server.handler().changed, 2); // `abc`, then `fine`
    assert_eq!(server.handler().closed, [URI]);
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert!(
        logs.iter()
            .all(|(kind, text)| *kind == 1 && text.contains("panicked"))
    );
    // The session carried on: requests are still answered.
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::METHOD_NOT_FOUND))
    );
}

// ---------------------------------------------------------------------------
// The Lisp server end to end: diagnostics, hover, definition
// ---------------------------------------------------------------------------

#[test]
fn test_lisp_server_publishes_versioned_diagnostics() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(define a 1)\n(define a 2)\n(print b))")
        .edit(URI, 2, (2, 7), (2, 8), "a")
        .shutdown();
    let (_, result, out) = run(LispServer::default(), &client);
    assert!(result.is_ok());
    let published = published(&out);
    assert_eq!(published.len(), 2);
    assert_eq!(published[0]["version"], 1);
    let first: Vec<&str> = published[0]["diagnostics"]
        .as_array()
        .map(|d| d.iter().filter_map(|d| d["message"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(first.len(), 3);
    assert!(first.iter().any(|m| m.starts_with("unexpected `)`")));
    assert!(
        first
            .iter()
            .any(|m| m.contains("`a`") && m.contains("help: remove or rename"))
    );
    assert!(first.iter().any(|m| m.starts_with("cannot find `b`")));
    // The duplicate links to the first definition.
    let duplicate = published[0]["diagnostics"]
        .as_array()
        .and_then(|d| d.iter().find(|d| d.get("relatedInformation").is_some()))
        .map(|d| d["relatedInformation"][0]["location"]["range"]["start"].clone());
    assert_eq!(duplicate, Some(json!({ "line": 0, "character": 8 })));
    assert_eq!(published[1]["version"], 2);
    assert_eq!(
        published[1]["diagnostics"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn test_lisp_server_hover_and_definition() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "; c\n(define size 3)\n(* size 2)")
        .at("textDocument/hover", URI, 2, 5) // inside `size`
        .at("textDocument/definition", URI, 2, 7) // just after `size`
        .at("textDocument/hover", URI, 2, 1) // `*`
        .at("textDocument/definition", URI, 2, 1) // a builtin has no definition
        .at("textDocument/hover", "file:///unknown", 0, 0)
        .shutdown();
    let (_, _, out) = run(LispServer::default(), &client);
    let hover = &response(&out, 2)["result"];
    assert_eq!(hover["contents"]["value"], "`size`: defined on line 2");
    assert_eq!(
        hover["range"],
        json!({ "start": { "line": 2, "character": 3 }, "end": { "line": 2, "character": 7 } })
    );
    assert_eq!(
        response(&out, 3)["result"],
        json!({ "uri": URI, "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 12 } } })
    );
    assert_eq!(
        response(&out, 4)["result"]["contents"]["value"],
        "`*`: builtin"
    );
    assert!(response(&out, 5)["result"].is_null());
    assert!(response(&out, 6)["result"].is_null());
}

// ---------------------------------------------------------------------------
// Server-to-client requests
// ---------------------------------------------------------------------------

#[test]
fn test_context_request_numbers_ids_and_routes_responses() {
    let mut handler = recorder();
    handler.ask = 2;
    let mut client = Client::new();
    client.initialize(&[]);
    client.send(&json!({ "jsonrpc": "2.0", "id": 1, "result": [{ "a": 1 }] }));
    client
        .send(&json!({ "jsonrpc": "2.0", "id": 0, "error": { "code": -32603, "message": "no" } }));
    // A second answer to id 0, ids the server never sent, and an answer with
    // no id are all logged and dropped.
    client.send(&json!({ "jsonrpc": "2.0", "id": 0 , "result": null }));
    client.send(&json!({ "jsonrpc": "2.0", "id": "zero", "result": 0 }));
    client.send(&json!({ "jsonrpc": "2.0", "id": 7, "result": 0 }));
    client.send(&json!({ "jsonrpc": "2.0", "error": { "code": -32700, "message": "parse" } }));
    client.shutdown();
    let (server, result, out) = run(handler, &client);
    assert!(result.is_ok());

    let sent: Vec<&Value> = out
        .iter()
        .filter(|m| m["method"] == "workspace/configuration")
        .map(|m| &m["id"])
        .collect();
    assert_eq!(sent, [&json!(0), &json!(1)]);
    assert_eq!(
        server.handler().responses,
        [(1, Ok(r#"[{"a":1}]"#.to_owned())), (0, Err(-32603))]
    );
    let warnings = logs(&out);
    assert_eq!(warnings.len(), 4);
    assert!(warnings.iter().all(|(kind, _)| *kind == 2));
    // Nothing that looks like a response is ever answered.
    assert!(out.iter().all(|m| m.get("error").is_none()));
}

#[test]
fn test_client_answers_are_read_leniently() {
    let mut handler = recorder();
    handler.ask = 4;
    let mut client = Client::new();
    client.initialize(&[]);
    client.send_raw(r#"{"jsonrpc":"2.0","id":0,"result":null}"#);
    client.send_raw(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601}}"#);
    client.send_raw(r#"{"jsonrpc":"2.0","id":2,"error":{"code":1e3,"message":"odd"}}"#);
    client.send_raw(r#"{"jsonrpc":"2.0","id":3,"error":null}"#);
    client.shutdown();
    let (server, result, out) = run(handler, &client);
    assert!(result.is_ok());
    assert_eq!(
        server.handler().responses,
        [
            (0, Ok("null".to_owned())),
            (1, Err(ResponseError::METHOD_NOT_FOUND)),
            (2, Err(ResponseError::INTERNAL_ERROR)),
            (3, Err(ResponseError::INTERNAL_ERROR)),
        ]
    );
    assert!(out.iter().all(|m| m.get("error").is_none()));
}

#[test]
fn test_answer_before_any_request_is_dropped() {
    let mut handler = recorder();
    handler.ask = 1;
    let mut client = Client::new();
    // Before `initialize`, the server has asked nothing; this is not an answer.
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": "early" }));
    client.initialize(&[]);
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": "on time" }));
    client.shutdown();
    let (server, result, out) = run(handler, &client);
    assert!(result.is_ok());
    assert_eq!(
        server.handler().responses,
        [(0, Ok("\"on time\"".to_owned()))]
    );
    assert_eq!(logs(&out).len(), 1);
}
