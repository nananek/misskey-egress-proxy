// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pure functions that decide where a media request may be sent to.
//!
//! Two destinations exist, and neither is ever the Misskey UDS:
//!
//! - [`internal_location`]: a request that carries an internal `Referer` is
//!   redirected to `INTERNAL_BASE_URL` plus its own path and query.
//! - [`original_location`]: in `redirect` mode a `/proxy/*?url=` request is
//!   redirected to the URL in its `url` parameter, provided that URL falls
//!   under an allowed prefix (`MEDIA_ALLOWED_PREFIXES`).
//!
//! This process never fetches the `url` it is given; it only points the
//! *client* at it. What can go wrong is therefore not SSRF but an open
//! redirect, and above all a *parser differential*: this code and whatever
//! follows the `Location` (a browser, a WAF, a log pipeline) reading the same
//! string as two different hosts. The rules below refuse every shape where
//! the `url` crate (WHATWG) and an RFC 3986 reader are known to disagree, and
//! the `Location` that is finally emitted is the re-serialised, normalised
//! form, never the raw input. The reasoning for each rule is in
//! `docs/routes.md`; `tests/dependency_behavior.rs` pins the crate behaviour
//! it depends on.

use url::{Host, Url, form_urlencoded};

/// Longest `path?query` accepted: far more than a real media URL needs, and
/// small enough that no hop in front of us has to carry a huge `Location`.
const MAX_TARGET_LEN: usize = 8192;

/// Longest decoded value of the `url` parameter.
const MAX_URL_VALUE_LEN: usize = 2048;

/// How many times a path segment may be percent-decoded before it is judged
/// to be hiding something behind layers of encoding.
const MAX_DECODE_ROUNDS: usize = 8;

/// One entry of `MEDIA_ALLOWED_PREFIXES`, already normalised the way a
/// candidate URL is (lowercase host, punycode, default port made explicit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedPrefix {
    pub host: String,
    pub port: u16,
    /// Starts with `/` and ends with `/`.
    pub path: String,
}

/// Which rule refused a target. The variants mirror the rule IDs in
/// `docs/routes.md` one to one, so a test can say not just "refused" but
/// "refused by the rule it was written to hit".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// F1: the whole target is longer than 8192 bytes.
    F1Len,
    /// F2: the target holds a byte outside printable ASCII, or a `\`.
    F2Charset,
    /// F3: the path is not under `/files/` or `/proxy/`.
    F3Prefix,
    /// F4: the path fails [`path_is_safe`].
    F4Path,
    /// F5: `base + target` does not parse back to the same origin and path.
    F5ParseBack,
    /// U0: there is not exactly one `url` parameter.
    U0Param,
    /// U1: the `url` value is empty or longer than 2048 bytes.
    U1Len,
    /// U2: the `url` value holds a byte outside printable ASCII.
    U2Bytes,
    /// U3: the `url` value does not start with `https://`.
    U3Scheme,
    /// U4: the `url` value contains `\` or `#`.
    U4Chars,
    /// U5: the authority is not a plain `host[:port]`.
    U5Authority,
    /// U6: the raw path is missing or fails [`path_is_safe`].
    U6Path,
    /// U7: the value does not parse to an `https` URL with a domain host.
    U7Parse,
    /// U8: not under an allowed prefix, or not stable under re-serialisation.
    U8Allow,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Whether a raw (still percent-encoded) path can be trusted to name one
/// object, however many layers of decoding sit between here and the file.
///
/// Every segment must be non-empty; must not decode, at any of up to 8
/// layers, to `.` / `..` or to something holding `/`, `\`, a control byte or
/// DEL; and must not carry a malformed `%`. UTF-8 percent-escapes
/// (`%E3%81%82`) are fine. Past the first layer a stray `%` just ends the
/// decoding, so a file literally called `%zz` (`%25zz`) is not refused.
///
/// The path is judged as it was written, not as the `url` crate would
/// normalise it, because the normalisation is exactly what would hide a
/// `..` from this check.
pub fn path_is_safe(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    rest.split('/').all(segment_is_safe)
}

fn segment_is_safe(segment: &str) -> bool {
    if segment.is_empty() || !escapes_are_wellformed(segment) {
        return false;
    }

    let mut current = segment.as_bytes().to_vec();
    for round in 0..=MAX_DECODE_ROUNDS {
        if current == b"."
            || current == b".."
            || current
                .iter()
                .any(|&b| b == b'/' || b == b'\\' || b < 0x20 || b == 0x7f)
        {
            return false;
        }

        let next = decode_valid_escapes(&current);
        if next == current {
            return true;
        }
        if round == MAX_DECODE_ROUNDS {
            return false;
        }
        current = next;
    }
    false
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|d| d as u8)
}

/// The two hex digits after the `%` at `bytes[at]`, if there are two.
fn escape_at(bytes: &[u8], at: usize) -> Option<u8> {
    let high = hex_value(*bytes.get(at + 1)?)?;
    let low = hex_value(*bytes.get(at + 2)?)?;
    Some(high << 4 | low)
}

fn escapes_are_wellformed(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if escape_at(bytes, at).is_none() {
                return false;
            }
            at += 3;
        } else {
            at += 1;
        }
    }
    true
}

/// Decodes every well-formed `%XX`, leaving a malformed `%` as it is.
fn decode_valid_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match (bytes[at], escape_at(bytes, at)) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    out
}

fn is_printable_ascii(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| (0x21..=0x7e).contains(b))
}

// ---------------------------------------------------------------------------
// Internal redirect
// ---------------------------------------------------------------------------

/// Where an internal-`Referer` request is sent: `base` followed by the
/// request's own `path?query`.
///
/// `base` is `INTERNAL_BASE_URL` (no trailing `/`). Only the path and query
/// are used; the request's authority and `Host` never reach the result. The
/// path must be under `/files/` or `/proxy/` and pass [`path_is_safe`]; the
/// query is opaque apart from length and charset, since nothing on this side
/// decodes it.
///
/// The final check parses the result back: `http` lets a few characters
/// through in a path (`"`, `{`, `}`, ...) that the `url` crate would rewrite,
/// and a redirect whose target a WHATWG reader resolves differently from the
/// bytes we wrote is not one to send.
pub fn internal_location(base: &str, path_and_query: &str) -> Result<String, Reject> {
    if path_and_query.len() > MAX_TARGET_LEN {
        return Err(Reject::F1Len);
    }
    if !is_printable_ascii(path_and_query.as_bytes()) || path_and_query.contains('\\') {
        return Err(Reject::F2Charset);
    }

    let path = path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path);
    if !(path.starts_with("/files/") || path.starts_with("/proxy/")) {
        return Err(Reject::F3Prefix);
    }
    if !path_is_safe(path) {
        return Err(Reject::F4Path);
    }

    let base_url = Url::parse(base).map_err(|_| Reject::F5ParseBack)?;
    let location = format!("{base}{path_and_query}");
    let parsed = Url::parse(&location).map_err(|_| Reject::F5ParseBack)?;

    let expected_path = format!("{}{path}", base_url.path().trim_end_matches('/'));
    let same_origin = parsed.scheme() == base_url.scheme()
        && parsed.host_str() == base_url.host_str()
        && parsed.port_or_known_default() == base_url.port_or_known_default();
    let plain =
        parsed.username().is_empty() && parsed.password().is_none() && parsed.fragment().is_none();
    if !(same_origin && plain && parsed.path() == expected_path) {
        return Err(Reject::F5ParseBack);
    }

    Ok(location)
}

// ---------------------------------------------------------------------------
// Allowed prefixes
// ---------------------------------------------------------------------------

/// Parses `MEDIA_ALLOWED_PREFIXES`: a comma-separated list of
/// `https://host[:port][/path-prefix]`.
///
/// Entries go through the same `url` parser as the candidates they are
/// compared against, so case, IDN and the default port are treated the same
/// on both sides. Only exact host names are accepted (no IP addresses, no
/// wildcards, no trailing dot), and the path is matched on segment
/// boundaries, so `https://s3.example.com/bucket` allows `/bucket/…` and not
/// `/bucketevil/…`. Any invalid entry is an error naming it. Blank entries
/// and duplicates are dropped; `None` or nothing at all is an empty list,
/// which means every `/proxy/*?url=` is refused.
pub fn parse_allowed_prefixes(raw: Option<&str>) -> Result<Vec<AllowedPrefix>, String> {
    let mut prefixes: Vec<AllowedPrefix> = Vec::new();
    for entry in raw.unwrap_or("").split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let prefix = parse_prefix_entry(entry)
            .map_err(|why| format!("MEDIA_ALLOWED_PREFIXES entry {entry:?}: {why}"))?;
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    Ok(prefixes)
}

fn parse_prefix_entry(entry: &str) -> Result<AllowedPrefix, String> {
    if !entry
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err(if entry.contains("://") {
            "only https:// is allowed: a redirect to plain http would be a downgrade".to_string()
        } else {
            "it must start with `https://`".to_string()
        });
    }
    if entry.bytes().any(|b| b <= 0x20 || b == 0x7f || b == b'\\') {
        return Err("it contains whitespace, a control character or a backslash".to_string());
    }

    // The authority and the path are read from the raw text: the `url` crate
    // normalises both, and normalisation is what would hide a mistake here.
    let rest = &entry[8..];
    let (raw_authority, raw_path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    if raw_authority.contains('@') {
        return Err("userinfo is not allowed".to_string());
    }
    if raw_path.contains(['?', '#']) {
        return Err("a query or fragment is not allowed".to_string());
    }
    if raw_authority.is_empty() || raw_authority.contains(['%', '[', ']']) {
        return Err("the authority must be a plain host name with an optional port".to_string());
    }
    if let Some((_, port)) = raw_authority.rsplit_once(':')
        && !((1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("the port must be 1 to 5 digits".to_string());
    }

    let parsed = Url::parse(entry).map_err(|e| e.to_string())?;
    let Some(Host::Domain(domain)) = parsed.host() else {
        return Err("the host must be a domain name, not an IP address".to_string());
    };
    if domain.is_empty()
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
        || !domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(
            "the host must be an exact domain name (no wildcard, no trailing dot)".to_string(),
        );
    }
    let port = parsed
        .port_or_known_default()
        .ok_or("the entry has no usable port")?;

    // A path the parser had to rewrite (dot-segments, characters that need
    // encoding) is not the path the operator wrote; ask for the exact form.
    let written_path = if raw_path.is_empty() { "/" } else { raw_path };
    if parsed.path() != written_path {
        return Err(
            "the path is not in its normal form (no dot-segments, and percent-encode anything \
             that is not plain ASCII)"
                .to_string(),
        );
    }
    let body = raw_path.strip_suffix('/').unwrap_or(raw_path);
    let path = if body.is_empty() {
        "/".to_string()
    } else if path_is_safe(body) {
        format!("{body}/")
    } else {
        return Err(
            "the path has an empty segment, a dot-segment, an encoded separator or a malformed \
             escape"
                .to_string(),
        );
    };

    Ok(AllowedPrefix {
        host: domain.to_string(),
        port,
        path,
    })
}

// ---------------------------------------------------------------------------
// Redirect to the original URL
// ---------------------------------------------------------------------------

/// Where a `/proxy/*?url=` request is sent in `redirect` mode: the `url`
/// parameter, normalised, if and only if it falls under an allowed prefix.
///
/// `path_and_query` is what the request line carried. Its path is not read
/// (Misskey's `/proxy/<host>/<path>` form is not supported); the query must
/// hold exactly one `url` parameter, and every other parameter is dropped.
/// The rules U0-U8 are applied in order and the first that fails names the
/// [`Reject`]:
///
/// - **U0** exactly one `url` key, after decoding the keys. Misskey answers
///   400 to a repeated one, so this side refuses rather than pick a copy.
/// - **U1/U2** 1 to 2048 bytes, all printable ASCII. The `url` crate silently
///   strips TAB / LF / CR from anywhere, which would turn one host into
///   another after the check.
/// - **U3** starts with `https://`. The `url` crate also reads `https:host`,
///   `https:/host` and `https:\\host` as `https://host`; other parsers do not.
/// - **U4** no `\` (WHATWG reads it as `/`, RFC 3986 does not) and no `#`.
/// - **U5** the authority is `[A-Za-z0-9.-]+` with an optional `:1-5 digits`:
///   no userinfo, no percent-encoding, no IPv6, no empty port.
/// - **U6** a raw path that is present and passes [`path_is_safe`].
/// - **U7** parses to `https`, a domain host (numeric hosts such as
///   `2130706433` or `0x7f.1` become IPv4 and are refused), no userinfo, no
///   fragment.
/// - **U8** host and port equal an allowed entry's, the path is under its
///   prefix with a non-empty object name, the parsed path is byte-identical
///   to the raw one, and the result is ASCII and stable if parsed again.
///
/// The returned `Location` is the parsed URL's own serialisation, never the
/// text that came in: the differences are the host's case, a default port,
/// and the re-encoding of the query.
pub fn original_location(
    allowed: &[AllowedPrefix],
    path_and_query: &str,
) -> Result<String, Reject> {
    if path_and_query.len() > MAX_TARGET_LEN {
        return Err(Reject::F1Len);
    }

    let (_, query) = path_and_query.split_once('?').ok_or(Reject::U0Param)?;
    let mut candidates = form_urlencoded::parse(query.as_bytes()).filter(|(key, _)| key == "url");
    let value = candidates.next().ok_or(Reject::U0Param)?.1;
    if candidates.next().is_some() {
        return Err(Reject::U0Param);
    }
    let value: &str = &value;

    if value.is_empty() || value.len() > MAX_URL_VALUE_LEN {
        return Err(Reject::U1Len);
    }
    if !is_printable_ascii(value.as_bytes()) {
        return Err(Reject::U2Bytes);
    }
    if !value
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err(Reject::U3Scheme);
    }
    if value.contains(['\\', '#']) {
        return Err(Reject::U4Chars);
    }

    let rest = &value[8..];
    let (authority, after_authority) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    if !authority_is_plain(authority) {
        return Err(Reject::U5Authority);
    }

    let raw_path = after_authority
        .split_once('?')
        .map_or(after_authority, |(path, _)| path);
    if !raw_path.starts_with('/') || !path_is_safe(raw_path) {
        return Err(Reject::U6Path);
    }

    let parsed = Url::parse(value).map_err(|_| Reject::U7Parse)?;
    let Some(Host::Domain(host)) = parsed.host() else {
        return Err(Reject::U7Parse);
    };
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Reject::U7Parse);
    }
    let port = parsed.port_or_known_default().ok_or(Reject::U7Parse)?;

    if parsed.path() != raw_path {
        return Err(Reject::U8Allow);
    }
    // The object name must be non-empty. `path_is_safe` already refuses a
    // trailing `/`, so today the length test is redundant with U6; it stays so
    // that the prefix itself can never be allowed should U6 ever be relaxed.
    let allowed_here = allowed.iter().any(|entry| {
        entry.host == host
            && entry.port == port
            && raw_path.len() > entry.path.len()
            && raw_path.starts_with(&entry.path)
    });
    if !allowed_here {
        return Err(Reject::U8Allow);
    }

    let location = parsed.as_str().to_string();
    let stable = Url::parse(&location).is_ok_and(|again| again.as_str() == location);
    if !location.is_ascii() || !stable {
        return Err(Reject::U8Allow);
    }
    Ok(location)
}

/// `[A-Za-z0-9.-]+` with an optional `:` and 1 to 5 digits.
fn authority_is_plain(authority: &str) -> bool {
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && port.is_none_or(|port| {
            (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("../tests/common/media_corpus.rs");

    // -- path_is_safe (TP-PATH) --------------------------------------------

    /// `%41` with its `%` escaped `level` times over (`%2541`, `%252541`, ...):
    /// it takes `level + 1` decodings, each of which changes the segment, to
    /// reach `A`.
    fn layered(level: usize) -> String {
        format!("/%{}41", "25".repeat(level))
    }

    #[test]
    fn path_is_safe_refuses_empty_segments_and_dot_segments() {
        for path in [
            "",
            "a",
            "/",
            "/a/",
            "//",
            "/a//b",
            "/./a",
            "/a/./b",
            "/../a",
            "/a/..",
            "/a/../b",
            "/%2e%2e",
            "/%2E%2E",
            "/.%2e",
            "/%2e.",
            "/%2e",
            "/a/%2e%2e/b",
        ] {
            assert!(!path_is_safe(path), "{path:?} should be refused");
        }
    }

    #[test]
    fn path_is_safe_refuses_encoded_separators_and_control_bytes() {
        for path in [
            "/a%2fb",
            "/a%2Fb",
            "/a%5cb",
            "/a%5Cb",
            "/a%00b",
            "/a%0d%0ab",
            "/a%0Ab",
            "/a%7fb",
            "/a%09b",
        ] {
            assert!(!path_is_safe(path), "{path:?} should be refused");
        }
    }

    #[test]
    fn path_is_safe_sees_through_repeated_encoding() {
        for path in [
            "/%252e%252e",
            "/a%252fb",
            "/a%255cb",
            "/%25252e%25252e",
            "/%2525252e%2525252e",
        ] {
            assert!(!path_is_safe(path), "{path:?} should be refused");
        }
    }

    #[test]
    fn path_is_safe_gives_up_after_eight_layers_of_encoding() {
        // Eight decodings that all change the segment are tolerated, a ninth
        // is not.
        assert!(path_is_safe(&layered(7)), "{}", layered(7));
        assert!(!path_is_safe(&layered(8)), "{}", layered(8));
    }

    #[test]
    fn path_is_safe_refuses_malformed_escapes() {
        for path in ["/a%zz", "/a%", "/a%2", "/%", "/a%2/b", "/a%g0"] {
            assert!(!path_is_safe(path), "{path:?} should be refused");
        }
    }

    #[test]
    fn path_is_safe_lets_ordinary_names_through() {
        for path in [
            "/a",
            "/a/b/c.png",
            "/%E3%81%82.png",
            "/a%20b",
            "/100%25.png",
            "/%2541",
            "/%25zz",
            "/.hidden",
            "/a..b",
            "/...",
            "/@user",
            "/a%7Cb",
        ] {
            assert!(path_is_safe(path), "{path:?} should be accepted");
        }
    }

    // -- internal_location (TF) --------------------------------------------

    const BASE: &str = "https://internal.example.ts.net";

    fn internal(target: &str) -> Result<String, Reject> {
        internal_location(BASE, target)
    }

    #[test]
    fn internal_location_appends_a_valid_target_unchanged() {
        for target in [
            "/files/abc123",
            "/files/app-default.jpg",
            "/files/abc/x.webp",
            "/files/a%20b%E3%81%82.png",
            "/proxy/image.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png&static=1",
            "/proxy/s3.example.com/bucket/a.png",
            // The query is opaque: control bytes stay encoded, and nothing here decodes them.
            "/files/x?a=%00",
            "/files/x?a=%0d%0a",
            "/files/x?a=%2e%2e%2f",
        ] {
            assert_eq!(
                internal(target),
                Ok(format!("{BASE}{target}")),
                "{target:?}"
            );
        }
    }

    #[test]
    fn internal_location_refuses_what_a_redirect_could_resolve_differently() {
        for (target, expected) in [
            ("/files/a\\b", Reject::F2Charset),
            ("/files/\u{3042}", Reject::F2Charset),
            ("/files/a b", Reject::F2Charset),
            ("/files/a\tb", Reject::F2Charset),
            ("/files/x?a=b\\c", Reject::F2Charset),
            ("/files/a\"b", Reject::F5ParseBack),
            ("/files/{x}", Reject::F5ParseBack),
            ("/files/a{b", Reject::F5ParseBack),
            ("/files//x", Reject::F4Path),
            ("/files/x/", Reject::F4Path),
            ("/files/", Reject::F4Path),
            ("/files/../x", Reject::F4Path),
            ("/files/./x", Reject::F4Path),
            ("/files/%2e%2e/x", Reject::F4Path),
            ("/files/a%2fb", Reject::F4Path),
            ("/files/a%5cb", Reject::F4Path),
            ("/files/a%zz", Reject::F4Path),
            ("/proxy/%252e%252e/x", Reject::F4Path),
            ("/files", Reject::F3Prefix),
            ("/proxy", Reject::F3Prefix),
            ("/api/meta", Reject::F3Prefix),
            ("//evil.example/x", Reject::F3Prefix),
            ("/filesx/a", Reject::F3Prefix),
            ("", Reject::F3Prefix),
            ("files/a", Reject::F3Prefix),
        ] {
            assert_eq!(internal(target), Err(expected), "{target:?}");
        }
    }

    #[test]
    fn internal_location_bounds_the_length_at_8192() {
        let exactly = format!("/files/{}", "a".repeat(8192 - "/files/".len()));
        assert_eq!(exactly.len(), 8192);
        assert_eq!(internal(&exactly), Ok(format!("{BASE}{exactly}")));

        let one_more = format!("{exactly}a");
        assert_eq!(internal(&one_more), Err(Reject::F1Len));
    }

    #[test]
    fn internal_location_keeps_a_path_prefix_of_the_base() {
        assert_eq!(
            internal_location("https://h.example/prefix", "/files/x"),
            Ok("https://h.example/prefix/files/x".to_string())
        );
    }

    #[test]
    fn internal_location_refuses_a_base_that_does_not_parse() {
        assert_eq!(
            internal_location("not a url", "/files/x"),
            Err(Reject::F5ParseBack)
        );
        assert_eq!(
            internal_location("https://user@h.example", "/files/x"),
            Err(Reject::F5ParseBack)
        );
    }

    // -- parse_allowed_prefixes (CP1, CP2) ---------------------------------

    fn prefix(host: &str, port: u16, path: &str) -> AllowedPrefix {
        AllowedPrefix {
            host: host.to_string(),
            port,
            path: path.to_string(),
        }
    }

    #[test]
    fn allowed_prefixes_parse_a_list() {
        let parsed = parse_allowed_prefixes(Some(
            " https://s3.example.com/bucket , https://r2.example.net ,\
             https://s3.example.com:9000/other/,, ",
        ))
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                prefix("s3.example.com", 443, "/bucket/"),
                prefix("r2.example.net", 443, "/"),
                prefix("s3.example.com", 9000, "/other/"),
            ]
        );
    }

    #[test]
    fn allowed_prefixes_normalise_like_a_candidate_url() {
        assert_eq!(
            parse_allowed_prefixes(Some("HTTPS://S3.EXAMPLE.COM:443/a/b")).unwrap(),
            vec![prefix("s3.example.com", 443, "/a/b/")]
        );
    }

    #[test]
    fn allowed_prefixes_accept_an_idn_written_either_way() {
        let unicode = parse_allowed_prefixes(Some("https://メディア.example/")).unwrap();
        let punycode = Url::parse("https://メディア.example/").unwrap();
        assert_eq!(unicode.len(), 1);
        assert_eq!(Some(unicode[0].host.as_str()), punycode.host_str());
        assert!(unicode[0].host.starts_with("xn--"), "{:?}", unicode[0]);
        assert_eq!(
            parse_allowed_prefixes(Some(&format!("https://{}/", unicode[0].host))).unwrap(),
            unicode
        );
    }

    #[test]
    fn allowed_prefixes_drop_duplicates_and_blanks() {
        assert_eq!(
            parse_allowed_prefixes(Some(
                "https://r2.example.net/,https://R2.example.net:443,https://r2.example.net/"
            ))
            .unwrap(),
            vec![prefix("r2.example.net", 443, "/")]
        );
        assert_eq!(parse_allowed_prefixes(None).unwrap(), vec![]);
        assert_eq!(parse_allowed_prefixes(Some("")).unwrap(), vec![]);
        assert_eq!(parse_allowed_prefixes(Some(" , ,")).unwrap(), vec![]);
    }

    #[test]
    fn allowed_prefixes_refuse_an_invalid_entry_and_name_it() {
        for entry in [
            "http://s3.example.com/",
            "s3.example.com",
            "s3.example.com/bucket/",
            "ftp://s3.example.com/",
            "https:s3.example.com/",
            "https:///s3.example.com/",
            "https://127.0.0.1/",
            "https://2130706433/",
            "https://[::1]/",
            "https://*.example.com/",
            "https://s3.example.com./",
            "https://.s3.example.com/",
            "https://s3..example.com/",
            "https://u:p@s3.example.com/",
            "https://@s3.example.com/",
            "https://s3.example.com/?q",
            "https://s3.example.com/#f",
            "https://s3.example.com?q",
            "https://s3.example.com:/",
            "https://s3.example.com:abc/",
            "https://s3.example.com:65536/",
            "https://s3.example.com/a/../b/",
            "https://s3.example.com/a/./b/",
            "https://s3.example.com/a%2fb/",
            "https://s3.example.com/a//b/",
            "https://s3.example.com/a\\b/",
            "https://s3.example.com/a b/",
            // The `url` crate reads all of these as a plain, valid entry (`\`
            // ends the authority, TAB / LF inside the host are dropped), so
            // only the check on the raw text refuses them.
            "https://s3.example.com\\",
            "https://s3.exa\tmple.com/",
            "https://s3.exa\nmple.com/",
            "https://%73%33.example.com/",
            "https://s3.example.com/\u{3042}/",
        ] {
            let err = parse_allowed_prefixes(Some(entry)).unwrap_err();
            assert!(err.contains("MEDIA_ALLOWED_PREFIXES"), "{entry:?}: {err}");
            assert!(err.contains(&format!("{entry:?}")), "{entry:?}: {err}");
        }
    }

    #[test]
    fn one_bad_entry_fails_the_whole_list() {
        let err = parse_allowed_prefixes(Some("https://r2.example.net/,http://s3.example.com/"))
            .unwrap_err();
        assert!(err.contains("http://s3.example.com/"), "{err}");
    }

    #[test]
    fn a_scheme_less_entry_is_told_to_add_https() {
        let err = parse_allowed_prefixes(Some("s3.example.com")).unwrap_err();
        assert!(err.contains("https://"), "{err}");
    }

    // -- original_location (TP-K / TP-S / TP-I / TP-F / TP-OK) -------------

    fn allowed() -> Vec<AllowedPrefix> {
        parse_allowed_prefixes(Some(T_SET)).unwrap()
    }

    /// `T_SET_PERMITS` is the invariant check's own, hand-written statement of
    /// what `T_SET` permits. It is only an independent oracle if it is pinned
    /// to what the parser actually makes of `T_SET`.
    #[test]
    fn the_corpus_oracle_states_what_the_allowlist_parses_to() {
        let allowed = allowed();
        let parsed: Vec<(&str, u16, &str)> = allowed
            .iter()
            .map(|entry| (entry.host.as_str(), entry.port, entry.path.as_str()))
            .collect();
        assert_eq!(parsed, T_SET_PERMITS);
    }

    /// The outcome in the corpus's own vocabulary.
    fn outcome(result: Result<String, Reject>) -> String {
        match result {
            Ok(location) => format!("ok:{location}"),
            Err(reject) => format!("{reject:?}"),
        }
    }

    #[test]
    fn every_value_in_the_corpus_gets_its_expected_outcome() {
        let allowed = allowed();
        for (value, expected) in VALUE_CASES {
            let target = proxy_target(value);
            assert_eq!(
                outcome(original_location(&allowed, &target)),
                *expected,
                "url={value:?}"
            );
        }
    }

    #[test]
    fn every_raw_target_in_the_corpus_gets_its_expected_outcome() {
        let allowed = allowed();
        for (target, expected) in RAW_CASES {
            assert_eq!(
                outcome(original_location(&allowed, target)),
                *expected,
                "target={target:?}"
            );
        }
    }

    #[test]
    fn every_accepted_location_satisfies_the_invariants() {
        let allowed = allowed();
        let mut accepted = 0;
        let targets = VALUE_CASES
            .iter()
            .map(|(value, _)| proxy_target(value))
            .chain(RAW_CASES.iter().map(|(target, _)| target.to_string()));
        for target in targets {
            if let Ok(location) = original_location(&allowed, &target) {
                assert_location_invariants(&location);
                accepted += 1;
            }
        }
        assert!(
            accepted >= 15,
            "the corpus barely accepts anything: {accepted}"
        );
    }

    #[test]
    fn the_corpus_exercises_every_url_rule() {
        let outcomes: Vec<String> = VALUE_CASES
            .iter()
            .chain(RAW_CASES.iter())
            .map(|(_, expected)| expected.to_string())
            .collect();
        for rule in [
            "U0Param",
            "U1Len",
            "U2Bytes",
            "U3Scheme",
            "U4Chars",
            "U5Authority",
            "U6Path",
            "U7Parse",
            "U8Allow",
        ] {
            assert!(
                outcomes.iter().any(|o| o == rule),
                "no corpus row is refused by {rule}"
            );
        }
    }

    #[test]
    fn the_url_value_is_bounded_at_2048_and_the_target_at_8192() {
        let allowed = allowed();
        let head = "https://s3.example.com/bucket/";

        let value = |len: usize| format!("{head}{}", "a".repeat(len - head.len()));
        for (len, expected) in [(2048, true), (2049, false)] {
            let result = original_location(&allowed, &proxy_target(&value(len)));
            if expected {
                assert!(result.is_ok(), "{len}: {result:?}");
            } else {
                assert_eq!(result, Err(Reject::U1Len), "{len}");
            }
        }

        // Padding after the `url` parameter is ignored, which is what lets a
        // target reach exactly 8192 bytes with a valid value.
        let base = proxy_target(&value(100));
        let pad = |total: usize| format!("{base}&pad={}", "x".repeat(total - base.len() - 5));
        assert_eq!(pad(8192).len(), 8192);
        assert!(original_location(&allowed, &pad(8192)).is_ok());
        assert_eq!(original_location(&allowed, &pad(8193)), Err(Reject::F1Len));
    }

    #[test]
    fn an_empty_allowlist_refuses_every_original_url() {
        for (value, expected) in VALUE_CASES {
            if expected.starts_with("ok:") {
                assert_eq!(
                    original_location(&[], &proxy_target(value)),
                    Err(Reject::U8Allow),
                    "url={value:?}"
                );
            }
        }
    }

    #[test]
    fn the_path_of_the_request_is_not_part_of_the_decision() {
        let allowed = allowed();
        let url = "url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png";
        let expected = Ok("https://s3.example.com/bucket/a.png".to_string());
        for path in [
            "/proxy/image.webp",
            "/proxy/x",
            "/proxy/a/b/c",
            "/proxy/../x",
        ] {
            assert_eq!(
                original_location(&allowed, &format!("{path}?{url}")),
                expected,
                "{path}"
            );
        }
    }

    #[test]
    fn a_host_written_in_capitals_or_with_the_default_port_matches() {
        let allowed = allowed();
        assert_eq!(
            original_location(&allowed, &proxy_target("https://R2.EXAMPLE.NET:443/a.png")),
            Ok("https://r2.example.net/a.png".to_string())
        );
    }
}
