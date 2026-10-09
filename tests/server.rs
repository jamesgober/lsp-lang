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
            "test/text" => {
                let uri: String = params.parse()?;
                let doc = cx
                    .document(&uri)
                    .ok_or_else(|| ResponseError::new(ResponseError::REQUEST_FAILED, "not open"))?;
                Ok(Reply::new(&(doc.version(), doc.text()))?)
            }
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
fn test_did_change_out_of_range_columns_clamp_but_missing_lines_go_out_of_step() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "ab\ncd")
        .edit(URI, 2, (0, 99), (0, 99), "!") // past the end of line 0: clamped
        .edit(URI, 3, (2, 0), (2, 9), "?") // the line just past the last: the end
        .edit(URI, 4, (50, 0), (60, 0), "X") // lines this copy does not have
        .edit(URI, 5, (0, 0), (0, 0), "Y") // ignored while out of step
        .request("test/text", json!(URI))
        .shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let texts: Vec<&str> = server
        .handler()
        .changes
        .iter()
        .map(|c| c.2.as_str())
        .collect();
    assert_eq!(texts, ["ab\ncd", "ab!\ncd", "ab!\ncd?"]);
    // The document is kept at its last good state, not dropped.
    assert!(server.handler().closed.is_empty());
    assert_eq!(response(&out, 2)["result"], json!([3, "ab!\ncd?"]));
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|(kind, _)| *kind == 2));
    assert!(logs[0].1.contains("out of step") && logs[0].1.contains("does not have"));
    assert!(logs[1].1.contains("ignored a change"));
}

#[test]
fn test_out_of_step_document_resyncs_on_a_whole_text_change() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .edit(URI, 2, (9, 0), (9, 0), "lost") // out of step from here
        .edit(URI, 3, (0, 0), (0, 0), "x"); // ignored
    // The whole text brings it back in step, and edits after it in the same
    // change apply on top of it; the edit before it is superseded.
    let ranged = json!({ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "text": ";" });
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 4 }, "contentChanges": [ranged, { "text": "(b)" }, ranged] }),
    );
    client
        .edit(URI, 5, (0, 4), (0, 4), "!")
        .request("test/text", json!(URI))
        .shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let changes: Vec<(i32, &str)> = server
        .handler()
        .changes
        .iter()
        .map(|c| (c.1, c.2.as_str()))
        .collect();
    assert_eq!(changes, [(1, "(a)"), (4, ";(b)"), (5, ";(b)!")]);
    assert_eq!(response(&out, 2)["result"], json!([5, ";(b)!"]));
}

#[test]
fn test_did_change_with_a_stale_version_goes_out_of_step() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .edit(URI, 2, (0, 1), (0, 2), "b")
        .edit(URI, 2, (0, 1), (0, 2), "c") // a repeat of version 2
        .edit(URI, 3, (0, 1), (0, 2), "d") // ignored: out of step
        .request("test/text", json!(URI))
        .shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert_eq!(server.handler().changes.len(), 2);
    assert_eq!(response(&out, 2)["result"], json!([2, "(b)"]));
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert!(logs[0].1.contains("version 2 arrived after version 2"));
    assert_eq!(logs[0].0, 2);
}

#[test]
fn test_did_change_failing_partway_restores_the_starting_text() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "abc");
    let edit = |line: u32, character: u32, text: &str| json!({ "range": { "start": { "line": line, "character": character }, "end": { "line": line, "character": character } }, "text": text });
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": URI, "version": 2 },
            "contentChanges": [edit(0, 0, "1"), edit(0, 0, "2"), edit(7, 0, "lost")],
        }),
    );
    client.request("test/text", json!(URI)).shutdown();
    let (server, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    assert_eq!(server.handler().changes.len(), 1);
    // Not "21abc": the first two edits were rolled back with the third.
    assert_eq!(response(&out, 2)["result"], json!([1, "abc"]));
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
    changed: Vec<(i32, String)>,
    closed: Vec<String>,
}

impl Handler<Gappy> for GappyRecorder {
    fn changed(&mut self, doc: &Document<Gappy>, _: &mut Context<'_, Gappy>) {
        self.changed.push((doc.version(), doc.text().to_owned()));
    }
    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Gappy>) {
        self.closed.push(uri.to_owned());
        cx.clear_diagnostics(uri);
    }
}

#[test]
fn test_unparsable_edit_keeps_the_last_good_state_until_resynced() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "abc")
        .edit(URI, 2, (0, 1), (0, 1), "!") // the grammar cannot lex `!`
        .edit(URI, 3, (0, 0), (0, 0), "z"); // ignored: out of step
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 4 }, "contentChanges": [{ "text": "fine" }] }),
    );
    client.shutdown();
    let mut server = Server::new(Gappy, GappyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    // Never dropped: no `closed`, and the whole-text change brought it back.
    assert!(server.handler().closed.is_empty());
    assert_eq!(
        server.handler().changed,
        [(1, "abc".to_owned()), (4, "fine".to_owned())]
    );
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].0, 1); // a grammar bug is an error
    assert!(logs[0].1.contains("could not be parsed") && logs[0].1.contains("keeping version 1"));
    assert_eq!(logs[1].0, 2); // the ignored change is a warning
}

#[test]
fn test_failed_reopen_keeps_the_open_version_out_of_step() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "ok")
        .open(URI, "not ok!")
        .edit(URI, 2, (0, 0), (0, 0), "x") // ignored: out of step
        .open(URI, "ok again")
        .shutdown();
    let mut server = Server::new(Gappy, GappyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    assert!(server.handler().closed.is_empty());
    assert_eq!(
        server.handler().changed,
        [(1, "ok".to_owned()), (1, "ok again".to_owned())]
    );
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert!(logs[0].1.contains("could not reopen") && logs[0].1.contains("keeping version 1"));
    assert!(logs[1].1.contains("out of step"));
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
    changed: Vec<(String, i32, String)>,
    closed: Vec<String>,
}

impl Handler<Panicky> for PanickyRecorder {
    fn changed(&mut self, doc: &Document<Panicky>, _: &mut Context<'_, Panicky>) {
        self.changed
            .push((doc.uri().to_owned(), doc.version(), doc.text().to_owned()));
    }
    fn closed(&mut self, uri: &str, _: &mut Context<'_, Panicky>) {
        self.closed.push(uri.to_owned());
    }
}

#[test]
fn test_grammar_panic_costs_neither_the_document_nor_the_session() {
    let other = "file:///other.lisp";
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "abc")
        .edit(URI, 2, (0, 1), (0, 1), "#") // panics while reparsing, whole or not
        .open(other, "a # b") // panics while opening
        .edit(other, 2, (0, 0), (0, 0), "x") // ignored: it never opened
        .request("test/echo", json!(1));
    // Whole-text changes recover both.
    for (uri, text) in [(URI, "abcd"), (other, "fine")] {
        client.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": 3 }, "contentChanges": [{ "text": text }] }),
        );
    }
    client
        .notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": other } }),
        )
        .shutdown();
    let mut server = Server::new(Panicky, PanickyRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    let changed: Vec<(&str, i32, &str)> = server
        .handler()
        .changed
        .iter()
        .map(|(u, v, t)| (u.as_str(), *v, t.as_str()))
        .collect();
    assert_eq!(
        changed,
        [(URI, 1, "abc"), (URI, 3, "abcd"), (other, 3, "fine")]
    );
    assert_eq!(server.handler().closed, [other]);
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 3);
    assert!(
        logs[0]
            .1
            .contains("the grammar panicked: the lexer cannot handle `#`")
    );
    assert!(logs[1].1.contains("could not open") && logs[1].1.contains("panicked"));
    assert!(logs[2].1.contains("could not be opened"));
    // The session carried on: requests are still answered.
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::METHOD_NOT_FOUND))
    );
}

/// The Lisp grammar with a bug in its fragment path: reparsing a single list
/// on its own panics. Only incremental reparses take that path, so a full
/// reparse of the same text succeeds.
#[derive(Clone, Copy)]
struct FragileLisp;

impl Grammar for FragileLisp {
    type Kind = <Lisp as Grammar>::Kind;
    const ROOT: Self::Kind = Lisp::ROOT;
    fn lex(&self, text: &str, tokens: &mut Vec<Token<Self::Kind>>) {
        Lisp.lex(text, tokens);
    }
    fn parse(&self, kind: Self::Kind, b: &mut Builder<'_, Self::Kind>) {
        assert!(kind == Self::ROOT, "fragment reparse is broken");
        Lisp.parse(kind, b);
    }
    fn is_reparsable(&self, kind: Self::Kind) -> bool {
        Lisp.is_reparsable(kind)
    }
}

#[derive(Default)]
struct FragileRecorder {
    texts: Vec<String>,
    full_parses: Vec<u64>,
}

impl Handler<FragileLisp> for FragileRecorder {
    fn changed(&mut self, doc: &Document<FragileLisp>, _: &mut Context<'_, FragileLisp>) {
        self.texts.push(doc.text().to_owned());
        self.full_parses.push(doc.incremental().stats().full);
    }
}

#[test]
fn test_failed_incremental_reparse_falls_back_to_a_full_parse() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)\n(b)")
        .edit(URI, 2, (1, 1), (1, 2), "c") // inside a list: a fragment reparse
        .edit(URI, 3, (0, 1), (0, 1), "x")
        .shutdown();
    let mut server = Server::new(FragileLisp, FragileRecorder::default());
    let mut output = Vec::new();
    assert!(server.run(client.bytes(), &mut output).is_ok());
    assert_eq!(
        server.handler().texts,
        ["(a)\n(b)", "(a)\n(c)", "(xa)\n(c)"]
    );
    // Each edit was parsed whole, by a fresh document.
    assert_eq!(server.handler().full_parses, [1, 1, 1]);
    let out = messages(&output);
    let logs = logs(&out);
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|(kind, text)| *kind == 2
        && text.contains("fragment reparse is broken")
        && text.contains("the whole text was parsed instead")));
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

// ---------------------------------------------------------------------------
// Handler panics, initialize ordering, and the bound on outstanding requests
// ---------------------------------------------------------------------------

/// A handler that panics in the entry points named in `panic_in`, and records
/// every call that returned normally.
#[derive(Default)]
struct Bomb {
    panic_in: Vec<&'static str>,
    calls: Vec<String>,
    /// Requests to send on `initialized`.
    ask: usize,
}

impl Bomb {
    fn armed(entry_points: &[&'static str]) -> Self {
        Self {
            panic_in: entry_points.to_vec(),
            ..Self::default()
        }
    }

    fn enter(&mut self, entry: &'static str, detail: &str) {
        assert!(!self.panic_in.contains(&entry), "handler bug in {entry}");
        self.calls.push(format!("{entry} {detail}"));
    }
}

impl Handler<Lisp> for Bomb {
    fn initialize(
        &mut self,
        _: Json<'_>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Value, ResponseError> {
        let starting = json!({ "type": 3, "message": "starting" });
        assert!(cx.notify("window/logMessage", &starting).is_ok());
        self.enter("initialize", "");
        Ok(json!({ "hoverProvider": true }))
    }

    fn changed(&mut self, doc: &Document<Lisp>, _: &mut Context<'_, Lisp>) {
        self.enter("changed", doc.text());
    }

    fn closed(&mut self, uri: &str, cx: &mut Context<'_, Lisp>) {
        self.enter("closed", uri);
        cx.clear_diagnostics(uri);
    }

    fn request(
        &mut self,
        method: &str,
        _: Json<'_>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Reply, ResponseError> {
        self.enter("request", method);
        Ok(Reply::null())
    }

    fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, Lisp>) {
        if method == "initialized" {
            for _ in 0..self.ask {
                assert!(cx.request("workspace/configuration", &json!({})).is_ok());
            }
        }
        self.enter("notification", method);
    }

    fn response(
        &mut self,
        id: u64,
        result: Result<Json<'_>, ResponseError>,
        _: &mut Context<'_, Lisp>,
    ) {
        let detail = match result {
            Ok(json) => format!("{id} ok {}", json.as_str()),
            Err(e) => format!("{id} err {}", e.code()),
        };
        self.enter("response", &detail);
    }
}

/// A session that reaches every entry point once after `initialize` (with one
/// server request answered as id 0), then proves the session is alive with a
/// final request, id 3.
fn every_entry_point(client: &mut Client) {
    client
        .initialize(&[])
        .open(URI, "(a)")
        .edit(URI, 2, (0, 1), (0, 2), "b")
        .request("custom/ask", json!({}))
        .notify("custom/tell", json!({}));
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": "answer" }));
    client
        .notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": URI } }),
        )
        .request("custom/alive", json!({}))
        .shutdown();
}

#[test]
fn test_handler_panic_in_request_answers_internal_error_and_session_lives() {
    let mut client = Client::new();
    every_entry_point(&mut client);
    let mut bomb = Bomb::armed(&["request"]);
    bomb.ask = 1;
    let (server, result, out) = run(bomb, &client);
    assert!(result.is_ok());
    for id in [2, 3] {
        assert_eq!(
            error_code(&out, id),
            Some(i64::from(ResponseError::INTERNAL_ERROR))
        );
        let message = response(&out, id)["error"]["message"]
            .as_str()
            .unwrap_or("");
        assert!(message.contains("panicked") && message.contains("handler bug in request"));
    }
    // Every other entry point still ran.
    let calls = &server.handler().calls;
    assert!(calls.iter().any(|c| c == "changed (b)"));
    assert!(calls.iter().any(|c| c == "closed file:///t.lisp"));
    assert!(calls.iter().any(|c| c == "response 0 ok \"answer\""));
}

#[test]
fn test_handler_panic_in_each_notification_entry_point_is_logged() {
    for entry in ["changed", "closed", "notification", "response"] {
        let mut client = Client::new();
        every_entry_point(&mut client);
        let mut bomb = Bomb::armed(&[entry]);
        bomb.ask = 1;
        let (server, result, out) = run(bomb, &client);
        assert!(result.is_ok(), "{entry}: the session ended");
        // The session is alive to the end: the last request is answered.
        let last = response(&out, 3);
        assert!(
            last.get("result").is_some() && last.get("error").is_none(),
            "{entry}: the final request was not answered"
        );
        let errors: Vec<String> = logs(&out)
            .into_iter()
            .filter(|(kind, _)| *kind == 1)
            .map(|(_, text)| text)
            .collect();
        let expected = format!("handler bug in {entry}");
        assert!(!errors.is_empty(), "{entry}: nothing logged");
        assert!(
            errors
                .iter()
                .all(|e| e.contains("the handler panicked") && e.contains(&expected)),
            "{entry}: {errors:?}"
        );
        let calls = &server.handler().calls;
        assert!(!calls.iter().any(|c| c.starts_with(entry)), "{entry}");
        // The rest of the session's entry points ran normally.
        assert!(calls.iter().any(|c| c == "request custom/alive"), "{entry}");
    }
}

#[test]
fn test_handler_panic_in_changed_keeps_the_document() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .edit(URI, 2, (0, 1), (0, 2), "b")
        .at("textDocument/hover", URI, 0, 1)
        .shutdown();
    // The Lisp server answers hover from the document; a panicking `changed`
    // before it must not have dropped it.
    struct PanicsOnChange(LispServer);
    impl Handler<Lisp> for PanicsOnChange {
        fn changed(&mut self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
            self.0.changed(doc, cx);
            assert!(doc.version() < 2, "handler bug in changed");
        }
        fn request(
            &mut self,
            method: &str,
            params: Json<'_>,
            cx: &mut Context<'_, Lisp>,
        ) -> Result<Reply, ResponseError> {
            self.0.request(method, params, cx)
        }
    }
    let (_, result, out) = run(PanicsOnChange(LispServer::default()), &client);
    assert!(result.is_ok());
    assert_eq!(
        response(&out, 2)["result"]["contents"]["value"],
        "`b`: not defined"
    );
}

#[test]
fn test_handler_panic_in_initialize_refuses_it_and_discards_what_it_sent() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .request("custom/early", json!({}))
        .shutdown();
    let (_, result, out) = run(Bomb::armed(&["initialize"]), &client);
    assert_eq!(
        error_code(&out, 1),
        Some(i64::from(ResponseError::INTERNAL_ERROR))
    );
    assert!(
        response(&out, 1)["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("handler bug in initialize"))
    );
    // Still uninitialized, and the handler's "starting" was never written.
    assert_eq!(
        error_code(&out, 2),
        Some(i64::from(ResponseError::SERVER_NOT_INITIALIZED))
    );
    assert!(logs(&out).is_empty());
    assert!(matches!(result, Err(Error::ExitWithoutShutdown)));
}

/// Sends a notification and a request from `initialize`.
struct EagerInit {
    fail: bool,
    responses: Vec<u64>,
}

impl Handler<Lisp> for EagerInit {
    fn initialize(
        &mut self,
        _: Json<'_>,
        cx: &mut Context<'_, Lisp>,
    ) -> Result<Value, ResponseError> {
        let message = json!({ "type": 3, "message": "hi" });
        assert!(cx.notify("window/showMessage", &message).is_ok());
        assert!(cx.request("window/showMessageRequest", &message).is_ok());
        if self.fail {
            return Err(ResponseError::new(ResponseError::REQUEST_FAILED, "no"));
        }
        Ok(json!({}))
    }

    fn response(&mut self, id: u64, _: Result<Json<'_>, ResponseError>, _: &mut Context<'_, Lisp>) {
        self.responses.push(id);
    }
}

#[test]
fn test_initialize_result_goes_out_before_anything_the_handler_sent() {
    let mut client = Client::new();
    client.initialize(&[]);
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": null }));
    client.shutdown();
    let handler = EagerInit {
        fail: false,
        responses: Vec::new(),
    };
    let (server, result, out) = run(handler, &client);
    assert!(result.is_ok());
    let order: Vec<String> = out
        .iter()
        .map(|m| match m["method"].as_str() {
            Some(method) => method.to_owned(),
            None => format!("response {}", m["id"]),
        })
        .collect();
    assert_eq!(
        order[..3],
        [
            "response 1",
            "window/showMessage",
            "window/showMessageRequest"
        ]
    );
    // The request sent during `initialize` is outstanding and gets its answer.
    assert_eq!(server.handler().responses, [0]);
}

#[test]
fn test_failed_initialize_sends_nothing_from_the_handler_and_forgets_its_requests() {
    let mut client = Client::new();
    client.initialize(&[]);
    // An answer to the request the failed `initialize` never sent.
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": null }));
    client.notify("exit", Value::Null);
    let handler = EagerInit {
        fail: true,
        responses: Vec::new(),
    };
    let (server, _, out) = run(handler, &client);
    assert_eq!(
        error_code(&out, 1),
        Some(i64::from(ResponseError::REQUEST_FAILED))
    );
    assert!(
        out.iter()
            .all(|m| m["method"] != "window/showMessage"
                && m["method"] != "window/showMessageRequest")
    );
    assert!(server.handler().responses.is_empty());
    assert_eq!(logs(&out).len(), 1); // the stray answer
}

#[test]
fn test_outstanding_requests_are_capped_by_cancelling_the_oldest() {
    const CAP: u64 = 1024;
    let bomb = Bomb {
        ask: CAP as usize + 3,
        ..Bomb::default()
    };
    let mut client = Client::new();
    client.initialize(&[]);
    // An answer to a cancelled request is dropped; the newest still lands.
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": "late" }));
    client.send(&json!({ "jsonrpc": "2.0", "id": CAP + 2, "result": "on time" }));
    client.shutdown();
    let (server, result, out) = run(bomb, &client);
    assert!(result.is_ok());
    let sent = out
        .iter()
        .filter(|m| m["method"] == "workspace/configuration")
        .count();
    assert_eq!(sent, CAP as usize + 3);
    let cancels: Vec<&Value> = out
        .iter()
        .filter(|m| m["method"] == "$/cancelRequest")
        .map(|m| &m["params"]["id"])
        .collect();
    assert_eq!(cancels, [&json!(0), &json!(1), &json!(2)]);
    let responses: Vec<&str> = server
        .handler()
        .calls
        .iter()
        .filter_map(|c| c.strip_prefix("response "))
        .collect();
    let cancelled = ResponseError::REQUEST_CANCELLED;
    let expected = [
        format!("0 err {cancelled}"),
        format!("1 err {cancelled}"),
        format!("2 err {cancelled}"),
        format!("{} ok \"on time\"", CAP + 2),
    ];
    assert_eq!(responses, expected);
    // Besides the handler's own "starting", only the late answer is logged.
    let warnings: Vec<(i64, String)> = logs(&out).into_iter().filter(|l| l.0 == 2).collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].1.contains("id 0"));
}

#[test]
fn test_malformed_request_with_a_valid_id_is_answered_under_that_id() {
    let mut client = Client::new();
    client.initialize(&[]);
    client.send_raw(r#"{"jsonrpc":"2.0","id":7,"method":42}"#);
    client.send_raw(r#"{"jsonrpc":"2.0","id":"s-1","method":{"a":1}}"#);
    client.send_raw(r#"{"jsonrpc":"2.0","id":7.5,"method":42}"#);
    client.request("test/echo", json!("alive")).shutdown();
    let (_, result, out) = run(recorder(), &client);
    assert!(result.is_ok());
    let invalid: Vec<&Value> = out
        .iter()
        .filter(|m| m["error"]["code"] == ResponseError::INVALID_REQUEST)
        .map(|m| &m["id"])
        .collect();
    assert_eq!(invalid, [&json!(7), &json!("s-1"), &Value::Null]);
    assert_eq!(response(&out, 2)["result"], "alive");
}
