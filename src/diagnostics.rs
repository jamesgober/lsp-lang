//! Converts [`diag_lang::Diagnostic`]s into the parameters of
//! `textDocument/publishDiagnostics`, writing JSON directly.
//!
//! # Mapping
//!
//! | diag-lang | Protocol |
//! |---|---|
//! | primary label's span | `range` |
//! | `Severity::Error` / `Warning` / `Note` / `Help` | `severity` 1 / 2 / 3 / 4 (Error, Warning, Information, Hint) |
//! | message, primary label, notes, help | `message`: the headline, then the primary label's text, each note as `note: …`, each help line as `help: …`, one per line |
//! | secondary labels | `relatedInformation`, one entry per label, located in the same document |
//!
//! Editors show `message` in full on hover and its first line inline, so the
//! headline leads and the detail follows. Secondary labels become related
//! information because that is what clients render as clickable "see also"
//! locations.

use diag_lang::{Diagnostic, Label, Severity};
use incremental_lang::Grammar;

use crate::{
    document::Document,
    json::{write_i64, write_str, write_u64},
    position::{Position, Range},
};

/// Reused buffers for converting a batch of diagnostics' spans.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    offsets: Vec<u32>,
    order: Vec<u32>,
    positions: Vec<Position>,
    message: String,
}

/// Writes `{"uri":…,"version":…,"diagnostics":[…]}` for `document`.
///
/// Every span is converted up front in one batch
/// ([`Document::positions`]), so publishing many diagnostics on one long line
/// counts that line once rather than once per diagnostic.
pub(crate) fn write_publish<G: Grammar>(
    out: &mut String,
    scratch: &mut Scratch,
    document: &Document<G>,
    diagnostics: &[Diagnostic],
) {
    let Scratch {
        offsets,
        order,
        positions,
        message,
    } = scratch;
    offsets.clear();
    for diagnostic in diagnostics {
        let labels = std::iter::once(diagnostic.primary()).chain(diagnostic.secondary());
        for span in labels.map(Label::span) {
            offsets.extend([span.start().to_u32(), span.end().to_u32()]);
        }
    }
    document.positions(offsets, order, positions);
    // Ranges in label order: each diagnostic's primary, then its secondaries.
    let mut ranges = positions
        .chunks_exact(2)
        .map(|pair| Range::new(pair[0], pair[1]));

    out.push_str("{\"uri\":");
    write_str(out, document.uri());
    out.push_str(",\"version\":");
    write_i64(out, i64::from(document.version()));
    out.push_str(",\"diagnostics\":[");
    for (i, diagnostic) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_diagnostic(out, message, document.uri(), diagnostic, &mut ranges);
    }
    out.push_str("]}");
}

/// Writes `{"uri":…,"diagnostics":[]}`, clearing a document's diagnostics.
pub(crate) fn write_clear(out: &mut String, uri: &str) {
    out.push_str("{\"uri\":");
    write_str(out, uri);
    out.push_str(",\"diagnostics\":[]}");
}

fn write_diagnostic(
    out: &mut String,
    message: &mut String,
    uri: &str,
    diagnostic: &Diagnostic,
    ranges: &mut impl Iterator<Item = Range>,
) {
    let primary = diagnostic.primary();
    out.push_str("{\"range\":");
    ranges.next().unwrap_or_default().write(out);
    out.push_str(",\"severity\":");
    write_u64(out, severity(diagnostic.severity()));
    out.push_str(",\"message\":");
    message.clear();
    message.push_str(diagnostic.message());
    if !primary.message().is_empty() {
        message.push('\n');
        message.push_str(primary.message());
    }
    for note in diagnostic.notes() {
        message.push_str("\nnote: ");
        message.push_str(note);
    }
    for help in diagnostic.help() {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }
    write_str(out, message);
    let secondary = diagnostic.secondary();
    if !secondary.is_empty() {
        out.push_str(",\"relatedInformation\":[");
        for (i, label) in secondary.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let range = ranges.next().unwrap_or_default();
            write_related(out, uri, range, diagnostic, label);
        }
        out.push(']');
    }
    out.push('}');
}

/// One `DiagnosticRelatedInformation`. The protocol requires a message; an
/// unlabelled secondary span borrows the diagnostic's headline.
fn write_related(
    out: &mut String,
    uri: &str,
    range: Range,
    diagnostic: &Diagnostic,
    label: &Label,
) {
    out.push_str("{\"location\":{\"uri\":");
    write_str(out, uri);
    out.push_str(",\"range\":");
    range.write(out);
    out.push_str("},\"message\":");
    let message = if label.message().is_empty() {
        diagnostic.message()
    } else {
        label.message()
    };
    write_str(out, message);
    out.push('}');
}

/// The protocol's `DiagnosticSeverity` for a diag-lang severity.
const fn severity(severity: Severity) -> u64 {
    match severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 3,
        Severity::Help => 4,
    }
}

#[cfg(test)]
mod tests {
    use diag_lang::{Label, Severity};
    use incremental_lang::Span;
    use jsonx_lib::{Value, json};

    use super::*;
    use crate::{position::Encoding, testing::Chars};

    fn publish(text: &str, diagnostics: &[Diagnostic]) -> Value<'static> {
        let mut out = String::new();
        if let Ok(doc) = Document::open(Chars, "file:///d", 4, text.to_owned(), Encoding::Utf16) {
            write_publish(&mut out, &mut Scratch::default(), &doc, diagnostics);
        }
        jsonx_lib::parse(&out).map_or(Value::Null, Value::into_owned)
    }

    #[test]
    fn test_write_publish_maps_every_part() {
        let diagnostic = Diagnostic::new(
            Severity::Error,
            "duplicate \"x\"",
            Label::new(Span::new(8, 9), "redefined here"),
        )
        .with_secondary(Label::new(Span::new(0, 1), "first defined here"))
        .with_secondary(Label::unlabelled(Span::new(2, 3)))
        .with_note("names are global")
        .with_help("rename one");
        let params = publish("x\ny\n😀x", &[diagnostic]);

        assert_eq!(params["uri"], "file:///d");
        assert_eq!(params["version"], 4);
        let d = &params["diagnostics"][0];
        assert_eq!(d["range"]["start"], json!({"line": 2, "character": 2}));
        assert_eq!(d["range"]["end"], json!({"line": 2, "character": 3}));
        assert_eq!(d["severity"], 1);
        assert_eq!(
            d["message"],
            "duplicate \"x\"\nredefined here\nnote: names are global\nhelp: rename one"
        );
        let related = &d["relatedInformation"];
        assert_eq!(related[0]["location"]["uri"], "file:///d");
        assert_eq!(related[0]["message"], "first defined here");
        assert_eq!(related[1]["location"]["range"]["start"]["line"], 1);
        assert_eq!(related[1]["message"], "duplicate \"x\"");
    }

    #[test]
    fn test_write_publish_severities_and_minimal_shape() {
        let at = Label::unlabelled(Span::empty(0));
        let all = [
            Severity::Error,
            Severity::Warning,
            Severity::Note,
            Severity::Help,
        ]
        .map(|s| Diagnostic::new(s, "m", at.clone()));
        let params = publish("", &all);
        for (i, expected) in [1, 2, 3, 4].into_iter().enumerate() {
            let d = &params["diagnostics"][i];
            assert_eq!(d["severity"], expected);
            assert_eq!(d["message"], "m");
            assert!(d.get("relatedInformation").is_none());
        }
    }

    #[test]
    fn test_write_publish_out_of_range_span_is_clamped() {
        let far = Diagnostic::new(Severity::Warning, "m", Label::unlabelled(Span::new(50, 90)));
        let params = publish("ab", &[far]);
        let end = json!({"line": 0, "character": 2});
        assert_eq!(params["diagnostics"][0]["range"]["start"], end);
        assert_eq!(params["diagnostics"][0]["range"]["end"], end);
    }

    #[test]
    fn test_write_clear_has_no_version() {
        let mut out = String::new();
        write_clear(&mut out, "file:///\"q\"");
        assert_eq!(out, r#"{"uri":"file:///\"q\"","diagnostics":[]}"#);
    }
}
