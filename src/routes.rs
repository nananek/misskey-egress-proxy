// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};

use crate::accept_gate::force_ap_accept;
use crate::config::Config;
use crate::feed_routes::reject_feed_paths;
use crate::home;
use crate::media_redirect::redirect_internal_referer;
use crate::proxy::{ProxyState, forward};
use crate::reject;

/// The entire public surface, in one place. The two landing-page routes are
/// served locally; every other registered path is allowed through to
/// Misskey, and everything else falls through to axum's default 404. See
/// `docs/routes.md` for why each of these, and only these, paths is here.
pub fn build(proxy_state: ProxyState, config: Arc<Config>) -> Router {
    // A deliberately tiny human-facing surface: one informational page and
    // its vendored Misskey wordmark. Neither route reaches Misskey.
    let home = home::routes(config.static_dir.clone());

    // `/files/*` and `/proxy/*`: media, gated on an internal-Referer redirect.
    // `route_layer`, not `layer`: the redirect is scoped to these four
    // routes and must not run on the 404 fallback, or a spoofed internal
    // Referer would turn every unknown path into a 302 to the internal host
    // instead of a 404.
    let media = Router::new()
        .route("/files/app-default.jpg", get(forward))
        .route("/files/{key}", get(forward))
        .route("/files/{key}/{*rest}", get(forward))
        .route("/proxy/{*rest}", get(forward))
        .route_layer(middleware::from_fn_with_state(
            config,
            redirect_internal_referer,
        ));

    // The three dual-purpose (AP-or-HTML) paths: `Accept` is rewritten to
    // AP JSON and the client-only feed twins of `/@{acct}` are rejected, so
    // Misskey never picks a non-AP branch for a public caller. `route_layer`
    // for the same fallback-scoping reason as above.
    let gated = Router::new()
        .route("/notes/{note}", get(forward))
        .route("/users/{user}", get(forward))
        .route("/@{acct}", get(forward))
        .route_layer(middleware::from_fn(force_ap_accept))
        .route_layer(middleware::from_fn(reject_feed_paths));

    // Everything else: unconditionally AP-only in Misskey itself, so no
    // extra gating is needed here.
    let plain = Router::new()
        // well-known / nodeinfo discovery
        .route("/.well-known/webfinger", get(forward).options(forward))
        .route("/.well-known/nodeinfo", get(forward))
        .route("/.well-known/host-meta", get(forward))
        .route("/.well-known/host-meta.json", get(forward))
        .route("/nodeinfo/2.0", get(forward))
        .route("/nodeinfo/2.1", get(forward))
        // activitypub delivery / query
        .route("/inbox", post(forward))
        .route("/users/{user}/inbox", post(forward))
        .route("/notes/{note}/activity", get(forward))
        .route("/users/{user}/outbox", get(forward))
        .route("/users/{user}/followers", get(forward))
        .route("/users/{user}/following", get(forward))
        .route("/users/{user}/collections/featured", get(forward))
        .route("/users/{user}/publickey", get(forward))
        .route("/emojis/{emoji}", get(forward))
        .route("/likes/{like}", get(forward))
        .route("/follows/{a}", get(forward))
        .route("/follows/{a}/{b}", get(forward))
        // avatarless-user actor icon fallback (see docs/routes.md)
        .route("/identicon/{x}", get(forward));

    Router::new()
        .merge(home)
        .merge(plain)
        .merge(gated)
        .merge(media)
        // Rejections drain the request body before answering, so a caller
        // that is still writing its body sees the 404/405 instead of the
        // connection dying under it. See `src/reject.rs`.
        .fallback(reject::not_found)
        .method_not_allowed_fallback(reject::method_not_allowed)
        .with_state(proxy_state)
}
