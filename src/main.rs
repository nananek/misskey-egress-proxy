// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;

use misskey_egress_proxy::config::Config;
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

    let listener = tokio::net::TcpListener::bind(&config.listen_addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {}: {err}", config.listen_addr));

    tracing::info!(
        addr = %config.listen_addr,
        socket = %config.misskey_socket.display(),
        "misskey-egress-proxy listening"
    );

    axum::serve(listener, app).await.expect("server error");
}
