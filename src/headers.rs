// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Header hygiene for the relay in `src/proxy.rs`.
//!
//! The allowlist decides *which* requests reach Misskey; this module decides
//! what of such a request's header block reaches it. Everything else about the
//! relay stays byte-for-byte — the raw body in particular must not be touched,
//! because `/inbox`'s HTTP Signature `Digest` is computed over it — but a
//! header block is not payload: it is metadata about the connection and about
//! who the caller claims to be, and a public caller must not get to write
//! either in the way Misskey reads it.
//!
//! Two classes are removed:
//!
//! - **Connection-specific headers** (RFC 9110 §7.6.1) describe this hop, not
//!   the request. A proxy MUST NOT forward them. A stale `Transfer-Encoding`,
//!   `Upgrade` or `Connection` reaching the next hop is a framing / protocol
//!   desync vector, and `Connection` can nominate further headers to drop.
//!   This applies in both directions.
//! - **Caller-asserted identity and target headers** are ones only the trusted
//!   TLS terminator may set: the forwarding headers Misskey's `trustProxy`
//!   reads (`request.ip`, `request.protocol`, `request.hostname`), and the
//!   overriding headers some front-ends honour to change the method or target.
//!   The proxy cannot tell a value the terminator set from one the caller
//!   spoofed inside a terminator that only appends, so it drops all of them on
//!   the way in. No reachable route needs any of them: the terminator normally
//!   *replaces* the forwarding headers, and if it does, their only consumers —
//!   the `/api/*` and sign-in rate limits — are unreachable anyway
//!   (`README.md`, "Deployment notes").
//!
//! Headers that are part of the request's or response's own semantics are left
//! alone, including the ones an HTTP Signature may cover (`Host`, `Date`,
//! `Digest`, `Content-Type`, `Signature`, `Authorization`, `Accept`) and every
//! response header Misskey sends (including `Set-Cookie`; the allowlisted
//! routes do not set session material, and stripping it would make this relay
//! lie about what Misskey said).

use axum::http::{HeaderMap, HeaderName, header};

/// Connection-specific headers (RFC 9110 §7.6.1), plus the non-standard
/// `Proxy-Connection` that proxies have historically treated the same way.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Forwarding headers a caller could use to assert its own address, protocol
/// or host to a `trustProxy` upstream. `Forwarded` is RFC 7239; the
/// `X-Forwarded-*` and `X-Real-IP` spellings are what Fastify's `trustProxy`
/// reads; the remaining names are ones common edge proxies set, kept here so a
/// caller cannot smuggle one through even if Misskey is later configured to
/// read them.
const FORWARDING: &[&str] = &[
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-real-ip",
    "x-client-ip",
    "cf-connecting-ip",
    "true-client-ip",
];

/// Headers some front-ends honour to rewrite the request's method or target.
/// The allowlist decides the target here, and federation never uses these, so
/// a caller must not get to smuggle one past it.
const ROUTING_OVERRIDES: &[&str] = &["x-http-method-override", "x-original-url", "x-rewrite-url"];

/// Removes everything Misskey must not see from a caller's request.
pub fn sanitize_request(headers: &mut HeaderMap) {
    strip_hop_by_hop(headers);
    for name in FORWARDING.iter().chain(ROUTING_OVERRIDES) {
        headers.remove(*name);
    }
}

/// Removes connection-specific headers from a response on its way back out.
/// Everything else Misskey said is left as it was.
pub fn sanitize_response(headers: &mut HeaderMap) {
    strip_hop_by_hop(headers);
}

/// Removes the connection-specific headers, and every header a `Connection`
/// header nominates (RFC 9110 §7.6.1: a proxy must remove the fields named by
/// the connection options too).
///
/// The nominations are read before `Connection` itself is removed. A token
/// that is not a valid header name is ignored: `HeaderMap` cannot hold it, so
/// there is nothing to remove. A token naming `Host` or `Content-Length` is
/// honoured like any other — a caller can only break its own request that way.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let nominated: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter_map(|token| HeaderName::from_bytes(token.as_bytes()).ok())
        .collect();

    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    for name in nominated {
        headers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn every_hop_by_hop_header_is_removed_in_both_directions() {
        for name in HOP_BY_HOP {
            let mut request = map(&[("host", "h"), (name, "x")]);
            sanitize_request(&mut request);
            assert!(!request.contains_key(*name), "request kept {name}");
            assert!(request.contains_key("host"), "request dropped host");

            let mut response = map(&[("content-type", "text/plain"), (name, "x")]);
            sanitize_response(&mut response);
            assert!(!response.contains_key(*name), "response kept {name}");
            assert!(
                response.contains_key("content-type"),
                "response dropped content-type"
            );
        }
    }

    #[test]
    fn a_connection_nomination_is_removed_too() {
        let mut headers = map(&[
            ("connection", "keep-alive, X-Custom-Hop"),
            ("x-custom-hop", "drop me"),
            ("x-kept", "keep me"),
        ]);
        sanitize_request(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(
            !headers.contains_key("x-custom-hop"),
            "a header named by Connection is hop-by-hop"
        );
        assert_eq!(headers.get("x-kept").unwrap(), "keep me");
    }

    #[test]
    fn nominations_are_read_across_repeated_connection_headers() {
        let mut headers = HeaderMap::new();
        headers.append(header::CONNECTION, HeaderValue::from_static("a-hop"));
        headers.append(header::CONNECTION, HeaderValue::from_static("b-hop"));
        headers.append("a-hop", HeaderValue::from_static("1"));
        headers.append("b-hop", HeaderValue::from_static("2"));

        sanitize_request(&mut headers);

        assert!(!headers.contains_key("a-hop"));
        assert!(!headers.contains_key("b-hop"));
    }

    /// A token that is not a valid header name cannot be in the map to begin
    /// with; it must not panic or take a valid neighbour with it.
    #[test]
    fn an_invalid_connection_token_is_ignored() {
        let mut headers = map(&[
            ("connection", "keep-alive, not a header name, ,\ttrimmed\t"),
            ("trimmed", "gone"),
            ("x-kept", "kept"),
        ]);
        sanitize_request(&mut headers);

        assert!(!headers.contains_key("trimmed"));
        assert_eq!(headers.get("x-kept").unwrap(), "kept");
    }

    #[test]
    fn a_caller_cannot_set_a_forwarding_or_override_header() {
        for name in FORWARDING.iter().chain(ROUTING_OVERRIDES) {
            let mut request = map(&[("host", "h"), (name, "spoofed")]);
            sanitize_request(&mut request);
            assert!(!request.contains_key(*name), "request kept {name}");
            assert!(request.contains_key("host"));
        }
    }

    /// The headers an HTTP signature may cover, and the rest of a normal
    /// federation request, come through unchanged.
    #[test]
    fn a_federation_request_survives_untouched() {
        let mut request = map(&[
            ("host", "misskey.example"),
            ("date", "Mon, 12 Oct 2026 00:00:00 GMT"),
            ("digest", "SHA-256=x"),
            ("content-type", "application/activity+json"),
            ("signature", "keyId=\"k\""),
            ("authorization", "Signature keyId=\"k\""),
            ("accept", "application/activity+json"),
            ("user-agent", "Mastodon/4.0"),
            ("content-length", "123"),
        ]);
        let before = request.clone();

        sanitize_request(&mut request);

        assert_eq!(request, before);
    }

    /// A response is only ever relieved of its connection headers.
    #[test]
    fn a_response_only_loses_its_hop_by_hop_headers() {
        let mut response = map(&[
            ("content-type", "text/plain"),
            ("cache-control", "public, max-age=60"),
            ("set-cookie", "a=b"),
            ("vary", "Accept-Encoding"),
            ("connection", "keep-alive"),
        ]);
        let mut expected = response.clone();
        expected.remove("connection");

        sanitize_response(&mut response);

        assert_eq!(response, expected);
    }
}
