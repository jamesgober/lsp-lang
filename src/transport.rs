//! The wire: base-protocol framing (`Content-Length` headers around JSON
//! bodies) and the outgoing message buffer.
//!
//! Incoming messages are read into one buffer that is reused for the whole
//! session. Outgoing messages are built in a reused body buffer, framed, and
//! appended to an outbox that the server writes with a single `write_all` and
//! `flush` after each incoming message — one system call per turn however many
//! notifications a handler sends, and nothing at all for a turn that sends
//! nothing.

use std::io::{BufRead, Read, Write};

use serde::Serialize;

use crate::{error::Error, json::ResponseError};

/// Longest header line accepted, terminator included. Real headers are well
/// under 100 bytes; the cap keeps a stream with no line breaks from growing a
/// buffer without bound.
const MAX_HEADER_LINE: u64 = 1024;

/// Most header lines accepted before the blank line. The protocol defines two.
const MAX_HEADERS: usize = 16;

/// Most body capacity reserved before any of the body has arrived.
const MAX_UPFRONT_RESERVE: usize = 1 << 20;

/// Reads one message body into `body`, using `line` as header scratch.
///
/// Returns `Ok(false)` if the input ends cleanly before a message begins.
pub(crate) fn read_message<R: BufRead>(
    input: &mut R,
    body: &mut Vec<u8>,
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, Error> {
    let mut length = None;
    let mut headers = 0;
    loop {
        line.clear();
        let n = input
            .by_ref()
            .take(MAX_HEADER_LINE)
            .read_until(b'\n', line)?;
        if n == 0 {
            return if headers == 0 && length.is_none() {
                Ok(false)
            } else {
                Err(Error::Disconnected)
            };
        }
        if line.last() != Some(&b'\n') {
            return Err(if n as u64 == MAX_HEADER_LINE {
                Error::InvalidHeader {
                    reason: "header line too long",
                }
            } else {
                Error::Disconnected
            });
        }
        let text = trim_line(line);
        if text.is_empty() {
            if headers == 0 {
                // Tolerate stray blank lines between messages.
                continue;
            }
            break;
        }
        headers += 1;
        if headers > MAX_HEADERS {
            return Err(Error::InvalidHeader {
                reason: "too many header lines",
            });
        }
        if let Some(value) = header_value(text, b"content-length") {
            length = Some(parse_length(value)?);
        }
    }
    let len = length.ok_or(Error::InvalidHeader {
        reason: "missing Content-Length",
    })?;
    if len > limit {
        return Err(Error::TooLarge { len, limit });
    }
    body.clear();
    // Reserve what the header announces, up to a cap: the buffer then grows
    // only as bytes actually arrive, so a client cannot make the server
    // allocate the whole message limit by announcing a body it never sends.
    // The buffer is reused, so a large message's growth is paid once.
    body.reserve(len.min(MAX_UPFRONT_RESERVE));
    let read = input.by_ref().take(len as u64).read_to_end(body)?;
    if read == len {
        Ok(true)
    } else {
        Err(Error::Disconnected)
    }
}

/// Strips the line terminator (`\r\n`, or a bare `\n` from a lenient client).
fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The value of a `name: value` header line if its name matches `name`
/// (lowercase), ignoring case, as HTTP-style header names are.
fn header_value<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let colon = line.iter().position(|&b| b == b':')?;
    let (key, value) = line.split_at(colon);
    key.eq_ignore_ascii_case(name)
        .then(|| value[1..].trim_ascii())
}

/// Parses a `Content-Length` value: decimal digits only, no overflow.
fn parse_length(value: &[u8]) -> Result<usize, Error> {
    const INVALID: Error = Error::InvalidHeader {
        reason: "Content-Length is not a non-negative integer",
    };
    if value.is_empty() {
        return Err(INVALID);
    }
    value.iter().try_fold(0usize, |n, &b| {
        if b.is_ascii_digit() {
            n.checked_mul(10)
                .and_then(|n| n.checked_add(usize::from(b - b'0')))
                .ok_or(INVALID)
        } else {
            Err(INVALID)
        }
    })
}

/// Outgoing messages, framed and waiting to be written.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    /// Framed messages ready to write.
    out: Vec<u8>,
    /// The body under construction. Building it apart from `out` means a
    /// serialization that fails halfway leaves `out` untouched.
    body: Vec<u8>,
}

impl Outbox {
    /// Sends a response to the request whose raw id is `id`.
    pub(crate) fn respond(&mut self, id: &str, result: Result<&str, &ResponseError>) {
        self.body.clear();
        self.body.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
        self.body.extend_from_slice(id.as_bytes());
        match result {
            Ok(json) => {
                self.body.extend_from_slice(b",\"result\":");
                self.body.extend_from_slice(json.as_bytes());
            }
            Err(error) => {
                self.body.extend_from_slice(b",\"error\":{\"code\":");
                write_i64(&mut self.body, i64::from(error.code()));
                self.body.extend_from_slice(b",\"message\":");
                write_str(&mut self.body, error.message());
                self.body.push(b'}');
            }
        }
        self.body.push(b'}');
        self.frame();
    }

    /// Sends a notification whose params `serde_json` serializes.
    pub(crate) fn notify<P: Serialize + ?Sized>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<(), Error> {
        self.start(method, None);
        serde_json::to_writer(&mut self.body, params).map_err(Error::Json)?;
        self.body.push(b'}');
        self.frame();
        Ok(())
    }

    /// Sends a request with a numeric `id` whose params `serde_json` serializes.
    pub(crate) fn request<P: Serialize + ?Sized>(
        &mut self,
        id: u64,
        method: &str,
        params: &P,
    ) -> Result<(), Error> {
        self.start(method, Some(id));
        serde_json::to_writer(&mut self.body, params).map_err(Error::Json)?;
        self.body.push(b'}');
        self.frame();
        Ok(())
    }

    /// Sends a notification whose params `write` appends as JSON directly. For
    /// the messages the crate builds itself, which cannot fail to serialize.
    pub(crate) fn notify_with(&mut self, method: &str, write: impl FnOnce(&mut Vec<u8>)) {
        self.start(method, None);
        write(&mut self.body);
        self.body.push(b'}');
        self.frame();
    }

    /// Sends `window/logMessage` — how the server reports problems it cannot
    /// answer, such as a malformed notification. `kind` is the protocol's
    /// `MessageType`: 1 error, 2 warning, 3 info, 4 log.
    pub(crate) fn log(&mut self, kind: u8, message: &str) {
        self.notify_with("window/logMessage", |out| {
            out.extend_from_slice(b"{\"type\":");
            write_u64(out, u64::from(kind));
            out.extend_from_slice(b",\"message\":");
            write_str(out, message);
            out.push(b'}');
        });
    }

    /// Writes all pending messages to `output` and flushes it. Does nothing
    /// when there is nothing to send.
    pub(crate) fn flush<W: Write>(&mut self, output: &mut W) -> Result<(), Error> {
        if self.out.is_empty() {
            return Ok(());
        }
        output.write_all(&self.out)?;
        self.out.clear();
        output.flush()?;
        Ok(())
    }

    /// Begins a request or notification body, up to `"params":`.
    fn start(&mut self, method: &str, id: Option<u64>) {
        self.body.clear();
        self.body.extend_from_slice(b"{\"jsonrpc\":\"2.0\",");
        if let Some(id) = id {
            self.body.extend_from_slice(b"\"id\":");
            write_u64(&mut self.body, id);
            self.body.push(b',');
        }
        self.body.extend_from_slice(b"\"method\":");
        write_str(&mut self.body, method);
        self.body.extend_from_slice(b",\"params\":");
    }

    /// Frames the finished body into `out`.
    fn frame(&mut self) {
        self.out.extend_from_slice(b"Content-Length: ");
        write_u64(&mut self.out, self.body.len() as u64);
        self.out.extend_from_slice(b"\r\n\r\n");
        self.out.extend_from_slice(&self.body);
    }

    /// The framed bytes waiting to be written, for tests.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> &[u8] {
        &self.out
    }
}

/// Appends `s` as a JSON string literal, quotes included.
pub(crate) fn write_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    write_str_contents(out, s);
    out.push(b'"');
}

/// Appends `s` escaped for the inside of a JSON string literal.
///
/// Copies runs of bytes that need no escape in one `extend`, so text without
/// quotes, backslashes, or control characters is a single copy. Non-ASCII text
/// is valid in JSON as-is and is copied unchanged.
pub(crate) fn write_str_contents(out: &mut Vec<u8>, s: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = s.as_bytes();
    let mut run = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b >= 0x20 && b != b'"' && b != b'\\' {
            continue;
        }
        out.extend_from_slice(&bytes[run..i]);
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            _ => out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[usize::from(b >> 4)],
                HEX[usize::from(b & 0xF)],
            ]),
        }
        run = i + 1;
    }
    out.extend_from_slice(&bytes[run..]);
}

/// Appends `n` in decimal.
pub(crate) fn write_u64(out: &mut Vec<u8>, mut n: u64) {
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
    out.extend_from_slice(&digits[at..]);
}

/// Appends `n` in decimal, with a leading `-` when negative.
pub(crate) fn write_i64(out: &mut Vec<u8>, n: i64) {
    if n < 0 {
        out.push(b'-');
    }
    write_u64(out, n.unsigned_abs());
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn read(input: &[u8], limit: usize) -> Result<Option<Vec<u8>>, Error> {
        let mut cursor = Cursor::new(input);
        let (mut body, mut line) = (Vec::new(), Vec::new());
        Ok(read_message(&mut cursor, &mut body, &mut line, limit)?.then_some(body))
    }

    #[test]
    fn test_read_message_well_formed_returns_body() {
        let body = read(b"Content-Length: 2\r\n\r\n{}", 64).ok().flatten();
        assert_eq!(body.as_deref(), Some(&b"{}"[..]));
    }

    #[test]
    fn test_read_message_header_name_is_case_insensitive_and_extra_headers_ignored() {
        let input = b"content-type: application/vscode-jsonrpc; charset=utf-8\r\nCONTENT-LENGTH:  3 \r\n\r\n[1]";
        assert_eq!(read(input, 64).ok().flatten().as_deref(), Some(&b"[1]"[..]));
    }

    #[test]
    fn test_read_message_bare_newlines_are_accepted() {
        let body = read(b"Content-Length: 2\n\n{}", 64).ok().flatten();
        assert_eq!(body.as_deref(), Some(&b"{}"[..]));
    }

    #[test]
    fn test_read_message_empty_input_returns_none() {
        assert!(matches!(read(b"", 64), Ok(None)));
        assert!(matches!(read(b"\r\n", 64), Ok(None)));
    }

    #[test]
    fn test_read_message_missing_length_is_invalid_header() {
        assert!(matches!(
            read(b"Content-Type: x\r\n\r\n{}", 64),
            Err(Error::InvalidHeader { .. })
        ));
    }

    #[test]
    fn test_read_message_bad_length_is_invalid_header() {
        for bad in [&b"-1"[..], b"", b"1e3", b"99999999999999999999999999"] {
            let mut input = b"Content-Length: ".to_vec();
            input.extend_from_slice(bad);
            input.extend_from_slice(b"\r\n\r\n");
            assert!(matches!(read(&input, 64), Err(Error::InvalidHeader { .. })));
        }
    }

    #[test]
    fn test_read_message_over_limit_returns_too_large() {
        assert!(matches!(
            read(b"Content-Length: 65\r\n\r\n", 64),
            Err(Error::TooLarge { len: 65, limit: 64 })
        ));
        // Exactly at the limit is fine.
        let mut input = b"Content-Length: 4\r\n\r\n".to_vec();
        input.extend_from_slice(b"null");
        assert!(matches!(read(&input, 4), Ok(Some(_))));
    }

    #[test]
    fn test_read_message_truncated_input_is_disconnected() {
        assert!(matches!(
            read(b"Content-Length: 9\r\n\r\n{}", 64),
            Err(Error::Disconnected)
        ));
        assert!(matches!(
            read(b"Content-Length: 9\r\n", 64),
            Err(Error::Disconnected)
        ));
        assert!(matches!(read(b"Content-Len", 64), Err(Error::Disconnected)));
    }

    #[test]
    fn test_read_message_overlong_header_line_is_invalid() {
        let input = vec![b'x'; 4096];
        assert!(matches!(read(&input, 64), Err(Error::InvalidHeader { .. })));
    }

    #[test]
    fn test_read_message_too_many_headers_is_invalid() {
        let input = b"X: y\r\n".repeat(MAX_HEADERS + 1);
        assert!(matches!(read(&input, 64), Err(Error::InvalidHeader { .. })));
    }

    #[test]
    fn test_write_str_escapes_what_json_requires() {
        let mut out = Vec::new();
        write_str(&mut out, "a\"b\\c\nd\te\u{1}é");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\te\\u0001é\"".as_bytes());
        // Round-trips through a real JSON parser.
        let back: String = serde_json::from_slice(&out).unwrap_or_default();
        assert_eq!(back, "a\"b\\c\nd\te\u{1}é");
    }

    #[test]
    fn test_write_numbers_cover_extremes() {
        let mut out = Vec::new();
        write_u64(&mut out, 0);
        out.push(b' ');
        write_u64(&mut out, u64::MAX);
        out.push(b' ');
        write_i64(&mut out, i64::MIN);
        assert_eq!(out, b"0 18446744073709551615 -9223372036854775808");
    }

    #[test]
    fn test_outbox_frames_messages_and_flushes_once() {
        let mut outbox = Outbox::default();
        outbox.respond("7", Ok("null"));
        outbox.log(3, "hi");
        let pending = String::from_utf8_lossy(outbox.pending()).into_owned();
        assert!(pending.starts_with(
            "Content-Length: 38\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":null}"
        ));
        assert!(pending.ends_with("\"params\":{\"type\":3,\"message\":\"hi\"}}"));

        let mut sink = Vec::new();
        assert!(outbox.flush(&mut sink).is_ok());
        assert_eq!(String::from_utf8_lossy(&sink), pending);
        assert!(outbox.pending().is_empty());
    }

    #[test]
    fn test_outbox_failed_serialization_sends_nothing() {
        let mut outbox = Outbox::default();
        let mut bad = std::collections::BTreeMap::new();
        let _previous = bad.insert(vec![1u8], 1);
        assert!(matches!(outbox.notify("x", &bad), Err(Error::Json(_))));
        assert!(outbox.pending().is_empty());
    }
}
