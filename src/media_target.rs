// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

/// One entry of `MEDIA_ALLOWED_PREFIXES`, already normalised the way a
/// candidate URL is (lowercase host, punycode, default port made explicit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedPrefix {
    pub host: String,
    pub port: u16,
    /// Starts with `/` and ends with `/`.
    pub path: String,
}
