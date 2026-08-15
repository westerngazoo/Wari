// SPDX-License-Identifier: AGPL-3.0-only
//! Pure HTTP/1.1 request parsing and response building.
//!
//! One concern: turn wire bytes into a typed [`Request`] view and
//! typed response parts back into wire bytes — with **zero
//! allocation, zero copies, and no `unsafe`**. Every parsed field is
//! a borrowed slice of the caller's receive buffer; every response is
//! written into a caller-provided send buffer. The crate compiles for
//! `wasm32` (the Tier-2 MCP server consumes it) and for the host
//! (where its tests run) — the `wari-policy`/`wari-validate`
//! extraction discipline.
//!
//! ## Scope (deliberately narrow — CLAUDE.md "Simplicity First")
//!
//! Exactly what the MCP Streamable-HTTP transport and a plain
//! cloud-OS upload path need:
//!
//! - Request line + headers + `Content-Length`-delimited body.
//! - Incremental use: [`parse`] on a partially received buffer
//!   returns [`Parse::Partial`], so the caller keeps `recv`-ing into
//!   the same buffer and re-parses — no parser state to persist.
//! - Response building with a fixed status/header/body shape.
//!
//! NOT here (rejected until a caller exists, not deferred vaguely):
//! chunked transfer encoding, multipart, HTTP/2, percent-decoding,
//! header continuation lines (obsolete per RFC 7230 §3.2.4 — folded
//! headers are rejected as malformed rather than silently joined).
//!
//! ## Bounds
//!
//! Every dimension an attacker controls is capped by a named
//! constant: header count ([`MAX_HEADERS`]), request-line length
//! ([`MAX_REQUEST_LINE`]), header-line length ([`MAX_HEADER_LINE`]),
//! and body length ([`MAX_BODY`]). Exceeding any cap is a parse
//! error, not a truncation — a request that does not fit the model
//! is refused whole (refuse-by-default, INV-20 spirit).
//!
//! ## Prior art
//!
//! Shape follows `httparse` (borrow-based, caller-owned buffers,
//! Partial/Complete) — reimplemented rather than depended on because
//! the Tier-2 trust base admits no third-party code without its own
//! review, and the subset we need is small (see `docs/prior-art.md`
//! for the dependency policy).

#![no_std]

/// Maximum number of headers a request may carry. MCP clients send
/// well under ten; 16 leaves margin without inviting header-bomb
/// games.
pub const MAX_HEADERS: usize = 16;

/// Maximum bytes in the request line (`METHOD SP target SP version`).
pub const MAX_REQUEST_LINE: usize = 1024;

/// Maximum bytes in a single header line.
pub const MAX_HEADER_LINE: usize = 1024;

/// Maximum declared `Content-Length`. Matches the module registry's
/// per-slot budget (`kernel/src/runtime/modreg.rs::MAX_MODULE_BYTES`)
/// — the largest thing this server ever legitimately receives is a
/// staged module envelope.
pub const MAX_BODY: usize = 256 * 1024;

/// Parse failure taxonomy. One variant per *class* of refusal so a
/// caller can log something actionable without the parser ever
/// carrying attacker bytes in its error path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// Request line malformed (missing SP, empty target, bad version).
    BadRequestLine,
    /// Method not in the supported set.
    BadMethod,
    /// A header line without a `:`, with a folded continuation, or
    /// containing bytes outside the token/field grammar we accept.
    BadHeader,
    /// More than [`MAX_HEADERS`] headers.
    TooManyHeaders,
    /// Request line or a header line exceeds its length cap.
    LineTooLong,
    /// `Content-Length` unparsable, duplicated, or above [`MAX_BODY`].
    BadContentLength,
}

/// The methods this server accepts. `Other` is deliberately absent:
/// an unknown method is [`HttpError::BadMethod`], not a value the
/// caller must remember to reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Read a resource (health, MCP SSE stream open).
    Get,
    /// Submit a body (MCP messages, module upload).
    Post,
    /// Tear down a session (MCP session close).
    Delete,
}

/// One parsed header: name and value as borrowed byte slices.
/// Name comparison is the caller's job via [`Request::header`]
/// (case-insensitive, per RFC 7230 §3.2).
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    /// Field name, exactly as received (no case normalization).
    pub name: &'a [u8],
    /// Field value with surrounding whitespace trimmed.
    pub value: &'a [u8],
}

/// A fully received request, borrowing the caller's buffer.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Request method.
    pub method: Method,
    /// Request target, byte-exact (no percent-decoding).
    pub target: &'a [u8],
    headers: [Option<Header<'a>>; MAX_HEADERS],
    /// Body as delimited by `Content-Length` (empty if absent).
    pub body: &'a [u8],
}

impl<'a> Request<'a> {
    /// Look up a header by name, ASCII-case-insensitively.
    ///
    /// Returns the first match's value. Duplicate `Content-Length`
    /// is rejected at parse time; other duplicates follow the
    /// first-wins rule, documented here so it is a decision rather
    /// than an accident.
    pub fn header(&self, name: &[u8]) -> Option<&'a [u8]> {
        self.headers
            .iter()
            .flatten()
            .find(|h| eq_ignore_ascii_case(h.name, name))
            .map(|h| h.value)
    }

    /// Iterate the parsed headers in arrival order.
    pub fn headers(&self) -> impl Iterator<Item = &Header<'a>> {
        self.headers.iter().flatten()
    }
}

/// Outcome of [`parse`].
#[derive(Debug)]
// Complete carries the full Request by value (~300 bytes of borrowed
// slices); Partial is a unit. Boxing is unavailable (no_std, no
// alloc) and the value lives for one match arm on the caller's
// stack, so the size skew is accepted deliberately.
#[allow(clippy::large_enum_variant)]
pub enum Parse<'a> {
    /// A complete request; `consumed` is the total bytes it occupied
    /// in the buffer (head + body), so pipelined callers know where
    /// the next request begins.
    Complete {
        /// The parsed request view.
        request: Request<'a>,
        /// Bytes consumed from the front of `buf`.
        consumed: usize,
    },
    /// The buffer holds a valid *prefix* of a request; recv more
    /// bytes into the same buffer and call [`parse`] again.
    Partial,
}

/// Parse one HTTP/1.1 request from the front of `buf`.
///
/// # Contract
///
/// - Stateless and restartable: on [`Parse::Partial`] the caller
///   extends `buf` and calls again; no state is kept between calls.
/// - Zero-copy: every slice in the result borrows `buf`.
/// - Errors are final for this buffer: a malformed request cannot
///   become well-formed by receiving more bytes, so on `Err` the
///   caller should respond 400 and close.
/// - Panics: never — all indexing is bounds-derived.
///
/// ```
/// use wari_http::{parse, Method, Parse};
/// let wire = b"POST /mcp HTTP/1.1\r\ncontent-length: 2\r\n\r\nhi";
/// let Ok(Parse::Complete { request, consumed }) = parse(wire) else { panic!() };
/// assert_eq!(request.method, Method::Post);
/// assert_eq!(request.target, b"/mcp");
/// assert_eq!(request.body, b"hi");
/// assert_eq!(consumed, wire.len());
///
/// // A prefix of the same request is Partial, not an error:
/// assert!(matches!(parse(&wire[..10]), Ok(Parse::Partial)));
///
/// // Garbage is an error, not Partial:
/// assert!(parse(b"NONSENSE\r\n\r\n").is_err());
/// ```
pub fn parse(buf: &[u8]) -> Result<Parse<'_>, HttpError> {
    // ── Request line ────────────────────────────────────────────
    let Some(line_end) = find_crlf(buf, 0) else {
        // No CRLF yet. Only Partial while it could still fit.
        return if buf.len() > MAX_REQUEST_LINE {
            Err(HttpError::LineTooLong)
        } else {
            Ok(Parse::Partial)
        };
    };
    if line_end > MAX_REQUEST_LINE {
        return Err(HttpError::LineTooLong);
    }
    let line = &buf[..line_end];
    let (method, rest) = split_once(line, b' ').ok_or(HttpError::BadRequestLine)?;
    let (target, version) = split_once(rest, b' ').ok_or(HttpError::BadRequestLine)?;
    let method = match method {
        b"GET" => Method::Get,
        b"POST" => Method::Post,
        b"DELETE" => Method::Delete,
        _ => return Err(HttpError::BadMethod),
    };
    if target.is_empty() || target.contains(&b' ') {
        return Err(HttpError::BadRequestLine);
    }
    // HTTP/1.0 is accepted (curl -0, health probes); anything else
    // that isn't 1.x is refused rather than guessed at.
    if version != b"HTTP/1.1" && version != b"HTTP/1.0" {
        return Err(HttpError::BadRequestLine);
    }

    // ── Headers ─────────────────────────────────────────────────
    let mut headers: [Option<Header<'_>>; MAX_HEADERS] = [None; MAX_HEADERS];
    let mut count = 0usize;
    let mut cursor = line_end + 2;
    let mut content_length: Option<usize> = None;
    loop {
        let Some(h_end) = find_crlf(buf, cursor) else {
            return if buf.len() - cursor > MAX_HEADER_LINE {
                Err(HttpError::LineTooLong)
            } else {
                Ok(Parse::Partial)
            };
        };
        if h_end == cursor {
            // Empty line: end of headers.
            cursor += 2;
            break;
        }
        if h_end - cursor > MAX_HEADER_LINE {
            return Err(HttpError::LineTooLong);
        }
        let line = &buf[cursor..h_end];
        // Obsolete line folding (leading SP/HTAB) is a request-
        // smuggling vector; refuse it outright (RFC 7230 §3.2.4).
        if line[0] == b' ' || line[0] == b'\t' {
            return Err(HttpError::BadHeader);
        }
        let (name, value) = split_once(line, b':').ok_or(HttpError::BadHeader)?;
        if name.is_empty() || name.iter().any(|&b| b == b' ' || b == b'\t') {
            // "Header : v" is another smuggling classic — a name
            // containing whitespace is refused, not trimmed.
            return Err(HttpError::BadHeader);
        }
        let value = trim_ows(value);
        if eq_ignore_ascii_case(name, b"content-length") {
            if content_length.is_some() {
                // Duplicate Content-Length = smuggling attempt.
                return Err(HttpError::BadContentLength);
            }
            let n = parse_decimal(value).ok_or(HttpError::BadContentLength)?;
            if n > MAX_BODY {
                return Err(HttpError::BadContentLength);
            }
            content_length = Some(n);
        }
        if count == MAX_HEADERS {
            return Err(HttpError::TooManyHeaders);
        }
        headers[count] = Some(Header { name, value });
        count += 1;
        cursor = h_end + 2;
    }

    // ── Body ────────────────────────────────────────────────────
    let body_len = content_length.unwrap_or(0);
    if buf.len() < cursor + body_len {
        return Ok(Parse::Partial);
    }
    let body = &buf[cursor..cursor + body_len];
    Ok(Parse::Complete {
        request: Request {
            method,
            target,
            headers,
            body,
        },
        consumed: cursor + body_len,
    })
}

/// Response status codes this server emits. Closed set on purpose —
/// a status the code base never sends is a status nobody has to
/// reason about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 200 — request served.
    Ok,
    /// 202 — accepted for asynchronous processing (MCP notifications).
    Accepted,
    /// 400 — malformed request; connection should close.
    BadRequest,
    /// 404 — no handler for the target.
    NotFound,
    /// 405 — target exists, method not supported for it.
    MethodNotAllowed,
    /// 413 — body larger than the declared caps.
    PayloadTooLarge,
    /// 500 — handler failed; body carries no internals.
    InternalError,
}

impl Status {
    const fn line(self) -> &'static [u8] {
        match self {
            Status::Ok => b"HTTP/1.1 200 OK\r\n",
            Status::Accepted => b"HTTP/1.1 202 Accepted\r\n",
            Status::BadRequest => b"HTTP/1.1 400 Bad Request\r\n",
            Status::NotFound => b"HTTP/1.1 404 Not Found\r\n",
            Status::MethodNotAllowed => b"HTTP/1.1 405 Method Not Allowed\r\n",
            Status::PayloadTooLarge => b"HTTP/1.1 413 Payload Too Large\r\n",
            Status::InternalError => b"HTTP/1.1 500 Internal Server Error\r\n",
        }
    }
}

/// Build a complete response into `out`; returns the byte length.
///
/// Emits status line, `content-type` (when a body is present),
/// `content-length`, `connection: close`, and the body. The fixed
/// header set is the point: this server never emits a header it did
/// not decide to emit at compile time.
///
/// # Contract
///
/// - Returns `None` if `out` is too small — the caller sized the
///   buffer wrong, and a truncated HTTP response is worse than none.
/// - Panics: never.
///
/// ```
/// use wari_http::{respond, Status};
/// let mut out = [0u8; 256];
/// let n = respond(&mut out, Status::Ok, b"application/json", b"{}").unwrap();
/// let s = core::str::from_utf8(&out[..n]).unwrap();
/// assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
/// assert!(s.contains("content-length: 2\r\n"));
/// assert!(s.ends_with("\r\n\r\n{}"));
/// ```
pub fn respond(out: &mut [u8], status: Status, content_type: &[u8], body: &[u8]) -> Option<usize> {
    let mut w = Writer { out, len: 0 };
    w.put(status.line())?;
    if !body.is_empty() {
        w.put(b"content-type: ")?;
        w.put(content_type)?;
        w.put(b"\r\n")?;
    }
    w.put(b"content-length: ")?;
    w.put_decimal(body.len())?;
    w.put(b"\r\nconnection: close\r\n\r\n")?;
    w.put(body)?;
    Some(w.len)
}

// ── Internal helpers (all pure, all total) ──────────────────────

struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.len.checked_add(bytes.len())?;
        if end > self.out.len() {
            return None;
        }
        self.out[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Some(())
    }

    fn put_decimal(&mut self, mut v: usize) -> Option<()> {
        // usize fits in 20 decimal digits.
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&digits[i..])
    }
}

/// Index of the `\r` of the next CRLF at/after `from`, if any.
fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < buf.len() {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn split_once(buf: &[u8], sep: u8) -> Option<(&[u8], &[u8])> {
    let i = buf.iter().position(|&b| b == sep)?;
    Some((&buf[..i], &buf[i + 1..]))
}

fn trim_ows(mut v: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = v {
        v = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = v {
        v = rest;
    }
    v
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Strict decimal parse: digits only, no sign, no whitespace, no
/// empty string. Overflow is a refusal, not a wrap.
fn parse_decimal(v: &[u8]) -> Option<usize> {
    if v.is_empty() || v.len() > 10 {
        return None;
    }
    let mut n: usize = 0;
    for &b in v {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(wire: &[u8]) -> (Request<'_>, usize) {
        match parse(wire) {
            Ok(Parse::Complete { request, consumed }) => (request, consumed),
            other => panic!("expected Complete, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn get_without_body() {
        let (r, consumed) = complete(b"GET /health HTTP/1.1\r\nhost: wari\r\n\r\n");
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.target, b"/health");
        assert_eq!(r.body, b"");
        assert_eq!(r.header(b"HOST"), Some(&b"wari"[..]));
        assert_eq!(consumed, b"GET /health HTTP/1.1\r\nhost: wari\r\n\r\n".len());
    }

    #[test]
    fn post_with_body_and_case_insensitive_headers() {
        let wire = b"POST /mcp HTTP/1.1\r\nContent-Type: application/json\r\nCONTENT-LENGTH: 7\r\n\r\n{\"a\":1}";
        let (r, consumed) = complete(wire);
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.body, b"{\"a\":1}");
        assert_eq!(r.header(b"content-type"), Some(&b"application/json"[..]));
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn value_whitespace_is_trimmed_name_whitespace_is_refused() {
        let (r, _) = complete(b"GET / HTTP/1.1\r\nx:  padded \r\n\r\n");
        assert_eq!(r.header(b"x"), Some(&b"padded"[..]));
        // "name :" — whitespace inside the name is a smuggling
        // vector, refused rather than trimmed.
        assert!(matches!(parse(b"GET / HTTP/1.1\r\nx : v\r\n\r\n"), Err(HttpError::BadHeader)));
    }

    #[test]
    fn every_prefix_is_partial_never_a_panic_or_false_error() {
        // The restartability contract, exhaustively: each proper
        // prefix of a valid request must parse to Partial.
        let wire = b"POST /m HTTP/1.1\r\ncontent-length: 4\r\n\r\nbody";
        for cut in 0..wire.len() {
            match parse(&wire[..cut]) {
                Ok(Parse::Partial) => {}
                other => panic!("prefix {} gave {:?}", cut, other.map(|_| ())),
            }
        }
        assert!(matches!(parse(wire), Ok(Parse::Complete { .. })));
    }

    #[test]
    fn adversarial_shapes_are_refused() {
        // Unknown method.
        assert!(parse(b"BREW /pot HTTP/1.1\r\n\r\n").is_err());
        // Folded header continuation (request smuggling).
        assert!(parse(b"GET / HTTP/1.1\r\na: b\r\n c\r\n\r\n").is_err());
        // Duplicate Content-Length (request smuggling).
        assert!(
            parse(b"POST / HTTP/1.1\r\ncontent-length: 1\r\ncontent-length: 2\r\n\r\nx").is_err()
        );
        // Non-numeric and negative lengths.
        assert!(parse(b"POST / HTTP/1.1\r\ncontent-length: nope\r\n\r\n").is_err());
        assert!(parse(b"POST / HTTP/1.1\r\ncontent-length: -1\r\n\r\n").is_err());
        // Content-Length over the cap.
        assert!(parse(b"POST / HTTP/1.1\r\ncontent-length: 99999999\r\n\r\n").is_err());
        // Bad version.
        assert!(parse(b"GET / HTTP/9.9\r\n\r\n").is_err());
        // Header bomb: 17 headers.
        let mut bomb = alloc_free_concat(b"GET / HTTP/1.1\r\n");
        for _ in 0..17 {
            bomb = push(bomb, b"a: b\r\n");
        }
        let (buf, len) = bomb;
        assert!(matches!(parse(&buf[..len]), Err(HttpError::TooManyHeaders)));
    }

    #[test]
    fn oversized_lines_error_rather_than_hang_partial() {
        // A request line that can no longer fit must be an error —
        // returning Partial forever would let a peer hold the
        // connection open with an unbounded line.
        let mut long = [b'A'; MAX_REQUEST_LINE + 2];
        long[0] = b'G';
        assert!(matches!(parse(&long), Err(HttpError::LineTooLong)));
    }

    #[test]
    fn respond_shapes_are_exact() {
        let mut out = [0u8; 512];
        let n = respond(&mut out, Status::NotFound, b"", b"").unwrap();
        assert_eq!(
            &out[..n],
            &b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"[..]
        );
        // Too-small buffer refuses rather than truncates.
        let mut tiny = [0u8; 8];
        assert!(respond(&mut tiny, Status::Ok, b"t", b"body").is_none());
    }

    // no_std-friendly test helpers (no Vec).
    fn alloc_free_concat(head: &[u8]) -> ([u8; 512], usize) {
        let mut buf = [0u8; 512];
        buf[..head.len()].copy_from_slice(head);
        (buf, head.len())
    }
    fn push((mut buf, len): ([u8; 512], usize), s: &[u8]) -> ([u8; 512], usize) {
        buf[len..len + s.len()].copy_from_slice(s);
        (buf, len + s.len())
    }
}
