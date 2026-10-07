//! Talking back to the client: a server-to-client request and its answer.
//!
//! Once the client says `initialized`, the server asks for its settings with
//! `workspace/configuration`. The answer arrives later through
//! `Handler::response`; the server applies it — a cap on how many problems to
//! report — and republishes diagnostics for every open document.
//!
//! ```text
//! cargo run --example client_requests
//! ```

mod common;

use common::{Client, Lisp, analyze, messages};
use lsp_lang::{Context, Document, Handler, Json, ResponseError, Server, Value, json};

const URI: &str = "file:///demo/typos.lisp";

const SOURCE: &str = "(print wdth)\n(print heigth)\n(print dpeth)\n";

/// Reports at most `max_problems` diagnostics per document, a limit the user
/// sets in the editor.
struct Configurable {
    max_problems: usize,
    /// The id of the outstanding `workspace/configuration` request.
    pending: Option<u64>,
}

impl Configurable {
    fn publish(&self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
        let mut found = analyze(doc.text(), doc.tree()).diagnostics;
        found.truncate(self.max_problems);
        cx.publish_diagnostics(doc, &found);
    }
}

impl Handler<Lisp> for Configurable {
    fn notification(&mut self, method: &str, _: Json<'_>, cx: &mut Context<'_, Lisp>) {
        if method != "initialized" {
            return;
        }
        let ask = json!({ "items": [{ "section": "lisp" }] });
        match cx.request("workspace/configuration", &ask) {
            Ok(id) => self.pending = Some(id),
            Err(err) => eprintln!("could not ask for settings: {err}"),
        }
    }

    fn response(
        &mut self,
        id: u64,
        result: Result<Json<'_>, ResponseError>,
        cx: &mut Context<'_, Lisp>,
    ) {
        if self.pending != Some(id) {
            return;
        }
        self.pending = None;
        // The result is an array with one entry per requested item.
        let settings = result
            .and_then(|json| json.parse::<Vec<Value>>())
            .unwrap_or_default();
        if let Some(max) = settings.first().and_then(|s| s["maxProblems"].as_u64()) {
            self.max_problems = usize::try_from(max).unwrap_or(usize::MAX);
            let plural = if max == 1 { "" } else { "s" };
            let note =
                json!({ "type": 3, "message": format!("reporting at most {max} problem{plural}") });
            if let Err(err) = cx.notify("window/showMessage", &note) {
                eprintln!("could not notify: {err}");
            }
            for doc in cx.documents() {
                self.publish(doc, cx);
            }
        }
    }

    fn changed(&mut self, doc: &Document<Lisp>, cx: &mut Context<'_, Lisp>) {
        self.publish(doc, cx);
    }
}

fn main() {
    let mut client = Client::new();
    client.initialize(&["utf-16"]).open(URI, SOURCE);
    // The editor answers the server's request (the server's first id is 0).
    client.send(&json!({ "jsonrpc": "2.0", "id": 0, "result": [{ "maxProblems": 1 }] }));
    client.shutdown();

    let handler = Configurable {
        max_problems: usize::MAX,
        pending: None,
    };
    let mut server = Server::new(Lisp, handler);
    let mut output = Vec::new();
    if let Err(err) = server.run(client.bytes(), &mut output) {
        eprintln!("session failed: {err}");
        return;
    }

    for message in messages(&output) {
        match message["method"].as_str() {
            Some("workspace/configuration") => {
                println!("server asks (id {}): {}", message["id"], message["params"]);
            }
            Some("window/showMessage") => println!("server says: {}", message["params"]["message"]),
            Some("textDocument/publishDiagnostics") => {
                let count = message["params"]["diagnostics"]
                    .as_array()
                    .map_or(0, Vec::len);
                println!("server publishes {count} diagnostic(s)");
            }
            _ => {}
        }
    }
}
