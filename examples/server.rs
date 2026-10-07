//! A complete language server for a tiny Lisp: live diagnostics, hover, and
//! go-to-definition.
//!
//! By default it plays a scripted editor session in memory and prints what the
//! server sent back. With `--stdio` it serves a real editor instead:
//!
//! ```text
//! cargo run --example server               # scripted session
//! cargo run --example server -- --stdio    # point your editor's LSP client here
//! ```
//!
//! The language, its analysis, and the handler live in `examples/common`.

mod common;

use std::io;
use std::process::ExitCode;

use common::{Client, Lisp, LispServer, messages};
use lsp_lang::{Server, Value};

const URI: &str = "file:///demo/area.lisp";

const SOURCE: &str = "\
; the area of a rectangle
(define width 80)
(define area (* width height))
(print area)
";

fn main() -> ExitCode {
    let mut server = Server::new(Lisp, LispServer::default());

    if std::env::args().any(|arg| arg == "--stdio") {
        return match server.run(io::stdin().lock(), io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("lisp language server: {err}");
                ExitCode::FAILURE
            }
        };
    }

    let mut client = Client::new();
    client
        .initialize(&["utf-16"])
        .open(URI, SOURCE)
        // Hover over `width` on line 3, then jump to its definition.
        .at("textDocument/hover", URI, 2, 17)
        .at("textDocument/definition", URI, 2, 17)
        // Define the missing `height` by typing a new line 2.
        .edit(URI, 2, (2, 0), (2, 0), "(define height 25)\n")
        // Break the syntax: delete the `)` that closes `(print area)`.
        .edit(URI, 3, (4, 11), (4, 12), "")
        .shutdown();

    let mut output = Vec::new();
    if let Err(err) = server.run(client.bytes(), &mut output) {
        eprintln!("session failed: {err}");
        return ExitCode::FAILURE;
    }

    for message in messages(&output) {
        describe(&message);
    }
    println!(
        "\nThe document was analysed {} times, once per open or change.",
        server.handler().analysed
    );
    ExitCode::SUCCESS
}

/// Prints one server message as a line or two of plain text.
fn describe(message: &Value) {
    if message["method"] == "textDocument/publishDiagnostics" {
        let found = message["params"]["diagnostics"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let version = &message["params"]["version"];
        match found.len() {
            0 => println!("diagnostics for version {version}: none"),
            n => println!("diagnostics for version {version}: {n}"),
        }
        for d in found {
            let start = &d["range"]["start"];
            let headline = d["message"]
                .as_str()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            println!("    {}:{}  {headline}", start["line"], start["character"]);
        }
    } else if let Some(caps) = message["result"].get("capabilities") {
        println!("initialized: {caps}");
    } else if message.get("result").is_some() {
        println!("response {}: {}", message["id"], message["result"]);
    } else {
        println!("{message}");
    }
}
