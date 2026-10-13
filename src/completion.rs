//! [`CompletionItem`] and [`CompletionItemKind`]: the answer to
//! `textDocument/completion`.

use jsonx_lib::Value;

use crate::{
    json::{to_value, write_str, write_u64},
    types::{MarkupKind, TextEdit, write_markup},
};

/// What a completion item is, which clients show as an icon.
///
/// The numbers are the protocol's `CompletionItemKind` values.
///
/// # Examples
///
/// ```
/// use lsp_lang::CompletionItemKind;
///
/// assert_eq!(CompletionItemKind::Keyword.as_u32(), 14);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)] // The variants are the protocol's names, one per icon.
pub enum CompletionItemKind {
    Text = 1,
    Method = 2,
    Function = 3,
    Constructor = 4,
    Field = 5,
    Variable = 6,
    Class = 7,
    Interface = 8,
    Module = 9,
    Property = 10,
    Unit = 11,
    Value = 12,
    Enum = 13,
    Keyword = 14,
    Snippet = 15,
    Color = 16,
    File = 17,
    Reference = 18,
    Folder = 19,
    EnumMember = 20,
    Constant = 21,
    Struct = 22,
    Event = 23,
    Operator = 24,
    TypeParameter = 25,
}

impl CompletionItemKind {
    /// The protocol's number for this kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItemKind;
    ///
    /// assert_eq!(CompletionItemKind::Function.as_u32(), 3);
    /// ```
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// One suggestion in a completion list.
///
/// Only `label` is required. Without `insert_text` or `text_edit`, the client
/// inserts the label. Build one with [`new`](CompletionItem::new) and chain the
/// `with_*` methods.
///
/// # Examples
///
/// ```
/// use lsp_lang::{CompletionItem, CompletionItemKind};
///
/// let item = CompletionItem::new("define")
///     .with_kind(CompletionItemKind::Keyword)
///     .with_detail("(define name value)")
///     .with_documentation("Binds `name` to `value`.");
/// let json = item.to_json();
/// assert_eq!(json["label"], "define");
/// assert_eq!(json["kind"], 14);
/// assert_eq!(json["documentation"]["kind"], "markdown");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CompletionItem {
    /// What the user sees in the list, and what is inserted by default.
    pub label: String,
    /// The icon.
    pub kind: Option<CompletionItemKind>,
    /// A short line after the label, such as a signature.
    pub detail: Option<String>,
    /// Longer help, as Markdown.
    pub documentation: Option<String>,
    /// What the list is ordered by, instead of the label.
    pub sort_text: Option<String>,
    /// What the typed prefix is matched against, instead of the label.
    pub filter_text: Option<String>,
    /// What is inserted, instead of the label.
    pub insert_text: Option<String>,
    /// An edit that is applied instead of inserting text at the cursor.
    pub text_edit: Option<TextEdit>,
}

impl CompletionItem {
    /// An item that inserts and shows `label`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// assert_eq!(CompletionItem::new("define").label, "define");
    /// ```
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: None,
            detail: None,
            documentation: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            text_edit: None,
        }
    }

    /// Sets the icon.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{CompletionItem, CompletionItemKind};
    ///
    /// let item = CompletionItem::new("x").with_kind(CompletionItemKind::Keyword);
    /// assert_eq!(item.kind, Some(CompletionItemKind::Keyword));
    /// ```
    #[must_use]
    pub fn with_kind(mut self, kind: CompletionItemKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Sets the short detail line.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// let item = CompletionItem::new("x").with_detail("(x y)");
    /// assert_eq!(item.detail.as_deref(), Some("(x y)"));
    /// ```
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Sets the Markdown documentation.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// let item = CompletionItem::new("x").with_documentation("*x* does y");
    /// assert_eq!(item.to_json()["documentation"]["value"], "*x* does y");
    /// ```
    #[must_use]
    pub fn with_documentation(mut self, documentation: impl Into<String>) -> Self {
        self.documentation = Some(documentation.into());
        self
    }

    /// Sets the sort key.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// let item = CompletionItem::new("x").with_sort_text("0001");
    /// assert_eq!(item.to_json()["sortText"], "0001");
    /// ```
    #[must_use]
    pub fn with_sort_text(mut self, sort_text: impl Into<String>) -> Self {
        self.sort_text = Some(sort_text.into());
        self
    }

    /// Sets the filter key.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// let item = CompletionItem::new("x").with_filter_text("ex");
    /// assert_eq!(item.to_json()["filterText"], "ex");
    /// ```
    #[must_use]
    pub fn with_filter_text(mut self, filter_text: impl Into<String>) -> Self {
        self.filter_text = Some(filter_text.into());
        self
    }

    /// Sets the inserted text.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// let item = CompletionItem::new("x").with_insert_text("x()");
    /// assert_eq!(item.to_json()["insertText"], "x()");
    /// ```
    #[must_use]
    pub fn with_insert_text(mut self, insert_text: impl Into<String>) -> Self {
        self.insert_text = Some(insert_text.into());
        self
    }

    /// Sets the edit applied on acceptance.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{CompletionItem, Position, Range, TextEdit};
    ///
    /// let edit = TextEdit::new(Range::new(Position::new(0, 0), Position::new(0, 2)), "xy");
    /// let item = CompletionItem::new("xy").with_text_edit(edit);
    /// assert_eq!(item.to_json()["textEdit"]["newText"], "xy");
    /// ```
    #[must_use]
    pub fn with_text_edit(mut self, text_edit: TextEdit) -> Self {
        self.text_edit = Some(text_edit);
        self
    }

    /// The protocol's JSON for this item.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::CompletionItem;
    ///
    /// assert_eq!(CompletionItem::new("x").to_json().to_string(), r#"{"label":"x"}"#);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write(&mut out);
        to_value(&out)
    }

    pub(crate) fn write(&self, out: &mut String) {
        out.push_str("{\"label\":");
        write_str(out, &self.label);
        if let Some(kind) = self.kind {
            out.push_str(",\"kind\":");
            write_u64(out, u64::from(kind.as_u32()));
        }
        let strings = [
            ("detail", &self.detail),
            ("sortText", &self.sort_text),
            ("filterText", &self.filter_text),
            ("insertText", &self.insert_text),
        ];
        for (name, value) in strings {
            if let Some(value) = value {
                out.push_str(",\"");
                out.push_str(name);
                out.push_str("\":");
                write_str(out, value);
            }
        }
        if let Some(documentation) = &self.documentation {
            out.push_str(",\"documentation\":");
            write_markup(out, MarkupKind::Markdown, documentation);
        }
        if let Some(edit) = &self.text_edit {
            out.push_str(",\"textEdit\":");
            edit.write(out);
        }
        out.push('}');
    }
}

/// Writes a complete `CompletionList` holding `items`.
pub(crate) fn write_list(out: &mut String, items: &[CompletionItem]) {
    out.push_str("{\"isIncomplete\":false,\"items\":[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        item.write(out);
    }
    out.push_str("]}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{Position, Range};

    #[test]
    fn test_minimal_item_has_only_a_label() {
        let json = CompletionItem::new("x").to_json();
        assert_eq!(json.to_string(), r#"{"label":"x"}"#);
    }

    #[test]
    fn test_full_item_writes_every_member() {
        let edit = TextEdit::new(Range::new(Position::new(0, 0), Position::new(0, 1)), "xy");
        let item = CompletionItem::new("a\"b")
            .with_kind(CompletionItemKind::TypeParameter)
            .with_detail("d")
            .with_documentation("*doc*")
            .with_sort_text("1")
            .with_filter_text("f")
            .with_insert_text("ins")
            .with_text_edit(edit);
        let json = item.to_json();
        assert_eq!(json["label"], "a\"b");
        assert_eq!(json["kind"], 25);
        assert_eq!(json["detail"], "d");
        assert_eq!(json["documentation"]["value"], "*doc*");
        assert_eq!(json["sortText"], "1");
        assert_eq!(json["filterText"], "f");
        assert_eq!(json["insertText"], "ins");
        assert_eq!(json["textEdit"]["newText"], "xy");
    }

    #[test]
    fn test_list_is_complete_and_valid_json() {
        let mut out = String::new();
        write_list(
            &mut out,
            &[CompletionItem::new("a"), CompletionItem::new("b")],
        );
        let json = jsonx_lib::parse(&out).map_or(Value::Null, Value::into_owned);
        assert_eq!(json["isIncomplete"], false);
        assert_eq!(json["items"][1]["label"], "b");
        out.clear();
        write_list(&mut out, &[]);
        assert_eq!(out, r#"{"isIncomplete":false,"items":[]}"#);
    }

    #[test]
    fn test_kind_numbers_match_the_protocol() {
        assert_eq!(CompletionItemKind::Text.as_u32(), 1);
        assert_eq!(CompletionItemKind::TypeParameter.as_u32(), 25);
    }
}
