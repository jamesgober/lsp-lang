//! Small JSON helpers shared by the protocol types: writing numbers and
//! strings straight into a `String`, and reading typed members out of a
//! `jsonx_lib::Value` with errors that say what was wrong.
//!
//! The protocol types write themselves with `write`/`write_json` methods
//! instead of building a `Value` tree first: a semantic-token answer is a few
//! hundred thousand integers, and a diagnostics notification is written field
//! by field. Reading goes through `Value`, which borrows from the message.

use jsonx_lib::{Limits, Parser, Value};
use rpc_io::Raw;

use crate::response::ResponseError;

/// Appends `n` in decimal.
pub(crate) fn write_u64(out: &mut String, mut n: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &digit in &digits[at..] {
        out.push(char::from(digit));
    }
}

/// Appends `n` in decimal.
pub(crate) fn write_i64(out: &mut String, n: i64) {
    if n < 0 {
        out.push('-');
    }
    write_u64(out, n.unsigned_abs());
}

/// Appends `text` as a JSON string literal, quotes included.
#[inline]
pub(crate) fn write_str(out: &mut String, text: &str) {
    jsonx_lib::write_string(out, text);
}

/// Parses text a protocol type wrote itself into an owned value.
///
/// The writers only produce valid JSON, so a failure here is a bug; it is
/// reported as `null` rather than a panic, and the writers' tests would
/// catch the difference.
pub(crate) fn to_value(text: &str) -> Value<'static> {
    TRUSTED.parse(text).map_or(Value::Null, Value::into_owned)
}

/// A parser with no budgets, for text this crate wrote itself: a semantic-token
/// answer is millions of values, and a deep outline is deep, so the budgets
/// that guard against hostile input do not apply (the text is validated, not
/// trusted blindly, but it is ours).
pub(crate) const TRUSTED: Parser = Parser::new().limits(Limits::unlimited());

/// Validates text this crate wrote as one JSON value, without budgets, and
/// keeps it as the raw text of a message.
pub(crate) fn trusted_raw(text: &str) -> Result<Raw<'static>, ResponseError> {
    Raw::new_with(text, &TRUSTED)
        .map(Raw::into_owned)
        .map_err(|e| {
            ResponseError::new(
                ResponseError::INTERNAL_ERROR,
                format!("the server wrote invalid JSON: {e}"),
            )
        })
}

/// The member `name` of an object, or `Null` for anything else.
#[inline]
pub(crate) fn member<'v, 'a>(value: &'v Value<'a>, name: &str) -> &'v Value<'a> {
    &value[name]
}

/// A required string member.
pub(crate) fn req_str<'v>(value: &'v Value<'_>, name: &str) -> Result<&'v str, ResponseError> {
    match member(value, name) {
        Value::String(s) => Ok(s),
        Value::Null => Err(missing(name)),
        other => Err(wrong(name, "a string", other)),
    }
}

/// A required unsigned 32-bit integer member (the protocol's `uinteger`).
pub(crate) fn req_u32(value: &Value<'_>, name: &str) -> Result<u32, ResponseError> {
    match member(value, name) {
        Value::Null => Err(missing(name)),
        other => as_u32(other).ok_or_else(|| wrong(name, "an unsigned 32-bit integer", other)),
    }
}

/// A required signed 32-bit integer member (the protocol's `integer`).
pub(crate) fn req_i32(value: &Value<'_>, name: &str) -> Result<i32, ResponseError> {
    match member(value, name) {
        Value::Null => Err(missing(name)),
        other => other
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .ok_or_else(|| wrong(name, "a 32-bit integer", other)),
    }
}

/// An integer that fits in `u32`, nothing rounded.
pub(crate) fn as_u32(value: &Value<'_>) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// A required object member; the value itself must be an object.
pub(crate) fn req_object<'v, 'a>(
    value: &'v Value<'a>,
    name: &str,
) -> Result<&'v Value<'a>, ResponseError> {
    match member(value, name) {
        found @ Value::Object(_) => Ok(found),
        Value::Null => Err(missing(name)),
        other => Err(wrong(name, "an object", other)),
    }
}

/// An optional member that, when present and not `null`, must be an object.
pub(crate) fn opt_object<'v, 'a>(
    value: &'v Value<'a>,
    name: &str,
) -> Result<Option<&'v Value<'a>>, ResponseError> {
    match member(value, name) {
        Value::Null => Ok(None),
        found @ Value::Object(_) => Ok(Some(found)),
        other => Err(wrong(name, "an object", other)),
    }
}

fn missing(name: &str) -> ResponseError {
    ResponseError::invalid_params(format!("invalid params: `{name}` is required"))
}

fn wrong(name: &str, expected: &str, found: &Value<'_>) -> ResponseError {
    ResponseError::invalid_params(format!(
        "invalid params: `{name}` must be {expected}, not {}",
        found.kind()
    ))
}

#[cfg(test)]
mod tests {
    use jsonx_lib::json;

    use super::*;

    #[test]
    fn test_write_numbers() {
        let mut out = String::new();
        write_u64(&mut out, 0);
        out.push(' ');
        write_u64(&mut out, u64::MAX);
        out.push(' ');
        write_i64(&mut out, i64::MIN);
        out.push(' ');
        write_i64(&mut out, -7);
        assert_eq!(out, "0 18446744073709551615 -9223372036854775808 -7");
    }

    #[test]
    fn test_required_members_report_what_was_wrong() {
        let v = json!({ "s": "x", "n": 5, "neg": -1, "big": 4294967296_u64, "o": {} });
        assert_eq!(req_str(&v, "s").ok(), Some("x"));
        assert_eq!(req_u32(&v, "n").ok(), Some(5));
        assert!(req_u32(&v, "neg").is_err());
        assert!(req_u32(&v, "big").is_err());
        assert_eq!(req_i32(&v, "neg").ok(), Some(-1));
        assert!(req_i32(&v, "big").is_err());
        let missing = req_str(&v, "absent").err().map(|e| e.message().to_owned());
        assert!(missing.is_some_and(|m| m.contains("`absent` is required")));
        let wrong = req_str(&v, "n").err().map(|e| e.message().to_owned());
        assert!(wrong.is_some_and(|m| m.contains("must be a string, not number")));
        assert!(req_object(&v, "o").is_ok());
        assert!(req_object(&v, "s").is_err());
        assert!(matches!(opt_object(&v, "absent"), Ok(None)));
        assert!(matches!(opt_object(&v, "o"), Ok(Some(_))));
        assert!(opt_object(&v, "n").is_err());
    }

    #[test]
    fn test_to_value_round_trips_written_text() {
        assert_eq!(to_value(r#"{"a":[1,2]}"#), json!({ "a": [1, 2] }));
        assert_eq!(to_value("not json"), Value::Null);
    }
}
