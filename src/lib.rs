//! # lsp_lang
//!
//! Language Server Protocol scaffolding for the `-lang` family: everything a
//! language server needs that is not about the language itself, on the family's
//! own stack. There are no third-party dependencies: the transport is
//! [`rpc_io`], the JSON is [`jsonx_lib`], documents are reparsed by
//! [`incremental_lang`], and diagnostics come from [`diag_lang`].
//!
//! A language server spends most of its code on plumbing: framing JSON-RPC over
//! stdio, the `initialize`/`shutdown`/`exit` handshake, keeping a copy of every
//! open file in step with the editor, converting UTF-16 positions to byte
//! offsets and back, and turning compiler diagnostics into protocol
//! diagnostics. This crate does all of that, so a server built on it is a
//! grammar and a handler:
//!
//! - an [`incremental_lang::Grammar`], which parses each open document, and
//!   reparses only the node around each edit as the user types;
//! - a [`Handler`], which publishes [`diag_lang::Diagnostic`]s as documents
//!   change and answers the requests the language supports. Each method has a
//!   default, so a handler implements only what it offers, and the capabilities
//!   the server advertises come from what the handler lists in
//!   [`Handler::features`].
//!
//! ## The pieces
//!
//! - [`Server`] and [`serve_stdio`]: the protocol engine. Reads messages from
//!   any `Read`, writes to any `Write`, runs the lifecycle, synchronizes
//!   documents, and calls the handler.
//! - [`Handler`]: the trait a language implements, every method defaulted.
//! - [`Features`]: what the handler offers: hover, definition, completion,
//!   document symbols, semantic tokens.
//! - [`Context`]: what a handler reaches while it runs: the open
//!   [`Document`]s and a channel back to the client (notifications, requests,
//!   log messages, diagnostics).
//! - [`Document`]: an open file: its text, its incrementally maintained syntax
//!   tree, and position conversions in the negotiated [`Encoding`].
//! - The protocol types the features speak, each with hand-written JSON:
//!   [`Position`], [`Range`], [`Location`], [`Hover`], [`CompletionItem`],
//!   [`DocumentSymbol`], [`SemanticTokens`] with its [`SemanticTokensBuilder`]
//!   and [`SemanticTokensLegend`], and [`TextEdit`].
//! - [`ResponseError`] and [`Error`]: a request that failed, and what ends a
//!   session.
//!
//! ## Example
//!
//! A server that reports every `TODO` in a document as a warning. The grammar
//! here treats a whole document as one token; a real language supplies its own
//! (see [`incremental_lang::Grammar`]).
//!
//! ```
//! use lsp_lang::incremental_lang::{Builder, Grammar, Span, Token, TokenKind};
//! use lsp_lang::diag_lang::{Label, Severity};
//! use lsp_lang::{Context, Diagnostic, Document, Handler, Server};
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! enum K { Root, Text }
//! impl TokenKind for K {}
//!
//! #[derive(Clone)]
//! struct Plain;
//!
//! impl Grammar for Plain {
//!     type Kind = K;
//!     const ROOT: K = K::Root;
//!     fn lex(&self, text: &str, tokens: &mut Vec<Token<K>>) {
//!         if !text.is_empty() {
//!             tokens.push(Token::new(K::Text, Span::new(0, text.len() as u32)));
//!         }
//!     }
//!     fn parse(&self, _: K, b: &mut Builder<'_, K>) {
//!         b.start(K::Root);
//!         while !b.at_end() { b.bump(); }
//!         b.finish();
//!     }
//! }
//!
//! struct Todos;
//!
//! impl Handler<Plain> for Todos {
//!     fn changed(&mut self, doc: &Document<Plain>, cx: &mut Context<'_, Plain>) {
//!         let found: Vec<Diagnostic> = doc
//!             .text()
//!             .match_indices("TODO")
//!             .map(|(at, _)| {
//!                 let span = Span::new(at as u32, at as u32 + 4);
//!                 Diagnostic::new(Severity::Warning, "unfinished work", Label::new(span, "here"))
//!             })
//!             .collect();
//!         cx.publish_diagnostics(doc, &found);
//!     }
//! }
//!
//! // Drive one session from memory, as an editor would over stdio.
//! fn frame(json: &str) -> String {
//!     format!("Content-Length: {}\r\n\r\n{json}", json.len())
//! }
//! let input = [
//!     frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
//!     frame(r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":
//!         {"uri":"file:///notes.txt","languageId":"text","version":1,"text":"ship it\nTODO: tests"}}}"#),
//!     frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#),
//!     frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
//! ]
//! .concat();
//!
//! let mut output = Vec::new();
//! Server::new(Plain, Todos).run(input.as_bytes(), &mut output)?;
//!
//! let output = String::from_utf8_lossy(&output);
//! assert!(output.contains(r#""method":"textDocument/publishDiagnostics""#));
//! assert!(output.contains(r#""range":{"start":{"line":1,"character":0},"end":{"line":1,"character":4}}"#));
//! # Ok::<(), lsp_lang::Error>(())
//! ```
//!
//! ## Positions
//!
//! The protocol counts columns in UTF-16 code units by default; the rest of
//! the family counts byte offsets. [`Document`] converts between the two in
//! the encoding negotiated at `initialize`: UTF-8 whenever the client offers
//! it, since byte columns convert without scanning, and UTF-16 otherwise. Line
//! breaks are `\n`, `\r\n`, and lone `\r`, as the protocol specifies.
//! Out-of-range positions are clamped the way the protocol prescribes rather
//! than rejected, and a column inside a character (between the halves of a
//! surrogate pair) rounds down to its start.
//!
//! ## Diagnostics
//!
//! [`Context::publish_diagnostics`] takes `diag_lang` diagnostics as any stage
//! of a `-lang` front end produces them (their spans are byte offsets into the
//! document) and writes `textDocument/publishDiagnostics` directly:
//!
//! | diag-lang | Protocol |
//! |---|---|
//! | primary label's span | `range` |
//! | `Error`, `Warning`, `Note`, `Help` | `severity` 1, 2, 3, 4 |
//! | message, primary label, notes, help | `message`, one part per line (`note: …`, `help: …`) |
//! | secondary labels | `relatedInformation` in the same document |
//!
//! ## Cancellation
//!
//! `$/cancelRequest` cancels a request that has been received and not yet
//! started. The transport answers it with `REQUEST_CANCELLED` the moment the
//! cancellation is read, and the handler is never called for it. The server
//! runs queued requests only when its input has nothing more buffered, so a
//! cancellation that arrives with, or just behind, the request it names is read
//! first. A handler that is already running is not interrupted (see
//! [`Server`]).
//!
//! ## Performance
//!
//! Each incoming message is decoded once, in place, by `rpc-io` and `jsonx-lib`
//! (the envelope is read without building a value tree, and parameters are
//! parsed only when the server needs them). Outgoing results are written
//! directly into a reused string: diagnostics, semantic tokens, and outlines
//! with no value tree, and everything sent during one message or one job
//! reaches the client in a single write. An edit converts its range with a
//! binary search over line starts, reparses only the innermost reparsable node
//! around it, and patches the line index in place; the work that stays linear
//! in the file (moving the text after the edit, shifting later line starts) is
//! a `memmove` and a pass over integers. Publishing diagnostics converts all
//! their spans in one ascending pass.
//!
//! ## Stability
//!
//! The public surface is the `2.0` contract: it follows Semantic Versioning,
//! with no breaking changes before `3.0`. See
//! [`docs/API.md`](https://github.com/jamesgober/lsp-lang/blob/main/docs/API.md#stability)
//! for the surface, the protocol behaviour that is part of the contract, and
//! what is not promised.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(
    warnings,
    missing_docs,
    unsafe_op_in_unsafe_fn,
    unused_must_use,
    unused_results,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::undocumented_unsafe_blocks
)]

mod completion;
mod context;
mod diagnostics;
mod document;
mod error;
mod features;
mod handler;
mod jobs;
mod json;
mod lines;
mod outbox;
mod position;
mod response;
mod semantic;
mod server;
mod session;
mod symbols;
#[cfg(test)]
mod testing;
mod types;

pub use completion::{CompletionItem, CompletionItemKind};
pub use context::{Context, MessageType};
pub use document::Document;
pub use error::Error;
pub use features::Features;
pub use handler::Handler;
pub use position::{Encoding, Position, Range};
pub use response::ResponseError;
pub use semantic::{SemanticTokens, SemanticTokensBuilder, SemanticTokensLegend};
pub use server::{Server, serve_stdio};
pub use symbols::{DocumentSymbol, SymbolKind};
pub use types::{Hover, Location, MarkupKind, TextEdit};

// The crates whose types appear in this crate's API, re-exported so a server
// names them through one dependency and the versions always match: a grammar is
// written against `incremental_lang`, diagnostics are built with `diag_lang`,
// parameters and custom results are `jsonx_lib` values, and the transport's
// types (`Json`, `Raw`, `FrameError`) are `rpc_io`'s.
pub use diag_lang;
pub use incremental_lang;
pub use jsonx_lib;
pub use rpc_io;

pub use diag_lang::Diagnostic;
pub use jsonx_lib::{Value, json};

/// Compiles and runs the `rust` code blocks in `README.md` and `docs/API.md` as
/// part of `cargo test`, so the published examples cannot drift from the API.
///
/// Present only while collecting doctests (`#[cfg(doctest)]`); it is not part of
/// the public surface and does not appear in the built library or its docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
#[doc = include_str!("../docs/API.md")]
pub struct MarkdownDocTests;
