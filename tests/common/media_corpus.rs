// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Inputs for the `url` validation of `MEDIA_MODE=redirect`, shared by the unit
// tests in `src/media_target.rs` and the router tests in `tests/router.rs`.
// Both `include!` this file, so it is plain data and helpers with no `use`
// lines of its own: the unit tests check each row against the pure function,
// the router tests replay the same rows over HTTP and assert the invariants
// on every `Location` that comes out.
//
// An expectation is `ok:<Location>` for a redirect, or the name of the
// `Reject` variant (its `Debug` form) of the rule that must refuse the row.
// Rows are written from the rules in `docs/routes.md` (§ "洗い出し表" IDs in
// the comments), not from what the code happens to do.
//
// The hosts are placeholders (`example.com`, `example.net`): no real
// deployment's values belong in code or tests.

/// The allowlist every row is judged against.
pub const T_SET: &str = "https://s3.example.com/bucket/,https://r2.example.net/,\
                         https://misskey.example.com/files/,https://s3.example.com:9000/other/";

/// What T_SET permits, spelled out for the invariant check: `(host, port,
/// path prefix)`.
pub const T_SET_PERMITS: &[(&str, u16, &str)] = &[
    ("s3.example.com", 443, "/bucket/"),
    ("r2.example.net", 443, "/"),
    ("misskey.example.com", 443, "/files/"),
    ("s3.example.com", 9000, "/other/"),
];

/// `(decoded value of the url parameter, expectation)`.
pub const VALUE_CASES: &[(&str, &str)] = &[
    // Ok1: the ordinary shapes must not be over-refused.
    (
        "https://s3.example.com/bucket/a%20b%E3%81%82.png",
        "ok:https://s3.example.com/bucket/a%20b%E3%81%82.png",
    ),
    (
        "https://s3.example.com/bucket/dir/a.png?x=%2F..%2F",
        "ok:https://s3.example.com/bucket/dir/a.png?x=%2F..%2F",
    ),
    (
        "https://misskey.example.com/files/0f9d7c1e-8a52-4d3a-9b6f-1f2e3d4c5b6a",
        "ok:https://misskey.example.com/files/0f9d7c1e-8a52-4d3a-9b6f-1f2e3d4c5b6a",
    ),
    (
        "https://misskey.example.com/files/thumbnail-0f9d7c1e-8a52-4d3a-9b6f-1f2e3d4c5b6a",
        "ok:https://misskey.example.com/files/thumbnail-0f9d7c1e-8a52-4d3a-9b6f-1f2e3d4c5b6a",
    ),
    ("https://r2.example.net/a.png", "ok:https://r2.example.net/a.png"),
    (
        "https://r2.example.net/dir/a.png",
        "ok:https://r2.example.net/dir/a.png",
    ),
    // The query is re-serialised by the parser (`'` becomes `%27`).
    (
        "https://s3.example.com/bucket/a?q='x'",
        "ok:https://s3.example.com/bucket/a?q=%27x%27",
    ),
    // Sch5: scheme and host case are normalised.
    (
        "HTTPS://S3.EXAMPLE.COM/bucket/a.png",
        "ok:https://s3.example.com/bucket/a.png",
    ),
    // Po1: the default port is dropped, leading zeros are read as a number.
    (
        "https://s3.example.com:443/bucket/a.png",
        "ok:https://s3.example.com/bucket/a.png",
    ),
    (
        "https://s3.example.com:00443/bucket/a.png",
        "ok:https://s3.example.com/bucket/a.png",
    ),
    // Po3: a non-default port named in the allowlist.
    (
        "https://s3.example.com:9000/other/a.png",
        "ok:https://s3.example.com:9000/other/a.png",
    ),
    // Au8: an `@` in the path is just a path on the allowed host.
    (
        "https://r2.example.net/@evil.example/a.png",
        "ok:https://r2.example.net/@evil.example/a.png",
    ),
    // Fr2: an encoded `#` stays encoded.
    (
        "https://s3.example.com/bucket/a%23b.png",
        "ok:https://s3.example.com/bucket/a%23b.png",
    ),
    // Sch1: other schemes.
    ("http://s3.example.com/bucket/a.png", "U3Scheme"),
    ("ftp://s3.example.com/bucket/a.png", "U3Scheme"),
    ("javascript:alert(1)", "U3Scheme"),
    ("data:text/html,x", "U3Scheme"),
    ("file:///etc/passwd", "U3Scheme"),
    // Sch2: no scheme.
    ("//s3.example.com/bucket/a.png", "U3Scheme"),
    ("/bucket/a.png", "U3Scheme"),
    ("s3.example.com/bucket/a.png", "U3Scheme"),
    // Sch3: shapes the `url` crate folds into `https://host`.
    ("https:/s3.example.com/bucket/a.png", "U3Scheme"),
    ("https:\\\\s3.example.com\\bucket\\a.png", "U3Scheme"),
    ("https:s3.example.com/bucket/a.png", "U3Scheme"),
    ("https:///s3.example.com/bucket/a.png", "U5Authority"),
    // Sch4: leading space, TAB inside the scheme.
    (" https://s3.example.com/bucket/a.png", "U2Bytes"),
    ("ht\ttps://s3.example.com/bucket/a.png", "U2Bytes"),
    // Au1-Au4: userinfo.
    ("https://evil.example@s3.example.com/bucket/a.png", "U5Authority"),
    ("https://s3.example.com@evil.example/bucket/a.png", "U5Authority"),
    ("https://a@b@s3.example.com/bucket/a.png", "U5Authority"),
    (
        "https://s3.example.com:pw@evil.example/bucket/a.png",
        "U5Authority",
    ),
    ("https://@s3.example.com/bucket/a.png", "U5Authority"),
    // Au5: `\` ends the authority for WHATWG and not for RFC 3986.
    ("https://s3.example.com\\@evil.example/bucket/a.png", "U4Chars"),
    ("https://s3.example.com\\.evil.example/bucket/a.png", "U4Chars"),
    // Au6: the same tricks, percent-encoded.
    ("https://s3.example.com%5c@evil.example/bucket/a.png", "U5Authority"),
    ("https://s3.example.com%40evil.example/bucket/a.png", "U5Authority"),
    // Au7: hiding an `@` in a fragment or a query.
    ("https://s3.example.com#@evil.example/bucket/a.png", "U4Chars"),
    ("https://s3.example.com?@evil.example/bucket/a.png", "U6Path"),
    // Ho1: percent-encoded host.
    ("https://%73%33.example.com/bucket/a.png", "U5Authority"),
    // Ho2: trailing dot is a different string, so it does not match.
    ("https://s3.example.com./bucket/a.png", "U8Allow"),
    ("https://s3.example.com../", "U6Path"),
    ("https://.s3.example.com/bucket/a.png", "U8Allow"),
    ("https://s3..example.com/bucket/a.png", "U8Allow"),
    // Ho3: prefix and suffix look-alikes.
    ("https://s3.example.com.evil.example/bucket/a.png", "U8Allow"),
    ("https://evil-s3.example.com/bucket/a.png", "U8Allow"),
    ("https://xs3.example.com/bucket/a.png", "U8Allow"),
    // Ho4: homoglyph (Cyrillic s) and an ideographic full stop.
    ("https://\u{0455}3.example.com/bucket/a.png", "U2Bytes"),
    ("https://s3\u{3002}example\u{3002}com/bucket/a.png", "U2Bytes"),
    // Ho5: an encoded IDNA separator.
    ("https://s3.example.com%E3%80%82evil/bucket/a.png", "U5Authority"),
    // IP1: every spelling of an IPv4 address ends up as `Host::Ipv4`.
    ("https://127.0.0.1/bucket/a.png", "U7Parse"),
    ("https://2130706433/bucket/a.png", "U7Parse"),
    ("https://0x7f.1/bucket/a.png", "U7Parse"),
    ("https://0177.0.0.1/bucket/a.png", "U7Parse"),
    ("https://127.1/bucket/a.png", "U7Parse"),
    ("https://169.254.169.254/bucket/a.png", "U7Parse"),
    // IP2: IPv6 literals and zone ids.
    ("https://[::1]/bucket/a.png", "U5Authority"),
    ("https://[::ffff:127.0.0.1]/bucket/a.png", "U5Authority"),
    ("https://[fe80::1%25eth0]/bucket/a.png", "U5Authority"),
    // IP3: internal names.
    ("https://localhost/bucket/a.png", "U8Allow"),
    ("https://s3/bucket/a.png", "U8Allow"),
    // Po2: a port the allowlist does not name.
    ("https://s3.example.com:8443/bucket/a.png", "U8Allow"),
    ("https://s3.example.com:80/bucket/a.png", "U8Allow"),
    ("https://s3.example.com:0/bucket/a.png", "U8Allow"),
    // A named port only allows its own prefix.
    ("https://s3.example.com:9000/bucket/a.png", "U8Allow"),
    // Po4: empty, non-numeric and non-ASCII ports.
    ("https://s3.example.com:/bucket/a.png", "U5Authority"),
    ("https://s3.example.com:abc/bucket/a.png", "U5Authority"),
    ("https://s3.example.com:-1/bucket/a.png", "U5Authority"),
    (
        "https://s3.example.com:\u{FF14}\u{FF14}\u{FF13}/bucket/a.png",
        "U2Bytes",
    ),
    // Po5: out of range.
    ("https://s3.example.com:65536/bucket/a.png", "U7Parse"),
    // Fr1: a fragment.
    ("https://s3.example.com/bucket/a.png#frag", "U4Chars"),
    // Ct1: control characters the `url` crate would silently drop.
    ("https://s3.example.com/bucket/a\t.png", "U2Bytes"),
    ("https://s3.example.com/bucket/a\n.png", "U2Bytes"),
    ("https://s3.example.com/bucket/a\r.png", "U2Bytes"),
    ("https://s3.example.com/bucket/a\0.png", "U2Bytes"),
    ("https://s3.example.com/bucket/a\u{7f}.png", "U2Bytes"),
    ("https://s3.exa\tmple.com/bucket/a.png", "U2Bytes"),
    // En1: still encoded after the one decoding the query gets.
    (
        "https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "U3Scheme",
    ),
    // En2: dot-segments, separators and control bytes hiding in the path.
    ("https://s3.example.com/bucket/%2e%2e/a.png", "U6Path"),
    ("https://s3.example.com/bucket/%2E%2E/a.png", "U6Path"),
    ("https://s3.example.com/bucket/.%2e/a.png", "U6Path"),
    ("https://s3.example.com/bucket/%2e./a.png", "U6Path"),
    ("https://s3.example.com/bucket/%2e/a.png", "U6Path"),
    ("https://s3.example.com/bucket/a%2fb", "U6Path"),
    ("https://s3.example.com/bucket/a%5cb", "U6Path"),
    ("https://s3.example.com/bucket/%252e%252e/a.png", "U6Path"),
    ("https://s3.example.com/bucket/a%252fb", "U6Path"),
    ("https://s3.example.com/bucket/a%255cb", "U6Path"),
    ("https://s3.example.com/bucket/%25252e%25252e/a.png", "U6Path"),
    ("https://s3.example.com/bucket/a%00b", "U6Path"),
    ("https://s3.example.com/bucket/a%0d%0ab", "U6Path"),
    ("https://s3.example.com/bucket/a%zz", "U6Path"),
    // A path the `url` crate would rewrite is not the path that was checked.
    ("https://s3.example.com/bucket/a\"b", "U8Allow"),
    ("https://s3.example.com/bucket/a{b}", "U8Allow"),
    ("https://s3.example.com/bucket/a<b", "U8Allow"),
    // Pa1: outside the prefix, on a segment boundary, and the bare prefix.
    ("https://s3.example.com/other/a.png", "U8Allow"),
    ("https://s3.example.com/bucketevil/a.png", "U8Allow"),
    ("https://s3.example.com/bucket", "U8Allow"),
    ("https://s3.example.com/bucket/", "U6Path"),
    ("https://s3.example.com/bucket//a.png", "U6Path"),
    ("https://r2.example.net/", "U6Path"),
    ("https://r2.example.net", "U6Path"),
    // Pa2: leaving the prefix by dot-segments.
    ("https://s3.example.com/bucket/../other/a.png", "U6Path"),
    ("https://s3.example.com/bucket/%2e%2e/other/a.png", "U6Path"),
    // Pa3: our own domain, other than under /files/.
    (
        "https://misskey.example.com/proxy/x?url=https://s3.example.com/bucket/a.png",
        "U8Allow",
    ),
    ("https://misskey.example.com/", "U6Path"),
    ("https://misskey.example.com/files/../proxy/x", "U6Path"),
    ("https://misskey.example.com/files/", "U6Path"),
];

/// `(raw path?query, expectation)`: the rows that are about the query
/// string's own shape and so cannot be written as a single decoded value.
pub const RAW_CASES: &[(&str, &str)] = &[
    // K1: no `url`, an empty `url`.
    ("/proxy/image.webp", "U0Param"),
    ("/proxy/image.webp?", "U0Param"),
    ("/proxy/image.webp?static=1", "U0Param"),
    ("/proxy/image.webp?url", "U1Len"),
    ("/proxy/image.webp?url=", "U1Len"),
    // K2: repeated and disguised `url` keys, in both orders.
    (
        "/proxy/i?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png&url=https%3A%2F%2Fevil.example%2Fbucket%2Fa.png",
        "U0Param",
    ),
    (
        "/proxy/i?url=https%3A%2F%2Fevil.example%2Fbucket%2Fa.png&url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "U0Param",
    ),
    (
        "/proxy/i?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png&url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "U0Param",
    ),
    (
        "/proxy/i?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png&u%72l=https%3A%2F%2Fevil.example%2Fbucket%2Fa.png",
        "U0Param",
    ),
    // K3: keys that are not `url`.
    (
        "/proxy/i?URL=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "U0Param",
    ),
    (
        "/proxy/i?url[]=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "U0Param",
    ),
    // K4: Misskey's `/proxy/<host>/<path>` form is not supported.
    ("/proxy/s3.example.com/bucket/a.png", "U0Param"),
    // K5: an inner `&` is not part of the value; the rest is dropped.
    (
        "/proxy/i?url=https://s3.example.com/bucket/a?x=1&y=2",
        "ok:https://s3.example.com/bucket/a?x=1",
    ),
    // K6: an outer `+` decodes to a space.
    ("/proxy/i?url=https://s3.example.com/bucket/a+b", "U2Bytes"),
    // Raw bytes an HTTP client may legally put in a query: `\` (Au5/Sch3
    // written unencoded), an escape that is not valid, one that decodes to
    // invalid UTF-8.
    ("/proxy/i?url=https:\\\\s3.example.com\\bucket\\a.png", "U3Scheme"),
    (
        "/proxy/i?url=https://s3.example.com\\@evil.example/bucket/a.png",
        "U4Chars",
    ),
    ("/proxy/i?url=https://s3.example.com/bucket/a%zz", "U6Path"),
    ("/proxy/i?url=https://s3.example.com/bucket/a%ff", "U2Bytes"),
    // Other parameters are ignored wherever they sit.
    (
        "/proxy/i?static=1&url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "ok:https://s3.example.com/bucket/a.png",
    ),
    (
        "/proxy/avatar.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png&avatar=1&static=1",
        "ok:https://s3.example.com/bucket/a.png",
    ),
    // Our own /files/ (D12): the second hop belongs to the edge.
    (
        "/proxy/avatar.webp?url=https%3A%2F%2Fmisskey.example.com%2Ffiles%2Fk1&avatar=1",
        "ok:https://misskey.example.com/files/k1",
    ),
];

/// A `/proxy/*` request-target that carries `value` as its `url` parameter.
/// Encoded the way a real client would, so a value holding a control
/// character or a space still makes a well-formed target.
pub fn proxy_target(value: &str) -> String {
    format!(
        "/proxy/image.webp?url={}",
        url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
    )
}

/// The host of `location` as an RFC 3986 reader sees it: everything after
/// `://` up to the first `/`, `?` or `#`, minus whatever precedes the last
/// `@` (userinfo) and the `:port`. Deliberately independent of the `url`
/// crate, so that agreeing with `Url::host_str()` is evidence the two
/// readings of a `Location` do not diverge.
pub fn rfc3986_host(location: &str) -> String {
    let after_scheme = location.split_once("://").expect("a scheme").1;
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    let host_and_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host_and_port.split(':').next().unwrap().to_string()
}

/// What must hold of every `Location` that leaves the proxy for an original
/// URL: it is stable under re-parsing, `https`, has no userinfo or fragment,
/// is ASCII without `\`, names a host and path the allowlist permits, and
/// both readings of its host agree.
pub fn assert_location_invariants(location: &str) {
    let parsed = url::Url::parse(location).unwrap_or_else(|e| panic!("{location}: {e}"));
    assert_eq!(parsed.as_str(), location, "not stable under re-parsing");
    assert_eq!(parsed.scheme(), "https", "{location}");
    assert!(parsed.username().is_empty(), "{location}");
    assert!(parsed.password().is_none(), "{location}");
    assert!(parsed.fragment().is_none(), "{location}");
    assert!(location.is_ascii() && !location.contains('\\'), "{location}");

    let host = parsed.host_str().expect("a host");
    let port = parsed.port_or_known_default().expect("a port");
    assert!(
        T_SET_PERMITS.iter().any(|(permitted_host, permitted_port, prefix)| {
            *permitted_host == host
                && *permitted_port == port
                && parsed.path().starts_with(prefix)
                && parsed.path().len() > prefix.len()
        }),
        "{location} is not under an allowed prefix"
    );
    assert_eq!(rfc3986_host(location), host, "{location}: the readings differ");
}
