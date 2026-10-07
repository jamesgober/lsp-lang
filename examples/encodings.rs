//! Position encodings: the same hover, asked by a UTF-16 client and by a UTF-8
//! client.
//!
//! The protocol counts columns in UTF-16 code units unless the client offers
//! another encoding at `initialize`; the server takes UTF-8 whenever it is
//! offered. Either way the handler works in byte offsets and never sees the
//! difference — `Document` converts at the edge.
//!
//! ```text
//! cargo run --example encodings
//! ```

mod common;

use common::{Client, Lisp, LispServer, messages};
use lsp_lang::{Server, Value};

const URI: &str = "file:///demo/unicode.lisp";

// `ï` is 2 bytes but 1 UTF-16 unit; `😀` is 4 bytes but 2 UTF-16 units.
const SOURCE: &str = "(define naïve 1) (define 😀 2)\n(+ naïve 😀)\n";

fn main() {
    // The column of `😀` on line 1, as each client counts it.
    session("UTF-16 client", &["utf-16"], 9);
    session("UTF-8 client ", &["utf-8", "utf-16"], 10);

    // A UTF-16 column between the two halves of the surrogate pair is not a
    // character boundary; it snaps to the start of `😀`.
    session("mid-surrogate", &["utf-16"], 10);
}

fn session(label: &str, offered: &[&str], column: u32) {
    let mut client = Client::new();
    client
        .initialize(offered)
        .open(URI, SOURCE)
        .at("textDocument/hover", URI, 1, column)
        .shutdown();

    let mut output = Vec::new();
    let mut server = Server::new(Lisp, LispServer::default());
    if let Err(err) = server.run(client.bytes(), &mut output) {
        eprintln!("{label}: session failed: {err}");
        return;
    }

    let replies = messages(&output);
    let encoding = replies
        .iter()
        .find_map(|m| m["result"]["capabilities"]["positionEncoding"].as_str())
        .unwrap_or("?");
    let hover = replies
        .iter()
        .find(|m| m["id"] == 2)
        .map_or(Value::Null, |m| m["result"].clone());
    let range = &hover["range"];
    println!(
        "{label}  encoding {encoding:<6}  asked column {column:>2}  ->  `😀` spans columns {}..{}  ({})",
        range["start"]["character"], range["end"]["character"], hover["contents"]["value"],
    );
}
