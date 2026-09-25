// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::config::{Config, MediaMode};
use crate::media_target::{internal_location, original_location};
use crate::reject;

/// Decides what happens to a request for `/files/*` or `/proxy/*`.
///
/// **Internal callers, in both modes.** These paths serve potentially large
/// media bytes, and internal callers already have a shorter, faster path
/// directly to Misskey via the internal Caddy instance. So when a request's
/// `Referer` points at our own internal hostname, redirect there
/// (`INTERNAL_BASE_URL` plus the request's own path and query) instead of
/// relaying the bytes ourselves. This is a bandwidth optimisation, not a
/// security boundary: `Referer` is trivially spoofable, but spoofing it only
/// earns the caller a redirect to a host they can't reach anyway, since the
/// internal host is only reachable over Tailscale. The redirect target is
/// checked first ([`internal_location`]); a request whose path fails the
/// check is answered 404, which is the one way `MEDIA_MODE=proxy` differs
/// from what it did before this check existed.
///
/// **Everyone else**, by `MEDIA_MODE`:
///
/// - `proxy` (the default): pass the request on to `forward`, which relays it
///   to Misskey unchanged. Nothing about this path is validated, because
///   federation peers rely on it being a transparent relay.
/// - `redirect`: the request never reaches Misskey. `/proxy/*?url=` is
///   answered with a 302 to the original URL if it falls under
///   `MEDIA_ALLOWED_PREFIXES` ([`original_location`]) and 404 otherwise, and
///   `/files/*` is answered 404. A `/files/*` request only gets here without
///   an internal `Referer` when something in front of this proxy was meant to
///   answer it instead (the edge routing `/files` elsewhere); a 404 makes a
///   gap in that routing obvious immediately, and cannot name the internal
///   host to a caller that has no business knowing it.
///
/// Every refusal is a 404 that drains the request body first (see
/// `src/reject.rs`), and is logged at debug level as the rule that refused
/// it: never the `url` the caller supplied.
pub async fn redirect_media(
    State(config): State<Arc<Config>>,
    req: Request,
    next: Next,
) -> Response {
    if referer_is_internal(&req, &config) {
        return match internal_location(&config.internal_base_url, path_and_query(&req)) {
            Ok(location) => redirect_to(req, location).await,
            Err(rule) => refuse(req, "internal redirect", rule).await,
        };
    }

    match config.media_mode {
        MediaMode::Proxy => next.run(req).await,
        MediaMode::Redirect if req.uri().path().starts_with("/files/") => {
            tracing::debug!("/files/ request without an internal Referer refused in redirect mode");
            reject::not_found(req).await
        }
        MediaMode::Redirect => {
            match original_location(&config.media_allowed_prefixes, path_and_query(&req)) {
                Ok(location) => redirect_to(req, location).await,
                Err(rule) => refuse(req, "original url", rule).await,
            }
        }
    }
}

/// The request-target's path and query, borrowed: `proxy` mode passes almost
/// every media request straight through, so nothing here may copy it.
fn path_and_query(req: &Request) -> &str {
    req.uri().path_and_query().map_or("/", |pq| pq.as_str())
}

fn referer_is_internal(req: &Request, config: &Config) -> bool {
    req.headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|referer| url::Url::parse(referer).ok())
        .and_then(|url| url.host_str().map(|h| h.to_string()))
        .map(|host| host.ends_with(config.internal_referer_suffix.as_str()))
        .unwrap_or(false)
}

async fn refuse(req: Request, what: &str, rule: impl std::fmt::Debug) -> Response {
    tracing::debug!(?rule, "{what} refused");
    reject::not_found(req).await
}

/// `location` has already been through `media_target`, so it is printable
/// ASCII and always a valid header value; the fallback keeps that from being
/// a panic or a 500 if it ever is not.
async fn redirect_to(req: Request, location: String) -> Response {
    let Ok(location) = HeaderValue::try_from(location) else {
        return reject::not_found(req).await;
    };

    // A caller still writing a body would otherwise see the connection die
    // under it when this response closes the request; see `src/reject.rs`.
    reject::drain_body(req.into_body()).await;

    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    response
}
