// SPDX-License-Identifier: AGPL-3.0-only
//! Pure JSON-RPC 2.0 framing for Wari's MCP surface.
//!
//! One concern: turn an HTTP body into a typed [`RpcRequest`] view
//! and typed results back into JSON-RPC response bytes — bounded,
//! zero-allocation, no `unsafe`, no dependencies. This is the layer
//! between `wari-http` (the transport) and the Executor's tool
//! dispatch (the policy-mediated surface in
//! `docs/ai-os-assistant-design.md`): everything MCP-specific about
//! *tools* lives above; everything byte-specific about *JSON* lives
//! here.
//!
//! ## Span-based, not tree-based
//!
//! The parser never builds a value tree. [`parse_request`] returns
//! the `method` and `id` decoded, and `params` as the **raw byte
//! span** of that JSON value inside the caller's buffer. Tool
//! handlers then pull the two or three fields they need with
//! [`obj_get_raw`] / [`as_str`] / [`as_i64`]. No allocation, no
//! intermediate representation, nothing retained.
//!
//! ## Bounds and refusals
//!
//! Nesting depth is capped at [`MAX_DEPTH`]; numbers are checked
//! `i64` (overflow refuses); floats and non-integer numbers are
//! refused where an integer is required. Strings are borrowed only
//! when escape-free — a `method`, key, or extracted string field
//! containing `\` escapes is refused rather than decoded, because
//! decoding needs an output buffer this layer refuses to own. MCP
//! method and tool names are plain ASCII identifiers, so this is a
//! constraint on attackers, not on the protocol.
//!
//! ## Why hand-rolled instead of `serde-json-core`
//!
//! Decision for the architect's review, flagged in the PR: the
//! Tier-2 trust base admits no third-party code without its own
//! review (`docs/prior-art.md` dependency policy), the needed subset
//! is small and total, and a span-skipper is fuzz-friendly in a way
//! a deserializer is not. Cost accepted: we own ~200 lines of JSON
//! lexing. If overruled, this crate's API survives — only the
//! internals would delegate.

#![no_std]

/// Maximum JSON nesting depth accepted anywhere. MCP payloads nest
/// 3–4 levels; 16 is margin, and past it a payload is refused whole
/// (stack safety is a correctness property, not a tuning knob).
pub const MAX_DEPTH: usize = 16;

/// JSON-RPC error codes this surface emits (JSON-RPC 2.0 §5.1).
pub mod code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i32 = -32700;
    /// The JSON is not a valid Request object.
    pub const INVALID_REQUEST: i32 = -32600;
    /// Method does not exist.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i32 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// Refusal taxonomy. Carries no attacker bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpError {
    /// Bytes are not the JSON shape required at this position.
    BadJson,
    /// Nesting exceeds [`MAX_DEPTH`].
    TooDeep,
    /// Not a JSON-RPC 2.0 request (missing/incorrect `jsonrpc`,
    /// missing `method`, or a non-object top level).
    BadRequest,
    /// A string needed borrowing but contains escapes.
    EscapedString,
    /// A number field is not a valid `i64`.
    BadNumber,
}

/// A JSON-RPC request id. `Absent` (a notification) is distinct from
/// `Null` (present-but-null, which JSON-RPC discourages but defines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id<'a> {
    /// Integer id.
    Int(i64),
    /// String id (escape-free; escaped ids are refused).
    Str(&'a [u8]),
    /// `"id": null`.
    Null,
    /// No id — a notification; the peer expects no response.
    Absent,
}

/// A parsed JSON-RPC request, borrowing the input buffer.
#[derive(Debug, Clone, Copy)]
pub struct RpcRequest<'a> {
    /// Request id, echoed into the response.
    pub id: Id<'a>,
    /// Method name, escape-free (e.g. `tools/call`).
    pub method: &'a [u8],
    /// Raw span of the `params` value, if present. Hand this to
    /// [`obj_get_raw`] for field extraction.
    pub params: Option<&'a [u8]>,
}

/// Parse one JSON-RPC 2.0 request from `body`.
///
/// # Contract
///
/// - Zero-copy: `method`, string ids, and `params` borrow `body`.
/// - Requires `jsonrpc: "2.0"` and a string `method`; unknown
///   top-level keys are skipped (forward-compatible).
/// - Duplicate keys: first occurrence wins, deliberately —
///   documented so it is a decision, not an accident.
/// - Errors are total refusals; nothing partial is returned.
/// - Panics: never.
///
/// ```
/// use wari_mcp::{parse_request, Id};
/// let body = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call",
///                 "params":{"name":"status","arguments":{}}}"#;
/// let r = parse_request(body).unwrap();
/// assert_eq!(r.id, Id::Int(7));
/// assert_eq!(r.method, b"tools/call");
/// let params = r.params.unwrap();
/// let name = wari_mcp::obj_get_raw(params, b"name").unwrap();
/// assert_eq!(wari_mcp::as_str(name).unwrap(), b"status");
///
/// // Not JSON-RPC: refused whole.
/// assert!(parse_request(br#"{"method":"x"}"#).is_err());
/// ```
pub fn parse_request(body: &[u8]) -> Result<RpcRequest<'_>, McpError> {
    let obj = object_span(body)?;
    // jsonrpc: "2.0" is mandatory.
    let ver = obj_get_raw(obj, b"jsonrpc").ok_or(McpError::BadRequest)?;
    if as_str(ver)? != b"2.0" {
        return Err(McpError::BadRequest);
    }
    let method = as_str(obj_get_raw(obj, b"method").ok_or(McpError::BadRequest)?)?;
    let id = match obj_get_raw(obj, b"id") {
        None => Id::Absent,
        Some(b"null") => Id::Null,
        Some(raw) if raw.first() == Some(&b'"') => Id::Str(as_str(raw)?),
        Some(raw) => Id::Int(parse_i64(raw)?),
    };
    let params = obj_get_raw(obj, b"params");
    Ok(RpcRequest { id, method, params })
}

/// Serialize a success response: `{"jsonrpc":"2.0","id":…,"result":R}`.
///
/// `result_json` is pre-serialized JSON the caller owns (tool
/// handlers build small fixed shapes). Returns the byte length, or
/// `None` if `out` is too small — refusal, never truncation.
///
/// ```
/// use wari_mcp::{respond_result, Id};
/// let mut out = [0u8; 128];
/// let n = respond_result(&mut out, Id::Int(7), b"{\"ok\":true}").unwrap();
/// assert_eq!(&out[..n], br#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#);
/// ```
pub fn respond_result(out: &mut [u8], id: Id<'_>, result_json: &[u8]) -> Option<usize> {
    let mut w = W { out, len: 0 };
    w.put(br#"{"jsonrpc":"2.0","id":"#)?;
    w.put_id(id)?;
    w.put(br#","result":"#)?;
    w.put(result_json)?;
    w.put(b"}")?;
    Some(w.len)
}

/// Serialize an error response with a static message.
///
/// `message` must be escape-free ASCII (it is ours, never the
/// attacker's — error text carrying attacker bytes is how log
/// injection starts).
pub fn respond_error(out: &mut [u8], id: Id<'_>, code: i32, message: &str) -> Option<usize> {
    let mut w = W { out, len: 0 };
    w.put(br#"{"jsonrpc":"2.0","id":"#)?;
    w.put_id(id)?;
    w.put(br#","error":{"code":"#)?;
    w.put_i64(code as i64)?;
    w.put(br#","message":""#)?;
    w.put(message.as_bytes())?;
    w.put(br#""}}"#)?;
    Some(w.len)
}

// ── Field extraction over raw spans ──────────────────────────────

/// Look up `key` in a JSON object span; returns the raw span of its
/// value. First occurrence wins on duplicates (see [`parse_request`]).
///
/// Returns `None` for a missing key **and** for malformed JSON —
/// extraction is refuse-by-default; parse errors were already
/// surfaced by [`parse_request`] for the top level.
pub fn obj_get_raw<'a>(obj: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let s = trim(obj);
    let inner = s.strip_prefix(b"{")?.strip_suffix(b"}")?;
    let mut cur = trim_start(inner);
    while !cur.is_empty() {
        // Key.
        let (k, rest) = take_string(cur).ok()?;
        let rest = trim_start(rest);
        let rest = rest.strip_prefix(b":")?;
        let rest = trim_start(rest);
        // Value span.
        let vlen = value_len(rest, 0).ok()?;
        let (v, rest) = rest.split_at(vlen);
        if k == key {
            return Some(v);
        }
        let rest = trim_start(rest);
        match rest.first() {
            Some(b',') => cur = trim_start(&rest[1..]),
            None => break,
            _ => return None,
        }
    }
    None
}

/// Borrow an escape-free JSON string's contents from its raw span.
pub fn as_str(raw: &[u8]) -> Result<&[u8], McpError> {
    let (s, rest) = take_string(trim(raw))?;
    if !trim(rest).is_empty() {
        return Err(McpError::BadJson);
    }
    Ok(s)
}

/// Parse a raw span as a strict `i64` (no floats, no leading `+`,
/// overflow refuses).
pub fn as_i64(raw: &[u8]) -> Result<i64, McpError> {
    parse_i64(trim(raw))
}

/// Is this raw span the JSON literal `true`?
pub fn is_true(raw: &[u8]) -> bool {
    trim(raw) == b"true"
}

// ── Internals: a total, depth-capped JSON skipper ────────────────

/// Byte length of the single JSON value at the start of `s`.
/// The recursion is bounded by [`MAX_DEPTH`], making stack use a
/// compile-time-reviewable constant rather than attacker input.
fn value_len(s: &[u8], depth: usize) -> Result<usize, McpError> {
    if depth > MAX_DEPTH {
        return Err(McpError::TooDeep);
    }
    match s.first().ok_or(McpError::BadJson)? {
        b'"' => {
            let (content_end, _) = string_end(s)?;
            Ok(content_end)
        }
        b'{' | b'[' => {
            let (open, close) = if s[0] == b'{' { (b'{', b'}') } else { (b'[', b']') };
            let mut i = 1;
            loop {
                let rest = &s[i..];
                let c = *trim_start(rest).first().ok_or(McpError::BadJson)?;
                i += rest.len() - trim_start(rest).len();
                if c == close {
                    return Ok(i + 1);
                }
                // Skip one element (or key:value pair).
                if open == b'{' {
                    let klen = value_len(&s[i..], depth + 1)?;
                    i += klen;
                    let rest = trim_start(&s[i..]);
                    i = s.len() - rest.len();
                    if s.get(i) != Some(&b':') {
                        return Err(McpError::BadJson);
                    }
                    i += 1;
                    let rest = trim_start(&s[i..]);
                    i = s.len() - rest.len();
                }
                let vlen = value_len(&s[i..], depth + 1)?;
                i += vlen;
                let rest = trim_start(&s[i..]);
                i = s.len() - rest.len();
                match s.get(i) {
                    Some(b',') => {
                        i += 1;
                        let rest = trim_start(&s[i..]);
                        i = s.len() - rest.len();
                    }
                    Some(&c2) if c2 == close => {
                        return Ok(i + 1);
                    }
                    _ => return Err(McpError::BadJson),
                }
            }
        }
        _ => {
            // Number / true / false / null: runs to a delimiter.
            let mut i = 0;
            while i < s.len() && !matches!(s[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n')
            {
                i += 1;
            }
            if i == 0 {
                return Err(McpError::BadJson);
            }
            Ok(i)
        }
    }
}

/// For a span starting at `"`: total length including quotes, and
/// whether escapes were seen.
fn string_end(s: &[u8]) -> Result<(usize, bool), McpError> {
    let mut i = 1;
    let mut escaped = false;
    while i < s.len() {
        match s[i] {
            b'\\' => {
                escaped = true;
                i += 2; // skip the escaped char (validity checked on borrow)
            }
            b'"' => return Ok((i + 1, escaped)),
            _ => i += 1,
        }
    }
    Err(McpError::BadJson)
}

/// Split an escape-free string's contents off a span starting at `"`.
fn take_string(s: &[u8]) -> Result<(&[u8], &[u8]), McpError> {
    if s.first() != Some(&b'"') {
        return Err(McpError::BadJson);
    }
    let (end, escaped) = string_end(s)?;
    if escaped {
        return Err(McpError::EscapedString);
    }
    Ok((&s[1..end - 1], &s[end..]))
}

fn parse_i64(s: &[u8]) -> Result<i64, McpError> {
    if s.is_empty() {
        return Err(McpError::BadNumber);
    }
    let (neg, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, s),
    };
    if digits.is_empty() || digits.len() > 19 {
        return Err(McpError::BadNumber);
    }
    let mut n: i64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return Err(McpError::BadNumber);
        }
        n = n
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as i64))
            .ok_or(McpError::BadNumber)?;
    }
    Ok(if neg { -n } else { n })
}

/// The top-level object span (trimmed), refusing non-objects.
fn object_span(s: &[u8]) -> Result<&[u8], McpError> {
    let t = trim(s);
    if t.first() != Some(&b'{') {
        return Err(McpError::BadRequest);
    }
    let len = value_len(t, 0)?;
    if !trim(&t[len..]).is_empty() {
        return Err(McpError::BadJson);
    }
    Ok(&t[..len])
}

fn trim_start(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t' | b'\r' | b'\n', rest @ ..] = s {
        s = rest;
    }
    s
}

fn trim(s: &[u8]) -> &[u8] {
    let mut s = trim_start(s);
    while let [rest @ .., b' ' | b'\t' | b'\r' | b'\n'] = s {
        s = rest;
    }
    s
}

struct W<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl W<'_> {
    fn put(&mut self, b: &[u8]) -> Option<()> {
        let end = self.len.checked_add(b.len())?;
        if end > self.out.len() {
            return None;
        }
        self.out[self.len..end].copy_from_slice(b);
        self.len = end;
        Some(())
    }
    fn put_i64(&mut self, v: i64) -> Option<()> {
        let mut buf = [0u8; 20];
        let mut i = buf.len();
        let neg = v < 0;
        let mut m = v.unsigned_abs();
        loop {
            i -= 1;
            buf[i] = b'0' + (m % 10) as u8;
            m /= 10;
            if m == 0 {
                break;
            }
        }
        if neg {
            i -= 1;
            buf[i] = b'-';
        }
        self.put(&buf[i..])
    }
    fn put_id(&mut self, id: Id<'_>) -> Option<()> {
        match id {
            Id::Int(v) => self.put_i64(v),
            Id::Str(s) => {
                self.put(b"\"")?;
                self.put(s)?;
                self.put(b"\"")
            }
            Id::Null | Id::Absent => self.put(b"null"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_call_round_trip() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"spawn_module","arguments":{"slot":2,"confirm":true}}}"#;
        let r = parse_request(body).unwrap();
        assert_eq!(r.method, b"tools/call");
        assert_eq!(r.id, Id::Int(1));
        let p = r.params.unwrap();
        assert_eq!(as_str(obj_get_raw(p, b"name").unwrap()).unwrap(), b"spawn_module");
        let args = obj_get_raw(p, b"arguments").unwrap();
        assert_eq!(as_i64(obj_get_raw(args, b"slot").unwrap()).unwrap(), 2);
        assert!(is_true(obj_get_raw(args, b"confirm").unwrap()));

        let mut out = [0u8; 128];
        let n = respond_result(&mut out, r.id, br#"{"pid":5}"#).unwrap();
        assert_eq!(&out[..n], br#"{"jsonrpc":"2.0","id":1,"result":{"pid":5}}"#);
    }

    #[test]
    fn id_forms() {
        let f = |j: &'static [u8]| parse_request(j).unwrap().id;
        assert_eq!(f(br#"{"jsonrpc":"2.0","id":42,"method":"m"}"#), Id::Int(42));
        assert_eq!(f(br#"{"jsonrpc":"2.0","id":-7,"method":"m"}"#), Id::Int(-7));
        assert_eq!(f(br#"{"jsonrpc":"2.0","id":"abc","method":"m"}"#), Id::Str(b"abc"));
        assert_eq!(f(br#"{"jsonrpc":"2.0","id":null,"method":"m"}"#), Id::Null);
        assert_eq!(f(br#"{"jsonrpc":"2.0","method":"m"}"#), Id::Absent);
    }

    #[test]
    fn notification_error_response_echoes_null_id() {
        let mut out = [0u8; 128];
        let n = respond_error(&mut out, Id::Absent, code::METHOD_NOT_FOUND, "no such method").unwrap();
        assert_eq!(
            &out[..n],
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"no such method"}}"#
        );
    }

    #[test]
    fn adversarial_shapes_are_refused() {
        // Wrong/missing version.
        assert!(parse_request(br#"{"method":"m"}"#).is_err());
        assert!(parse_request(br#"{"jsonrpc":"1.0","method":"m"}"#).is_err());
        // Top level not an object.
        assert!(parse_request(br#"[1,2]"#).is_err());
        // Trailing garbage after the object.
        assert!(parse_request(br#"{"jsonrpc":"2.0","method":"m"} extra"#).is_err());
        // Truncated at every cut of a valid body: never a panic.
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"m","params":{"a":[1,2,{"b":"c"}]}}"#;
        for cut in 0..body.len() {
            let _ = parse_request(&body[..cut]); // must not panic
        }
        // Escaped method name: refused, not decoded. (Raw string —
        // the JSON really contains a backslash-t escape sequence.)
        assert_eq!(
            parse_request(br#"{"jsonrpc":"2.0","method":"a\tb"}"#).unwrap_err(),
            McpError::EscapedString
        );
        // Overflowing id.
        assert!(parse_request(br#"{"jsonrpc":"2.0","id":99999999999999999999,"method":"m"}"#).is_err());
        // Float id: refused (ints only).
        assert!(parse_request(br#"{"jsonrpc":"2.0","id":1.5,"method":"m"}"#).is_err());
    }

    #[test]
    fn depth_bomb_is_refused_not_a_stack_overflow() {
        // 64 nested arrays in params — parse_request skips params
        // lazily, so bomb obj_get_raw instead, which walks it.
        let mut body = [0u8; 256];
        let prefix = br#"{"a":"#;
        let mut len = 0;
        body[..prefix.len()].copy_from_slice(prefix);
        len += prefix.len();
        for _ in 0..64 {
            body[len] = b'[';
            len += 1;
        }
        for _ in 0..64 {
            body[len] = b']';
            len += 1;
        }
        body[len] = b'}';
        len += 1;
        assert!(obj_get_raw(&body[..len], b"a").is_none());
    }

    #[test]
    fn duplicate_keys_first_wins() {
        let obj = br#"{"k":1,"k":2}"#;
        assert_eq!(as_i64(obj_get_raw(obj, b"k").unwrap()).unwrap(), 1);
    }

    #[test]
    fn nested_objects_and_strings_with_delimiters() {
        // Braces and commas inside strings must not confuse the
        // skipper.
        let obj = br#"{"a":"}{,","b":{"c":"]["},"d":3}"#;
        assert_eq!(as_str(obj_get_raw(obj, b"a").unwrap()).unwrap(), b"}{,");
        assert_eq!(as_i64(obj_get_raw(obj, b"d").unwrap()).unwrap(), 3);
    }
}
