//! A one-token-per-character grammar for the crate's unit tests.

use incremental_lang::{Builder, Grammar, Span, Token, TokenKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum K {
    Root,
    Char,
}

impl TokenKind for K {}

/// Lexes every character as its own token under one root node.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Chars;

impl Grammar for Chars {
    type Kind = K;
    const ROOT: K = K::Root;

    fn lex(&self, text: &str, tokens: &mut Vec<Token<K>>) {
        for (i, c) in text.char_indices() {
            let end = (i + c.len_utf8()) as u32;
            tokens.push(Token::new(K::Char, Span::new(i as u32, end)));
        }
    }

    fn parse(&self, _: K, b: &mut Builder<'_, K>) {
        b.start(K::Root);
        while !b.at_end() {
            b.bump();
        }
        b.finish();
    }
}
