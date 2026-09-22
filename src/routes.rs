// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};

use crate::accept_gate::require_ap_accept;
use crate::config::Config;
use crate::media_redirect::redirect_internal_referer;
use crate::proxy::{ProxyState, forward};

/// The entire public egress surface, in one place. Every path registered
/// here is allowed through to Misskey; everything else falls through to
/// axum's default 404. See `docs/routes.md` for why each of these, and only
/// these, paths is here (with citations into Misskey's own source).
pub fn build(proxy_state: ProxyState, config: Arc<Config>) -> Router {
    // `/files/*` and `/proxy/*`: media, gated on an internal-Referer redirect.
    let media = Router::new()
        .route("/files/app-default.jpg", get(forward))
        .route("/files/{key}", get(forward))
        .route("/files/{key}/{*rest}", get(forward))
        .route("/proxy/{*rest}", get(forward))
        .layer(middleware::from_fn_with_state(
            config,
            redirect_internal_referer,
        ));

    // The three dual-purpose (AP-or-HTML) paths: gated on `Accept`.
    let gated = Router::new()
        .route("/notes/{note}", get(forward))
        .route("/users/{user}", get(forward))
        .route("/@{acct}", get(forward))
        .layer(middleware::from_fn(require_ap_accept));

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
        .merge(plain)
        .merge(gated)
        .merge(media)
        .with_state(proxy_state)
}
