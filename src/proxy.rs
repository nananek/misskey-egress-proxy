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
use percent_encoding::percent_decode_str;

use crate::headers;

pub type UdsClient = Client<UnixConnector, Body>;

pub fn build_client() -> UdsClient {
    Client::builder(TokioExecutor::new()).build(UnixConnector)
}

#[derive(Clone)]
pub struct ProxyState {
    pub client: UdsClient,
    pub socket: PathBuf,
}

/// Forwards a request to the Misskey UDS: same method, same raw body, and
/// every header of the caller's except the ones `crate::headers` removes
/// first. Only the URI's scheme/authority are swapped for the Unix-socket
/// target; the path and query string are preserved exactly.
///
/// Bodies are streamed in both directions and never buffered, re-encoded, or
/// otherwise transformed — `/inbox`'s HTTP Signature `Digest` is computed
/// over the exact raw body, and any mutation here (or gzip, or a body
/// length change) would break signature verification on Misskey's side.
pub async fn forward(State(state): State<ProxyState>, mut req: Request) -> Response {
    headers::sanitize_request(req.headers_mut());

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    // Misskey (Fastify) rejects a request-target whose percent-escapes do not
    // decode to UTF-8 with `FST_ERR_BAD_URL`, and that error document echoes
    // the raw target and query back. Answer such a request here instead: it
    // has no valid route either way, and the public face must not relay an
    // internal framework error. Checking the raw target (`%2F` and `%ff` both
    // reach us intact) keeps the relay transparent for everything valid.
    if !is_valid_raw_target(path_and_query) {
        tracing::debug!("malformed percent-encoding in request-target refused");
        return (StatusCode::BAD_REQUEST, "").into_response();
    }

    let target: Uri = hyperlocal::Uri::new(&state.socket, path_and_query).into();
    *req.uri_mut() = target;

    match state.client.request(req).await {
        Ok(mut resp) => {
            headers::sanitize_response(resp.headers_mut());
            // The public face never serves Misskey's HTML (the SPA catch-all
            // or an error page): a response that claims to be a page is a
            // route miss from the caller's point of view, not something this
            // proxy relays.
            if headers::is_html(resp.headers()) {
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
            resp.map(Body::new).into_response()
        }
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

/// Whether a raw request-target can be handed to Misskey unchanged.
///
/// Every `%` must start a two-digit hex escape, and the whole target must
/// percent-decode to UTF-8. Fastify decodes the path and query the same way,
/// so anything this rejects would have become a `FST_ERR_BAD_URL` response
/// that quotes the raw target back to the caller.
fn is_valid_raw_target(target: &str) -> bool {
    let bytes = target.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !(bytes[i + 1] as char).is_ascii_hexdigit()
                || !(bytes[i + 2] as char).is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    percent_decode_str(target).decode_utf8().is_ok()
}

#[cfg(test)]
mod tests {
    use super::is_valid_raw_target;

    #[test]
    fn valid_percent_escapes_are_accepted() {
        for target in [
            "/files/abc",
            "/files/abc?x=1",
            "/files/%E3%81%82",
            "/files/a%2Fb",
            "/proxy/image.webp?url=https%3A%2F%2Fexample.com%2Fa.png",
            "/inbox?x=%00",
        ] {
            assert!(is_valid_raw_target(target), "{target} should be valid");
        }
    }

    #[test]
    fn malformed_or_non_utf8_escapes_are_refused() {
        for target in [
            "/files/%ff",
            "/files/%ff?canary=1",
            "/files/%",
            "/files/%f",
            "/files/%zz",
            "/files/%c3",
            "/files/%ed%a0%80",
        ] {
            assert!(!is_valid_raw_target(target), "{target} should be refused");
        }
    }
}
