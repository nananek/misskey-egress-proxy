// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::config::Config;

/// `/files/*` and `/proxy/*` serve potentially large media bytes that both
/// federated servers and our own internal (Tailscale) users need. Internal
/// callers already have a shorter, faster path directly to Misskey via the
/// internal Caddy instance, so when a request's `Referer` points at our own
/// internal hostname, redirect there instead of relaying the bytes
/// ourselves.
///
/// This is a bandwidth optimisation, not a security boundary: `Referer` is
/// trivially spoofable, but spoofing it only earns the caller a redirect to
/// a host they can't reach anyway, since the internal host is only
/// reachable over Tailscale.
pub async fn redirect_internal_referer(
    State(config): State<Arc<Config>>,
    req: Request,
    next: Next,
) -> Response {
    let is_internal = req
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|referer| url::Url::parse(referer).ok())
        .and_then(|url| url.host_str().map(|h| h.to_string()))
        .map(|host| host.ends_with(config.internal_referer_suffix.as_str()))
        .unwrap_or(false);

    if !is_internal {
        return next.run(req).await;
    }

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let target = format!("{}{}", config.internal_base_url, path_and_query);

    // A caller still writing a body would otherwise see the connection die
    // under it when this response closes the request; see `src/reject.rs`.
    crate::reject::drain_body(req.into_body()).await;

    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, target)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
