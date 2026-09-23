// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::path::{Path, PathBuf};

use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

const INDEX_HTML: &str = include_str!("../static/index.html");
const MISSKEY_LOGO: &[u8] = include_bytes!("../static/misskey.svg");

/// Builds the only human-facing routes on this otherwise federation-only
/// endpoint. Files in `static_dir` take precedence over the copies embedded
/// in the executable, so a bind mount can replace the page at runtime.
pub fn routes<S>(static_dir: PathBuf) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let index_dir = static_dir.clone();
    let logo_dir = static_dir;

    Router::new()
        .route(
            "/",
            get(move || {
                let dir = index_dir.clone();
                async move { index(dir).await }
            }),
        )
        .route(
            "/assets/misskey.svg",
            get(move || {
                let dir = logo_dir.clone();
                async move { misskey_logo(dir).await }
            }),
        )
}

async fn index(static_dir: PathBuf) -> Response {
    let body = read_override(&static_dir, "index.html", INDEX_HTML.as_bytes()).await;
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

/// Misskey's official wordmark, vendored from
/// `packages/frontend/assets/misskey.svg` in misskey-dev/misskey.
///
/// Carries a CSP like the index page does: `STATIC_DIR` lets an operator
/// replace this file at runtime, and an SVG opened directly is an active
/// document, not just an image. `default-src 'none'` plus `sandbox` keep a
/// replacement file from running script on this origin.
async fn misskey_logo(static_dir: PathBuf) -> Response {
    let body = read_override(&static_dir, "misskey.svg", MISSKEY_LOGO).await;
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
            (header::CACHE_CONTROL, "public, max-age=604800"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

async fn read_override(static_dir: &Path, name: &str, bundled: &'static [u8]) -> Vec<u8> {
    tokio::fs::read(static_dir.join(name))
        .await
        .unwrap_or_else(|_| bundled.to_vec())
}
