// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;

/// How much of a rejected request's body is read before the rejection
/// response goes out. Sized for the ordinary blocked API calls that
/// motivated issue #3 — their JSON bodies are a few hundred bytes — while
/// keeping a rejected path from becoming somewhere to push unbounded data.
pub const DRAIN_LIMIT: usize = 1024 * 1024;

/// And for how long. A caller trickling one byte at a time must not be able
/// to hold a rejected request open forever; before the drain existed, the
/// connection closed immediately.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads and discards the request body before a rejection response is
/// returned.
///
/// Hyper closes the connection when a response is returned with the request
/// body unread, so a caller that is still writing its body sees the socket
/// die mid-write. cloudflared reports that as a 502 "Unable to reach the
/// origin service" even though the proxy answered 404/405 cleanly, which
/// makes a blocked request indistinguishable from a real outage (issue #3).
/// Draining first lets the caller finish its write and read the real status.
///
/// Bounded in both bytes and time, and streamed frame by frame rather than
/// buffered: a rejected path must not be a memory sink or an open-ended
/// read. The byte bound counts decoded body bytes, not wire bytes — a body
/// split into tiny chunks with long chunk extensions makes the proxy read
/// more than `DRAIN_LIMIT` off the socket (hyper's per-line buffer, ~400
/// KiB, caps the overhead per chunk header) — so the wall-clock bound is
/// what caps that case. A body that hits any bound is left unfinished and
/// the connection closes as it did before the drain existed, which only
/// affects callers sending more at a blocked path than a blocked path
/// should ever see.
pub async fn drain_body(body: Body) {
    drain_within(body, DRAIN_LIMIT, DRAIN_TIMEOUT).await;
}

async fn drain_within(body: Body, limit: usize, timeout: Duration) {
    let _ = tokio::time::timeout(timeout, drain_up_to(body, limit)).await;
}

async fn drain_up_to(mut body: Body, limit: usize) {
    let mut drained = 0usize;
    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref() {
            drained += data.len();
            if drained >= limit {
                return;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use std::time::Instant;

    use axum::body::Bytes;
    use http_body::{Frame, SizeHint};

    use super::*;

    /// A body that never produces a frame and never ends.
    struct NeverEnding;

    impl http_body::Body for NeverEnding {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Pending
        }

        fn is_end_stream(&self) -> bool {
            false
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    /// Yields up to `remaining` frames of `chunk` bytes each and records how
    /// many bytes were actually polled.
    struct CountingBody {
        remaining: usize,
        chunk: usize,
        polled: Arc<AtomicUsize>,
    }

    impl http_body::Body for CountingBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            let this = self.get_mut();
            if this.remaining == 0 {
                return Poll::Ready(None);
            }
            this.remaining -= 1;
            this.polled.fetch_add(this.chunk, Ordering::SeqCst);
            Poll::Ready(Some(Ok(Frame::data(Bytes::from(vec![0u8; this.chunk])))))
        }

        fn is_end_stream(&self) -> bool {
            self.remaining == 0
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    #[tokio::test]
    async fn the_drain_is_time_bounded() {
        let started = Instant::now();
        drain_within(
            Body::new(NeverEnding),
            DRAIN_LIMIT,
            Duration::from_millis(50),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a body that never ends must not hold the drain open"
        );
    }

    #[tokio::test]
    async fn the_drain_stops_at_the_byte_bound() {
        const CHUNK: usize = 64 * 1024;
        let polled = Arc::new(AtomicUsize::new(0));
        let body = Body::new(CountingBody {
            remaining: (DRAIN_LIMIT * 4) / CHUNK,
            chunk: CHUNK,
            polled: Arc::clone(&polled),
        });

        drain_within(body, DRAIN_LIMIT, Duration::from_secs(60)).await;

        let polled = polled.load(Ordering::SeqCst);
        assert!(
            (DRAIN_LIMIT..DRAIN_LIMIT + CHUNK).contains(&polled),
            "the drain must stop at the byte bound, not read the whole body: polled {polled} of {}",
            DRAIN_LIMIT * 4
        );
    }
}
