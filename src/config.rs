// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::env;
use std::path::PathBuf;

/// Runtime configuration, loaded entirely from environment variables.
///
/// There is no config file: the proxy is deployed as a single container with
/// a handful of knobs, matching the "no unnecessary abstraction" scope of
/// this project.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the proxy itself listens on (plain HTTP; TLS is terminated
    /// upstream by Caddy/Cloudflare Tunnel/etc., not by this process).
    pub listen_addr: String,
    /// Path to the Misskey backend's Unix domain socket.
    pub misskey_socket: PathBuf,
    /// Base URL of the internal (Tailscale-reachable) Misskey deployment,
    /// e.g. `https://misskey.tailnet-name.ts.net`. Used only to build the
    /// redirect target for `/files/*` and `/proxy/*` when the request looks
    /// like it came from an internal caller.
    pub internal_base_url: String,
    /// Hostname suffix that identifies an internal `Referer`, e.g.
    /// `.tailnet-name.ts.net`. Any `Referer` whose host ends with this
    /// suffix is treated as internal and redirected instead of proxied.
    pub internal_referer_suffix: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            misskey_socket: env::var("MISSKEY_SOCKET")
                .map(PathBuf::from)
                .map_err(|_| "MISSKEY_SOCKET env var is required".to_string())?,
            internal_base_url: env::var("INTERNAL_BASE_URL")
                .map_err(|_| "INTERNAL_BASE_URL env var is required".to_string())?
                .trim_end_matches('/')
                .to_string(),
            internal_referer_suffix: env::var("INTERNAL_REFERER_SUFFIX")
                .map_err(|_| "INTERNAL_REFERER_SUFFIX env var is required".to_string())?,
        })
    }
}
