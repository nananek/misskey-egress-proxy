// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Verifies the allowlist, the `Accept` gate on the three dual-purpose
//! paths, and the internal-`Referer` media redirect — all without a real
//! Misskey instance. A tiny mock backend on a Unix socket stands in for
//! Misskey and echoes the request path back in `x-mock-path` (and the
//! `Accept` it was asked with in `x-mock-accept`), so tests can confirm both
//! that a request was let through *and* what the backend was asked for.
//!
//! This is deliberately not where federation correctness is proven — that
//! requires two real Misskey instances actually speaking ActivityPub
//! through this proxy, which lives in `tests/federation/`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tower::ServiceExt;

use misskey_egress_proxy::config::{Config, ListenTarget};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

const INTERNAL_BASE_URL: &str = "https://internal.example.ts.net";
const INTERNAL_SUFFIX: &str = ".internal.example.ts.net";
const AP_ACCEPT: (&str, &str) = ("accept", "application/activity+json");

/// Spawns a fake Misskey backend on a Unix socket that answers every
/// request with 200 and echoes the request path+query into `x-mock-path`
/// and the request's `Accept` into `x-mock-accept`.
async fn spawn_mock_backend() -> PathBuf {
    let socket = std::env::temp_dir().join(format!("mep-test-{}.sock", unique_suffix()));
    let _ = std::fs::remove_file(&socket);

    let app = Router::new().fallback(any(echo_path));
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind mock backend socket");

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock backend crashed");
    });

    // Give the listener a moment to actually be ready before tests dial it.
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
    let accept = req
        .headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (
        StatusCode::OK,
        [("x-mock-path", path), ("x-mock-accept", accept)],
        "mock-backend",
    )
        .into_response()
}

/// Tests run in parallel, and two of them can read the same nanosecond off
/// the clock, so the timestamp alone does not make the path unique.
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

async fn build_app_with_static_dir(static_dir: PathBuf) -> Router {
    let socket = spawn_mock_backend().await;
    let config = Arc::new(Config {
        listen: ListenTarget::Tcp("127.0.0.1:0".to_string()),
        misskey_socket: socket.clone(),
        internal_base_url: INTERNAL_BASE_URL.to_string(),
        internal_referer_suffix: INTERNAL_SUFFIX.to_string(),
        static_dir,
    });
    let proxy_state = ProxyState {
        client: build_client(),
        socket,
    };
    routes::build(proxy_state, config)
}

async fn build_app() -> Router {
    build_app_with_static_dir(PathBuf::from("/path/that/does/not/exist")).await
}

async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut builder = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Body::empty()).unwrap();
    app.clone().oneshot(req).await.unwrap()
}

async fn get(app: &Router, path: &str, headers: &[(&str, &str)]) -> Response {
    call(app, "GET", path, headers).await
}

#[tokio::test]
async fn allowlisted_ap_paths_are_forwarded() {
    let app = build_app().await;

    for path in [
        "/.well-known/webfinger?resource=acct:a@b",
        "/.well-known/nodeinfo",
        "/.well-known/host-meta",
        "/.well-known/host-meta.json",
        "/nodeinfo/2.0",
        "/nodeinfo/2.1",
        "/notes/abc/activity",
        "/users/abc/outbox",
        "/users/abc/followers",
        "/users/abc/following",
        "/users/abc/collections/featured",
        "/users/abc/publickey",
        "/emojis/blobcat",
        "/likes/abc",
        "/follows/a",
        "/follows/a/b",
        "/identicon/alice@example.com",
    ] {
        let resp = get(&app, path, &[]).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "path {path} should be forwarded"
        );
    }
}

#[tokio::test]
async fn inbox_only_accepts_post() {
    let app = build_app().await;

    assert_eq!(
        call(&app, "POST", "/inbox", &[]).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", "/users/abc/inbox", &[]).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&app, "/inbox", &[]).await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn client_and_admin_surface_is_not_reachable() {
    let app = build_app().await;

    for path in [
        "/api/meta",
        "/api/v1/instance/peers",
        "/streaming",
        "/oauth/token",
        "/healthz",
        "/.well-known/oauth-authorization-server",
        "/.well-known/change-password",
        "/url?url=https://example.com",
        "/manifest.json",
        "/robots.txt",
        "/settings",
        "/emoji/blobcat.webp",
        "/avatar/@alice",
    ] {
        let resp = get(&app, path, &[]).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "path {path} must not be publicly reachable"
        );
    }
}

#[tokio::test]
async fn root_serves_the_bundled_landing_page() {
    let app = build_app().await;

    let resp = get(&app, "/", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/html; charset=utf-8"
    );
    assert!(resp.headers().contains_key("content-security-policy"));
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("misskey-egress-proxy"),
        "the root should contain the project landing page"
    );

    let logo = get(&app, "/assets/misskey.svg", &[]).await;
    assert_eq!(logo.status(), StatusCode::OK);
    assert_eq!(
        logo.headers().get("content-type").unwrap(),
        "image/svg+xml; charset=utf-8"
    );
    let csp = logo
        .headers()
        .get("content-security-policy")
        .expect("the SVG must carry a CSP too: an SVG opened directly is an active document")
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(csp.contains("sandbox"), "{csp}");
}

#[tokio::test]
async fn mounted_static_files_override_the_bundled_page() {
    let static_dir = std::env::temp_dir().join(format!("mep-static-{}", unique_suffix()));
    std::fs::create_dir(&static_dir).unwrap();
    std::fs::write(static_dir.join("index.html"), "<h1>mounted page</h1>").unwrap();
    std::fs::write(static_dir.join("misskey.svg"), "<svg>mounted</svg>").unwrap();

    let app = build_app_with_static_dir(static_dir.clone()).await;
    let resp = get(&app, "/", &[]).await;
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"<h1>mounted page</h1>");

    let logo = get(&app, "/assets/misskey.svg", &[]).await;
    let body = to_bytes(logo.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"<svg>mounted</svg>");

    std::fs::remove_dir_all(static_dir).unwrap();
}

#[tokio::test]
async fn dual_purpose_paths_always_ask_misskey_for_ap_json() {
    let app = build_app().await;

    for path in ["/notes/abc", "/users/abc", "/@alice"] {
        for accept in [
            None,
            Some("text/html"),
            Some("*/*"),
            Some("text/html,application/xhtml+xml"),
            Some("application/activity+json"),
        ] {
            let headers: Vec<(&str, &str)> =
                accept.map(|a| vec![("accept", a)]).unwrap_or_default();
            let resp = get(&app, path, &headers).await;

            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "{path} with Accept {accept:?} must be forwarded, not refused"
            );
            assert_eq!(
                resp.headers().get("x-mock-accept").unwrap(),
                "application/activity+json",
                "{path} with Accept {accept:?} must reach Misskey asking for AP JSON"
            );
        }
    }
}

#[tokio::test]
async fn acct_capture_preserves_full_segment() {
    let app = build_app().await;
    let resp = get(&app, "/@alice@remote.example", &[AP_ACCEPT]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let path = resp.headers().get("x-mock-path").unwrap().to_str().unwrap();
    assert_eq!(path, "/@alice@remote.example");
}

#[tokio::test]
async fn media_paths_are_forwarded_for_external_callers() {
    let app = build_app().await;

    for path in ["/files/abc123", "/files/app-default.jpg", "/proxy/somekey"] {
        let resp = get(&app, path, &[]).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{path} should be proxied for external callers"
        );
    }
}

#[tokio::test]
async fn media_paths_redirect_internal_referer_to_internal_host() {
    let app = build_app().await;

    let resp = get(
        &app,
        "/files/abc123",
        &[(
            "referer",
            "https://misskey.internal.example.ts.net/notes/xyz",
        )],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("{INTERNAL_BASE_URL}/files/abc123"));
}

#[tokio::test]
async fn media_paths_ignore_non_internal_referer() {
    let app = build_app().await;

    let resp = get(
        &app,
        "/files/abc123",
        &[("referer", "https://evil.example.com/")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn client_feed_twins_of_the_acct_path_are_not_reachable() {
    let app = build_app().await;

    // `/@:user.rss`, `/@:user.atom`, and `/@:user.json` are client-only
    // feeds in Misskey, and find-my-way routes `*.rss`-suffixed segments to
    // them no matter what `Accept` says. `/@{acct}` would swallow them, so
    // they must be rejected instead of forwarded.
    for path in [
        "/@alice.rss",
        "/@alice.atom",
        "/@alice.json",
        "/@alice%2erss",
        "/@alice%2eatom",
        "/@alice%2ejson",
        "/@%61lice%2ejson",
    ] {
        assert_eq!(
            get(&app, path, &[AP_ACCEPT]).await.status(),
            StatusCode::NOT_FOUND,
            "{path} must be rejected, not forwarded"
        );
    }

    // The AP acct route itself still goes through: plain handles, remote
    // accts, non-feed dotted handles, and escaped reserved characters that
    // find-my-way never decodes into path structure.
    for path in [
        "/@alice",
        "/@alice@remote.example",
        "/@alice.RSS",
        "/@alice%2Frss",
    ] {
        assert_eq!(
            get(&app, path, &[AP_ACCEPT]).await.status(),
            StatusCode::OK,
            "{path} must still be forwarded"
        );
    }
}

#[tokio::test]
async fn internal_referer_does_not_redirect_non_media_paths() {
    let app = build_app().await;

    // The media redirect is a bandwidth optimization for four media routes;
    // it must not leak onto the 404 fallback, where a spoofed internal
    // Referer would otherwise turn any unknown path into a 302 to the
    // internal host (and disclose it) instead of a 404.
    let resp = get(
        &app,
        "/api/meta",
        &[("referer", "https://caller.internal.example.ts.net/")],
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the media redirect must not apply to the fallback"
    );
}
