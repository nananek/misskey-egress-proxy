// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! Adversarial checks, written from an attacker's side, on how the media
//! routes treat input that is built to look like something else:
//!
//! - an empty `INTERNAL_REFERER_SUFFIX`, which must not make every `Referer`
//!   internal, and one written without its leading dot, which matches on a dot
//!   boundary,
//! - an `INTERNAL_BASE_URL` with a dot-segment in it, which must not reach a
//!   `Location`,
//! - port 0 in an allowlist entry or in `INTERNAL_BASE_URL`,
//! - a method override header on a `POST`,
//! - an allowlist prefix written with a percent-escape,
//! - quotes in the query of an original URL,
//! - paths that resemble the media routes without being them.
//!
//! These use a socket nothing listens on, so a `502` is proof that a request
//! was handed to `forward`; a `302`/`404`/`405` is proof that it was answered
//! locally. They build `Config` directly rather than going through
//! `Config::from_env` and assert the outcome of a request.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use tower::ServiceExt;

use misskey_egress_proxy::config::{Config, ListenTarget, MediaMode, validate_internal_base_url};
use misskey_egress_proxy::media_target::{
    AllowedPrefix, internal_location, parse_allowed_prefixes,
};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

const INTERNAL_BASE: &str = "https://internal.example.ts.net";
const NO_DIR: &str = "/path/that/does/not/exist";

fn dead_socket() -> PathBuf {
    std::env::temp_dir().join(format!("mep-adversarial-dead-{}.sock", std::process::id()))
}

fn build(mode: MediaMode, suffix: &str, allowed: Vec<AllowedPrefix>) -> Router {
    build_with_base(mode, suffix, allowed, INTERNAL_BASE)
}

fn build_with_base(
    mode: MediaMode,
    suffix: &str,
    allowed: Vec<AllowedPrefix>,
    internal_base_url: &str,
) -> Router {
    let socket = dead_socket();
    let config = Arc::new(Config {
        listen: ListenTarget::Tcp("127.0.0.1:0".to_string()),
        misskey_socket: socket.clone(),
        internal_base_url: internal_base_url.to_string(),
        internal_referer_suffix: suffix.to_string(),
        static_dir: PathBuf::from(NO_DIR),
        media_mode: mode,
        media_allowed_prefixes: allowed,
    });
    routes::build(
        ProxyState {
            client: build_client(),
            socket,
        },
        config,
    )
}

async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn get(app: &Router, path: &str, headers: &[(&str, &str)]) -> Response {
    call(app, "GET", path, headers).await
}

fn location(resp: &Response) -> Option<String> {
    resp.headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
}

/// An empty suffix would make `host.ends_with("")` true for every host, so
/// *any* `Referer` (a browser's ordinary one, not a forged one) would turn a
/// media request into a `302` naming the internal host. Startup refuses an
/// empty value, and the matcher must not depend on that.
#[tokio::test]
async fn an_empty_internal_referer_suffix_must_not_make_every_referer_internal() {
    let foreign = [("referer", "https://unrelated.example/")];

    let app = build(MediaMode::Redirect, "", vec![]);
    let resp = get(&app, "/files/x", &foreign).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a foreign Referer is not internal; got {:?}",
        location(&resp)
    );

    // In `proxy` mode the request is relayed (to the dead socket here), not
    // redirected to the internal host.
    let app = build(MediaMode::Proxy, "", vec![]);
    let resp = get(&app, "/files/x", &foreign).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(location(&resp), None);
}

/// A suffix written without its leading dot names a host and its subdomains,
/// not every host that happens to end in the same characters: with
/// `internal.example.ts.net`, `https://notinternal.example.ts.net/` is not
/// internal, while the host itself and a subdomain are.
#[tokio::test]
async fn a_suffix_without_a_dot_boundary_must_not_match_a_longer_host() {
    let app = build(MediaMode::Redirect, "internal.example.ts.net", vec![]);

    let resp = get(
        &app,
        "/files/x",
        &[("referer", "https://notinternal.example.ts.net/")],
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "only a dot-boundary suffix match is internal; got {:?}",
        location(&resp)
    );

    for internal in [
        "https://internal.example.ts.net/",
        "https://misskey.internal.example.ts.net/",
    ] {
        let resp = get(&app, "/files/x", &[("referer", internal)]).await;
        assert_eq!(resp.status(), StatusCode::FOUND, "{internal}");
        assert_eq!(
            location(&resp),
            Some(format!("{INTERNAL_BASE}/files/x")),
            "{internal}"
        );
    }
}

/// `validate_internal_base_url` must judge the raw string. A raw path made
/// only of dot-segments normalises to `/`, and a percent-encoded dot (`%2e`)
/// to a single-dot segment, so a check on the normalised path would let them
/// through as "no path" while the raw string, which is what ends up in the
/// `Location`, still carries one.
#[test]
fn internal_base_url_validation_must_refuse_a_dot_segment_path() {
    for base in [
        "https://h/..",
        "https://h/.",
        "https://h/%2e",
        "https://h/x/..",
    ] {
        assert!(
            validate_internal_base_url(base).is_err(),
            "{base:?} must not be a usable base: the raw string carries a path"
        );
    }
}

/// The internal `Location` is never the product of dot-segment resolution: a
/// base that carries a raw `..` must not put it into a `Location`.
#[test]
fn internal_location_must_not_emit_a_dot_segment_from_the_base() {
    match internal_location("https://h/..", "/files/x") {
        Err(_) => {}
        Ok(location) => assert!(
            !location.contains("/../") && !location.contains("/./"),
            "{location:?} still carries a dot-segment"
        ),
    }
}

/// Startup refuses such a base in `redirect` mode only; `proxy` mode does not
/// validate it. In both, a request with an internal `Referer` gets a `404`
/// rather than a `302` whose `Location` still carries the dot-segment.
#[tokio::test]
async fn a_base_with_a_dot_segment_never_becomes_a_location() {
    let internal = [("referer", "https://misskey.internal.example.ts.net/")];

    for mode in [MediaMode::Proxy, MediaMode::Redirect] {
        for base in ["https://h/..", "https://h/%2e", "https://h/a/./b"] {
            let app = build_with_base(mode, ".internal.example.ts.net", vec![], base);
            let resp = get(&app, "/files/x", &internal).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{mode:?} {base}");
            assert_eq!(location(&resp), None, "{mode:?} {base}");
        }
    }
}

/// Port 0 is one to five digits, but no TLS server listens on it: an entry
/// there can only produce a `Location` no client can follow. It belongs with
/// the empty and non-numeric ports the parser already refuses, and so does the
/// same port in `INTERNAL_BASE_URL`.
#[test]
fn a_media_prefix_on_port_zero_must_be_refused() {
    let err = parse_allowed_prefixes(Some("https://host.example:0/"));
    assert!(
        err.is_err(),
        "port 0 must not be a usable prefix: {:?}",
        err.ok()
    );

    for base in ["https://h:0", "http://h:00"] {
        assert!(
            validate_internal_base_url(base).is_err(),
            "{base:?} must not be a usable base"
        );
    }
}

/// `X-HTTP-Method-Override` is a header some front-ends honour. This proxy must
/// not: a `POST` that turned into a `GET` on a media route would skip the
/// method gate and could be answered with a redirect.
#[tokio::test]
async fn a_method_override_header_must_not_redirect_a_post() {
    let allowed = parse_allowed_prefixes(Some("https://s3.example.com/bucket/")).unwrap();
    let app = build(MediaMode::Redirect, ".internal.example.ts.net", allowed);

    let resp = call(
        &app,
        "POST",
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        &[("x-http-method-override", "GET")],
    )
    .await;

    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(location(&resp), None);
}

/// A prefix written with a percent-escape matches only a `url` value spelled
/// the same way: the allowlist is compared after the request query is decoded
/// once, so `/A/` does not match an entry written `/%41/`, and the entry's own
/// spelling needs `%2541` in the request. The comparison is on bytes, not on
/// decoded paths.
#[tokio::test]
async fn an_encoded_prefix_matches_only_its_own_spelling() {
    let allowed = parse_allowed_prefixes(Some("https://h.example/%41/")).unwrap();
    let app = build(MediaMode::Redirect, ".internal.example.ts.net", allowed);

    let matched = get(
        &app,
        "/proxy/x?url=https%3A%2F%2Fh.example%2F%2541%2Fa",
        &[],
    )
    .await;
    assert_eq!(matched.status(), StatusCode::FOUND);
    assert_eq!(
        location(&matched).as_deref(),
        Some("https://h.example/%41/a")
    );

    let decoded = get(&app, "/proxy/x?url=https%3A%2F%2Fh.example%2FA%2Fa", &[]).await;
    assert_eq!(decoded.status(), StatusCode::NOT_FOUND);
    assert_eq!(location(&decoded), None);
}

/// The `Location` for an original URL is the parser's re-serialisation, not
/// the caller's text: a `"` or `'` in the query of an otherwise acceptable URL
/// comes out percent-encoded, never raw.
#[tokio::test]
async fn an_allowed_location_is_re_serialised_without_raw_quotes() {
    let allowed = parse_allowed_prefixes(Some("https://s3.example.com/bucket/")).unwrap();
    let app = build(MediaMode::Redirect, ".internal.example.ts.net", allowed);

    for quote in ["%22", "%27"] {
        let target =
            format!("/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa%3Fq%3D{quote}x");
        let resp = get(&app, &target, &[]).await;
        assert_eq!(resp.status(), StatusCode::FOUND, "{target}");
        let location = location(&resp).expect("a Location");
        assert!(
            !location.contains(['"', '\'']),
            "{location:?} must not carry a raw quote"
        );
    }
}

/// Shapes that look like the media routes but do not match them are a local
/// 404 in `redirect` mode: never a `502`, which is what dialling the (dead)
/// upstream would produce, and never a `302`.
#[tokio::test]
async fn lookalike_media_paths_never_reach_the_upstream_in_redirect_mode() {
    let allowed = parse_allowed_prefixes(Some("https://s3.example.com/bucket/")).unwrap();
    let app = build(MediaMode::Redirect, ".internal.example.ts.net", allowed);

    for target in [
        "/files%2fx",
        "/files%2Fx",
        "/FILES/x",
        "/Files/x",
        "//files/x",
        "/proxy%2fx",
        "/files/..;/x",
        "/files/x%00",
    ] {
        let resp = get(&app, target, &[]).await;
        assert_ne!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "{target} reached the upstream"
        );
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{target}");
        assert_eq!(location(&resp), None, "{target}");
    }
}
