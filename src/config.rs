// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::env;
use std::path::PathBuf;

use crate::media_target::{AllowedPrefix, parse_allowed_prefixes};

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

/// How `/files/*` and `/proxy/*` are answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaMode {
    /// Relay the request to Misskey over the UDS.
    #[default]
    Proxy,
    /// Never hand a media request to Misskey: answer with a redirect (or a
    /// 404) decided locally.
    Redirect,
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
    /// Directory checked for replacements for the bundled landing page.
    /// Mounting a directory here can override `index.html` and
    /// `misskey.svg` without rebuilding the image.
    pub static_dir: PathBuf,
    /// `MEDIA_MODE`: whether media requests are relayed (`proxy`, the
    /// default) or answered locally (`redirect`).
    pub media_mode: MediaMode,
    /// `MEDIA_ALLOWED_PREFIXES`: the URL prefixes a `/proxy/*?url=` request
    /// may be redirected to in `redirect` mode. Always empty in `proxy` mode.
    pub media_allowed_prefixes: Vec<AllowedPrefix>,
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

        let media_mode = parse_media_mode(optional_env("MEDIA_MODE")?.as_deref())?;

        let internal_base_url = env::var("INTERNAL_BASE_URL")
            .map_err(|_| "INTERNAL_BASE_URL env var is required".to_string())?
            .trim_end_matches('/')
            .to_string();

        // `INTERNAL_BASE_URL` is where an internal-`Referer` request is sent,
        // so in `redirect` mode a typo there would turn every such redirect
        // into a 404 at request time; refuse to start instead.
        let media_allowed_prefixes = match media_mode {
            MediaMode::Redirect => {
                validate_internal_base_url(&internal_base_url)?;
                parse_allowed_prefixes(optional_env("MEDIA_ALLOWED_PREFIXES")?.as_deref())?
            }
            MediaMode::Proxy => {
                if optional_env("MEDIA_ALLOWED_PREFIXES")?.is_some_and(|v| !v.trim().is_empty()) {
                    tracing::warn!("MEDIA_ALLOWED_PREFIXES is ignored unless MEDIA_MODE=redirect");
                }
                Vec::new()
            }
        };

        Ok(Self {
            listen,
            misskey_socket: env::var("MISSKEY_SOCKET")
                .map(PathBuf::from)
                .map_err(|_| "MISSKEY_SOCKET env var is required".to_string())?,
            internal_base_url,
            internal_referer_suffix: env::var("INTERNAL_REFERER_SUFFIX")
                .map_err(|_| "INTERNAL_REFERER_SUFFIX env var is required".to_string())?,
            static_dir: env::var("STATIC_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/usr/local/share/misskey-egress-proxy")),
            media_mode,
            media_allowed_prefixes,
        })
    }
}

/// Reads an optional variable, treating a value that is not valid Unicode as
/// an error rather than as "unset": a mode or an allowlist that silently fell
/// back to its default would be worse than a refusal to start.
fn optional_env(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} is not valid Unicode")),
    }
}

/// `MEDIA_MODE`: unset means `proxy`; anything other than `proxy` /
/// `redirect` (case and surrounding whitespace aside) is an error, including
/// the empty string, so a typo does not quietly fall back to the default.
pub fn parse_media_mode(raw: Option<&str>) -> Result<MediaMode, String> {
    let Some(raw) = raw else {
        return Ok(MediaMode::default());
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "proxy" => Ok(MediaMode::Proxy),
        "redirect" => Ok(MediaMode::Redirect),
        _ => Err(format!(
            "MEDIA_MODE must be `proxy` or `redirect`, got {raw:?}"
        )),
    }
}

/// Checks `INTERNAL_BASE_URL` (after its trailing `/` is trimmed) for use as
/// a redirect base: an `http(s)` origin with a host and nothing else.
pub fn validate_internal_base_url(base: &str) -> Result<(), String> {
    let bad = |why: &str| format!("INTERNAL_BASE_URL {base:?} is not usable: {why}");

    if !base.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(bad(
            "it contains whitespace, control or non-ASCII characters",
        ));
    }
    let url = url::Url::parse(base).map_err(|e| bad(&e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("the scheme must be http or https"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(bad("it has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("it must not carry userinfo"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(bad("it must not carry a query or a fragment"));
    }
    if url.path() != "/" {
        return Err(bad("it must not carry a path"));
    }
    Ok(())
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

    #[test]
    fn media_mode_defaults_to_proxy_and_parses_both_modes() {
        assert_eq!(parse_media_mode(None).unwrap(), MediaMode::Proxy);
        assert_eq!(parse_media_mode(Some("proxy")).unwrap(), MediaMode::Proxy);
        assert_eq!(
            parse_media_mode(Some("redirect")).unwrap(),
            MediaMode::Redirect
        );
        assert_eq!(
            parse_media_mode(Some(" Redirect ")).unwrap(),
            MediaMode::Redirect
        );
    }

    #[test]
    fn an_unknown_or_empty_media_mode_is_an_error_that_names_the_variable() {
        for raw in ["redirct", "", "off"] {
            let err = parse_media_mode(Some(raw)).unwrap_err();
            assert!(err.contains("MEDIA_MODE"), "{raw:?}: {err}");
        }
    }

    #[test]
    fn a_plain_origin_is_a_usable_internal_base_url() {
        for base in [
            "https://h",
            "https://h:8443",
            "http://100.64.0.1:3000",
            "https://misskey.your-tailnet.ts.net",
        ] {
            assert!(validate_internal_base_url(base).is_ok(), "{base}");
        }
    }

    #[test]
    fn an_internal_base_url_with_anything_beyond_an_origin_is_refused() {
        for base in [
            "h",
            "//h",
            "ftp://h",
            "https://user@h",
            "https://user:pw@h",
            "https://h?x",
            "https://h#x",
            "https://h/p",
            "https:///",
            "https://h ",
            "https://h\n",
        ] {
            let err = validate_internal_base_url(base).unwrap_err();
            assert!(err.contains("INTERNAL_BASE_URL"), "{base:?}: {err}");
        }
    }
}
