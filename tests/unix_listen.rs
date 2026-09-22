// SPDX-FileCopyrightText: misskey-egress-proxy contributors
// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end coverage for the Unix-socket listen path
//! (`LISTEN_ADDR=unix:...`), which the compose files — production and the
//! federation test — actually use. Spawns the real binary against a
//! throwaway mock Misskey UDS and speaks HTTP to it over the egress socket.
//!
//! `tests/router.rs` covers routing in-process; this covers `main.rs`'s
//! bind/chmod/stale-socket handling, which in-process tests can't reach.

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn unique_path(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "mep-uds-{prefix}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ))
}

/// Minimal Misskey stand-in: answers every request with 200 and a fixed body.
fn spawn_mock_misskey(socket: &Path) {
    let listener = UnixListener::bind(socket).expect("bind mock Misskey socket");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
}

fn spawn_proxy(listen: &Path, misskey: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_misskey-egress-proxy"))
        .env("LISTEN_ADDR", format!("unix:{}", listen.display()))
        .env("LISTEN_SOCKET_MODE", "0666")
        .env("MISSKEY_SOCKET", misskey)
        .env("INTERNAL_BASE_URL", "https://internal.example.ts.net")
        .env("INTERNAL_REFERER_SUFFIX", ".internal.example.ts.net")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn misskey-egress-proxy")
}

/// Readiness is "connect succeeds", not "the path exists": in the stale
/// socket test the path exists from the start, and before the proxy rebinds
/// it a connect there gets ECONNREFUSED.
fn wait_until_connectable(path: &Path) {
    for _ in 0..500 {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("proxy never started listening on {}", path.display());
}

fn http_get(socket: &Path, path: &str) -> String {
    let mut stream = UnixStream::connect(socket).expect("connect to egress socket");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

fn cleanup(proxy: &mut Child, listen: &Path, misskey: &Path) {
    let _ = proxy.kill();
    let _ = proxy.wait();
    let _ = std::fs::remove_file(listen);
    let _ = std::fs::remove_file(misskey);
}

#[test]
fn serves_http_over_the_listening_unix_socket() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    spawn_mock_misskey(&misskey);
    let mut proxy = spawn_proxy(&listen, &misskey);

    wait_until_connectable(&listen);

    let response = http_get(&listen, "/nodeinfo/2.0");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "unexpected response: {response}"
    );

    // The mode is applied after bind so the terminator — a different uid —
    // can dial the socket.
    let mode = std::fs::metadata(&listen).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o666, "socket mode must be the configured 0666");

    cleanup(&mut proxy, &listen, &misskey);
}

#[test]
fn replaces_a_stale_socket_left_by_an_unclean_exit() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    spawn_mock_misskey(&misskey);

    // A dropped listener leaves the socket file behind; a plain bind on it
    // would fail with EADDRINUSE.
    drop(UnixListener::bind(&listen).expect("bind stale socket"));
    assert!(std::fs::metadata(&listen).unwrap().file_type().is_socket());

    let mut proxy = spawn_proxy(&listen, &misskey);
    wait_until_connectable(&listen);

    let response = http_get(&listen, "/nodeinfo/2.0");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "unexpected response: {response}"
    );

    cleanup(&mut proxy, &listen, &misskey);
}

#[test]
fn refuses_to_clobber_a_non_socket_path() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    spawn_mock_misskey(&misskey);

    std::fs::write(&listen, b"not a socket").unwrap();

    let status = spawn_proxy(&listen, &misskey).wait().unwrap();
    assert!(
        !status.success(),
        "the proxy must refuse to bind a path that is not a socket"
    );
    assert_eq!(std::fs::read(&listen).unwrap(), b"not a socket");

    let _ = std::fs::remove_file(&listen);
    let _ = std::fs::remove_file(&misskey);
}
