//! Borrowing deserializers for the messages the server reads itself: the
//! JSON-RPC envelope, `initialize`, and document synchronization.
//!
//! Strings are `Cow<'a, str>` with `#[serde(borrow)]`: borrowed from the
//! message when they contain no escape sequences (URIs, method names, typed
//! characters), owned only when they must be unescaped. Fields the server does
//! not use are skipped by the parser without being materialized.

use std::borrow::Cow;

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use crate::position::Range;

/// Any JSON-RPC message. Which members are present says what it is: `method`
/// and `id` make a request, `method` alone a notification, no `method` a
/// response.
///
/// `id`, `result`, and `error` are `Some` whenever the member is present,
/// `null` included: an `"id": null` still makes a request (an invalid one, in
/// this protocol), and `null` is a valid result. A plain `Option` would read
/// `null` as absent. `error` stays raw so that a malformed error object is
/// still recognized as a response rather than failing the whole envelope.
#[derive(Deserialize)]
pub(crate) struct Envelope<'a> {
    #[serde(default, borrow, deserialize_with = "present")]
    pub(crate) id: Option<&'a RawValue>,
    #[serde(default, borrow)]
    pub(crate) method: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub(crate) params: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub(crate) result: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub(crate) error: Option<&'a RawValue>,
}

/// What is still worth reading from a message whose [`Envelope`] did not
/// parse — a `method` that is not a string, say. Every member is kept raw, so
/// this parses any JSON object without duplicate `id` or `method` members.
///
/// With a `method` the message was meant as a request, and if its `id` is
/// valid the error response goes under that id, so the client can match it to
/// the request. Without one it may be a broken answer to one of the server's
/// own requests, which must never be answered under its id.
#[derive(Deserialize)]
pub(crate) struct Salvage<'a> {
    #[serde(default, borrow, deserialize_with = "present")]
    pub(crate) id: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub(crate) method: Option<&'a RawValue>,
}

impl<'a> Salvage<'a> {
    /// The id to answer an unreadable message `body` under: its own if it was
    /// a request with a valid id, otherwise `null`.
    pub(crate) fn reply_id(body: &'a [u8]) -> &'a str {
        // A derived struct also reads from an array (`[5, "m"]` would be
        // id 5, method "m"), and a batch is not a request with an id.
        if body.trim_ascii_start().first() != Some(&b'{') {
            return "null";
        }
        serde_json::from_slice::<Salvage<'a>>(body)
            .ok()
            .filter(|message| message.method.is_some())
            .and_then(|message| message.id)
            .map(RawValue::get)
            .filter(|id| is_valid_id(id))
            .unwrap_or("null")
    }
}

/// Deserializes a member that is present, whatever its value, as `Some`.
fn present<'de, 'a, D>(deserializer: D) -> Result<Option<&'a RawValue>, D::Error>
where
    D: Deserializer<'de>,
    'de: 'a,
{
    <&'a RawValue>::deserialize(deserializer).map(Some)
}

/// The `error` member of a response from the client, read leniently: a
/// missing message reads as empty and a missing code as `None`, so a sloppy
/// client's error still reaches the handler.
#[derive(Deserialize)]
pub(crate) struct WireError<'a> {
    #[serde(default)]
    pub(crate) code: Option<i64>,
    #[serde(default, borrow)]
    pub(crate) message: Cow<'a, str>,
}

/// Whether a raw JSON-RPC id is one this protocol allows: a string or an
/// integer. (JSON-RPC also allows `null` and fractions; LSP does not.)
pub(crate) fn is_valid_id(raw: &str) -> bool {
    raw.starts_with('"') || raw.parse::<i64>().is_ok()
}

/// The parts of `InitializeParams` the server reads. Every level is optional
/// and `null` reads as absent: the server only looks for the client's
/// position encodings, and falls back to UTF-16 without them.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct InitializeParams<'a> {
    #[serde(borrow)]
    pub(crate) capabilities: Option<ClientCapabilities<'a>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct ClientCapabilities<'a> {
    #[serde(borrow)]
    pub(crate) general: Option<General<'a>>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct General<'a> {
    #[serde(borrow)]
    pub(crate) position_encodings: Option<Vec<Cow<'a, str>>>,
}

impl InitializeParams<'_> {
    /// The encodings the client offered, if it said.
    pub(crate) fn position_encodings(&self) -> impl Iterator<Item = &str> {
        self.capabilities
            .as_ref()
            .and_then(|c| c.general.as_ref())
            .and_then(|g| g.position_encodings.as_ref())
            .into_iter()
            .flatten()
            .map(AsRef::as_ref)
    }
}

/// `textDocument/didOpen`. The text is taken as an owned `String` because the
/// document keeps it: deserializing straight into it is the one copy.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DidOpen<'a> {
    #[serde(borrow)]
    pub(crate) text_document: OpenedDocument<'a>,
}

#[derive(Deserialize)]
pub(crate) struct OpenedDocument<'a> {
    #[serde(borrow)]
    pub(crate) uri: Cow<'a, str>,
    pub(crate) version: i32,
    pub(crate) text: String,
}

/// `textDocument/didChange`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DidChange<'a> {
    #[serde(borrow)]
    pub(crate) text_document: VersionedDocument<'a>,
    #[serde(borrow)]
    pub(crate) content_changes: Vec<ContentChange<'a>>,
}

#[derive(Deserialize)]
pub(crate) struct VersionedDocument<'a> {
    #[serde(borrow)]
    pub(crate) uri: Cow<'a, str>,
    pub(crate) version: i32,
}

/// One edit. Without a range it replaces the whole text.
#[derive(Deserialize)]
pub(crate) struct ContentChange<'a> {
    #[serde(default)]
    pub(crate) range: Option<Range>,
    #[serde(borrow)]
    pub(crate) text: Cow<'a, str>,
}

/// `textDocument/didClose`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DidClose<'a> {
    #[serde(borrow)]
    pub(crate) text_document: DocumentId<'a>,
}

#[derive(Deserialize)]
pub(crate) struct DocumentId<'a> {
    #[serde(borrow)]
    pub(crate) uri: Cow<'a, str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_classifies_by_present_fields() {
        let msg: Option<Envelope<'_>> =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":"a","method":"m","params":[1]}"#).ok();
        let msg = msg.map(|m| {
            (
                m.id.map(RawValue::get),
                m.method,
                m.params.map(RawValue::get),
            )
        });
        assert_eq!(
            msg,
            Some((Some("\"a\""), Some(Cow::Borrowed("m")), Some("[1]")))
        );
    }

    #[test]
    fn test_envelope_null_members_are_present() {
        let msg: Option<Envelope<'_>> = serde_json::from_str(r#"{"id":1,"result":null}"#).ok();
        assert_eq!(msg.and_then(|m| m.result).map(RawValue::get), Some("null"));
        let msg: Option<Envelope<'_>> = serde_json::from_str(r#"{"id":1}"#).ok();
        assert!(msg.is_some_and(|m| m.result.is_none() && m.error.is_none()));
        let msg: Option<Envelope<'_>> =
            serde_json::from_str(r#"{"id":null,"method":"m","error":null}"#).ok();
        assert!(msg.is_some_and(|m| m.id.map(RawValue::get) == Some("null") && m.error.is_some()));
    }

    #[test]
    fn test_envelope_malformed_error_object_still_parses() {
        let msg: Option<Envelope<'_>> =
            serde_json::from_str(r#"{"id":0,"error":{"code":1e3}}"#).ok();
        assert!(msg.is_some_and(|m| m.error.is_some()));
        let wire: Option<WireError<'_>> = serde_json::from_str(r#"{"code":-32601}"#).ok();
        assert!(wire.is_some_and(|w| w.code == Some(-32601) && w.message.is_empty()));
    }

    #[test]
    fn test_salvage_answers_a_broken_request_under_its_own_id() {
        let reply_id = |body: &str| Salvage::reply_id(body.as_bytes()).to_owned();
        // A request whose envelope does not parse keeps its id.
        assert_eq!(reply_id(r#"{"id":5,"method":42}"#), "5");
        assert_eq!(reply_id(r#"{"id":"a-1","method":["m"]}"#), "\"a-1\"");
        // An id the protocol does not allow is never echoed.
        assert_eq!(reply_id(r#"{"id":1.5,"method":42}"#), "null");
        assert_eq!(reply_id(r#"{"id":null,"method":42}"#), "null");
        // Without a method it may be an answer: never reply under its id.
        assert_eq!(reply_id(r#"{"id":5,"result":1,"result":2}"#), "null");
        // Not an object, not JSON, or an ambiguous id: `null`.
        assert_eq!(reply_id("[1,2]"), "null");
        assert_eq!(reply_id(r#" [5, "m"]"#), "null");
        assert_eq!(
            reply_id(r#"  {"id":7,"method":null,"params":1,"params":2}"#),
            "7"
        );
        assert_eq!(reply_id(r#"{"id":5,"method":"#), "null");
        assert_eq!(reply_id(r#"{"id":5,"id":6,"method":"m"}"#), "null");
    }

    #[test]
    fn test_is_valid_id_accepts_strings_and_integers_only() {
        for ok in ["0", "-5", "9007199254740993", "\"abc\"", "\"\""] {
            assert!(is_valid_id(ok), "{ok}");
        }
        for bad in [
            "null",
            "1.5",
            "1e3",
            "true",
            "[1]",
            "{\"a\":1}",
            "99999999999999999999",
        ] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn test_did_change_borrows_plain_text_and_unescapes_newlines() {
        let raw = r#"{"textDocument":{"uri":"u","version":3},"contentChanges":[
            {"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":1}},"text":"x"},
            {"text":"a\nb"}]}"#;
        let change: Option<DidChange<'_>> = serde_json::from_str(raw).ok();
        assert!(change.is_some(), "didChange params did not parse");
        let Some(change) = change else { return };
        assert_eq!(change.text_document.version, 3);
        assert!(matches!(change.content_changes[0].text, Cow::Borrowed("x")));
        assert!(change.content_changes[0].range.is_some());
        assert_eq!(change.content_changes[1].text, "a\nb");
        assert!(change.content_changes[1].range.is_none());
    }

    #[test]
    fn test_initialize_params_tolerate_missing_and_null_capabilities() {
        for raw in [
            "{}",
            r#"{"capabilities":null}"#,
            r#"{"capabilities":{"general":null}}"#,
            r#"{"capabilities":{"general":{"positionEncodings":null}}}"#,
        ] {
            let params: Option<InitializeParams<'_>> = serde_json::from_str(raw).ok();
            assert!(
                params.is_some_and(|p| p.position_encodings().count() == 0),
                "{raw}"
            );
        }
        let params: Option<InitializeParams<'_>> = serde_json::from_str(
            r#"{"capabilities":{"general":{"positionEncodings":["utf-8","utf-16"]}}}"#,
        )
        .ok();
        assert_eq!(params.map(|p| p.position_encodings().count()), Some(2));
    }
}
