// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::env;
use std::path::PathBuf;

/// Where the proxy accepts requests from the TLS terminator in front of it.
///
/// A Unix socket keeps the proxy off IP networking entirely (the container
/// can run with `network_mode: none`), which is the deployment this project
/// is written for; TCP stays available for terminators that can't dial a
/// socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenTarget {
    Tcp(String),
    /// `mode` is applied to the socket file after `bind`, since the
    /// terminator (cloudflared, Caddy, ...) usually runs as a different
    /// user than this proxy.
    Unix {
        path: PathBuf,
        mode: u32,
    },
}

/// Runtime configuration, loaded entirely from environment variables.
///
/// There is no config file: the proxy is deployed as a single container with
/// a handful of knobs, matching the "no unnecessary abstraction" scope of
/// this project.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the proxy itself listens (plain HTTP; TLS is terminated
    /// upstream by Caddy/Cloudflare Tunnel/etc., not by this process).
    pub listen: ListenTarget,
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

/// Socket permissions used when `LISTEN_SOCKET_MODE` is unset: readable and
/// writable by anyone, because the process that dials this socket is in
/// another container and its uid is not ours to predict. The socket only
/// exists inside a shared volume, so its reachability is already bounded by
/// which containers mount that volume.
const DEFAULT_SOCKET_MODE: u32 = 0o666;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let listen = parse_listen(
            env::var("LISTEN_ADDR").as_deref().unwrap_or("0.0.0.0:8080"),
            env::var("LISTEN_SOCKET_MODE").ok().as_deref(),
        )?;

        Ok(Self {
            listen,
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

/// `unix:/path/to.sock` selects a Unix socket; anything else is taken as a
/// TCP bind address. `mode` is an octal string (`0666`, `666`) and only
/// applies to the Unix form.
pub fn parse_listen(addr: &str, mode: Option<&str>) -> Result<ListenTarget, String> {
    let mode = match mode {
        Some(raw) => {
            let trimmed = raw.trim().trim_start_matches("0o");
            u32::from_str_radix(trimmed, 8)
                .map_err(|_| format!("LISTEN_SOCKET_MODE is not an octal mode: {raw}"))?
        }
        None => DEFAULT_SOCKET_MODE,
    };

    match addr.strip_prefix("unix:") {
        Some("") => Err("LISTEN_ADDR is `unix:` with no socket path".to_string()),
        Some(path) => Ok(ListenTarget::Unix {
            path: PathBuf::from(path),
            mode,
        }),
        None => Ok(ListenTarget::Tcp(addr.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_address_is_tcp() {
        assert_eq!(
            parse_listen("0.0.0.0:8080", None).unwrap(),
            ListenTarget::Tcp("0.0.0.0:8080".to_string())
        );
    }

    #[test]
    fn unix_prefix_selects_a_socket() {
        assert_eq!(
            parse_listen("unix:/run/egress/egress.sock", None).unwrap(),
            ListenTarget::Unix {
                path: PathBuf::from("/run/egress/egress.sock"),
                mode: DEFAULT_SOCKET_MODE,
            }
        );
    }

    #[test]
    fn socket_mode_is_octal() {
        let ListenTarget::Unix { mode, .. } =
            parse_listen("unix:/run/a.sock", Some("0660")).unwrap()
        else {
            panic!("expected a unix listener");
        };
        assert_eq!(mode, 0o660);
    }

    #[test]
    fn a_bad_socket_mode_is_an_error() {
        assert!(parse_listen("unix:/run/a.sock", Some("rw-rw-rw-")).is_err());
        assert!(parse_listen("unix:/run/a.sock", Some("999")).is_err());
    }

    #[test]
    fn unix_without_a_path_is_an_error() {
        assert!(parse_listen("unix:", None).is_err());
    }
}
