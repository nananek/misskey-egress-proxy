// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Misskey serves client-only feeds at `/@:user.rss`, `/@:user.atom`, and
/// `/@:user.json` (`ClientServerService.ts`), registered without the
/// `apOrHtml` constraint. find-my-way prefers a parameter with a static
/// suffix over a plain parameter, so `/@alice.rss` never reaches the
/// ActivityPub `/@:acct` route — it is a feed regardless of `Accept`. This
/// proxy's `/@{acct}` pattern is one segment wide and would otherwise expose
/// all three to the public internet, so reject the shapes that can only be a
/// feed. Federation never needs them; they are client features, like the
/// `/emoji/:path` and `/avatar/@:acct` shorthands `docs/routes.md` already
/// excludes.
///
/// The check decodes the segment the way find-my-way does before routing
/// (`safeDecodeURI` + `decodeURI`): percent-escapes for reserved characters
/// (`;/?:@&=+$,#`) never become path structure, while every other escape is
/// decoded (`%2e` is `.`, `%72` is `r`, ...). `/@alice%2erss` is therefore
/// the same feed as `/@alice.rss` and is rejected too. `%25` stays encoded,
/// matching find-my-way's double-decode guard.
pub async fn reject_feed_paths(req: Request, next: Next) -> Response {
    if is_feed_path(req.uri().path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

fn is_feed_path(path: &str) -> bool {
    let Some(acct) = path.strip_prefix("/@") else {
        return false;
    };
    if acct.is_empty() || acct.contains('/') {
        return false;
    }
    let decoded = decode_like_find_my_way(acct);
    decoded.ends_with(".rss") || decoded.ends_with(".atom") || decoded.ends_with(".json")
}

fn decode_like_find_my_way(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            let byte = high * 16 + low;
            // `decodeURI` leaves RFC 3986 reserved characters encoded;
            // find-my-way additionally keeps `%25` encoded so a double
            // escape cannot be decoded twice.
            let reserved = matches!(
                byte,
                b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'#' | b'%'
            );
            if !reserved {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_acct_is_not_a_feed() {
        assert!(!is_feed_path("/@alice"));
        assert!(!is_feed_path("/@alice@remote.example"));
        assert!(!is_feed_path("/@alice.RSS"));
    }

    #[test]
    fn feed_suffixes_are_rejected() {
        assert!(is_feed_path("/@alice.rss"));
        assert!(is_feed_path("/@alice.atom"));
        assert!(is_feed_path("/@alice.json"));
        assert!(is_feed_path("/@Alice.rss"));
    }

    #[test]
    fn percent_encoded_spellings_are_rejected() {
        assert!(is_feed_path("/@alice%2erss"));
        assert!(is_feed_path("/@alice%2Eatom"));
        assert!(is_feed_path("/@%61lice%2ejson"));
    }

    #[test]
    fn reserved_escapes_do_not_decode() {
        assert!(!is_feed_path("/@alice%2Frss"));
        assert!(!is_feed_path("/@alice%252erss"));
        assert!(!is_feed_path("/@alice%2e"));
    }

    #[test]
    fn non_acct_paths_are_ignored() {
        assert!(!is_feed_path("/notes/alice.rss"));
        assert!(!is_feed_path("/@alice.rss/extra"));
        assert!(!is_feed_path("/@"));
    }
}
