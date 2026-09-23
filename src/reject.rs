// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;

/// Reads and discards the rest of a request body.
///
/// Hyper closes the connection when a response is returned with the request
/// body unread, so a caller that is still writing its body sees the socket
/// die mid-write. cloudflared reports that as a 502 "Unable to reach the
/// origin service" even though the proxy answered 404/405 cleanly, which
/// makes a blocked request indistinguishable from a real outage (issue #3).
/// Draining first lets the caller finish its write and read the real status.
///
/// Streamed frame by frame, never buffered: a rejected path must not become
/// a memory sink for a large body.
pub async fn drain_body(mut body: Body) {
    // `None` ends the body; an error frame (client abort, malformed chunk)
    // ends the drain too, and the caller gets the rejection either way.
    while let Some(Ok(_)) = body.frame().await {}
}

/// Fallback for paths that are not on the allowlist.
pub async fn not_found(req: Request) -> Response {
    drain_body(req.into_body()).await;
    StatusCode::NOT_FOUND.into_response()
}

/// Fallback for allowlisted paths called with a method they don't accept.
pub async fn method_not_allowed(req: Request) -> Response {
    drain_body(req.into_body()).await;
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}
