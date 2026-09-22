// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

const AP_JSON: HeaderValue = HeaderValue::from_static("application/activity+json");

/// `/notes/{note}`, `/users/{user}`, and `/@{acct}` serve either an
/// ActivityPub JSON object or a full HTML page from the same URL, chosen by
/// Misskey via `Accept` content negotiation (its `apOrHtml` Fastify
/// constraint). This proxy never forwards the HTML variant to the public
/// internet: external callers only need AP JSON for federation, and human
/// browsing of these pages is served internally instead.
///
/// So rather than judging the caller's `Accept`, this rewrites it: every
/// request that reaches these three paths asks Misskey for AP JSON, and
/// gets AP JSON. Refusing the odd ones instead (a 406 for anything that
/// didn't name `application/activity+json`) would also keep HTML in, but it
/// would break any federated implementation that fetches with `*/*` or with
/// no `Accept` at all, and that is not a bet worth making for a header this
/// proxy is about to overwrite anyway.
pub async fn force_ap_accept(mut req: Request, next: Next) -> Response {
    req.headers_mut().insert(header::ACCEPT, AP_JSON);
    next.run(req).await
}
