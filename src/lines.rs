//! The line index behind every [`Document`](crate::Document): line start
//! offsets, kept current under edits, and the conversions between byte offsets
//! and protocol positions.
//!
//! # Line breaks
//!
//! The protocol counts `\n`, `\r\n`, and a lone `\r` as line breaks, so a byte
//! offset `s > 0` starts a line exactly when the byte before it is `\n`, or is
//! `\r` not followed by `\n`. Whether `s` starts a line therefore depends only on
//! the bytes at `s - 1` and `s`. That locality is what makes the incremental
//! update in [`LineIndex::edit`] exact: only starts whose two deciding bytes an
//! edit touched can change, and those are recomputed from the new text.
//!
//! # Columns
//!
//! A column is the length of the line's text before the offset, measured in the
//! session [`Encoding`]. UTF-8 columns are byte counts and need no scan. UTF-16
//! columns are counted over the line prefix with branch-free byte arithmetic
//! (every non-continuation byte begins a scalar value; every 4-byte lead adds a
//! surrogate), which the compiler vectorizes.

use memchr::memchr2_iter;

use crate::position::{Encoding, Position};

/// Byte offsets at which each line starts. `starts[0]` is always `0`, so the
/// index of an empty text has one (empty) line, as the protocol expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    /// Indexes `text` from scratch.
    pub(crate) fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = Vec::with_capacity(1 + bytes.len() / 40);
        starts.push(0);
        starts.extend(breaks(bytes, 0, bytes.len()));
        Self { starts }
    }

    /// Number of lines (always at least one).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// Brings the index up to date after the bytes `start..old_end` were
    /// replaced by `inserted` bytes. `text` is the text *after* the edit.
    ///
    /// Starts in `max(start, 1)..=old_end` are the only ones whose deciding
    /// bytes changed (see the module docs); they are dropped and recomputed by
    /// scanning the new bytes from one before the edit to its new end. Starts
    /// after `old_end` keep their line-breaking bytes and shift by the change in
    /// length. The cost is a binary search, a scan of the inserted text, and one
    /// pass adding a constant over the starts after the edit.
    pub(crate) fn edit(&mut self, text: &str, start: u32, old_end: u32, inserted: u32) {
        let lo = start.max(1);
        let first = self.starts.partition_point(|&s| s < lo);
        let last = self.starts.partition_point(|&s| s <= old_end).max(first);

        // Modular arithmetic: every shifted start lands inside the new text, so
        // the wrapped sum is the exact result.
        let delta = inserted.wrapping_sub(old_end - start);
        for s in &mut self.starts[last..] {
            *s = s.wrapping_add(delta);
        }

        let bytes = text.as_bytes();
        let from = (lo - 1) as usize;
        let to = (start + inserted) as usize;
        drop(
            self.starts
                .splice(first..last, breaks(bytes, from, to.max(from))),
        );
    }

    /// Converts a byte offset into a position.
    ///
    /// An offset past the end is clamped to the end; one inside a multi-byte
    /// character is rounded down to that character's first byte.
    pub(crate) fn position(&self, text: &str, offset: u32, encoding: Encoding) -> Position {
        let offset = floor_boundary(text, offset);
        let line = self.line_of(offset);
        let start = self.starts[line] as usize;
        let character = width(&text.as_bytes()[start..offset], encoding);
        Position::new(line as u32, character)
    }

    /// Converts many byte offsets at once: `out[i]` becomes the position of
    /// `offsets[i]`, exactly as [`position`](LineIndex::position) would give.
    ///
    /// Converting one offset counts its line from the start; converting `n`
    /// offsets on one long line one by one would count the same prefix `n`
    /// times. Here the offsets are visited in ascending order (through
    /// `order`, a reused scratch buffer) and each column continues from the
    /// previous offset on the same line, so every byte of text is counted at
    /// most once: `O(n log n)` plus the text spanned, whatever the order of
    /// `offsets`.
    pub(crate) fn positions(
        &self,
        text: &str,
        offsets: &[u32],
        encoding: Encoding,
        order: &mut Vec<u32>,
        out: &mut Vec<Position>,
    ) {
        out.clear();
        out.resize(offsets.len(), Position::default());
        order.clear();
        order.extend(0..offsets.len() as u32);
        order.sort_unstable_by_key(|&i| offsets[i as usize]);
        let bytes = text.as_bytes();
        // (line, byte offset, column) of the previous conversion.
        let mut cursor: Option<(usize, usize, u32)> = None;
        for &i in order.iter() {
            let offset = floor_boundary(text, offsets[i as usize]);
            let line = self.line_of(offset);
            let (from, base) = match cursor {
                // Ascending order: an earlier offset on the same line is never
                // past this one.
                Some((at_line, at, column)) if at_line == line => (at, column),
                _ => (self.starts[line] as usize, 0),
            };
            let column = base + width(&bytes[from..offset], encoding);
            cursor = Some((line, offset, column));
            out[i as usize] = Position::new(line as u32, column);
        }
    }

    /// The line containing byte `offset` (a character boundary within the
    /// text).
    #[inline]
    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&s| s as usize <= offset)
            .saturating_sub(1)
    }

    /// Converts a position into a byte offset.
    ///
    /// Follows the protocol's leniency rules rather than rejecting input: a line
    /// past the last one maps to the end of the text, and a column past the end
    /// of its line maps to the end of that line (before its line break). A column
    /// that falls inside a character — in the middle of a surrogate pair, or
    /// between the bytes of a UTF-8 sequence — is rounded down to the start of
    /// that character, so the result is always a character boundary.
    pub(crate) fn offset(&self, text: &str, position: Position, encoding: Encoding) -> u32 {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else {
            return text.len() as u32;
        };
        let start = start as usize;
        let bytes = text.as_bytes();
        let end = match self.starts.get(line + 1) {
            Some(&next) => content_end(bytes, start, next as usize),
            None => bytes.len(),
        };
        // `start` follows a line break (or is 0) and `end` precedes one (or is
        // the end), so both are character boundaries.
        let content = &text[start..end];
        let column = position.character as usize;
        let within = match encoding {
            Encoding::Utf8 => {
                let mut at = column.min(content.len());
                while !content.is_char_boundary(at) {
                    at -= 1;
                }
                at
            }
            Encoding::Utf16 => utf16_column_to_byte(content, column),
        };
        (start + within) as u32
    }
}

/// `offset` clamped to the text and rounded down to a character boundary.
#[inline]
fn floor_boundary(text: &str, offset: u32) -> usize {
    let mut offset = (offset as usize).min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Width of `prefix` (valid UTF-8 on character boundaries) in `encoding`.
#[inline]
fn width(prefix: &[u8], encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf8 => prefix.len() as u32,
        Encoding::Utf16 => utf16_len(prefix),
    }
}

/// Line starts produced by the line breaks in `bytes[from..to]`, in order.
///
/// Each `\n` and each `\r` not followed by `\n` at index `p` yields `p + 1`. The
/// `\r\n` test may read the byte just past `to`, which is what the line-break
/// rule asks for.
fn breaks(bytes: &[u8], from: usize, to: usize) -> impl Iterator<Item = u32> + '_ {
    memchr2_iter(b'\n', b'\r', &bytes[from..to]).filter_map(move |i| {
        let p = from + i;
        if bytes[p] == b'\r' && bytes.get(p + 1) == Some(&b'\n') {
            None
        } else {
            Some((p + 1) as u32)
        }
    })
}

/// End of a line's content: `next` (the start of the following line) less the
/// line break that precedes it.
fn content_end(bytes: &[u8], start: usize, next: usize) -> usize {
    let mut end = next;
    if end > start && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    end
}

/// Byte offset within `line` of the character at UTF-16 `column`. Stops at
/// the character that would straddle `column` (the first half of a surrogate
/// pair), so the result is a character boundary.
fn utf16_column_to_byte(line: &str, column: usize) -> usize {
    let bytes = line.as_bytes();
    let limit = column.min(bytes.len());
    // An ASCII character is one UTF-16 unit, so an ASCII prefix maps column to
    // byte one to one.
    let ascii = bytes[..limit]
        .iter()
        .position(|b| !b.is_ascii())
        .unwrap_or(limit);
    if ascii == limit {
        return limit;
    }
    let mut units = ascii;
    for (i, c) in line[ascii..].char_indices() {
        let width = c.len_utf16();
        if units + width > column {
            return ascii + i;
        }
        units += width;
    }
    line.len()
}

/// Number of UTF-16 code units in `bytes` (valid UTF-8): one per scalar value,
/// plus one more for each 4-byte sequence, which needs a surrogate pair.
#[inline]
fn utf16_len(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .map(|&b| u32::from((b as i8) >= -0x40) + u32::from(b >= 0xF0))
        .sum()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Line starts by the protocol's definition, computed the slow, obvious way.
    fn model_starts(text: &str) -> Vec<u32> {
        let b = text.as_bytes();
        let mut starts = vec![0];
        for s in 1..=b.len() {
            let prev = b[s - 1];
            if prev == b'\n' || (prev == b'\r' && b.get(s) != Some(&b'\n')) {
                starts.push(s as u32);
            }
        }
        starts
    }

    fn model_width(s: &str, encoding: Encoding) -> u32 {
        match encoding {
            Encoding::Utf8 => s.len() as u32,
            Encoding::Utf16 => s.encode_utf16().count() as u32,
        }
    }

    const ALL: [Encoding; 2] = [Encoding::Utf8, Encoding::Utf16];

    #[test]
    fn test_index_new_empty_text_has_one_line() {
        let index = LineIndex::new("");
        assert_eq!(index.len(), 1);
        assert_eq!(index.position("", 0, Encoding::Utf16), Position::new(0, 0));
        assert_eq!(index.offset("", Position::new(0, 5), Encoding::Utf16), 0);
    }

    #[test]
    fn test_index_new_counts_every_break_style() {
        let text = "a\nb\r\nc\rd";
        assert_eq!(LineIndex::new(text).starts, vec![0, 2, 5, 7]);
        // A trailing break opens a final empty line.
        assert_eq!(LineIndex::new("x\n").starts, vec![0, 2]);
        assert_eq!(LineIndex::new("x\r").starts, vec![0, 2]);
        assert_eq!(LineIndex::new("\r\n\r\n").starts, vec![0, 2, 4]);
    }

    #[test]
    fn test_position_counts_columns_in_each_encoding() {
        let text = "é😀x";
        let index = LineIndex::new(text);
        // Offset of `x`: 2 bytes for é, 4 for 😀.
        assert_eq!(index.position(text, 6, Encoding::Utf8), Position::new(0, 6));
        assert_eq!(
            index.position(text, 6, Encoding::Utf16),
            Position::new(0, 3)
        );
    }

    #[test]
    fn test_position_inside_character_rounds_down() {
        let text = "a😀b";
        let index = LineIndex::new(text);
        for inside in 2..5 {
            assert_eq!(
                index.position(text, inside, Encoding::Utf16),
                Position::new(0, 1)
            );
        }
    }

    #[test]
    fn test_position_past_end_clamps_to_end() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(
            index.position(text, 999, Encoding::Utf16),
            Position::new(1, 2)
        );
    }

    #[test]
    fn test_offset_column_past_line_end_clamps_before_break() {
        let text = "ab\r\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 99), Encoding::Utf16), 2);
        assert_eq!(index.offset(text, Position::new(1, 99), Encoding::Utf16), 6);
    }

    #[test]
    fn test_offset_line_past_end_maps_to_end() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(7, 0), Encoding::Utf8), 5);
    }

    #[test]
    fn test_offset_inside_surrogate_pair_rounds_down() {
        let text = "a😀b";
        let index = LineIndex::new(text);
        // Column 2 is between the two surrogates of 😀.
        assert_eq!(index.offset(text, Position::new(0, 2), Encoding::Utf16), 1);
        assert_eq!(index.offset(text, Position::new(0, 3), Encoding::Utf16), 5);
        // A UTF-8 column inside the sequence also rounds down.
        assert_eq!(index.offset(text, Position::new(0, 3), Encoding::Utf8), 1);
    }

    #[test]
    fn test_edit_joining_cr_and_lf_merges_the_break() {
        // "x\ry": inserting "\n" after "\r" turns two breaks' worth into one.
        let mut index = LineIndex::new("x\ry");
        index.edit("x\r\ny", 2, 2, 1);
        assert_eq!(index, LineIndex::new("x\r\ny"));
        // Deleting the "\n" of "\r\n" leaves a lone "\r" break.
        index.edit("x\ry", 2, 3, 0);
        assert_eq!(index, LineIndex::new("x\ry"));
    }

    #[test]
    fn test_edit_at_start_keeps_line_zero() {
        let mut index = LineIndex::new("\nab");
        index.edit("ab", 0, 1, 0);
        assert_eq!(index, LineIndex::new("ab"));
        index.edit("\n\nab", 0, 0, 2);
        assert_eq!(index, LineIndex::new("\n\nab"));
    }

    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::sample::select(vec!["a", "b", " ", "\n", "\r", "\r\n", "é", "😀", "中"]),
            0..40,
        )
        .prop_map(|parts| parts.concat())
    }

    /// Snaps a byte offset down to a character boundary of `text`.
    fn floor(text: &str, mut at: usize) -> usize {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn prop_new_matches_model(text in text_strategy()) {
            prop_assert_eq!(LineIndex::new(&text).starts, model_starts(&text));
        }

        #[test]
        fn prop_edits_match_fresh_index(
            initial in text_strategy(),
            edits in prop::collection::vec((0usize..64, 0usize..64, text_strategy()), 1..8),
        ) {
            let mut text = initial;
            let mut index = LineIndex::new(&text);
            for (a, b, insert) in edits {
                let (a, b) = (floor(&text, a.min(b)), floor(&text, a.max(b)));
                text.replace_range(a..b, &insert);
                index.edit(&text, a as u32, b as u32, insert.len() as u32);
                prop_assert_eq!(&index.starts, &model_starts(&text));
            }
        }

        #[test]
        fn prop_position_matches_model_and_round_trips(text in text_strategy(), at in 0usize..200) {
            let index = LineIndex::new(&text);
            let offset = floor(&text, at);
            let starts = model_starts(&text);
            let line = starts.iter().rposition(|&s| s as usize <= offset).unwrap_or(0);
            let start = starts[line] as usize;
            // An offset between the `\r` and `\n` of one break is not a caret
            // position and does not round-trip; every other boundary does.
            let inside_crlf = offset > 0
                && text.as_bytes()[offset - 1] == b'\r'
                && text.as_bytes().get(offset) == Some(&b'\n');
            for encoding in ALL {
                let pos = index.position(&text, offset as u32, encoding);
                prop_assert_eq!(pos.line as usize, line);
                prop_assert_eq!(pos.character, model_width(&text[start..offset], encoding));
                if !inside_crlf {
                    prop_assert_eq!(index.offset(&text, pos, encoding) as usize, offset);
                }
            }
        }

        #[test]
        fn prop_batch_positions_match_single_positions(
            text in text_strategy(),
            offsets in prop::collection::vec(0u32..220, 0..24),
        ) {
            let index = LineIndex::new(&text);
            let (mut order, mut out) = (Vec::new(), Vec::new());
            for encoding in ALL {
                index.positions(&text, &offsets, encoding, &mut order, &mut out);
                let single: Vec<Position> =
                    offsets.iter().map(|&o| index.position(&text, o, encoding)).collect();
                prop_assert_eq!(&out, &single);
            }
        }

        #[test]
        fn prop_offset_is_always_a_boundary_in_range(
            text in text_strategy(),
            line in 0u32..12,
            character in 0u32..40,
        ) {
            let index = LineIndex::new(&text);
            for encoding in ALL {
                let offset = index.offset(&text, Position::new(line, character), encoding) as usize;
                prop_assert!(offset <= text.len());
                prop_assert!(text.is_char_boundary(offset));
            }
        }
    }
}
