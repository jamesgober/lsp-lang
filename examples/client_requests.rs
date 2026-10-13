//! Talking back to the client: a server-to-client request and its answer.
//!
//! Once the client says `initialized`, the server asks for its settings with
//! `workspace/configuration`. The answer arrives later through
//! `Handler::response`; the server applies it (a cap on how many problems to
//! report) and republishes diagnostics for every open document.
//!
//! A conversation needs two parties, so this example runs the server on a
//! thread behind a pair of in-memory channels and plays the editor on the main
//! thread: it reads what the server sends and answers the question when it
//! arrives.
//!
//! ```text
//! cargo run --example client_requests
//! ```

mod common;

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use common::{Client, Lisp, analyze, messages};
use lsp_lang::{Context, Document, Handler, ResponseError, Server, Value, json};

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
    fn notification(&mut self, method: &str, _: &Value<'_>, cx: &mut Context<'_, Lisp>) {
        if method != "initialized" {
            return;
        }
        let ask = json!({ "items": [{ "section": "lisp" }] });
        match cx.request("workspace/configuration", ask) {
            Ok(id) => self.pending = Some(id),
            Err(err) => eprintln!("could not ask for settings: {err}"),
        }
    }

    fn response(
        &mut self,
        id: u64,
        result: Result<Value<'_>, ResponseError>,
        cx: &mut Context<'_, Lisp>,
    ) {
        if self.pending != Some(id) {
            return;
        }
        self.pending = None;
        // The result is an array with one entry per requested item.
        let settings = result.unwrap_or_default();
        if let Some(max) = settings[0]["maxProblems"].as_u64() {
            self.max_problems = usize::try_from(max).unwrap_or(usize::MAX);
            let plural = if max == 1 { "" } else { "s" };
            let note =
                json!({ "type": 3, "message": format!("reporting at most {max} problem{plural}") });
            if let Err(err) = cx.notify("window/showMessage", note) {
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

/// The server's input: bytes the editor sent, one chunk per `send`.
struct Incoming {
    chunks: Receiver<Vec<u8>>,
    buffered: VecDeque<u8>,
}

impl Read for Incoming {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.buffered.is_empty() {
            match self.chunks.recv() {
                Ok(chunk) => self.buffered.extend(chunk),
                Err(_) => return Ok(0), // the editor hung up
            }
        }
        let n = buf.len().min(self.buffered.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.buffered.pop_front().unwrap_or(0);
        }
        Ok(n)
    }
}

/// The server's output: each write goes to the editor as one chunk.
struct Outgoing(Sender<Vec<u8>>);

impl Write for Outgoing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map(|()| buf.len())
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn main() {
    let (to_server, server_input) = channel();
    let (server_output, from_server) = channel();
    let server = thread::spawn(move || {
        let handler = Configurable {
            max_problems: usize::MAX,
            pending: None,
        };
        let mut server = Server::new(Lisp, handler);
        let input = Incoming {
            chunks: server_input,
            buffered: VecDeque::new(),
        };
        server.run(input, Outgoing(server_output))
    });

    // The editor starts the session and opens a file.
    let mut hello = Client::new();
    hello.initialize(&["utf-16"]).open(URI, SOURCE);
    let _sent = to_server.send(hello.bytes().to_vec());

    // It reads until the server asks its question, then answers it.
    let mut heard = Vec::new();
    let mut seen = Vec::new();
    'conversation: while let Ok(chunk) = from_server.recv() {
        heard.extend(chunk);
        for message in messages(&heard) {
            if seen.contains(&message) {
                continue;
            }
            report(&message);
            if message["method"] == "workspace/configuration" {
                let mut reply = Client::new();
                reply.send(&json!({ "jsonrpc": "2.0", "id": message["id"].clone(),
                                    "result": [{ "maxProblems": 1 }] }));
                reply.shutdown();
                let _sent = to_server.send(reply.bytes().to_vec());
                seen.push(message);
                break 'conversation;
            }
            seen.push(message);
        }
    }

    // Hang up and read the rest of what the server had to say.
    drop(to_server);
    while let Ok(chunk) = from_server.recv() {
        heard.extend(chunk);
    }
    for message in messages(&heard) {
        if !seen.contains(&message) {
            report(&message);
        }
    }
    match server.join() {
        Ok(Ok(())) => {}
        other => eprintln!("session ended: {other:?}"),
    }
}

/// Prints one server message as a line of plain text.
fn report(message: &Value<'_>) {
    match message["method"].as_str() {
        Some("workspace/configuration") => {
            println!("server asks (id {}): {}", message["id"], message["params"]);
        }
        Some("window/showMessage") => println!("server says: {}", message["params"]["message"]),
        Some("textDocument/publishDiagnostics") => {
            let count = message["params"]["diagnostics"]
                .as_array()
                .map_or(0, |list| list.len());
            println!("server publishes {count} diagnostic(s)");
        }
        _ => {}
    }
}
