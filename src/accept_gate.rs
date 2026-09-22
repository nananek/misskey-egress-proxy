// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// `/notes/{note}`, `/users/{user}`, and `/@{acct}` serve either an
/// ActivityPub JSON object or a full HTML page from the same URL, chosen by
/// Misskey via `Accept` content negotiation (its `apOrHtml` Fastify
/// constraint). This proxy never forwards the HTML variant to the public
/// internet: external callers only need AP JSON for federation, and human
/// browsing of these pages is served internally instead.
///
/// A request only passes if its `Accept` header explicitly asks for AP
/// JSON. This is a plain substring check, not RFC 7231 q-value negotiation
/// — real ActivityPub implementations always send an explicit
/// `application/activity+json` (or `application/ld+json`) Accept header, so
/// this is sufficient in practice and keeps the check trivial to audit.
pub async fn require_ap_accept(req: Request, next: Next) -> Response {
    let wants_ap = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|accept| {
            accept.contains("application/activity+json") || accept.contains("application/ld+json")
        })
        .unwrap_or(false);

    if wants_ap {
        next.run(req).await
    } else {
        StatusCode::NOT_ACCEPTABLE.into_response()
    }
}
