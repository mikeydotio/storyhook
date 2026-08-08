//! HTTP/1.1 message vocabulary: methods, headers, status codes, and outgoing
//! responses.
//!
//! Deliberately shaped to match the subset of `tiny_http`'s types this daemon
//! used (SH-177): every call site outside this module changed only its `use`
//! line when the transport swapped underneath it.

use std::io::{self, Write};

/// An HTTP request method. `Other` carries anything this daemon does not
/// route to, so an unrecognized verb is a 405 downstream rather than a parse
/// failure here — the same latitude `tiny_http` gave callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Other(String),
}

impl Method {
    pub(super) fn parse(token: &str) -> Method {
        match token {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            other => Method::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Method::Get => write!(f, "GET"),
            Method::Head => write!(f, "HEAD"),
            Method::Post => write!(f, "POST"),
            Method::Put => write!(f, "PUT"),
            Method::Patch => write!(f, "PATCH"),
            Method::Delete => write!(f, "DELETE"),
            Method::Options => write!(f, "OPTIONS"),
            Method::Other(s) => write!(f, "{s}"),
        }
    }
}

/// A header name, compared the way HTTP requires: ASCII case-insensitively.
/// `PartialEq` is deliberately not derived on the raw string — every
/// comparison goes through [`HeaderField::equiv`], so a future field can
/// never accidentally end up compared case-sensitively.
#[derive(Debug, Clone)]
pub struct HeaderField(String);

impl HeaderField {
    pub fn equiv(&self, other: &str) -> bool {
        self.0.eq_ignore_ascii_case(other)
    }
}

impl std::fmt::Display for HeaderField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One HTTP header field. `field` compares case-insensitively; `value` is
/// used verbatim.
#[derive(Debug, Clone)]
pub struct Header {
    pub field: HeaderField,
    pub value: String,
}

/// A header's name or value contained a byte HTTP forbids in that position —
/// a control character, or anything outside ASCII. `tiny_http` enforced the
/// same rule for the same reason: header text is a wire format, not general
/// Unicode.
#[derive(Debug, Clone, Copy)]
pub struct HeaderError;

fn is_valid_field_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic() && b != b':')
}

fn is_valid_field_value(s: &str) -> bool {
    s.bytes().all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

impl Header {
    pub fn from_bytes(
        name: impl AsRef<[u8]>,
        value: impl AsRef<[u8]>,
    ) -> Result<Header, HeaderError> {
        let name = std::str::from_utf8(name.as_ref()).map_err(|_| HeaderError)?;
        let value = std::str::from_utf8(value.as_ref()).map_err(|_| HeaderError)?;
        if !is_valid_field_name(name) || !is_valid_field_value(value) {
            return Err(HeaderError);
        }
        Ok(Header {
            field: HeaderField(name.to_string()),
            value: value.to_string(),
        })
    }
}

/// An HTTP status code. Exists as its own type (rather than a bare `u16`) so
/// [`Response::with_status_code`] can accept either — matching the call sites
/// this module inherited from `tiny_http`, which pass a plain `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCode(pub u16);

impl From<u16> for StatusCode {
    fn from(code: u16) -> Self {
        StatusCode(code)
    }
}

fn reason_phrase(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        417 => "Expectation Failed",
        422 => "Unprocessable Entity",
        426 => "Upgrade Required",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

/// An outgoing response: a status, a header list, and a body already in
/// memory. Every response this daemon sends is small (JSON, HTML, or plain
/// text — see `crate::api::http::MAX_BODY_BYTES` for the inbound analogue),
/// so buffering the body rather than streaming it costs nothing observable
/// and removes a whole class of partial-write bookkeeping.
pub struct Response {
    pub(super) status: StatusCode,
    pub(super) headers: Vec<Header>,
    pub(super) body: Vec<u8>,
}

impl Response {
    pub fn from_string(body: impl Into<String>) -> Response {
        Response {
            status: StatusCode(200),
            headers: Vec::new(),
            body: body.into().into_bytes(),
        }
    }

    pub fn new_empty(code: impl Into<StatusCode>) -> Response {
        Response {
            status: code.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_status_code(mut self, code: impl Into<StatusCode>) -> Response {
        self.status = code.into();
        self
    }

    #[must_use]
    pub fn with_header(mut self, header: Header) -> Response {
        self.headers.push(header);
        self
    }
}

/// Writes `response`'s status line, headers, a computed `Content-Length`,
/// and body to `writer`. `keep_alive` decides the one header this function
/// adds on its own — every other header comes from `response.headers` as
/// given, so [`crate::api::http::finish`]'s security headers are never
/// duplicated or reordered.
///
/// `suppress_body` is set for a `HEAD` request and for status codes that
/// forbid one (1xx, 204, 304) — `Content-Length` is still sent so the peer
/// knows how long a `GET` of the same resource would be, matching
/// `tiny_http`'s behavior exactly.
///
/// Assembled into one buffer and sent as a single [`Write::write_all`]
/// rather than one call per line: nothing about `TcpStream` promises that
/// several small writes land in one TCP segment, so a peer reading with a
/// single `read()` — as a raw-socket test naturally does — could otherwise
/// see the response arrive split, exactly as if the connection had stalled
/// partway through.
pub(super) fn write_response<W: Write>(
    writer: &mut W,
    response: &Response,
    keep_alive: bool,
    suppress_body: bool,
) -> io::Result<()> {
    let mut head = String::new();
    use std::fmt::Write as _;
    let _ = write!(
        head,
        "HTTP/1.1 {} {}\r\n",
        response.status.0,
        reason_phrase(response.status.0)
    );
    for header in &response.headers {
        let _ = write!(head, "{}: {}\r\n", header.field, header.value);
    }
    let _ = write!(head, "Content-Length: {}\r\n", response.body.len());
    let _ = write!(
        head,
        "Connection: {}\r\n",
        if keep_alive { "keep-alive" } else { "close" }
    );
    head.push_str("\r\n");

    if suppress_body {
        writer.write_all(head.as_bytes())
    } else {
        let mut out = head.into_bytes();
        out.extend_from_slice(&response.body);
        writer.write_all(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_field_compares_ascii_case_insensitively() {
        let h = Header::from_bytes("Content-Type", "text/plain").unwrap();
        assert!(h.field.equiv("content-type"));
        assert!(h.field.equiv("CONTENT-TYPE"));
        assert!(!h.field.equiv("content-length"));
    }

    #[test]
    fn a_header_name_with_a_colon_is_refused() {
        assert!(Header::from_bytes("bad:name", "v").is_err());
    }

    #[test]
    fn a_non_ascii_header_value_is_refused() {
        assert!(Header::from_bytes("X-Name", "café").is_err());
    }

    #[test]
    fn write_response_sends_content_length_and_connection() {
        let resp = Response::from_string("hi").with_status_code(200u16);
        let mut out = Vec::new();
        write_response(&mut out, &resp, true, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains("Connection: keep-alive\r\n"));
        assert!(text.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn write_response_suppresses_body_but_keeps_content_length() {
        let resp = Response::from_string("hi").with_status_code(200u16);
        let mut out = Vec::new();
        write_response(&mut out, &resp, false, true).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(!text.ends_with("hi"));
    }
}
