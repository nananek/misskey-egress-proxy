// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::reject;

/// The one page a person is meant to open on this endpoint. Relative, so the
/// redirect stays on whatever public origin the caller used.
const HOME: HeaderValue = HeaderValue::from_static("/");

/// Sends a browser that opens one of the ActivityPub routes as a page (a link
/// to a note or a profile followed from a remote server, a URL pasted into
/// the address bar) to the landing page at `/`, instead of showing it the raw
/// AP JSON that `force_ap_accept` would otherwise get it.
///
/// A page load is recognised by `Sec-Fetch-Dest`, which browsers set on every
/// request and nothing else does: federated servers, crawlers and link-preview
/// fetchers never send it, so they are forwarded exactly as before. `Accept:
/// text/html` is deliberately not the signal: link-preview fetchers and some
/// federated implementations send it, and judging it would break them.
///
/// Scoped to the AP and discovery routes only. Media (`/files/*`, `/proxy/*`,
/// `/identicon/*`) is loaded by browsers all the time, and opening an image in
/// its own tab is a page load too, so it is left alone.
///
/// A browser without `Sec-Fetch-Dest` (an old one, or a caller that strips
/// it) still gets the AP JSON. That is fine: this is about what a person
/// lands on, not a security boundary.
///
/// The 302 depends on a request header a shared cache in front does not key
/// on, so it carries `Cache-Control: no-store`: a stored copy handed to a
/// federated server would break federation for that URL.
pub async fn redirect_navigation(req: Request, next: Next) -> Response {
    if !is_navigation(&req) {
        return next.run(req).await;
    }

    // A caller still writing a body would otherwise see the connection die
    // under it when this response closes the request; see `src/reject.rs`.
    reject::drain_body(req.into_body()).await;

    let mut response = StatusCode::FOUND.into_response();
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, HOME);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn is_navigation(req: &Request) -> bool {
    req.headers()
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|dest| {
            let dest = dest.trim();
            ["document", "iframe", "frame"]
                .iter()
                .any(|nav| dest.eq_ignore_ascii_case(nav))
        })
}
