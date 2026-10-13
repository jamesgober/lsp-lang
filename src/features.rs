//! [`Features`]: what a language server offers, from which the capabilities
//! sent in the `initialize` result are derived.

use jsonx_lib::{Object, Value};

use crate::{
    json::{to_value, write_str},
    position::Encoding,
    semantic::SemanticTokensLegend,
};

/// The typed features a [`Handler`](crate::Handler) implements.
///
/// A handler returns one from [`Handler::features`](crate::Handler::features)
/// after `initialize`, and the server advertises exactly these capabilities to
/// the client, answers only these requests, and answers every other feature
/// request with `MethodNotFound`. List a feature here and override the
/// matching method; a feature listed without its method answers `MethodNotFound`
/// from the method's default, and a method overridden without being listed is
/// never called.
///
/// | Builder | Capability | Handler method |
/// |---|---|---|
/// | [`hover`](Features::hover) | `hoverProvider` | [`hover`](crate::Handler::hover) |
/// | [`definition`](Features::definition) | `definitionProvider` | [`definition`](crate::Handler::definition) |
/// | [`completion`](Features::completion) | `completionProvider` | [`completion`](crate::Handler::completion) |
/// | [`document_symbols`](Features::document_symbols) | `documentSymbolProvider` | [`document_symbols`](crate::Handler::document_symbols) |
/// | [`semantic_tokens`](Features::semantic_tokens) | `semanticTokensProvider` (full and delta) | [`semantic_tokens`](crate::Handler::semantic_tokens) |
///
/// Document synchronization (`textDocumentSync`: open, close, incremental
/// changes) and `positionEncoding` are the server's own and are always
/// advertised. Anything else a language offers, such as `renameProvider` for
/// requests it answers through [`Handler::request`](crate::Handler::request),
/// goes in with [`capability`](Features::capability).
///
/// # Examples
///
/// ```
/// use lsp_lang::{Features, SemanticTokensLegend};
///
/// let features = Features::new()
///     .hover()
///     .definition()
///     .completion(&["(", " "])
///     .semantic_tokens(SemanticTokensLegend::standard());
/// let caps = features.capabilities_json();
/// assert_eq!(caps["hoverProvider"], true);
/// assert_eq!(caps["completionProvider"]["triggerCharacters"][0], "(");
/// assert_eq!(caps["semanticTokensProvider"]["full"]["delta"], true);
/// assert!(caps.get("documentSymbolProvider").is_none());
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Features {
    pub(crate) hover: bool,
    pub(crate) definition: bool,
    pub(crate) completion: Option<Vec<String>>,
    pub(crate) document_symbols: bool,
    pub(crate) semantic_tokens: Option<SemanticTokensLegend>,
    extra: Vec<(String, Value<'static>)>,
}

impl Features {
    /// No typed features: document synchronization only.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// assert_eq!(Features::new().capabilities_json().to_string(), "{}");
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offers `textDocument/hover`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// assert_eq!(Features::new().hover().capabilities_json()["hoverProvider"], true);
    /// ```
    #[must_use]
    pub fn hover(mut self) -> Self {
        self.hover = true;
        self
    }

    /// Offers `textDocument/definition`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// assert_eq!(Features::new().definition().capabilities_json()["definitionProvider"], true);
    /// ```
    #[must_use]
    pub fn definition(mut self) -> Self {
        self.definition = true;
        self
    }

    /// Offers `textDocument/completion`. `trigger_characters` are the
    /// characters that open the list on their own (`"."`, `"("`); pass none to
    /// complete only on request.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// let caps = Features::new().completion(&["."]).capabilities_json();
    /// assert_eq!(caps["completionProvider"]["triggerCharacters"][0], ".");
    /// ```
    #[must_use]
    pub fn completion(mut self, trigger_characters: &[&str]) -> Self {
        self.completion = Some(trigger_characters.iter().map(|&c| c.to_owned()).collect());
        self
    }

    /// Offers `textDocument/documentSymbol`, as a hierarchy.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// assert_eq!(Features::new().document_symbols().capabilities_json()["documentSymbolProvider"], true);
    /// ```
    #[must_use]
    pub fn document_symbols(mut self) -> Self {
        self.document_symbols = true;
        self
    }

    /// Offers `textDocument/semanticTokens/full` and its delta form, with this
    /// legend.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Features, SemanticTokensLegend};
    ///
    /// let caps = Features::new()
    ///     .semantic_tokens(SemanticTokensLegend::new(&["keyword"], &[]))
    ///     .capabilities_json();
    /// assert_eq!(caps["semanticTokensProvider"]["legend"]["tokenTypes"][0], "keyword");
    /// ```
    #[must_use]
    pub fn semantic_tokens(mut self, legend: SemanticTokensLegend) -> Self {
        self.semantic_tokens = Some(legend);
        self
    }

    /// Adds a capability the typed features do not cover, keyed as in the
    /// protocol's `ServerCapabilities` (`"renameProvider"`). It overrides a
    /// derived capability of the same name. The server owns two capabilities:
    /// `positionEncoding` cannot be overridden, and a `textDocumentSync`
    /// object is merged into the server's own (`openClose` and `change` are
    /// the server's; members such as `save` are kept).
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{json, Features};
    ///
    /// let caps = Features::new().capability("renameProvider", json!(true)).capabilities_json();
    /// assert_eq!(caps["renameProvider"], true);
    /// ```
    #[must_use]
    pub fn capability(mut self, name: impl Into<String>, value: Value<'static>) -> Self {
        self.extra.push((name.into(), value));
        self
    }

    /// The legend of the semantic tokens offered, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::{Features, SemanticTokensLegend};
    ///
    /// assert!(Features::new().semantic_tokens_legend().is_none());
    /// let features = Features::new().semantic_tokens(SemanticTokensLegend::standard());
    /// assert!(features.semantic_tokens_legend().is_some());
    /// ```
    #[must_use]
    pub fn semantic_tokens_legend(&self) -> Option<&SemanticTokensLegend> {
        self.semantic_tokens.as_ref()
    }

    /// The `ServerCapabilities` these features produce, before the server adds
    /// `textDocumentSync` and `positionEncoding`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lsp_lang::Features;
    ///
    /// let caps = Features::new().hover().capabilities_json();
    /// assert_eq!(caps.to_string(), r#"{"hoverProvider":true}"#);
    /// ```
    #[must_use]
    pub fn capabilities_json(&self) -> Value<'static> {
        let mut out = String::new();
        self.write_capabilities(&mut out, None);
        to_value(&out)
    }

    /// Writes `ServerCapabilities`. With an encoding, the server's own
    /// members are included.
    pub(crate) fn write_capabilities(&self, out: &mut String, encoding: Option<Encoding>) {
        let mut object = Object::new();
        if let Some(encoding) = encoding {
            object.push("positionEncoding", encoding.as_str());
            object.push("textDocumentSync", sync_options(&self.extra));
        }
        if self.hover {
            object.push("hoverProvider", true);
        }
        if self.definition {
            object.push("definitionProvider", true);
        }
        if let Some(triggers) = &self.completion {
            let mut text = String::from("{");
            if !triggers.is_empty() {
                text.push_str("\"triggerCharacters\":[");
                for (i, trigger) in triggers.iter().enumerate() {
                    if i > 0 {
                        text.push(',');
                    }
                    write_str(&mut text, trigger);
                }
                text.push(']');
            }
            text.push('}');
            object.push("completionProvider", to_value(&text));
        }
        if self.document_symbols {
            object.push("documentSymbolProvider", true);
        }
        if let Some(legend) = &self.semantic_tokens {
            let mut text = String::from("{\"legend\":");
            legend.write(&mut text);
            text.push_str(",\"full\":{\"delta\":true}}");
            object.push("semanticTokensProvider", to_value(&text));
        }
        for (name, value) in &self.extra {
            if !matches!(name.as_str(), "textDocumentSync" | "positionEncoding") {
                let _replaced = object.insert(name.clone(), value.clone());
            }
        }
        jsonx_lib::write_value(out, &Value::Object(object));
    }
}

/// The `textDocumentSync` capability: open and close notifications and
/// incremental changes, which are the server's, plus whatever other members a
/// handler added with [`Features::capability`] (`save`, `willSave`).
fn sync_options(extra: &[(String, Value<'static>)]) -> Value<'static> {
    let mut sync = extra
        .iter()
        .rev()
        .find(|(name, _)| name == "textDocumentSync")
        .and_then(|(_, value)| match value {
            Value::Object(members) => Some(members.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let _open_close = sync.insert("openClose", true);
    let _change = sync.insert("change", 2);
    Value::Object(sync)
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;

    #[test]
    fn test_no_features_advertise_nothing_typed() {
        let caps = Features::new().capabilities_json();
        assert_eq!(caps.to_string(), "{}");
    }

    #[test]
    fn test_server_owned_members_are_added_with_an_encoding() {
        let mut out = String::new();
        Features::new()
            .hover()
            .write_capabilities(&mut out, Some(Encoding::Utf8));
        let caps = to_value(&out);
        assert_eq!(caps["positionEncoding"], "utf-8");
        assert_eq!(caps["textDocumentSync"]["change"], 2);
        assert_eq!(caps["textDocumentSync"]["openClose"], true);
        assert_eq!(caps["hoverProvider"], true);
    }

    #[test]
    fn test_completion_without_triggers_is_an_empty_object() {
        let caps = Features::new().completion(&[]).capabilities_json();
        assert_eq!(caps["completionProvider"], json!({}));
    }

    #[test]
    fn test_extra_capabilities_override_derived_but_not_server_owned() {
        let caps = Features::new()
            .hover()
            .capability("hoverProvider", json!({ "workDoneProgress": true }))
            .capability("renameProvider", json!(true))
            .capability("textDocumentSync", json!(0))
            .capability("positionEncoding", json!("utf-32"));
        let mut out = String::new();
        caps.write_capabilities(&mut out, Some(Encoding::Utf16));
        let caps = to_value(&out);
        assert_eq!(caps["hoverProvider"]["workDoneProgress"], true);
        assert_eq!(caps["renameProvider"], true);
        // A non-object `textDocumentSync` is ignored; the server's stands.
        assert_eq!(caps["textDocumentSync"]["change"], 2);
        assert_eq!(caps["textDocumentSync"]["openClose"], true);
        assert_eq!(caps["positionEncoding"], "utf-16");
    }

    #[test]
    fn test_sync_options_keep_extra_members_but_not_the_servers_own() {
        let features = Features::new().capability(
            "textDocumentSync",
            json!({ "save": true, "change": 1, "openClose": false }),
        );
        let mut out = String::new();
        features.write_capabilities(&mut out, Some(Encoding::Utf8));
        let sync = &to_value(&out)["textDocumentSync"];
        assert_eq!(sync["save"], true);
        assert_eq!(sync["change"], 2);
        assert_eq!(sync["openClose"], true);
    }
}
