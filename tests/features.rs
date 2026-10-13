//! The typed features through a running server: what is advertised from what a
//! handler lists, completion, outlines, semantic tokens (full and delta), the
//! errors each request can earn, and `$/cancelRequest`.

#[path = "../examples/common/mod.rs"]
mod common;
mod support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

use common::{Client, Lisp, LispServer, frame, messages};
use lsp_lang::{
    CompletionItem, Context, Document, DocumentSymbol, Features, Handler, Hover, Position,
    ResponseError, SemanticTokens, Server, SymbolKind, Value, json,
};
use support::Live;

const URI: &str = "file:///f.lisp";

fn run<H: Handler<Lisp>>(handler: H, client: &Client) -> Vec<Value<'static>> {
    let mut server = Server::new(Lisp, handler);
    let mut output = Vec::new();
    let _ended = server.run(client.bytes(), &mut output);
    messages(&output)
}

fn response<'a>(messages: &'a [Value<'static>], id: u64) -> &'a Value<'static> {
    static NULL: Value<'static> = Value::Null;
    messages
        .iter()
        .find(|m| m["id"] == id && m.get("method").is_none())
        .unwrap_or(&NULL)
}

// ---------------------------------------------------------------------------
// Capabilities come from what the handler lists
// ---------------------------------------------------------------------------

#[test]
fn test_capabilities_advertise_exactly_the_listed_features() {
    let mut client = Client::new();
    client.initialize(&["utf-8"]).shutdown();
    let out = run(LispServer::default(), &client);
    let caps = &response(&out, 1)["result"]["capabilities"];
    assert_eq!(caps["hoverProvider"], true);
    assert_eq!(caps["definitionProvider"], true);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert_eq!(
        caps["completionProvider"]["triggerCharacters"],
        json!(["(", " "])
    );
    let legend = &caps["semanticTokensProvider"]["legend"];
    assert_eq!(
        legend["tokenTypes"],
        json!(["keyword", "number", "variable", "comment"])
    );
    assert_eq!(legend["tokenModifiers"], json!(["declaration"]));
    assert_eq!(caps["semanticTokensProvider"]["full"]["delta"], true);
    assert_eq!(caps["positionEncoding"], "utf-8");
    assert_eq!(caps["textDocumentSync"]["change"], 2);

    // A handler that lists nothing advertises none of them.
    struct Bare;
    impl Handler<Lisp> for Bare {}
    let out = run(Bare, &client);
    let caps = &response(&out, 1)["result"]["capabilities"];
    for absent in [
        "hoverProvider",
        "definitionProvider",
        "completionProvider",
        "documentSymbolProvider",
        "semanticTokensProvider",
    ] {
        assert!(caps.get(absent).is_none(), "{absent}");
    }
}

#[test]
fn test_features_may_depend_on_what_initialize_learned() {
    struct Picky {
        wants_hover: bool,
    }
    impl Handler<Lisp> for Picky {
        fn initialize(
            &mut self,
            params: &Value<'_>,
            _: &mut Context<'_, Lisp>,
        ) -> Result<(), ResponseError> {
            self.wants_hover = params["initializationOptions"]["hover"] == true;
            Ok(())
        }
        fn features(&self) -> Features {
            if self.wants_hover {
                Features::new().hover()
            } else {
                Features::new()
            }
        }
    }
    for (option, expected) in [(true, true), (false, false)] {
        let mut client = Client::new();
        client.request(
            "initialize",
            json!({ "initializationOptions": { "hover": option } }),
        );
        let out = run(Picky { wants_hover: false }, &client);
        let caps = &response(&out, 1)["result"]["capabilities"];
        assert_eq!(caps.get("hoverProvider").is_some(), expected);
    }
}

#[test]
fn test_a_feature_not_listed_is_not_found_even_if_the_method_is_overridden() {
    struct Unlisted;
    impl Handler<Lisp> for Unlisted {
        fn hover(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            _: &mut Context<'_, Lisp>,
        ) -> Result<Option<Hover>, ResponseError> {
            Ok(Some(Hover::markdown("never sent")))
        }
    }
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .at("textDocument/hover", URI, 0, 1)
        .shutdown();
    let out = run(Unlisted, &client);
    assert_eq!(
        response(&out, 2)["error"]["code"],
        ResponseError::METHOD_NOT_FOUND
    );
}

#[test]
fn test_a_feature_listed_but_not_implemented_is_not_found() {
    struct Forgot;
    impl Handler<Lisp> for Forgot {
        fn features(&self) -> Features {
            Features::new()
                .hover()
                .definition()
                .completion(&[])
                .document_symbols()
                .semantic_tokens(common::legend())
        }
    }
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)");
    client
        .at("textDocument/hover", URI, 0, 1)
        .at("textDocument/definition", URI, 0, 1)
        .at("textDocument/completion", URI, 0, 1)
        .of("textDocument/documentSymbol", URI)
        .of("textDocument/semanticTokens/full", URI);
    client.shutdown();
    let out = run(Forgot, &client);
    for id in 2..=6 {
        assert_eq!(
            response(&out, id)["error"]["code"],
            ResponseError::METHOD_NOT_FOUND,
            "request {id}"
        );
    }
}

// ---------------------------------------------------------------------------
// Completion, outline, semantic tokens
// ---------------------------------------------------------------------------

#[test]
fn test_completion_returns_a_complete_list() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(define width 80)\n(define height 2)\n(")
        .at("textDocument/completion", URI, 2, 1)
        .at("textDocument/completion", "file:///unknown", 0, 0)
        .shutdown();
    let out = run(LispServer::default(), &client);
    let list = &response(&out, 2)["result"];
    assert_eq!(list["isIncomplete"], false);
    let labels: Vec<&str> = list["items"]
        .as_array()
        .map(|items| items.iter().filter_map(|i| i["label"].as_str()).collect())
        .unwrap_or_default();
    assert!(labels.contains(&"define") && labels.contains(&"width") && labels.contains(&"height"));
    let width = list["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| i["label"] == "width"));
    assert_eq!(width.map(|w| w["kind"].clone()), Some(json!(6)));
    assert_eq!(width.map(|w| w["detail"].clone()), Some(json!("line 1")));
    assert!(response(&out, 3)["result"].is_null());
}

#[test]
fn test_document_symbols_are_hierarchical() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(
            URI,
            "(define width 80)\n(define area (list (define inner 1) 2))\n; nothing\n",
        )
        .of("textDocument/documentSymbol", URI)
        .of("textDocument/documentSymbol", "file:///unknown")
        .shutdown();
    let out = run(LispServer::default(), &client);
    let symbols = &response(&out, 2)["result"];
    assert_eq!(symbols[0]["name"], "width");
    assert_eq!(symbols[0]["kind"], 13); // Variable
    assert_eq!(symbols[1]["name"], "area");
    assert_eq!(symbols[1]["kind"], 12); // Function: its value is a list
    assert_eq!(symbols[1]["children"][0]["name"], "inner");
    assert!(symbols[0].get("children").is_none());
    // The selection lies inside the range.
    let (r, s) = (&symbols[1]["range"], &symbols[1]["selectionRange"]);
    assert!(r["start"]["character"].as_u64() <= s["start"]["character"].as_u64());
    assert!(s["end"]["character"].as_u64() <= r["end"]["character"].as_u64());
    assert!(response(&out, 3)["result"].is_null());
}

/// The edit of a `SemanticTokensDelta` applied to the tokens it was made from.
fn apply_delta(previous: &[u32], delta: &Value<'_>) -> Vec<u32> {
    let mut data = previous.to_vec();
    let edits = delta["edits"].as_array().cloned().unwrap_or_default();
    // Edits are in the coordinates of the previous data; apply from the back.
    for edit in edits.iter().rev() {
        let start = edit["start"].as_u64().unwrap_or(0) as usize;
        let delete = edit["deleteCount"].as_u64().unwrap_or(0) as usize;
        let insert: Vec<u32> = edit["data"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|n| n.as_u64())
                    .map(|n| n as u32)
                    .collect()
            })
            .unwrap_or_default();
        let _removed: Vec<u32> = data.splice(start..start + delete, insert).collect();
    }
    data
}

fn numbers(value: &Value<'_>) -> Vec<u32> {
    value
        .as_array()
        .map(|d| {
            d.iter()
                .filter_map(|n| n.as_u64())
                .map(|n| n as u32)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn test_semantic_tokens_full_encode_the_document() {
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(define x 1)\n; hi\n(+ x 2)")
        .of("textDocument/semanticTokens/full", URI)
        .of("textDocument/semanticTokens/full", "file:///unknown")
        .shutdown();
    let out = run(LispServer::default(), &client);
    let tokens = &response(&out, 2)["result"];
    assert!(tokens["resultId"].as_str().is_some());
    assert_eq!(
        numbers(&tokens["data"]),
        [
            0, 1, 6, 0, 0, // define: keyword
            0, 7, 1, 2, 0, // x: variable
            0, 2, 1, 1, 0, // 1: number
            1, 0, 4, 3, 0, // ; hi: comment
            1, 1, 1, 0, 0, // +: keyword
            0, 2, 1, 2, 0, // x: variable
            0, 2, 1, 1, 0, // 2: number
        ]
    );
    assert!(response(&out, 3)["result"].is_null());
}

#[test]
fn test_semantic_token_columns_follow_the_negotiated_encoding() {
    // `😀x` is one atom: 5 bytes, 3 UTF-16 units.
    for (encodings, length) in [(&["utf-16"][..], 3), (&["utf-8"][..], 5)] {
        let mut client = Client::new();
        client
            .initialize(encodings)
            .open(URI, "(😀x 1)")
            .of("textDocument/semanticTokens/full", URI)
            .shutdown();
        let out = run(LispServer::default(), &client);
        let data = numbers(&response(&out, 2)["result"]["data"]);
        assert_eq!(&data[..5], &[0, 1, length, 2, 0], "{encodings:?}");
        // The `1` follows after the space: 5 bytes or 3 units, plus the space.
        assert_eq!(data[6], length + 1, "{encodings:?}");
    }
}

#[test]
fn test_semantic_tokens_delta_applies_to_the_previous_tokens() {
    let mut live = Live::start(Server::new(Lisp, LispServer::default()));
    live.initialize(&[]);
    live.open(URI, "(define x 1)\n(+ x 2)\n(+ x 3)");
    let id = live.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": URI } }),
    );
    let full = live.recv_response(id)["result"].clone().into_owned();
    let before = numbers(&full["data"]);
    let first_id = full["resultId"].as_str().unwrap_or("").to_owned();

    // Change a token in the middle, and ask for the difference.
    live.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": 2 }, "contentChanges": [{
            "range": { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 6 } },
            "text": "22" }] }),
    );
    let id = live.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": URI }, "previousResultId": first_id.clone() }),
    );
    let delta = live.recv_response(id)["result"].clone().into_owned();
    assert!(delta.get("edits").is_some(), "{delta}");
    let second_id = delta["resultId"].as_str().unwrap_or("").to_owned();
    assert_ne!(second_id, first_id);

    // The same document asked for in full gives what the delta produced.
    let id = live.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": URI } }),
    );
    let after = numbers(&live.recv_response(id)["result"]["data"]);
    assert_eq!(apply_delta(&before, &delta), after);
    assert_ne!(before, after);
    // One edit, aligned to whole tokens, and small.
    let edits = delta["edits"].as_array().cloned().unwrap_or_default();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["start"].as_u64().unwrap_or(1) % 5, 0);
    assert_eq!(edits[0]["deleteCount"].as_u64().unwrap_or(1) % 5, 0);
    assert!(edits[0]["deleteCount"].as_u64().unwrap_or(99) <= 5);

    // Asking again against the id just issued, with nothing changed: no edits.
    let latest = live_last_id(&mut live);
    let id = live.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": URI }, "previousResultId": latest }),
    );
    let same = live.recv_response(id)["result"].clone().into_owned();
    assert_eq!(same["edits"], json!([]));

    live.shutdown();
    assert!(live.finish().0.is_ok());
}

/// The result id of a fresh full request (the server caches each answer, so
/// asking in full also leaves its id as the one to diff against).
fn live_last_id(live: &mut Live<LispServer>) -> String {
    let id = live.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": URI } }),
    );
    live.recv_response(id)["result"]["resultId"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

#[test]
fn test_semantic_tokens_delta_with_an_unknown_id_answers_in_full() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(+ 1 2)");
    client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": URI }, "previousResultId": "never issued" }),
    );
    client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": "file:///unknown" }, "previousResultId": "1" }),
    );
    client.shutdown();
    let out = run(LispServer::default(), &client);
    let answer = &response(&out, 2)["result"];
    assert!(answer.get("data").is_some() && answer.get("edits").is_none());
    assert!(response(&out, 3)["result"].is_null());
}

#[test]
fn test_closing_a_document_forgets_its_semantic_tokens() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(+ 1 2)");
    client.of("textDocument/semanticTokens/full", URI);
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    );
    client.open(URI, "(+ 1 2)");
    client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": { "uri": URI }, "previousResultId": "1" }),
    );
    client.shutdown();
    let out = run(LispServer::default(), &client);
    // The id from before the close is not honoured: a full answer.
    assert!(response(&out, 3)["result"].get("data").is_some());
}

#[test]
fn test_semantic_token_data_that_is_not_whole_tokens_is_an_internal_error() {
    struct Ragged;
    impl Handler<Lisp> for Ragged {
        fn features(&self) -> Features {
            Features::new().semantic_tokens(common::legend())
        }
        fn semantic_tokens(
            &mut self,
            _: &Document<Lisp>,
            _: &mut Context<'_, Lisp>,
        ) -> Result<SemanticTokens, ResponseError> {
            Ok(SemanticTokens::from_data(vec![0, 0, 1, 0]))
        }
    }
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .of("textDocument/semanticTokens/full", URI)
        .shutdown();
    let out = run(Ragged, &client);
    assert_eq!(
        response(&out, 2)["error"]["code"],
        ResponseError::INTERNAL_ERROR
    );
}

// ---------------------------------------------------------------------------
// Request errors
// ---------------------------------------------------------------------------

#[test]
fn test_malformed_feature_params_are_invalid_params() {
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)");
    for (method, params) in [
        (
            "textDocument/hover",
            json!({ "textDocument": { "uri": URI } }),
        ),
        (
            "textDocument/hover",
            json!({ "textDocument": { "uri": URI }, "position": { "line": -1, "character": 0 } }),
        ),
        (
            "textDocument/definition",
            json!({ "textDocument": { "uri": 5 }, "position": { "line": 0, "character": 0 } }),
        ),
        ("textDocument/completion", json!([])),
        ("textDocument/documentSymbol", json!({})),
        (
            "textDocument/semanticTokens/full",
            json!({ "textDocument": 3 }),
        ),
        (
            "textDocument/semanticTokens/full/delta",
            json!({ "textDocument": { "uri": URI } }),
        ),
    ] {
        let _id = client.request(method, params);
    }
    client.shutdown();
    let out = run(LispServer::default(), &client);
    for id in 2..=8 {
        assert_eq!(
            response(&out, id)["error"]["code"],
            ResponseError::INVALID_PARAMS,
            "request {id}: {}",
            response(&out, id)
        );
    }
}

#[test]
fn test_a_handler_error_is_sent_as_the_answer() {
    struct Refuses;
    impl Handler<Lisp> for Refuses {
        fn features(&self) -> Features {
            Features::new().hover()
        }
        fn hover(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            _: &mut Context<'_, Lisp>,
        ) -> Result<Option<Hover>, ResponseError> {
            Err(ResponseError::new(ResponseError::CONTENT_MODIFIED, "retry")
                .with_data(json!({ "n": 1 })))
        }
    }
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .at("textDocument/hover", URI, 0, 1)
        .shutdown();
    let out = run(Refuses, &client);
    let error = &response(&out, 2)["error"];
    assert_eq!(error["code"], ResponseError::CONTENT_MODIFIED);
    assert_eq!(error["message"], "retry");
    assert_eq!(error["data"]["n"], 1);
}

#[test]
fn test_a_panicking_feature_costs_one_request() {
    struct Bomb;
    impl Handler<Lisp> for Bomb {
        fn features(&self) -> Features {
            Features::new().hover().document_symbols()
        }
        #[allow(clippy::panic)]
        fn hover(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            _: &mut Context<'_, Lisp>,
        ) -> Result<Option<Hover>, ResponseError> {
            panic!("hover bug")
        }
        fn document_symbols(
            &mut self,
            _: &Document<Lisp>,
            _: &mut Context<'_, Lisp>,
        ) -> Result<Vec<DocumentSymbol>, ResponseError> {
            Ok(vec![DocumentSymbol::new(
                "ok",
                SymbolKind::File,
                lsp_lang::Range::default(),
                lsp_lang::Range::default(),
            )])
        }
    }
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .at("textDocument/hover", URI, 0, 1)
        .of("textDocument/documentSymbol", URI)
        .shutdown();
    let out = run(Bomb, &client);
    assert_eq!(
        response(&out, 2)["error"]["code"],
        ResponseError::INTERNAL_ERROR
    );
    assert!(
        response(&out, 2)["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("hover bug"))
    );
    assert_eq!(response(&out, 3)["result"][0]["name"], "ok");
}

#[test]
fn test_definition_in_another_document_carries_its_own_uri() {
    struct Elsewhere;
    impl Handler<Lisp> for Elsewhere {
        fn features(&self) -> Features {
            Features::new().definition()
        }
        fn definition(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            cx: &mut Context<'_, Lisp>,
        ) -> Result<Vec<lsp_lang::Location>, ResponseError> {
            Ok(cx
                .documents()
                .map(|d| lsp_lang::Location::new(d.uri(), d.range(d.tree().root().span())))
                .collect())
        }
    }
    let other = "file:///other.lisp";
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .open(other, "(b)\n(c)")
        .at("textDocument/definition", URI, 0, 0)
        .shutdown();
    let out = run(Elsewhere, &client);
    let found = &response(&out, 2)["result"];
    assert_eq!(found[0]["uri"], URI); // documents iterate in URI order
    assert_eq!(found[1]["uri"], other);
    assert_eq!(found[1]["range"]["end"]["line"], 1);
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// Counts hovers; its custom request `custom/slow` holds the server until the
/// test releases it.
struct Probe {
    hovers: Arc<AtomicUsize>,
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Handler<Lisp> for Probe {
    fn features(&self) -> Features {
        Features::new().hover()
    }

    fn hover(
        &mut self,
        _: &Document<Lisp>,
        _: Position,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Option<Hover>, ResponseError> {
        let _count = self.hovers.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Hover::markdown("hover")))
    }

    fn request(
        &mut self,
        method: &str,
        _: &Value<'_>,
        _: &mut Context<'_, Lisp>,
    ) -> Result<Value<'static>, ResponseError> {
        if method == "custom/slow" {
            let _sent = self.started.send(());
            let _released = self.release.recv_timeout(Duration::from_secs(20));
            return Ok(json!("slow done"));
        }
        Err(ResponseError::new(ResponseError::METHOD_NOT_FOUND, method))
    }
}

fn hover_message(id: u64) -> Vec<u8> {
    frame(
        &json!({ "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
                 "params": { "textDocument": { "uri": URI }, "position": { "line": 0, "character": 1 } } })
        .to_string(),
    )
}

fn cancel_message(id: Value<'_>) -> Vec<u8> {
    frame(
        &json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } })
            .to_string(),
    )
}

#[test]
fn test_cancel_request_in_the_same_read_stops_the_request_before_it_runs() {
    let hovers = Arc::new(AtomicUsize::new(0));
    let (started, _started_rx) = mpsc::channel();
    let (_release_tx, release) = mpsc::channel();
    let probe = Probe {
        hovers: Arc::clone(&hovers),
        started,
        release,
    };
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)");
    client
        .send_bytes(&hover_message(100))
        .send_bytes(&cancel_message(json!(100)))
        .send_bytes(&hover_message(101));
    client.shutdown();
    let out = run(probe, &client);
    // 100 was cancelled before it started; 101 ran.
    assert_eq!(
        response(&out, 100)["error"]["code"],
        ResponseError::REQUEST_CANCELLED
    );
    assert_eq!(response(&out, 101)["result"]["contents"]["value"], "hover");
    assert_eq!(hovers.load(Ordering::SeqCst), 1);
    // Exactly one answer to each.
    assert_eq!(out.iter().filter(|m| m["id"] == 100).count(), 1);
    assert_eq!(out.iter().filter(|m| m["id"] == 101).count(), 1);
}

#[test]
fn test_cancel_request_for_a_queued_request_arrives_while_another_runs() {
    let hovers = Arc::new(AtomicUsize::new(0));
    let (started, started_rx) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let probe = Probe {
        hovers: Arc::clone(&hovers),
        started,
        release,
    };
    let mut live = Live::start(Server::new(Lisp, probe).custom_request("custom/slow"));
    live.initialize(&[]);
    live.open(URI, "(a)");
    let slow = live.request("custom/slow", json!({}));
    // Wait until the server is inside the slow request.
    assert!(started_rx.recv_timeout(Duration::from_secs(20)).is_ok());
    // While it runs, the client sends a hover and then changes its mind, in one
    // write, so the server reads both together when it gets back to reading.
    let mut both = hover_message(500);
    both.extend(cancel_message(json!(500)));
    both.extend(hover_message(501));
    live.send_bytes(&both);
    assert!(release_tx.send(()).is_ok());

    assert_eq!(live.recv_response(slow)["result"], "slow done");
    assert_eq!(
        live.recv_response(500)["error"]["code"],
        ResponseError::REQUEST_CANCELLED
    );
    assert_eq!(
        live.recv_response(501)["result"]["contents"]["value"],
        "hover"
    );
    assert_eq!(
        hovers.load(Ordering::SeqCst),
        1,
        "the cancelled hover never ran"
    );
    live.shutdown();
    assert!(live.finish().0.is_ok());
}

#[test]
fn test_cancelling_a_request_that_already_finished_changes_nothing() {
    let hovers = Arc::new(AtomicUsize::new(0));
    let (started, _started_rx) = mpsc::channel();
    let (_release_tx, release) = mpsc::channel();
    let probe = Probe {
        hovers: Arc::clone(&hovers),
        started,
        release,
    };
    let mut live = Live::start(Server::new(Lisp, probe));
    live.initialize(&[]);
    live.open(URI, "(a)");
    live.send_bytes(&hover_message(7));
    assert_eq!(
        live.recv_response(7)["result"]["contents"]["value"],
        "hover"
    );
    // Late, for an id nobody ever sent, and for a string: all ignored.
    live.send_bytes(&cancel_message(json!(7)));
    live.send_bytes(&cancel_message(json!(99_999)));
    live.send_bytes(&cancel_message(json!("seven")));
    live.send_raw(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":null}}"#);
    live.send_raw(r#"{"jsonrpc":"2.0","method":"$/cancelRequest"}"#);
    live.send_bytes(&hover_message(8));
    assert_eq!(
        live.recv_response(8)["result"]["contents"]["value"],
        "hover"
    );
    live.shutdown();
    let (result, _, rest) = live.finish();
    assert!(result.is_ok());
    assert!(rest.iter().all(|m| m["id"] != 7), "no second answer to 7");
    assert_eq!(hovers.load(Ordering::SeqCst), 2);
}

#[test]
fn test_cancelling_a_request_by_string_id() {
    let hovers = Arc::new(AtomicUsize::new(0));
    let (started, _started_rx) = mpsc::channel();
    let (_release_tx, release) = mpsc::channel();
    let probe = Probe {
        hovers: Arc::clone(&hovers),
        started,
        release,
    };
    let mut client = Client::new();
    client.initialize(&[]).open(URI, "(a)");
    client.send_raw(
        r#"{"jsonrpc":"2.0","id":"req-1","method":"textDocument/hover","params":{"textDocument":{"uri":"file:///f.lisp"},"position":{"line":0,"character":1}}}"#,
    );
    client.send_bytes(&cancel_message(json!("req-1")));
    client.shutdown();
    let out = run(probe, &client);
    let answered = out.iter().find(|m| m["id"] == "req-1");
    assert_eq!(
        answered.map(|m| m["error"]["code"].clone()),
        Some(json!(ResponseError::REQUEST_CANCELLED))
    );
    assert_eq!(hovers.load(Ordering::SeqCst), 0);
}

#[test]
fn test_cancel_before_initialize_is_harmless() {
    let mut client = Client::new();
    client.send_bytes(&cancel_message(json!(1)));
    client.initialize(&[]).shutdown();
    let out = run(LispServer::default(), &client);
    assert!(
        response(&out, 1)["result"]["capabilities"]
            .as_object()
            .is_some()
    );
}

#[test]
fn test_items_in_one_completion_list_serialize_in_order() {
    // The list is the handler's, in the handler's order.
    struct Ordered;
    impl Handler<Lisp> for Ordered {
        fn features(&self) -> Features {
            Features::new().completion(&["."])
        }
        fn completion(
            &mut self,
            _: &Document<Lisp>,
            _: Position,
            _: &mut Context<'_, Lisp>,
        ) -> Result<Vec<CompletionItem>, ResponseError> {
            Ok((0..50)
                .map(|i| CompletionItem::new(format!("item{i}")))
                .collect())
        }
    }
    let mut client = Client::new();
    client
        .initialize(&[])
        .open(URI, "(a)")
        .at("textDocument/completion", URI, 0, 1)
        .shutdown();
    let out = run(Ordered, &client);
    let items = response(&out, 2)["result"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(items.len(), 50);
    assert_eq!(items[49]["label"], "item49");
}
