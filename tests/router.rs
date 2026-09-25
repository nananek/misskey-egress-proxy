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

use misskey_egress_proxy::config::{Config, ListenTarget, MediaMode};
use misskey_egress_proxy::media_target::{AllowedPrefix, parse_allowed_prefixes};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

// The rows shared with the unit tests in `src/media_target.rs`.
include!("common/media_corpus.rs");

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

fn build_app_with(
    media_mode: MediaMode,
    allowed: Vec<AllowedPrefix>,
    socket: PathBuf,
    static_dir: PathBuf,
) -> Router {
    let config = Arc::new(Config {
        listen: ListenTarget::Tcp("127.0.0.1:0".to_string()),
        misskey_socket: socket.clone(),
        internal_base_url: INTERNAL_BASE_URL.to_string(),
        internal_referer_suffix: INTERNAL_SUFFIX.to_string(),
        static_dir,
        media_mode,
        media_allowed_prefixes: allowed,
    });
    let proxy_state = ProxyState {
        client: build_client(),
        socket,
    };
    routes::build(proxy_state, config)
}

async fn build_app_with_static_dir(static_dir: PathBuf) -> Router {
    build_app_with(
        MediaMode::Proxy,
        vec![],
        spawn_mock_backend().await,
        static_dir,
    )
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

    // And the rejection applies to every method, not just GET: installing
    // the draining 405 handler after the feed layer used to replace the
    // layer-wrapped fallback, so a POST to a feed twin answered 405 while a
    // GET answered 404.
    assert_eq!(
        call(&app, "POST", "/@alice.rss", &[AP_ACCEPT])
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "a feed twin must be 404 for POST as well"
    );
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

// ---------------------------------------------------------------------------
// MEDIA_MODE=redirect
// ---------------------------------------------------------------------------
//
// Most of these build the app on a socket nothing listens on. `forward` turns
// an unreachable upstream into a 502, so a 302 or a 404 from such an app is
// proof that the request was answered locally and never forwarded.

const INTERNAL_REFERER: (&str, &str) = ("referer", "https://misskey.internal.example.ts.net/");
const EXTERNAL_REFERER: (&str, &str) = ("referer", "https://evil.example.com/");
const NO_SUCH_DIR: &str = "/path/that/does/not/exist";

fn allowed_set() -> Vec<AllowedPrefix> {
    parse_allowed_prefixes(Some(T_SET)).unwrap()
}

fn dead_socket() -> PathBuf {
    std::env::temp_dir().join(format!("mep-dead-{}.sock", unique_suffix()))
}

fn redirect_app(allowed: Vec<AllowedPrefix>) -> Router {
    build_app_with(
        MediaMode::Redirect,
        allowed,
        dead_socket(),
        PathBuf::from(NO_SUCH_DIR),
    )
}

async fn live_redirect_app(allowed: Vec<AllowedPrefix>) -> Router {
    build_app_with(
        MediaMode::Redirect,
        allowed,
        spawn_mock_backend().await,
        PathBuf::from(NO_SUCH_DIR),
    )
}

fn location(resp: &Response) -> Option<String> {
    resp.headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
}

fn mock_path(resp: &Response) -> String {
    resp.headers()
        .get("x-mock-path")
        .expect("response should come from the mock backend")
        .to_str()
        .unwrap()
        .to_string()
}

/// Every row of the shared corpus as a request-target with its expectation.
fn corpus_targets() -> Vec<(String, &'static str)> {
    VALUE_CASES
        .iter()
        .map(|(value, expected)| (proxy_target(value), *expected))
        .chain(
            RAW_CASES
                .iter()
                .map(|(target, expected)| (target.to_string(), *expected)),
        )
        .collect()
}

/// R1: an internal Referer is redirected to the internal host in redirect
/// mode too, whatever the target would otherwise have been, so the operator's
/// own UI keeps getting its remote media (D5).
#[tokio::test]
async fn redirect_mode_still_sends_an_internal_referer_to_the_internal_host() {
    let app = redirect_app(allowed_set());

    for target in [
        "/files/abc123",
        "/files/app-default.jpg",
        "/proxy/image.webp?url=https%3A%2F%2Fother.example.org%2Fa.png",
    ] {
        for method in ["GET", "HEAD"] {
            let resp = call(&app, method, target, &[INTERNAL_REFERER]).await;
            assert_eq!(resp.status(), StatusCode::FOUND, "{method} {target}");
            assert_eq!(
                location(&resp),
                Some(format!("{INTERNAL_BASE_URL}{target}")),
                "{method} {target}"
            );
        }
    }
}

/// R2 (D13): without an internal Referer `/files/*` is a 404, not a 302 to
/// the internal host and not a forward.
#[tokio::test]
async fn redirect_mode_answers_files_without_an_internal_referer_with_404() {
    let app = redirect_app(allowed_set());

    for target in [
        "/files/abc123",
        "/files/app-default.jpg",
        "/files/abc/x.webp",
        // A `url` parameter does not turn a `/files/` request into a `/proxy`
        // one: only the path decides.
        "/files/abc123?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fabc.png",
        "/files/abc/x.webp?url=https%3A%2F%2Fr2.example.net%2Fa.png",
    ] {
        for headers in [
            &[][..],
            &[EXTERNAL_REFERER][..],
            &[("referer", "not a url")][..],
        ] {
            let resp = get(&app, target, headers).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{target} {headers:?}");
            assert_eq!(location(&resp), None, "{target} {headers:?}");
        }
    }
}

/// R3: `/proxy/*?url=` goes to the original URL; the Misskey-side options
/// (`static`, `avatar`, ...) are dropped, not applied.
#[tokio::test]
async fn redirect_mode_sends_proxy_requests_to_the_original_url() {
    let app = redirect_app(allowed_set());

    for target in [
        "/proxy/image.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fabc.png&static=1",
        "/proxy/avatar.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fabc.png&avatar=1",
    ] {
        for method in ["GET", "HEAD"] {
            let resp = call(&app, method, target, &[]).await;
            assert_eq!(resp.status(), StatusCode::FOUND, "{method} {target}");
            assert_eq!(
                location(&resp).as_deref(),
                Some("https://s3.example.com/bucket/abc.png"),
                "{method} {target}"
            );
        }
    }
}

/// R4 (D12): an original URL on our own domain's `/files/` is redirected to.
/// Asking *this* proxy for that `/files/` URL is a 404, which is right: in
/// the deployment this mode is written for, the edge answers `/files` before
/// it can reach the proxy, so the second hop never lands here.
#[tokio::test]
async fn redirect_mode_hands_our_own_files_url_to_the_edge_rather_than_looping() {
    let app = redirect_app(allowed_set());

    let resp = get(
        &app,
        "/proxy/avatar.webp?url=https%3A%2F%2Fmisskey.example.com%2Ffiles%2Fk1&avatar=1",
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        location(&resp).as_deref(),
        Some("https://misskey.example.com/files/k1")
    );

    let second_hop = get(&app, "/files/k1", &[]).await;
    assert_eq!(second_hop.status(), StatusCode::NOT_FOUND);
}

/// R5: the internal Referer wins over the allowlist, and an external Referer
/// (suffix mismatch) changes nothing; a target that fails the internal check
/// is a 404 (D7).
#[tokio::test]
async fn redirect_mode_gives_the_internal_referer_priority_over_the_allowlist() {
    let app = redirect_app(allowed_set());
    let allowed_url = "/proxy/i.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png";
    let refused_url = "/proxy/i.webp?url=https%3A%2F%2Fevil.example%2Fa.png";

    let resp = get(&app, allowed_url, &[INTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        location(&resp),
        Some(format!("{INTERNAL_BASE_URL}{allowed_url}"))
    );

    let resp = get(&app, allowed_url, &[EXTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        location(&resp).as_deref(),
        Some("https://s3.example.com/bucket/a.png")
    );

    let resp = get(&app, refused_url, &[EXTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&resp), None);

    let resp = get(&app, "/files/../x", &[INTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&resp), None);
}

/// R6: every corpus row the rules refuse is a 404 with no `Location`: never
/// a 502, so never forwarded, and never a redirect to somewhere unintended.
#[tokio::test]
async fn redirect_mode_refuses_every_corpus_row_the_rules_refuse() {
    let app = redirect_app(allowed_set());
    let mut refused = 0;

    for (target, expected) in corpus_targets() {
        if expected.starts_with("ok:") {
            continue;
        }
        for headers in [&[][..], &[EXTERNAL_REFERER][..]] {
            let resp = get(&app, &target, headers).await;
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{target} ({expected}) {headers:?}"
            );
            assert_eq!(location(&resp), None, "{target}");
        }
        refused += 1;
    }
    assert!(refused >= 100, "the corpus lost rows: {refused} refused");
}

/// R7: the invariant, over the whole corpus. Either the request is refused,
/// or the `Location` is exactly the normalised URL the rules promise, and
/// that URL is `https`, on an allowed host and path, stable under
/// re-parsing, and read as the same host by an RFC 3986 splitter and by the
/// `url` crate. Nothing else may come out.
#[tokio::test]
async fn redirect_mode_only_ever_emits_a_normalised_allowed_location() {
    let app = redirect_app(allowed_set());
    let mut redirected = 0;

    for (target, expected) in corpus_targets() {
        let resp = get(&app, &target, &[]).await;
        match expected.strip_prefix("ok:") {
            Some(want) => {
                assert_eq!(resp.status(), StatusCode::FOUND, "{target}");
                let got = location(&resp).expect("a Location");
                assert_eq!(got, want, "{target}");
                assert_location_invariants(&got);

                let head = call(&app, "HEAD", &target, &[]).await;
                assert_eq!(head.status(), StatusCode::FOUND, "HEAD {target}");
                assert_eq!(location(&head).as_deref(), Some(want), "HEAD {target}");
                redirected += 1;
            }
            None => {
                assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{target}");
                assert_eq!(location(&resp), None, "{target}");
            }
        }
    }
    assert!(
        redirected >= 15,
        "the corpus lost rows: {redirected} accepted"
    );
}

/// The invariant also holds for a hostile allowlist-free input the corpus
/// does not list: a value that is refused must not leak into a `Location`.
#[tokio::test]
async fn redirect_mode_never_reflects_a_refused_url() {
    let app = redirect_app(allowed_set());

    let resp = get(
        &app,
        "/proxy/i?url=https%3A%2F%2Fevil.example%2Fsecret-token-123",
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&resp), None);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty(), "a refusal must not echo the input");
}

/// R8: with no allowed prefix nothing is redirected to an original URL, and
/// an internal Referer still gets its redirect.
#[tokio::test]
async fn redirect_mode_with_an_empty_allowlist_refuses_every_original_url() {
    let app = redirect_app(vec![]);

    for (target, expected) in corpus_targets() {
        if !expected.starts_with("ok:") {
            continue;
        }
        let resp = get(&app, &target, &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{target}");
        assert_eq!(location(&resp), None, "{target}");

        let internal = get(&app, &target, &[INTERNAL_REFERER]).await;
        assert_eq!(internal.status(), StatusCode::FOUND, "{target}");
        assert_eq!(
            location(&internal),
            Some(format!("{INTERNAL_BASE_URL}{target}")),
            "{target}"
        );
    }
}

/// R9: other methods stay a plain 405, with no `Location`, referer or not.
#[tokio::test]
async fn redirect_mode_keeps_405_for_other_methods() {
    let app = redirect_app(allowed_set());

    for target in [
        "/files/abc",
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
    ] {
        for method in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
            for headers in [&[][..], &[INTERNAL_REFERER][..]] {
                let resp = call(&app, method, target, headers).await;
                assert_eq!(
                    resp.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {target} {headers:?}"
                );
                assert_eq!(location(&resp), None, "{method} {target}");
            }
        }
    }
}

/// R10: everything that is not media behaves as before in redirect mode.
#[tokio::test]
async fn redirect_mode_leaves_non_media_paths_alone() {
    let app = live_redirect_app(allowed_set()).await;

    let resp = get(&app, "/nodeinfo/2.1", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_path(&resp), "/nodeinfo/2.1");

    let resp = get(&app, "/@alice", &[AP_ACCEPT]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_path(&resp), "/@alice");

    let resp = call(&app, "POST", "/inbox", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(mock_path(&resp), "/inbox");

    // And the fallback still does not turn an internal Referer into a 302.
    let resp = get(&app, "/api/meta", &[INTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&resp), None);
}

/// R11: the internal redirect's target is checked, in both modes: a path that
/// could resolve differently on the internal host is a 404, a clean one is a
/// 302 to exactly `INTERNAL_BASE_URL` plus itself.
#[tokio::test]
async fn an_internal_redirect_is_only_sent_for_a_clean_target() {
    let long_ok = format!("/files/{}", "a".repeat(8192 - "/files/".len()));
    let too_long = format!("{long_ok}a");

    for mode in [MediaMode::Proxy, MediaMode::Redirect] {
        let app = build_app_with(mode, vec![], dead_socket(), PathBuf::from(NO_SUCH_DIR));

        for target in [
            "/files/abc123",
            "/files/abc/x.webp",
            "/files/x%23y",
            "/files/a%20b%E3%81%82.png",
            "/proxy/image.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
            long_ok.as_str(),
        ] {
            let resp = get(&app, target, &[INTERNAL_REFERER]).await;
            assert_eq!(resp.status(), StatusCode::FOUND, "{mode:?} {target}");
            assert_eq!(
                location(&resp),
                Some(format!("{INTERNAL_BASE_URL}{target}")),
                "{mode:?} {target}"
            );
        }

        // A fragment is not part of the target, an encoded `#` is.
        let resp = get(&app, "/files/x#frag", &[INTERNAL_REFERER]).await;
        assert_eq!(
            location(&resp),
            Some(format!("{INTERNAL_BASE_URL}/files/x")),
            "{mode:?}"
        );

        // The authority and `Host` of an absolute-form request are ignored.
        let resp = get(
            &app,
            "http://evil.example/files/x",
            &[("host", "evil.example"), INTERNAL_REFERER],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FOUND, "{mode:?}");
        assert_eq!(
            location(&resp),
            Some(format!("{INTERNAL_BASE_URL}/files/x")),
            "{mode:?}"
        );

        for target in [
            "/files/a\\b",
            "/files/../x",
            "/files/./x",
            "/files/%2e%2e/x",
            "/files/a%2fb",
            "/files/a%5cb",
            "/files//x",
            "/files/x/",
            "/files/%252e%252e/x",
            "/files/a%zz",
            "/proxy/%2e%2e/x",
            "/files/\u{3042}",
            "/files/a\"b",
            "/files/{x}",
            too_long.as_str(),
            // Not media routes at all.
            "/files",
            "/files/",
            "/proxy",
            "/proxy/",
            "/\\evil.example/x",
            "//evil.example/x",
        ] {
            let resp = get(&app, target, &[INTERNAL_REFERER]).await;
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "{mode:?} {:?}",
                &target[..target.len().min(60)]
            );
            assert_eq!(location(&resp), None, "{mode:?} {target}");
        }
    }
}

/// R12 (D7's boundary): the default mode still relays media verbatim,
/// whatever it looks like, and an allowlist has no effect there. The one
/// difference is a malformed target with an internal Referer.
#[tokio::test]
async fn proxy_mode_still_relays_media_verbatim() {
    let app = build_app_with(
        MediaMode::Proxy,
        allowed_set(),
        spawn_mock_backend().await,
        PathBuf::from(NO_SUCH_DIR),
    );

    for target in [
        "/files/../x",
        "/files/%2e%2e/x",
        "/files//x",
        "/files/a\\b",
        "/proxy/i.webp?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "/proxy/i.webp?url=https%3A%2F%2Fevil.example%2Fa.png",
        "/proxy/s3.example.com/bucket/a.png",
    ] {
        for headers in [&[][..], &[EXTERNAL_REFERER][..]] {
            let resp = get(&app, target, headers).await;
            assert_eq!(resp.status(), StatusCode::OK, "{target} {headers:?}");
            assert_eq!(
                mock_path(&resp),
                target,
                "{target} must reach Misskey as is"
            );
        }
    }

    let resp = get(&app, "/files/../x", &[INTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&resp), None);

    let resp = get(&app, "/files/abc", &[INTERNAL_REFERER]).await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        location(&resp),
        Some(format!("{INTERNAL_BASE_URL}/files/abc"))
    );
}
