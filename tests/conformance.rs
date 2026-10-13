//! Protocol conformance: a scripted client talking to a running server over
//! in-memory pipes, the way an editor talks over stdio. These tests hold the
//! conversation message by message (send, wait for the answer, react), which
//! a one-shot script cannot, and exercise how bytes arrive: split anywhere,
//! one at a time, or all at once.

#[path = "../examples/common/mod.rs"]
mod common;
mod support;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::{Lisp, LispServer, frame};
use lsp_lang::{
    Context, Document, Error, Features, Handler, Position, ResponseError, Server, Value, json,
};
use support::Live;

const URI: &str = "file:///c.lisp";

fn live() -> Live<LispServer> {
    Live::start(Server::new(Lisp, LispServer::default()))
}

#[test]
fn test_a_whole_session_in_order() {
    let mut live = live();

    // The server says nothing until it is spoken to.
    let init = live.initialize(&[]);
    assert_eq!(init["result"]["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(init["result"]["capabilities"]["hoverProvider"], true);

    live.open(URI, "(define x 1)\n(+ x y)");
    let diagnostics = live.recv_method("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["params"]["uri"], URI);
    assert_eq!(diagnostics["params"]["version"], 1);
    assert_eq!(
        diagnostics["params"]["diagnostics"][0]["range"]["start"]["line"],
        1
    );

    let id = live.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 0, "character": 9 } }),
    );
    let hover = live.recv_response(id);
    assert_eq!(
        hover["result"]["contents"]["value"],
        "`x`: defined on line 1"
    );

    // The edit fixes the error: the next diagnostics are empty and versioned.
    live.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 2 }, "contentChanges": [{
            "range": { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 6 } },
            "text": "x" }] }),
    );
    let fixed = live.recv_method("textDocument/publishDiagnostics");
    assert_eq!(fixed["params"]["version"], 2);
    assert_eq!(fixed["params"]["diagnostics"], json!([]));

    assert!(live.shutdown()["result"].is_null());
    let (result, server, rest) = live.finish();
    assert!(result.is_ok());
    assert_eq!(server.handler().analysed, 2);
    assert!(rest.is_empty(), "nothing is sent after shutdown: {rest:?}");
}

#[test]
fn test_a_session_works_when_bytes_arrive_one_at_a_time() {
    let mut live = Live::start_chunked(Server::new(Lisp, LispServer::default()), 1);
    let init = live.initialize(&["utf-8"]);
    assert_eq!(init["result"]["capabilities"]["positionEncoding"], "utf-8");
    live.open(URI, "(define é 1)\n(+ é 2)");
    let published = live.recv_method("textDocument/publishDiagnostics");
    assert_eq!(published["params"]["diagnostics"], json!([]));
    let id = live.request(
        "textDocument/hover",
        // UTF-8 columns: `é` is two bytes.
        json!({ "textDocument": { "uri": URI }, "position": { "line": 1, "character": 4 } }),
    );
    assert_eq!(
        live.recv_response(id)["result"]["contents"]["value"],
        "`é`: defined on line 1"
    );
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_a_message_split_across_writes_with_pauses_is_read_whole() {
    let mut live = live();
    live.initialize(&[]);
    let bytes = frame(
        &json!({ "jsonrpc": "2.0", "id": 77, "method": "textDocument/hover",
                 "params": { "textDocument": { "uri": URI }, "position": { "line": 0, "character": 0 } } })
        .to_string(),
    );
    // Split mid-header, mid-body, and between messages.
    for piece in [
        &bytes[..7],
        &bytes[7..20],
        &bytes[20..bytes.len() - 3],
        &bytes[bytes.len() - 3..],
    ] {
        live.send_bytes(piece);
        thread::sleep(Duration::from_millis(15));
    }
    assert!(live.recv_response(77).get("result").is_some());
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_requests_sent_together_are_answered_in_order() {
    let mut live = live();
    live.initialize(&[]);
    live.open(URI, "(define x 1)");
    live.recv_method("textDocument/publishDiagnostics");
    let mut all = Vec::new();
    for id in 10..30 {
        all.extend(frame(
            &json!({ "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
                     "params": { "textDocument": { "uri": URI }, "position": { "line": 0, "character": 9 } } })
            .to_string(),
        ));
    }
    live.send_bytes(&all);
    for id in 10..30 {
        let message = live.recv().unwrap_or(Value::Null);
        assert_eq!(message["id"], id, "answers arrive in request order");
    }
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_nothing_after_exit_is_read_or_answered() {
    let mut live = live();
    live.initialize(&[]);
    live.shutdown();
    // The server has been told to exit; whatever follows is not its business.
    live.send_raw(r#"{"jsonrpc":"2.0","id":99,"method":"textDocument/hover"}"#);
    live.send_bytes(b"garbage that is not even a header\r\n\r\n");
    let (result, _, rest) = live.finish();
    assert!(result.is_ok());
    assert!(rest.iter().all(|m| m["id"] != 99), "{rest:?}");
}

#[test]
fn test_a_client_that_hangs_up_mid_message_gets_a_disconnect() {
    let mut live = live();
    live.initialize(&[]);
    let id = live.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": URI } }),
    );
    assert!(live.recv_response(id).get("result").is_some());
    let full = frame(r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#);
    live.send_bytes(&full[..full.len() - 4]);
    let (result, _, _) = live.finish();
    assert!(matches!(result, Err(Error::Disconnected)));
}

#[test]
fn test_requests_sent_before_a_hangup_are_answered() {
    let mut live = live();
    live.initialize(&[]);
    live.open(URI, "(define x 1)");
    let id = live.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 0, "character": 9 } }),
    );
    // Hang up without reading or saying goodbye.
    let (result, _, rest) = live.finish();
    assert!(matches!(result, Err(Error::Disconnected)));
    assert!(
        rest.iter()
            .any(|m| m["id"] == id && m.get("result").is_some())
    );
}

#[test]
fn test_a_large_document_opens_and_edits() {
    let mut text = String::new();
    for i in 0..8_000 {
        text.push_str(&format!("(define n{i} (+ {i} 1))\n"));
    }
    assert!(text.len() > 150_000);
    let mut live = live();
    live.initialize(&[]);
    live.open(URI, &text);
    let published = live.recv_method("textDocument/publishDiagnostics");
    assert_eq!(published["params"]["diagnostics"], json!([]));
    live.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 2 }, "contentChanges": [{
            "range": { "start": { "line": 4_000, "character": 0 }, "end": { "line": 4_000, "character": 0 } },
            "text": "(oops " }] }),
    );
    let published = live.recv_method("textDocument/publishDiagnostics");
    assert_eq!(published["params"]["version"], 2);
    assert!(
        published["params"]["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("unclosed list"))
    );
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_standard_notifications_reach_the_handler_and_unknown_ones_are_ignored() {
    #[derive(Default)]
    struct Spy(Arc<Mutex<Vec<String>>>);
    impl Handler<Lisp> for Spy {
        fn notification(&mut self, method: &str, params: &Value<'_>, _: &mut Context<'_, Lisp>) {
            if let Ok(mut seen) = self.0.lock() {
                seen.push(format!("{method} {params}"));
            }
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = Server::new(Lisp, Spy(Arc::clone(&seen))).custom_notification("spy/custom");
    let mut live = Live::start(server);
    live.initialize(&[]);
    live.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "a": 1 } }),
    );
    live.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": URI } }),
    );
    live.notify("$/setTrace", json!({ "value": "off" }));
    live.notify("spy/custom", json!([1, 2]));
    live.notify("some/unknown", json!({}));
    live.notify("$/anythingElse", json!({}));
    live.shutdown();
    let (result, _, rest) = live.finish();
    assert!(result.is_ok());
    let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
    assert_eq!(
        seen,
        [
            "initialized {}",
            r#"workspace/didChangeConfiguration {"settings":{"a":1}}"#,
            r#"textDocument/didSave {"textDocument":{"uri":"file:///c.lisp"}}"#,
            r#"$/setTrace {"value":"off"}"#,
            "spy/custom [1,2]",
        ]
    );
    // Unknown notifications are not an error and not worth a log line.
    assert!(rest.is_empty(), "{rest:?}");
}

#[test]
fn test_unknown_requests_are_answered_not_ignored() {
    let mut live = live();
    live.initialize(&[]);
    for (i, method) in ["$/progress", "textDocument/rename", "workspace/symbol"]
        .into_iter()
        .enumerate()
    {
        let id = live.request(method, json!({}));
        let answer = live.recv_response(id);
        assert_eq!(
            answer["error"]["code"],
            ResponseError::METHOD_NOT_FOUND,
            "request {i}"
        );
    }
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_features_are_refused_before_initialize_and_after_shutdown() {
    let mut live = live();
    let early = live.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 0, "character": 0 } }),
    );
    assert_eq!(
        live.recv_response(early)["error"]["code"],
        ResponseError::SERVER_NOT_INITIALIZED
    );
    live.initialize(&[]);
    let id = live.request("shutdown", Value::Null);
    live.recv_response(id);
    let late = live.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 0, "character": 0 } }),
    );
    assert_eq!(
        live.recv_response(late)["error"]["code"],
        ResponseError::INVALID_REQUEST
    );
    live.notify("exit", Value::Null);
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_the_server_asks_the_client_and_the_answer_reaches_the_handler() {
    struct Asker {
        answers: Arc<Mutex<Vec<String>>>,
    }
    impl Handler<Lisp> for Asker {
        fn features(&self) -> Features {
            Features::new().hover()
        }
        fn hover(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            cx: &mut Context<'_, Lisp>,
        ) -> Result<Option<lsp_lang::Hover>, ResponseError> {
            // Ask for the user's settings, as a real server asks on demand.
            let _id = cx.request(
                "workspace/configuration",
                json!({ "items": [{ "section": "lisp" }] }),
            )?;
            Ok(None)
        }
        fn response(
            &mut self,
            id: u64,
            result: Result<Value<'_>, ResponseError>,
            _: &mut Context<'_, Lisp>,
        ) {
            if let Ok(mut answers) = self.answers.lock() {
                answers.push(format!(
                    "{id} {}",
                    result.map(|v| v.to_string()).unwrap_or_default()
                ));
            }
        }
    }
    let answers = Arc::new(Mutex::new(Vec::new()));
    let mut live = Live::start(Server::new(
        Lisp,
        Asker {
            answers: Arc::clone(&answers),
        },
    ));
    live.initialize(&[]);
    live.open(URI, "(a)");
    let id = live.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": URI }, "position": { "line": 0, "character": 1 } }),
    );
    // The question arrives before the hover's own answer.
    let question = live.recv_method("workspace/configuration");
    assert_eq!(question["params"]["items"][0]["section"], "lisp");
    assert!(live.recv_response(id)["result"].is_null());
    live.send(
        &json!({ "jsonrpc": "2.0", "id": question["id"].clone(), "result": [{ "tabWidth": 2 }] }),
    );
    live.shutdown();
    assert!(live.finish().0.is_ok());
    let answers = answers.lock().map(|a| a.clone()).unwrap_or_default();
    assert_eq!(answers, [r#"1 [{"tabWidth":2}]"#]);
}
