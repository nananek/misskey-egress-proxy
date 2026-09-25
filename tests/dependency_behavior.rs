// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pins the behaviour of the third-party crates that the media path
//! validation rules (`F*` / `U*` in `docs/routes.md`) are written against.
//!
//! The rules are derived from reading `http`, `axum`, `url` and
//! `form_urlencoded`; these tests measure what those crates actually do, so a
//! `Cargo.lock` update that changes any of it fails here first. **If one of
//! these fails after a dependency bump, revisit the reasoning behind the
//! `F*` / `U*` rules in `docs/routes.md` before touching the expected value.**
//!
//! `DB-H*` measure `axum::http::Uri`, `DB-R*` measure the router's own
//! matching (default `proxy` mode, mock backend), `DB-U*` measure
//! `url::Url`, and `DB-F1` measures `url::form_urlencoded`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tower::ServiceExt;
use url::{Host, ParseError, Url, form_urlencoded};

use misskey_egress_proxy::config::{Config, ListenTarget};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

const INTERNAL_BASE_URL: &str = "https://internal.example.ts.net";
const INTERNAL_SUFFIX: &str = ".internal.example.ts.net";

// ---------------------------------------------------------------------------
// DB-H: `axum::http::Uri`
// ---------------------------------------------------------------------------

fn uri(s: &str) -> Result<Uri, axum::http::uri::InvalidUri> {
    s.parse::<Uri>()
}

/// DB-H1: `http` lets `\`, `"`, `{`, `}`, `|`, `^` through in a path.
#[test]
fn db_h1_http_uri_accepts_backslash_and_friends_in_a_path() {
    for target in [
        "/files/a\\b",
        "/files/a\"b",
        "/files/{x}",
        "/files/a|b",
        "/files/a^b",
    ] {
        assert!(uri(target).is_ok(), "{target:?} should parse");
    }
}

/// DB-H2: non-ASCII bytes are kept in `path()`.
#[test]
fn db_h2_http_uri_keeps_non_ascii_in_a_path() {
    let parsed = uri("/files/あ").expect("non-ASCII path should parse");
    assert_eq!(parsed.path(), "/files/あ");
}

/// DB-H3: space, `<`, `>` and the backtick are refused.
#[test]
fn db_h3_http_uri_rejects_space_angle_brackets_and_backtick() {
    for target in ["/files/a b", "/files/a<b", "/files/a>b", "/files/a`b"] {
        assert!(uri(target).is_err(), "{target:?} should be refused");
    }
}

/// DB-H4: a fragment is cut off the target.
#[test]
fn db_h4_http_uri_drops_the_fragment() {
    let parsed = uri("/files/x#frag").expect("fragment target should parse");
    assert_eq!(parsed.path_and_query().unwrap().as_str(), "/files/x");
}

/// DB-H5: in a query, `\` passes and `"` does not.
#[test]
fn db_h5_http_uri_query_accepts_backslash_but_not_a_quote() {
    assert!(uri("/files/x?a=b\\c").is_ok());
    assert!(uri("/files/x?a=\"b").is_err());
}

/// DB-H6: an absolute-form target keeps its authority separate from `path()`.
#[test]
fn db_h6_http_uri_absolute_form_splits_authority_from_path() {
    let parsed = uri("http://evil.example/files/x").expect("absolute form should parse");
    assert_eq!(parsed.path(), "/files/x");
    assert!(parsed.authority().is_some());
}

// ---------------------------------------------------------------------------
// DB-R: the router itself (default `proxy` mode, current forwarding code)
// ---------------------------------------------------------------------------

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Same fake Misskey as `tests/router.rs`: 200 for everything, with the
/// request path+query echoed in `x-mock-path`.
async fn spawn_mock_backend() -> PathBuf {
    let socket = std::env::temp_dir().join(format!("mep-depb-{}.sock", unique_suffix()));
    let _ = std::fs::remove_file(&socket);

    let app = Router::new().fallback(any(echo_path));
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind mock backend socket");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock backend crashed");
    });

    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    socket
}

async fn echo_path(req: Request<Body>) -> Response {
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_default();
    (StatusCode::OK, [("x-mock-path", path)], "mock-backend").into_response()
}

async fn build_app() -> Router {
    let socket = spawn_mock_backend().await;
    let config = Arc::new(Config {
        listen: ListenTarget::Tcp("127.0.0.1:0".to_string()),
        misskey_socket: socket.clone(),
        internal_base_url: INTERNAL_BASE_URL.to_string(),
        internal_referer_suffix: INTERNAL_SUFFIX.to_string(),
        static_dir: PathBuf::from("/path/that/does/not/exist"),
    });
    let proxy_state = ProxyState {
        client: build_client(),
        socket,
    };
    routes::build(proxy_state, config)
}

async fn send(app: &Router, method: &str, target: &str, headers: &[(&str, &str)]) -> Response {
    let mut builder = Request::builder().method(method).uri(target);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn mock_path(resp: &Response) -> String {
    resp.headers()
        .get("x-mock-path")
        .expect("response should come from the mock backend")
        .to_str()
        .unwrap()
        .to_string()
}

/// DB-R1: an empty `{key}` segment still matches, so `/files//x` is forwarded.
#[tokio::test]
async fn db_r1_files_with_an_empty_segment_is_forwarded() {
    let app = build_app().await;

    let resp = send(&app, "GET", "/files//x", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_path(&resp), "/files//x");
}

/// DB-R2: the router matches on the raw path: no dot-segment removal, no
/// percent-decoding, no `\` rewriting, and the path reaches the backend
/// exactly as sent.
#[tokio::test]
async fn db_r2_router_does_not_normalise_or_decode_the_path() {
    let app = build_app().await;

    for target in ["/files/../x", "/files/%2e%2e/x", "/files/a\\b"] {
        let resp = send(&app, "GET", target, &[]).await;
        assert_eq!(resp.status(), StatusCode::OK, "{target:?}");
        assert_eq!(mock_path(&resp), target, "{target:?} must arrive verbatim");
    }
}

/// DB-R3: the bare prefixes match no media route.
#[tokio::test]
async fn db_r3_bare_media_prefixes_are_not_found() {
    let app = build_app().await;

    for target in ["/files", "/files/", "/proxy", "/proxy/"] {
        let resp = send(&app, "GET", target, &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{target:?}");
    }
}

/// DB-R4: the media `route_layer` also runs for HEAD (axum serves HEAD from
/// the GET handler), so an internal-Referer HEAD is redirected too.
#[tokio::test]
async fn db_r4_head_with_an_internal_referer_is_redirected() {
    let app = build_app().await;

    let resp = send(
        &app,
        "HEAD",
        "/files/abc",
        &[("referer", "https://misskey.internal.example.ts.net/")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        &format!("{INTERNAL_BASE_URL}/files/abc")
    );
}

/// DB-R5: in an absolute-form request the router (and the forwarder) look at
/// the path only; the authority and `Host` are ignored.
#[tokio::test]
async fn db_r5_absolute_form_authority_is_ignored() {
    let app = build_app().await;

    let resp = send(
        &app,
        "GET",
        "http://evil.example/files/x",
        &[("host", "evil.example")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_path(&resp), "/files/x");
}

// ---------------------------------------------------------------------------
// DB-U: `url::Url::parse`
// ---------------------------------------------------------------------------

fn parse(s: &str) -> Url {
    Url::parse(s).unwrap_or_else(|e| panic!("{s:?} should parse, got {e:?}"))
}

/// DB-U1: WHATWG collapses any run of `/` and `\` (or none) after `https:`
/// into a single authority; a missing scheme is a relative-URL error.
#[test]
fn db_u1_slash_variants_after_the_scheme_collapse_into_one_host() {
    for input in [
        "https:\\\\s3.example.com\\bucket\\a.png",
        "https:s3.example.com/a",
        "https:/s3.example.com/a",
        "https:///s3.example.com/a",
    ] {
        assert_eq!(parse(input).host_str(), Some("s3.example.com"), "{input:?}");
    }
    for input in ["/bucket/a.png", "//s3.example.com/a"] {
        assert_eq!(
            Url::parse(input),
            Err(ParseError::RelativeUrlWithoutBase),
            "{input:?}"
        );
    }
}

/// DB-U2: `\` ends the authority; the last `@` is the userinfo separator; an
/// empty userinfo is dropped.
#[test]
fn db_u2_backslash_ends_the_authority_and_last_at_splits_userinfo() {
    let u = parse("https://s3.example.com\\@evil.example/x");
    assert_eq!(u.host_str(), Some("s3.example.com"));
    assert_eq!(u.path(), "/@evil.example/x");

    let u = parse("https://a@b@s3.example.com/x");
    assert_eq!(u.host_str(), Some("s3.example.com"));
    assert_eq!(u.username(), "a%40b");

    let u = parse("https://@s3.example.com/x");
    assert_eq!(u.username(), "");
}

/// DB-U3: TAB (and LF / CR) are stripped from anywhere in the input, and
/// leading whitespace is trimmed.
#[test]
fn db_u3_tabs_are_stripped_and_leading_space_is_trimmed() {
    assert_eq!(
        parse("https://s3.exa\tmple.com/x").host_str(),
        Some("s3.example.com")
    );
    assert!(Url::parse(" https://s3.example.com/x").is_ok());
    assert_eq!(parse("ht\ttps://s3.example.com/x").scheme(), "https");
}

/// DB-U4: a percent-encoded host is decoded; a trailing dot is kept.
#[test]
fn db_u4_host_is_percent_decoded_and_keeps_a_trailing_dot() {
    assert_eq!(
        parse("https://%73%33.example.com/x").host_str(),
        Some("s3.example.com")
    );
    assert_eq!(
        parse("https://s3.example.com./x").host_str(),
        Some("s3.example.com.")
    );
}

/// DB-U5: numeric hosts collapse to `Host::Ipv4`; a zone-id IPv6 is an error.
#[test]
fn db_u5_numeric_hosts_become_ipv4_and_zone_ids_are_errors() {
    for input in [
        "https://2130706433/",
        "https://0x7f.1/",
        "https://0177.0.0.1/",
        "https://127.1/",
    ] {
        assert!(
            matches!(parse(input).host(), Some(Host::Ipv4(_))),
            "{input:?} should be Host::Ipv4"
        );
    }
    assert!(Url::parse("https://[fe80::1%25eth0]/").is_err());
}

/// DB-U6: empty and default ports read as "no port"; an out-of-range or
/// non-numeric port is an error.
#[test]
fn db_u6_port_handling() {
    for input in [
        "https://s3.example.com:/x",
        "https://s3.example.com:00443/x",
        "https://s3.example.com:443/x",
    ] {
        assert_eq!(parse(input).port(), None, "{input:?}");
    }
    assert_eq!(parse("https://s3.example.com:8443/x").port(), Some(8443));
    for input in [
        "https://s3.example.com:65536/x",
        "https://s3.example.com:abc/x",
    ] {
        assert!(Url::parse(input).is_err(), "{input:?} should be an error");
    }
}

/// DB-U7: dot-segments and `\` are normalised in the parsed path; `%2f` is
/// kept; a URL with no path gets `/`. Because the parsed path hides all of
/// this, the validators must judge the *raw* path.
#[test]
fn db_u7_path_normalisation() {
    assert_eq!(parse("https://s3.example.com/bucket/../x").path(), "/x");
    assert_eq!(parse("https://s3.example.com/bucket/%2e%2e/x").path(), "/x");
    assert_eq!(parse("https://s3.example.com/a%2fb").path(), "/a%2fb");
    assert_eq!(parse("https://s3.example.com/a\\b").path(), "/a/b");
    assert_eq!(parse("https://s3.example.com").path(), "/");
}

/// DB-U8: serialisation normalises scheme/host case, drops the default port,
/// re-encodes the query, and is idempotent.
#[test]
fn db_u8_reserialisation_is_normalised_and_idempotent() {
    let u = parse("HTTPS://S3.EXAMPLE.COM:443/Bucket/a%20b?q='x'");
    assert_eq!(u.as_str(), "https://s3.example.com/Bucket/a%20b?q=%27x%27");

    for input in [
        "HTTPS://S3.EXAMPLE.COM:443/Bucket/a%20b?q='x'",
        "https://s3.example.com/bucket/a%20b%E3%81%82.png",
        "https://s3.example.com/bucket/dir/a.png?x=%2F..%2F",
        "https://s3.example.com:9000/other/a.png",
        "https://misskey.example.com/files/0f9d7c1e-8a52-4d3a-9b6f-1f2e3d4c5b6a",
        "https://r2.example.net/a.png",
        "https://s3.example.com/bucket/a?x=1",
    ] {
        let u = parse(input);
        assert_eq!(
            parse(u.as_str()).as_str(),
            u.as_str(),
            "{input:?} must reserialise idempotently"
        );
    }
}

// ---------------------------------------------------------------------------
// DB-F1: `url::form_urlencoded`
// ---------------------------------------------------------------------------

/// DB-F1: `+` is a space, keys are percent-decoded too, invalid UTF-8 becomes
/// U+FFFD, and a bare key has an empty value.
#[test]
fn db_f1_form_urlencoded_parsing() {
    let pairs: Vec<(String, String)> = form_urlencoded::parse(b"url=a+b%26c&url=d")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("url".to_string(), "a b&c".to_string()),
            ("url".to_string(), "d".to_string())
        ]
    );

    let pairs: Vec<(String, String)> = form_urlencoded::parse(b"u%72l=x")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs, vec![("url".to_string(), "x".to_string())]);

    let (_, value) = form_urlencoded::parse(b"url=%ff").next().unwrap();
    assert!(value.contains('\u{FFFD}'), "{value:?}");

    let pairs: Vec<(String, String)> = form_urlencoded::parse(b"url")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs, vec![("url".to_string(), String::new())]);
}
