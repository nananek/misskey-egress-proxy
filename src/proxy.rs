// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::UnixConnector;

pub type UdsClient = Client<UnixConnector, Body>;

pub fn build_client() -> UdsClient {
    Client::builder(TokioExecutor::new()).build(UnixConnector)
}

#[derive(Clone)]
pub struct ProxyState {
    pub client: UdsClient,
    pub socket: PathBuf,
}

/// Forwards a request to the Misskey UDS verbatim: same method, same
/// headers, same raw body. Only the URI's scheme/authority are swapped for
/// the Unix-socket target; the path and query string are preserved exactly.
///
/// Bodies are streamed in both directions and never buffered, re-encoded, or
/// otherwise transformed — `/inbox`'s HTTP Signature `Digest` is computed
/// over the exact raw body, and any mutation here (or gzip, or a body
/// length change) would break signature verification on Misskey's side.
pub async fn forward(State(state): State<ProxyState>, mut req: Request) -> Response {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let target: Uri = hyperlocal::Uri::new(&state.socket, path_and_query).into();
    *req.uri_mut() = target;

    match state.client.request(req).await {
        Ok(resp) => resp.map(Body::new).into_response(),
        Err(err) => {
            // An upstream that answers before the request body is fully sent
            // and then closes (Misskey's 64 KiB bodyLimit on `/inbox` does
            // exactly this) is a race for hyper's client: it usually
            // surfaces the upstream's 413, but when the close's RST wins the
            // connection errors first and the only honest status left is
            // 502. Measured on the federation stack: 9/10 413, 1/10 502 for
            // a 10 MiB `/inbox` body, direct and through nginx alike.
            tracing::error!(error = %err, "upstream request to misskey UDS failed");
            (StatusCode::BAD_GATEWAY, "upstream unavailable").into_response()
        }
    }
}
