//! Request-head parsing and the framing decision.
//!
//! Parsing itself is `httparse`'s job (already in `Cargo.lock` via `ureq`);
//! this module owns only what sits around it: turning parsed fields into
//! this crate's [`super::message`] types, and deciding — from the headers
//! alone — how the body that follows is delimited. That second half is
//! security-load-bearing: RFC 9112 §6.1 requires refusing a request that
//! names both `Content-Length` and `Transfer-Encoding`, because a server and
//! an intermediary that disagree on which one governs is exactly the
//! request-smuggling primitive.

use super::message::{Header, Method};

/// A fully-parsed request head — nothing past the blank line that ends it.
#[derive(Debug)]
pub struct Head {
    pub method: Method,
    pub url: String,
    pub http_version: (u8, u8),
    pub headers: Vec<Header>,
}

/// How the body that follows a [`Head`] is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// No body: absent on a request unless a length or chunking header says
    /// otherwise (RFC 9112 §6.3).
    None,
    /// Exactly this many bytes, from `Content-Length`.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
}

/// The result of one parse attempt over the bytes accumulated so far.
#[derive(Debug)]
pub enum ParseOutcome {
    /// Not a complete head yet — more bytes are needed.
    Partial,
    /// A complete head, the framing of the body that follows it, and how
    /// many bytes of the input buffer the head itself consumed.
    Complete {
        head: Head,
        framing: Framing,
        consumed: usize,
    },
}

/// Why a head could not be parsed or accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// More header lines than the caller allotted room for.
    TooManyHeaders,
    /// The request line or a header line was not well-formed.
    Malformed,
    /// A header name or value contained a byte outside what HTTP allows.
    InvalidHeader,
    /// Both `Content-Length` and `Transfer-Encoding` were present, or
    /// `Content-Length` appeared more than once with disagreeing values —
    /// the smuggling primitive RFC 9112 §6.1 exists to name and forbid.
    AmbiguousFraming,
    /// `Content-Length`'s value was not a valid non-negative integer.
    InvalidContentLength,
    /// `Transfer-Encoding` named something other than (only) `chunked`.
    UnsupportedTransferEncoding,
}

/// Attempts to parse a complete request head out of `buf`. `max_headers`
/// bounds how many header lines are accepted, independent of `buf`'s own
/// size cap (enforced by the caller before this is called again).
pub fn parse_head(buf: &[u8], max_headers: usize) -> Result<ParseOutcome, ParseError> {
    let mut raw_headers = vec![httparse::EMPTY_HEADER; max_headers];
    let mut req = httparse::Request::new(&mut raw_headers);
    let status = req.parse(buf).map_err(|e| match e {
        httparse::Error::TooManyHeaders => ParseError::TooManyHeaders,
        _ => ParseError::Malformed,
    })?;
    let consumed = match status {
        httparse::Status::Partial => return Ok(ParseOutcome::Partial),
        httparse::Status::Complete(n) => n,
    };

    let method = Method::parse(req.method.ok_or(ParseError::Malformed)?);
    let url = req.path.ok_or(ParseError::Malformed)?.to_string();
    // httparse reports a request line's HTTP/x.y as a single minor-version
    // integer against an implicit major of 1; both versions this daemon
    // needs to distinguish (1.0 keep-alive default vs 1.1) are HTTP/1.x.
    let minor = req.version.ok_or(ParseError::Malformed)?;
    let http_version = (1, minor);

    let mut headers = Vec::with_capacity(req.headers.len());
    for h in req.headers.iter() {
        headers.push(Header::from_bytes(h.name, h.value).map_err(|_| ParseError::InvalidHeader)?);
    }

    let framing = framing_from_headers(&headers)?;

    Ok(ParseOutcome::Complete {
        head: Head {
            method,
            url,
            http_version,
            headers,
        },
        framing,
        consumed,
    })
}

/// Decides [`Framing`] from a parsed header list. `Content-Length` and
/// `Transfer-Encoding` together, or a repeated `Content-Length` with
/// disagreeing values, are both refused outright rather than resolved by
/// picking one — see this module's doc comment.
fn framing_from_headers(headers: &[Header]) -> Result<Framing, ParseError> {
    let mut content_length: Option<u64> = None;
    let mut saw_content_length = false;
    let mut chunked = false;

    for h in headers {
        if h.field.equiv("Content-Length") {
            saw_content_length = true;
            let n: u64 = h
                .value
                .trim()
                .parse()
                .map_err(|_| ParseError::InvalidContentLength)?;
            match content_length {
                None => content_length = Some(n),
                Some(existing) if existing == n => {}
                Some(_) => return Err(ParseError::AmbiguousFraming),
            }
        } else if h.field.equiv("Transfer-Encoding") {
            // A comma-separated coding list is legal in general HTTP, but
            // this daemon accepts only the trivial one-element case: `chunked`
            // and nothing else. Anything more (`gzip, chunked`, or `chunked`
            // repeated across multiple header lines) is refused rather than
            // guessed at.
            let coding = h.value.trim();
            if !coding.eq_ignore_ascii_case("chunked") {
                return Err(ParseError::UnsupportedTransferEncoding);
            }
            if chunked {
                return Err(ParseError::AmbiguousFraming);
            }
            chunked = true;
        }
    }

    if chunked && saw_content_length {
        return Err(ParseError::AmbiguousFraming);
    }
    if chunked {
        return Ok(Framing::Chunked);
    }
    if let Some(n) = content_length {
        return Ok(Framing::Length(n));
    }
    Ok(Framing::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> ParseOutcome {
        parse_head(bytes, 64).unwrap()
    }

    #[test]
    fn a_simple_get_has_no_body() {
        let ParseOutcome::Complete { head, framing, .. } =
            parse(b"GET /api/v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        else {
            panic!("expected Complete");
        };
        assert_eq!(head.method, Method::Get);
        assert_eq!(head.url, "/api/v1/hello");
        assert_eq!(head.http_version, (1, 1));
        assert_eq!(framing, Framing::None);
    }

    #[test]
    fn a_partial_head_asks_for_more_bytes() {
        assert!(matches!(
            parse_head(b"GET /api/v1/hello HTTP/1.1\r\nHost: 127", 64).unwrap(),
            ParseOutcome::Partial
        ));
    }

    #[test]
    fn content_length_is_read_as_body_length() {
        let ParseOutcome::Complete { framing, .. } =
            parse(b"POST /api/v1/invoke HTTP/1.1\r\nContent-Length: 1025\r\n\r\n")
        else {
            panic!("expected Complete");
        };
        assert_eq!(framing, Framing::Length(1025));
    }

    #[test]
    fn transfer_encoding_chunked_is_recognized() {
        let ParseOutcome::Complete { framing, .. } =
            parse(b"POST /api/v1/invoke HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n")
        else {
            panic!("expected Complete");
        };
        assert_eq!(framing, Framing::Chunked);
    }

    #[test]
    fn content_length_and_transfer_encoding_together_are_refused() {
        let err = parse_head(
            b"POST / HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n",
            64,
        )
        .unwrap_err();
        assert_eq!(err, ParseError::AmbiguousFraming);
    }

    #[test]
    fn repeated_content_length_with_different_values_is_refused() {
        let err = parse_head(
            b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n",
            64,
        )
        .unwrap_err();
        assert_eq!(err, ParseError::AmbiguousFraming);
    }

    #[test]
    fn repeated_content_length_with_the_same_value_is_accepted() {
        let ParseOutcome::Complete { framing, .. } =
            parse(b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n")
        else {
            panic!("expected Complete");
        };
        assert_eq!(framing, Framing::Length(5));
    }

    #[test]
    fn an_unsupported_transfer_coding_is_refused() {
        let err =
            parse_head(b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n", 64).unwrap_err();
        assert_eq!(err, ParseError::UnsupportedTransferEncoding);
    }

    #[test]
    fn too_many_headers_is_refused() {
        let mut req = String::from("GET / HTTP/1.1\r\n");
        for i in 0..70 {
            req.push_str(&format!("X-H{i}: v\r\n"));
        }
        req.push_str("\r\n");
        let err = parse_head(req.as_bytes(), 64).unwrap_err();
        assert_eq!(err, ParseError::TooManyHeaders);
    }

    #[test]
    fn http_1_0_version_is_reported() {
        let ParseOutcome::Complete { head, .. } = parse(b"GET / HTTP/1.0\r\n\r\n") else {
            panic!("expected Complete");
        };
        assert_eq!(head.http_version, (1, 0));
    }
}
