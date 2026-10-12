// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Pins what the relay in `src/proxy.rs` does to the header block, in both
//! directions: the allowlist decides *which* requests reach Misskey, and
//! `src/headers.rs` decides what of such a request's headers (and what of
//! Misskey's response headers) get through.
//!
//! These run in-process against a mock Misskey UDS that echoes the names of
//! the headers it received, so "Misskey never saw it" is asserted directly
//! rather than inferred. The end-to-end path, where a real HTTP/1.1 request is
//! parsed by the server before `forward` ever sees it, is covered in
//! `tests/unix_listen.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http_body_util::BodyExt;
use tower::ServiceExt;

use misskey_egress_proxy::config::{Config, ListenTarget, MediaMode};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

const INTERNAL_BASE_URL: &str = "https://internal.example.ts.net";
const INTERNAL_SUFFIX: &str = ".internal.example.ts.net";

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

/// A stand-in Misskey that reports the sorted, lowercased names of the headers
/// it received in `x-mock-header-names` and the body length in
/// `x-mock-body-len`, and answers with connection-nominated headers of its own
/// so the response direction can be checked the same way.
async fn echo_headers(req: Request<Body>) -> Response {
    let mut names: Vec<String> = req
        .headers()
        .keys()
        .map(|name| name.as_str().to_string())
        .collect();
    names.sort();
    let names = HeaderValue::from_str(&names.join(",")).unwrap();

    let body_len = req
        .into_body()
        .collect()
        .await
        .map(|bytes| bytes.to_bytes().len())
        .unwrap_or(0);

    let mut response = (StatusCode::OK, "mock").into_response();
    let headers = response.headers_mut();
    headers.insert("x-mock-header-names", names);
    headers.insert("x-mock-body-len", HeaderValue::from(body_len as u64));
    headers.insert("x-mock-kept", HeaderValue::from_static("yes"));
    headers.insert(
        header::CONNECTION,
        HeaderValue::from_static("keep-alive, x-mock-hop"),
    );
    headers.insert("x-mock-hop", HeaderValue::from_static("nominee"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=9"));
    response
}

async fn build_app() -> Router {
    let socket = std::env::temp_dir().join(format!("mep-headers-{}.sock", unique_suffix()));
    let _ = std::fs::remove_file(&socket);
    let backend = Router::new().fallback(any(echo_headers));
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind mock socket");
    tokio::spawn(async move {
        axum::serve(listener, backend)
            .await
            .expect("mock backend crashed");
    });

    let config = Arc::new(Config {
        listen: ListenTarget::Tcp("127.0.0.1:0".to_string()),
        misskey_socket: socket.clone(),
        internal_base_url: INTERNAL_BASE_URL.to_string(),
        internal_referer_suffix: INTERNAL_SUFFIX.to_string(),
        static_dir: PathBuf::from("/path/that/does/not/exist"),
        media_mode: MediaMode::Proxy,
        media_allowed_prefixes: vec![],
    });
    routes::build(
        ProxyState {
            client: build_client(),
            socket,
        },
        config,
    )
}

async fn call(app: &Router, headers: &[(&str, &str)], body: &'static str) -> Response {
    let mut builder = Request::builder().method("POST").uri("/inbox");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.clone()
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

/// The header names the mock Misskey saw, as a `Vec`.
fn seen(resp: &Response) -> Vec<String> {
    resp.headers()
        .get("x-mock-header-names")
        .expect("response should come from the mock backend")
        .to_str()
        .unwrap()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Hop-by-hop headers describe the caller's connection, not the request, and a
/// proxy must not pass them on: a stale `Transfer-Encoding`, `Upgrade` or
/// `Connection` at the next hop is a framing / protocol desync vector.
#[tokio::test]
async fn hop_by_hop_request_headers_never_reach_misskey() {
    let app = build_app().await;

    let resp = call(
        &app,
        &[
            ("connection", "keep-alive, x-custom-hop"),
            ("x-custom-hop", "named by Connection"),
            ("keep-alive", "timeout=5"),
            ("te", "trailers"),
            ("trailer", "Expires"),
            ("upgrade", "h2c"),
            ("proxy-authenticate", "Basic zzz"),
            ("proxy-authorization", "Basic zzz"),
            ("proxy-connection", "keep-alive"),
        ],
        "",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let names = seen(&resp);
    for header in [
        "connection",
        "x-custom-hop",
        "keep-alive",
        "te",
        "trailer",
        "upgrade",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
    ] {
        assert!(
            !names.iter().any(|name| name == header),
            "Misskey saw hop-by-hop header {header:?}: {names:?}"
        );
    }
}

/// The proxy cannot tell a forwarding header the terminator set from one the
/// caller spoofed, and no reachable route needs any of them, so all of them
/// are dropped: a caller must not be able to pick `request.ip` (Misskey's
/// `trustProxy`) or the protocol/host it believes it is serving.
#[tokio::test]
async fn forwarding_headers_never_reach_misskey() {
    let app = build_app().await;

    let resp = call(
        &app,
        &[
            ("forwarded", "for=1.2.3.4"),
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-port", "8443"),
            ("x-real-ip", "1.2.3.4"),
            ("x-client-ip", "1.2.3.4"),
            ("cf-connecting-ip", "1.2.3.4"),
            ("true-client-ip", "1.2.3.4"),
        ],
        "",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let names = seen(&resp);
    for header in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-port",
        "x-real-ip",
        "x-client-ip",
        "cf-connecting-ip",
        "true-client-ip",
    ] {
        assert!(
            !names.iter().any(|name| name == header),
            "Misskey saw caller-asserted header {header:?}: {names:?}"
        );
    }
}

/// An overriding header a front-end might honour is removed too, so a caller
/// cannot try to turn an allowed target into a different method or URL.
#[tokio::test]
async fn routing_override_headers_never_reach_misskey() {
    let app = build_app().await;

    let resp = call(
        &app,
        &[
            ("x-http-method-override", "GET"),
            ("x-original-url", "/api/admin"),
            ("x-rewrite-url", "/api/admin"),
        ],
        "",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let names = seen(&resp);
    for header in ["x-http-method-override", "x-original-url", "x-rewrite-url"] {
        assert!(
            !names.iter().any(|name| name == header),
            "Misskey saw override header {header:?}: {names:?}"
        );
    }
}

/// Everything an HTTP signature may cover, and the rest of a federation
/// request, reaches Misskey unchanged.
#[tokio::test]
async fn a_federation_request_reaches_misskey_intact() {
    let app = build_app().await;

    let resp = call(
        &app,
        &[
            ("host", "misskey.example.com"),
            ("date", "Mon, 12 Oct 2026 00:00:00 GMT"),
            ("digest", "SHA-256=abc"),
            ("content-type", "application/activity+json"),
            ("signature", "keyId=\"k\""),
            ("authorization", "Signature keyId=\"k\""),
            ("accept", "application/activity+json"),
            ("user-agent", "Mastodon/4.0"),
        ],
        "{}",
    )
    .await;

    assert_eq!(resp.status(), StatusCode::OK);
    let names = seen(&resp);
    for header in [
        "host",
        "date",
        "digest",
        "content-type",
        "signature",
        "authorization",
        "accept",
        "user-agent",
    ] {
        assert!(
            names.iter().any(|name| name == header),
            "Misskey did not see {header:?}: {names:?}"
        );
    }
    assert_eq!(
        resp.headers().get("x-mock-body-len").unwrap(),
        "2",
        "the raw body must reach Misskey untouched"
    );
}

/// Removing `Transfer-Encoding` must not cost the body: hyper re-frames the
/// stream, and (a fresh body having a known length here) sends it with a
/// length of its own.
#[tokio::test]
async fn a_chunked_body_still_reaches_misskey_after_transfer_encoding_is_removed() {
    let app = build_app().await;

    let resp = call(&app, &[("transfer-encoding", "chunked")], "hello world").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let names = seen(&resp);
    assert!(
        !names.iter().any(|name| name == "transfer-encoding"),
        "the stale Transfer-Encoding must not be forwarded: {names:?}"
    );
    assert_eq!(
        resp.headers().get("x-mock-body-len").unwrap(),
        "11",
        "the body must survive the re-framing"
    );
}

/// The response direction: Misskey's own connection headers (and the ones its
/// `Connection` nominates) must not be passed on to the caller.
#[tokio::test]
async fn response_hop_by_hop_headers_are_not_handed_to_the_caller() {
    let app = build_app().await;

    let resp = call(&app, &[], "x").await;

    assert_eq!(resp.status(), StatusCode::OK);
    for header in ["connection", "keep-alive"] {
        assert!(
            !resp.headers().contains_key(header),
            "the caller was handed {header:?}"
        );
    }
    assert!(
        !resp.headers().contains_key("x-mock-hop"),
        "the caller was handed a header Misskey's Connection nominated"
    );
    // Misskey's ordinary response headers are left alone.
    assert_eq!(resp.headers().get("x-mock-kept").unwrap(), "yes");
    let names = seen(&resp);
    assert!(
        names.iter().any(|name| name == "content-length"),
        "what the relay sent carried a length: {names:?}"
    );
}
