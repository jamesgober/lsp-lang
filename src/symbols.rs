//! [`DocumentSymbol`] and [`SymbolKind`]: the answer to
//! `textDocument/documentSymbol`, as a tree.
//!
//! A symbol tree follows the source, so it can nest as deeply as the source
//! does. Serializing and dropping one never recurse: both walk an explicit
//! stack, so the depth of a tree costs memory, not call stack.

use jsonx_lib::Value;

use crate::{
    json::{to_value, write_str, write_u64},
    position::Range,
};

/// What a symbol is, which clients show as an icon in the outline.
///
/// The numbers are the protocol's `SymbolKind` values.
///
/// # Examples
///
/// ```
/// use lsp_lang::SymbolKind;
///
/// assert_eq!(SymbolKind::Function.as_u32(), 12);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)] // The variants are the protocol's names, one per icon.
pub enum SymbolKind {
    File = 1,
    Module = 2,
    Namespace = 3,
    Package = 4,
    Class = 5,
    Method = 6,
    Property = 7,
    Field = 8,
    Constructor = 9,
    Enum = 10,
    Interface = 11,
    Function = 12,
    Variable = 13,
    Constant = 14,
    String = 15,
    Number = 16,
    Boolean = 17,
    Array = 18,
    Object = 19,
    Key = 20,
    Null = 21,
    EnumMember = 22,
    Struct = 23,
    Event = 24,
    Operator = 25,
    TypeParameter = 26,
}

impl SymbolKind {
    /// The protocol's number for this kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::SymbolKind;
    ///
    /// assert_eq!(SymbolKind::Variable.as_u32(), 13);
    /// ```
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// One entry of a document's outline, with the entries nested inside it.
///
/// `range` covers the whole symbol (a function with its body); `selection_range`
/// is the part to highlight when the symbol is picked (the function's name). The
/// protocol requires the selection to lie inside the range, and clients reject
/// the whole answer otherwise, so the server clamps `selection_range` into
/// `range` when it writes the symbol.
///
/// Cloning, dropping, and sending a tree are iterative. `Debug` and `PartialEq`
/// follow the nesting and are meant for realistic depths, such as tests.
///
/// # Examples
///
/// ```
/// use lsp_lang::{DocumentSymbol, Position, Range, SymbolKind};
///
/// let line = |n| Range::new(Position::new(n, 0), Position::new(n, 20));
/// let mut class = DocumentSymbol::new("Shape", SymbolKind::Class, line(0), line(0));
/// class.children.push(DocumentSymbol::new("area", SymbolKind::Method, line(1), line(1)));
/// let json = class.to_json();
/// assert_eq!(json["kind"], 5);
/// assert_eq!(json["children"][0]["name"], "area");
/// ```
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DocumentSymbol {
    /// The name shown in the outline.
    pub name: String,
    /// More detail shown after the name, such as a signature.
    pub detail: Option<String>,
    /// The icon.
    pub kind: SymbolKind,
    /// The whole extent of the symbol.
    pub range: Range,
    /// The part to select when the symbol is picked.
    pub selection_range: Range,
    /// The symbols nested inside this one.
    pub children: Vec<DocumentSymbol>,
}

impl DocumentSymbol {
    /// A symbol with no children.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{DocumentSymbol, Position, Range, SymbolKind};
    ///
    /// let line = Range::new(Position::new(0, 0), Position::new(0, 9));
    /// let symbol = DocumentSymbol::new("main", SymbolKind::Function, line, line);
    /// assert_eq!(symbol.name, "main");
    /// ```
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        range: Range,
        selection_range: Range,
    ) -> Self {
        Self {
            name: name.into(),
            detail: None,
            kind,
            range,
            selection_range,
            children: Vec::new(),
        }
    }

    /// Sets the detail shown after the name.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{DocumentSymbol, Range, SymbolKind};
    ///
    /// let symbol = DocumentSymbol::new("f", SymbolKind::Function, Range::default(), Range::default())
    ///     .with_detail("(a b)");
    /// assert_eq!(symbol.detail.as_deref(), Some("(a b)"));
    /// ```
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Appends a nested symbol.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{DocumentSymbol, Range, SymbolKind};
    ///
    /// let at = Range::default();
    /// let class = DocumentSymbol::new("C", SymbolKind::Class, at, at)
    ///     .with_child(DocumentSymbol::new("m", SymbolKind::Method, at, at));
    /// assert_eq!(class.children.len(), 1);
    /// ```
    #[must_use]
    pub fn with_child(mut self, child: DocumentSymbol) -> Self {
        self.children.push(child);
        self
    }

    /// The protocol's JSON for this symbol and everything nested in it.
    ///
    /// The selection range is clamped into the range, as when the server
    /// sends it.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{DocumentSymbol, Range, SymbolKind};
    ///
    /// let symbol = DocumentSymbol::new("x", SymbolKind::Constant, Range::default(), Range::default());
    /// assert_eq!(symbol.to_json()["kind"], 14);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        write_symbols(&mut out, std::slice::from_ref(self));
        match to_value(&out) {
            Value::Array(mut items) if items.len() == 1 => items.pop().unwrap_or_default(),
            other => other,
        }
    }
}

impl Clone for DocumentSymbol {
    /// Copies the tree with an explicit stack, so cloning a deeply nested
    /// outline does not recurse once per level.
    fn clone(&self) -> Self {
        /// A node being copied: the source, how many of its children have been
        /// started, and the copies of those that are done.
        struct Frame<'a> {
            source: &'a DocumentSymbol,
            started: usize,
            done: Vec<DocumentSymbol>,
        }
        fn frame(source: &DocumentSymbol) -> Frame<'_> {
            Frame {
                source,
                started: 0,
                done: Vec::with_capacity(source.children.len()),
            }
        }
        let mut stack = vec![frame(self)];
        loop {
            let Some(top) = stack.last_mut() else {
                // Unreachable: the root is only popped by the return below.
                return Self::new(
                    self.name.clone(),
                    self.kind,
                    self.range,
                    self.selection_range,
                );
            };
            if let Some(child) = top.source.children.get(top.started) {
                top.started += 1;
                stack.push(frame(child));
                continue;
            }
            let Some(finished) = stack.pop() else {
                continue;
            };
            let mut copy = DocumentSymbol {
                name: finished.source.name.clone(),
                detail: finished.source.detail.clone(),
                kind: finished.source.kind,
                range: finished.source.range,
                selection_range: finished.source.selection_range,
                children: Vec::new(),
            };
            copy.children = finished.done;
            match stack.last_mut() {
                Some(parent) => parent.done.push(copy),
                None => return copy,
            }
        }
    }
}

impl Drop for DocumentSymbol {
    /// Flattens the subtree onto a stack, so dropping a deeply nested outline
    /// does not recurse once per level.
    fn drop(&mut self) {
        let mut stack = std::mem::take(&mut self.children);
        while let Some(mut symbol) = stack.pop() {
            stack.append(&mut symbol.children);
        }
    }
}

/// Clamps `selection` into `range` (which may arrive reversed).
fn contain(range: Range, selection: Range) -> Range {
    let (low, high) = if range.start <= range.end {
        (range.start, range.end)
    } else {
        (range.end, range.start)
    };
    let start = selection.start.clamp(low, high);
    let end = selection.end.clamp(start, high);
    Range::new(start, end)
}

/// Writes a JSON array of `symbols` with their children, iteratively.
pub(crate) fn write_symbols(out: &mut String, symbols: &[DocumentSymbol]) {
    out.push('[');
    // Each frame is a list being written and how many of its entries are done.
    let mut stack: Vec<(&[DocumentSymbol], usize)> = vec![(symbols, 0)];
    while let Some((items, done)) = stack.last_mut() {
        let Some(symbol) = items.get(*done) else {
            // This list is finished: close it, and the symbol that owns it.
            out.push(']');
            let _finished = stack.pop();
            if !stack.is_empty() {
                out.push('}');
            }
            continue;
        };
        if *done > 0 {
            out.push(',');
        }
        *done += 1;
        write_head(out, symbol);
        if symbol.children.is_empty() {
            out.push('}');
        } else {
            out.push_str(",\"children\":[");
            stack.push((&symbol.children, 0));
        }
    }
}

/// Writes a symbol's own members, up to but not including `children` and the
/// closing brace.
fn write_head(out: &mut String, symbol: &DocumentSymbol) {
    out.push_str("{\"name\":");
    write_str(out, &symbol.name);
    if let Some(detail) = &symbol.detail {
        out.push_str(",\"detail\":");
        write_str(out, detail);
    }
    out.push_str(",\"kind\":");
    write_u64(out, u64::from(symbol.kind.as_u32()));
    out.push_str(",\"range\":");
    symbol.range.write(out);
    out.push_str(",\"selectionRange\":");
    contain(symbol.range, symbol.selection_range).write(out);
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::position::Position;

    fn at(line: u32, from: u32, to: u32) -> Range {
        Range::new(Position::new(line, from), Position::new(line, to))
    }

    #[test]
    fn test_symbols_write_nested_children_in_order() {
        let tree = vec![
            DocumentSymbol::new("a", SymbolKind::Module, at(0, 0, 9), at(0, 0, 1))
                .with_detail("mod")
                .with_child(
                    DocumentSymbol::new("b", SymbolKind::Function, at(1, 0, 5), at(1, 0, 1))
                        .with_child(DocumentSymbol::new(
                            "c",
                            SymbolKind::Variable,
                            at(2, 0, 3),
                            at(2, 0, 1),
                        )),
                )
                .with_child(DocumentSymbol::new(
                    "d",
                    SymbolKind::Constant,
                    at(3, 0, 3),
                    at(3, 0, 1),
                )),
            DocumentSymbol::new("e", SymbolKind::Class, at(4, 0, 3), at(4, 0, 1)),
        ];
        let mut out = String::new();
        write_symbols(&mut out, &tree);
        let json = jsonx_lib::parse(&out).map_or(Value::Null, Value::into_owned);
        assert_eq!(json[0]["name"], "a");
        assert_eq!(json[0]["detail"], "mod");
        assert_eq!(json[0]["children"][0]["children"][0]["name"], "c");
        assert_eq!(json[0]["children"][1]["name"], "d");
        assert_eq!(json[1]["name"], "e");
        assert!(json[1].get("children").is_none());
        assert_eq!(json[1]["kind"], 5);
    }

    #[test]
    fn test_empty_list_is_an_empty_array() {
        let mut out = String::new();
        write_symbols(&mut out, &[]);
        assert_eq!(out, "[]");
    }

    #[test]
    fn test_selection_is_clamped_into_the_range() {
        let wild = DocumentSymbol::new("x", SymbolKind::Field, at(2, 5, 9), at(0, 0, 99));
        let json = wild.to_json();
        assert_eq!(json["selectionRange"]["start"]["line"], 2);
        assert_eq!(json["selectionRange"]["start"]["character"], 5);
        // Entirely outside the range: collapsed to its start.
        assert_eq!(json["selectionRange"]["end"]["character"], 5);
        // Inside already: untouched.
        let fine = DocumentSymbol::new("x", SymbolKind::Field, at(0, 0, 9), at(0, 2, 4)).to_json();
        assert_eq!(fine["selectionRange"]["start"]["character"], 2);
        assert_eq!(fine["selectionRange"]["end"]["character"], 4);
        // A reversed range is read as the span between its ends.
        let reversed = Range::new(Position::new(0, 9), Position::new(0, 1));
        let r = DocumentSymbol::new("x", SymbolKind::Field, reversed, at(0, 3, 4)).to_json();
        assert_eq!(r["selectionRange"]["start"]["character"], 3);
    }

    #[test]
    fn test_clone_copies_the_hierarchy() {
        let tree = DocumentSymbol::new("a", SymbolKind::Module, at(0, 0, 9), at(0, 0, 1))
            .with_detail("d")
            .with_child(
                DocumentSymbol::new("b", SymbolKind::Field, at(1, 0, 5), at(1, 0, 1)).with_child(
                    DocumentSymbol::new("c", SymbolKind::Field, at(2, 0, 5), at(2, 0, 1)),
                ),
            )
            .with_child(DocumentSymbol::new(
                "d",
                SymbolKind::Field,
                at(3, 0, 5),
                at(3, 0, 1),
            ));
        assert_eq!(tree.clone(), tree);
    }

    #[test]
    fn test_a_very_deep_outline_is_written_and_dropped_without_recursing() {
        const DEPTH: usize = 200_000;
        // The test thread has a small stack: a recursive writer or drop would
        // overflow it.
        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut tree =
                    DocumentSymbol::new("leaf", SymbolKind::Null, at(0, 0, 1), at(0, 0, 1));
                for _ in 0..DEPTH {
                    tree = DocumentSymbol::new("n", SymbolKind::Null, at(0, 0, 1), at(0, 0, 1))
                        .with_child(tree);
                }
                let copy = tree.clone();
                let mut out = String::new();
                write_symbols(&mut out, std::slice::from_ref(&tree));
                drop(copy);
                drop(tree);
                // Every symbol opens and closes its object once.
                out.matches('{').count() == out.matches('}').count()
                    && out.matches('{').count() > DEPTH
            });
        assert!(handle.is_ok_and(|h| h.join().unwrap_or(false)));
    }

    /// A tree of symbols up to four levels deep, named by their position.
    fn tree_strategy() -> impl Strategy<Value = Vec<DocumentSymbol>> {
        let leaf = Just(Vec::<DocumentSymbol>::new());
        leaf.prop_recursive(4, 40, 4, |children| {
            prop::collection::vec((0u32..30, 0u32..30, children), 0..4).prop_map(|nodes| {
                nodes
                    .into_iter()
                    .map(|(a, b, kids)| {
                        let range = at(a.min(b), 0, 9);
                        let mut symbol = DocumentSymbol::new(
                            format!("s{a}-{b}"),
                            SymbolKind::Field,
                            range,
                            range,
                        );
                        symbol.children = kids;
                        symbol
                    })
                    .collect()
            })
        })
    }

    /// The names of a symbol tree in pre-order with their depths, by recursion
    /// (fine for the shallow trees the strategy makes).
    fn outline(symbols: &[DocumentSymbol], depth: usize, out: &mut Vec<(usize, String)>) {
        for symbol in symbols {
            out.push((depth, symbol.name.clone()));
            outline(&symbol.children, depth + 1, out);
        }
    }

    /// The same from the written JSON.
    fn outline_json(value: &Value<'_>, depth: usize, out: &mut Vec<(usize, String)>) {
        for entry in value.as_array().into_iter().flat_map(|a| a.iter()) {
            out.push((depth, entry["name"].as_str().unwrap_or("?").to_owned()));
            outline_json(&entry["children"], depth + 1, out);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// The iterative writer produces valid JSON that holds the same
        /// hierarchy, in the same order, as the tree it was given.
        #[test]
        fn prop_written_symbols_hold_the_same_hierarchy(tree in tree_strategy()) {
            let mut out = String::new();
            write_symbols(&mut out, &tree);
            let parsed = jsonx_lib::parse(&out);
            prop_assert!(parsed.is_ok(), "{out}");
            let json = parsed.map(Value::into_owned).unwrap_or_default();
            let (mut want, mut got) = (Vec::new(), Vec::new());
            outline(&tree, 0, &mut want);
            outline_json(&json, 0, &mut got);
            prop_assert_eq!(want, got);
        }
    }
}
