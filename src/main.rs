// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use axum::Router;

use misskey_egress_proxy::config::{Config, ListenTarget};
use misskey_egress_proxy::proxy::{ProxyState, build_client};
use misskey_egress_proxy::routes;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(err) => {
            eprintln!("config error: {err}");
            std::process::exit(1);
        }
    };

    let proxy_state = ProxyState {
        client: build_client(),
        socket: config.misskey_socket.clone(),
    };

    let app = routes::build(proxy_state, config.clone());

    match &config.listen {
        ListenTarget::Tcp(addr) => {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));

            tracing::info!(
                addr = %addr,
                socket = %config.misskey_socket.display(),
                media_mode = ?config.media_mode,
                media_allowed_prefixes = config.media_allowed_prefixes.len(),
                "misskey-egress-proxy listening"
            );

            serve(listener, app).await;
        }
        ListenTarget::Unix { path, mode } => {
            let listener = bind_unix(path, *mode);

            tracing::info!(
                addr = %format!("unix:{}", path.display()),
                socket = %config.misskey_socket.display(),
                media_mode = ?config.media_mode,
                media_allowed_prefixes = config.media_allowed_prefixes.len(),
                "misskey-egress-proxy listening"
            );

            serve(listener, app).await;
        }
    }
}

async fn serve<L>(listener: L, app: Router)
where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
{
    axum::serve(listener, app).await.expect("server error");
}

/// Binds the listening socket, clearing a stale one left behind by an
/// unclean exit first: a Unix socket file outlives the process that made
/// it, and `bind` on an existing path fails with `EADDRINUSE`.
///
/// The mode is set after `bind` because the umask applies during it, and
/// the container that dials this socket runs as a different user.
fn bind_unix(path: &Path, mode: u32) -> tokio::net::UnixListener {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_socket() {
            let _ = fs::remove_file(path);
        } else {
            panic!(
                "refusing to bind {}: path exists and is not a socket",
                path.display()
            );
        }
    }

    let listener = tokio::net::UnixListener::bind(path)
        .unwrap_or_else(|err| panic!("failed to bind unix:{}: {err}", path.display()));

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|err| panic!("failed to chmod {} to {mode:o}: {err}", path.display()));

    listener
}
