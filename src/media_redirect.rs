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
///
/// **Caching.** The same URL is answered differently depending on the
/// `Referer` (an internal one is sent to the internal host, anyone else gets
/// the original URL or a 404), and a shared cache in front of this proxy does
/// not know that. So every 302 and 404 produced here carries
/// `Cache-Control: no-store`; otherwise an internal `Location` (with the
/// internal host name in it) could be handed to an outsider, or an outsider's
/// 404 to the operator's own UI. What `forward` returns, and what any other
/// route returns, is left exactly as it was.
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
            not_found(req).await
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
        .map(|host| host_is_internal(&host, &config.internal_referer_suffix))
        .unwrap_or(false)
}

/// Whether a `Referer`'s host falls under the internal suffix (already
/// trimmed and lowercased by `Config::from_env`, like the host).
///
/// - A suffix that starts with `.` (`.your-tailnet.ts.net`, the documented
///   form) matches any host that ends with it, as it always has.
/// - A suffix without the leading dot (`your-tailnet.ts.net`) matches that
///   host and the hosts under it, on a label boundary: `notyour-tailnet.ts.net`
///   is not internal.
/// - An empty suffix never matches. `ends_with("")` is true for every host, and
///   startup refuses an empty value, but this must not be the only thing
///   standing between a misconfiguration and every `Referer` counting as
///   internal.
fn host_is_internal(host: &str, suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }
    if suffix.starts_with('.') {
        return host.ends_with(suffix);
    }
    host.strip_suffix(suffix)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
}

async fn refuse(req: Request, what: &str, rule: impl std::fmt::Debug) -> Response {
    tracing::debug!(?rule, "{what} refused");
    not_found(req).await
}

/// The 404 of this layer: `reject::not_found`, marked uncacheable.
async fn not_found(req: Request) -> Response {
    no_store(reject::not_found(req).await)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `location` has already been through `media_target`, so it is printable
/// ASCII and always a valid header value; the fallback keeps that from being
/// a panic or a 500 if it ever is not.
async fn redirect_to(req: Request, location: String) -> Response {
    let Ok(location) = HeaderValue::try_from(location) else {
        return not_found(req).await;
    };

    // A caller still writing a body would otherwise see the connection die
    // under it when this response closes the request; see `src/reject.rs`.
    reject::drain_body(req.into_body()).await;

    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    no_store(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_suffix_matches_no_host() {
        for host in ["", "example.com", "misskey.internal.example.ts.net"] {
            assert!(!host_is_internal(host, ""), "{host:?}");
        }
    }

    /// The documented form: matched as `ends_with`, exactly as before.
    #[test]
    fn a_dotted_suffix_matches_the_hosts_under_it() {
        let suffix = ".internal.example.ts.net";
        for (host, expected) in [
            ("misskey.internal.example.ts.net", true),
            ("a.b.internal.example.ts.net", true),
            // The bare name is not "under" a dotted suffix.
            ("internal.example.ts.net", false),
            ("notinternal.example.ts.net", false),
            ("internal.example.ts.net.evil.example", false),
            ("evil.example", false),
            ("", false),
        ] {
            assert_eq!(host_is_internal(host, suffix), expected, "{host:?}");
        }
    }

    #[test]
    fn a_suffix_without_a_dot_matches_the_host_and_its_subdomains_only() {
        let suffix = "internal.example.ts.net";
        for (host, expected) in [
            ("internal.example.ts.net", true),
            ("misskey.internal.example.ts.net", true),
            ("a.b.internal.example.ts.net", true),
            ("notinternal.example.ts.net", false),
            ("xinternal.example.ts.net", false),
            ("internal.example.ts.net.evil.example", false),
            ("example.ts.net", false),
            ("evil.example", false),
            ("", false),
        ] {
            assert_eq!(host_is_internal(host, suffix), expected, "{host:?}");
        }
    }
}
