// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Verifies the allowlist, the `Accept` gate on the three dual-purpose
//! paths, and the internal-`Referer` media redirect — all without a real
//! Misskey instance. A tiny mock backend on a Unix socket stands in for
//! Misskey and echoes the request path back in `x-mock-path`, so tests can
//! confirm both that a request was let through *and* that it reached the
//! backend with the exact path/query it arrived with.
//!
//! This is deliberately not where federation correctness is proven — that
//! requires two real Misskey instances actually speaking ActivityPub
//! through this proxy, which lives in `tests/federation/`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tower::ServiceExt;

use misskey_egress_proxy::config::Config;
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

const INTERNAL_BASE_URL: &str = "https://internal.example.ts.net";
const INTERNAL_SUFFIX: &str = ".internal.example.ts.net";
const AP_ACCEPT: (&str, &str) = ("accept", "application/activity+json");

/// Spawns a fake Misskey backend on a Unix socket that answers every
/// request with 200 and echoes the request path+query into `x-mock-path`.
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
    (StatusCode::OK, [("x-mock-path", path)], "mock-backend").into_response()
}

fn unique_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

async fn build_app() -> Router {
    let socket = spawn_mock_backend().await;
    let config = Arc::new(Config {
        listen_addr: "127.0.0.1:0".to_string(),
        misskey_socket: socket.clone(),
        internal_base_url: INTERNAL_BASE_URL.to_string(),
        internal_referer_suffix: INTERNAL_SUFFIX.to_string(),
    });
    let proxy_state = ProxyState {
        client: build_client(),
        socket,
    };
    routes::build(proxy_state, config)
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
        "/",
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
async fn dual_purpose_paths_require_explicit_ap_accept() {
    let app = build_app().await;

    for path in ["/notes/abc", "/users/abc", "/@alice"] {
        assert_eq!(
            get(&app, path, &[]).await.status(),
            StatusCode::NOT_ACCEPTABLE,
            "{path} with no Accept must 406"
        );
        assert_eq!(
            get(&app, path, &[("accept", "text/html")]).await.status(),
            StatusCode::NOT_ACCEPTABLE,
            "{path} with html-only Accept must 406"
        );
        assert_eq!(
            get(&app, path, &[AP_ACCEPT]).await.status(),
            StatusCode::OK,
            "{path} with AP Accept must be forwarded"
        );
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
