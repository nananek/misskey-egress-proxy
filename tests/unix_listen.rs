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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    spawn_counting_mock_misskey(socket);
}

/// Same stand-in, but counts the connections it accepts, so a test can assert
/// that a request never reached the upstream at all.
fn spawn_counting_mock_misskey(socket: &Path) -> Arc<AtomicUsize> {
    let listener = UnixListener::bind(socket).expect("bind mock Misskey socket");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    connections
}

fn spawn_proxy(listen: &Path, misskey: &Path) -> Child {
    spawn_proxy_with(listen, misskey, &[])
}

/// The proxy command with its fixed environment; `extra_env` is applied on
/// top, so it can set `MEDIA_MODE` and friends (or override a fixed value).
fn proxy_command(listen: &Path, misskey: &Path, extra_env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_misskey-egress-proxy"));
    command
        .env("LISTEN_ADDR", format!("unix:{}", listen.display()))
        .env("LISTEN_SOCKET_MODE", "0666")
        .env("MISSKEY_SOCKET", misskey)
        .env("INTERNAL_BASE_URL", "https://internal.example.ts.net")
        .env("INTERNAL_REFERER_SUFFIX", ".internal.example.ts.net");
    for (name, value) in extra_env {
        command.env(name, value);
    }
    command
}

fn spawn_proxy_with(listen: &Path, misskey: &Path, extra_env: &[(&str, &str)]) -> Child {
    proxy_command(listen, misskey, extra_env)
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

/// Writes `request` exactly as given and returns whatever comes back until
/// the proxy closes the connection. A read error after a rejected request
/// (the proxy closing with unread bytes) is not a failure of the exchange, so
/// it just ends the read.
fn http_raw(socket: &Path, request: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(socket).expect("connect to egress socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request).expect("write request");
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
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

/// Issue #3: a rejection must not close the connection while the caller is
/// still writing the request body. Deterministic form: a chunked body with
/// no terminal chunk yet — the proxy must stay quiet until the body is
/// complete, then answer with the real status.
#[test]
fn rejections_wait_for_the_request_body_before_answering() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    spawn_mock_misskey(&misskey);
    let mut proxy = spawn_proxy(&listen, &misskey);
    wait_until_connectable(&listen);

    for (method, path, extra_headers, expected) in [
        ("POST", "/api/meta", "", "HTTP/1.1 404 Not Found"),
        (
            "POST",
            "/.well-known/nodeinfo",
            "",
            "HTTP/1.1 405 Method Not Allowed",
        ),
        ("GET", "/@alice.rss", "", "HTTP/1.1 404 Not Found"),
        // The media redirect and the landing page drain too: a GET carrying
        // a body must not be cut off by their non-rejection responses.
        (
            "GET",
            "/files/x",
            "Referer: https://caller.internal.example.ts.net/\r\n",
            "HTTP/1.1 302 Found",
        ),
        ("GET", "/", "", "HTTP/1.1 200 OK"),
        ("GET", "/assets/misskey.svg", "", "HTTP/1.1 200 OK"),
    ] {
        let mut stream = UnixStream::connect(&listen).expect("connect");
        stream
            .write_all(
                format!(
                    "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\nConnection: close\r\n{extra_headers}Transfer-Encoding: chunked\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write request head");
        stream
            .write_all(b"7\r\n{\"a\":1}\r\n")
            .expect("write one chunk");

        // No terminal chunk yet: the proxy must be waiting on the body, not
        // answering and closing.
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut buf = [0u8; 64];
        match stream.read(&mut buf) {
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Ok(0) => panic!("{method} {path}: closed before the body was complete"),
            Ok(n) => panic!(
                "{method} {path}: answered {:?} before the body was complete",
                String::from_utf8_lossy(&buf[..n])
            ),
            Err(e) => panic!("{method} {path}: unexpected read error: {e}"),
        }

        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(b"0\r\n\r\n").expect("terminate the body");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        assert!(
            response.starts_with(expected),
            "{method} {path}: expected {expected}, got {response}"
        );
    }

    cleanup(&mut proxy, &listen, &misskey);
}

/// W1 (dependency_behavior DB-H*): a raw control character or space in the
/// request-target never gets past hyper's request-line parser, so the proxy
/// answers 400 and the upstream is never dialled. This is what lets the
/// path validators assume such bytes cannot appear in a `path_and_query`.
#[test]
fn a_control_character_or_space_in_the_target_is_a_400_and_never_reaches_misskey() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream_connections = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy(&listen, &misskey);
    wait_until_connectable(&listen);

    for (name, byte) in [
        ("TAB", 0x09u8),
        ("LF", 0x0a),
        ("NUL", 0x00),
        ("space", 0x20),
        ("DEL", 0x7f),
    ] {
        let mut request = b"GET /files/a".to_vec();
        request.push(byte);
        request.extend_from_slice(b"b HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");

        let response = http_raw(&listen, &request);
        assert!(
            response.starts_with(b"HTTP/1.1 400"),
            "raw {name} in the target: expected 400, got {:?}",
            String::from_utf8_lossy(&response)
        );
    }
    assert_eq!(
        upstream_connections.load(Ordering::SeqCst),
        0,
        "a malformed target must not reach the upstream"
    );

    cleanup(&mut proxy, &listen, &misskey);
}

/// W2: a raw `\xff` (not valid UTF-8) in the request-target is refused with a
/// 4xx and the upstream is never dialled.
#[test]
fn an_invalid_utf8_byte_in_the_target_is_a_4xx_and_never_reaches_misskey() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream_connections = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy(&listen, &misskey);
    wait_until_connectable(&listen);

    let response = http_raw(
        &listen,
        b"GET /files/a\xffb HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    );
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 4"),
        "raw 0xff in the target: expected a 4xx, got {text:?}"
    );
    assert_eq!(
        upstream_connections.load(Ordering::SeqCst),
        0,
        "a malformed target must not reach the upstream"
    );

    cleanup(&mut proxy, &listen, &misskey);
}

// ---------------------------------------------------------------------------
// MEDIA_MODE=redirect, against the real binary
// ---------------------------------------------------------------------------
//
// Each test counts the connections the mock Misskey accepts, so "never
// reached the upstream" is asserted directly rather than inferred.

const REDIRECT_ENV: &[(&str, &str)] = &[
    ("MEDIA_MODE", "redirect"),
    ("MEDIA_ALLOWED_PREFIXES", "https://s3.example.com/bucket/"),
];

const INTERNAL_REFERER: &str = "Referer: https://misskey.internal.example.ts.net/\r\n";

/// A request with the given target and extra header lines (each ending in
/// `\r\n`), read back as text.
fn request(socket: &Path, target: &str, extra_headers: &str) -> String {
    let raw =
        format!("GET {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{extra_headers}\r\n");
    String::from_utf8_lossy(&http_raw(socket, raw.as_bytes())).into_owned()
}

fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or("")
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// W3: the request-target's authority is ignored (only path and query are
/// read), and a request that is redirected never reaches the upstream.
#[test]
fn redirect_mode_answers_an_absolute_form_proxy_request_locally() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy_with(&listen, &misskey, REDIRECT_ENV);
    wait_until_connectable(&listen);

    let response = request(
        &listen,
        "http://evil.example/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "",
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 302"),
        "{response}"
    );
    assert_eq!(
        header(&response, "location"),
        Some("https://s3.example.com/bucket/a.png"),
        "{response}"
    );
    assert_eq!(upstream.load(Ordering::SeqCst), 0);

    cleanup(&mut proxy, &listen, &misskey);
}

/// W4: shapes HTTP carries that the `url` rules refuse: a raw `\` in the
/// query, and an encoded `#` in the value. The upstream is not dialled.
#[test]
fn redirect_mode_refuses_url_values_the_rules_forbid() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy_with(&listen, &misskey, REDIRECT_ENV);
    wait_until_connectable(&listen);

    for target in [
        // A raw `\` where WHATWG would read a `/`.
        "/proxy/x?url=https:\\\\s3.example.com\\bucket\\a.png",
        "/proxy/x?url=https://s3.example.com\\@evil.example/bucket/a.png",
        // An encoded `#` in the value.
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png%23frag",
        // Not on the allowlist, and userinfo dressed as the allowed host.
        "/proxy/x?url=https%3A%2F%2Fevil.example%2Fbucket%2Fa.png",
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%40evil.example%2Fbucket%2Fa.png",
    ] {
        let response = request(&listen, target, "");
        assert!(
            status_line(&response).starts_with("HTTP/1.1 404"),
            "{target}: {response}"
        );
        assert_eq!(header(&response, "location"), None, "{target}");
    }
    assert_eq!(upstream.load(Ordering::SeqCst), 0);

    cleanup(&mut proxy, &listen, &misskey);
}

/// A `#` written raw in the target never gets that far: HTTP does not carry a
/// fragment, and `http` cuts it off the request-target, so what is judged
/// (and redirected to) is the URL without it.
#[test]
fn a_raw_fragment_in_the_target_is_dropped_before_the_rules_see_it() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy_with(&listen, &misskey, REDIRECT_ENV);
    wait_until_connectable(&listen);

    let response = request(
        &listen,
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png#frag",
        "",
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 302"),
        "{response}"
    );
    assert_eq!(
        header(&response, "location"),
        Some("https://s3.example.com/bucket/a.png"),
        "the fragment must not survive into the Location: {response}"
    );
    assert_eq!(upstream.load(Ordering::SeqCst), 0);

    cleanup(&mut proxy, &listen, &misskey);
}

/// W5: a bad `MEDIA_MODE` or `MEDIA_ALLOWED_PREFIXES` (or, in redirect mode,
/// `INTERNAL_BASE_URL`) stops the process at startup and names what was wrong.
#[test]
fn a_bad_media_setting_stops_the_proxy_at_startup_and_names_it() {
    for (env, expected_in_stderr) in [
        (vec![("MEDIA_MODE", "bogus")], vec!["MEDIA_MODE", "bogus"]),
        (vec![("MEDIA_MODE", "")], vec!["MEDIA_MODE"]),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("MEDIA_ALLOWED_PREFIXES", "http://s3.example.com/"),
            ],
            vec!["MEDIA_ALLOWED_PREFIXES", "http://s3.example.com/"],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("MEDIA_ALLOWED_PREFIXES", "https://127.0.0.1/"),
            ],
            vec!["MEDIA_ALLOWED_PREFIXES", "https://127.0.0.1/"],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("MEDIA_ALLOWED_PREFIXES", "s3.example.com"),
            ],
            vec!["MEDIA_ALLOWED_PREFIXES", "s3.example.com", "https://"],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                (
                    "INTERNAL_BASE_URL",
                    "https://internal.example.ts.net/prefix",
                ),
            ],
            vec!["INTERNAL_BASE_URL"],
        ),
        // The base is judged as written: what the `url` crate would normalise
        // these to is not what goes into a `Location`.
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("INTERNAL_BASE_URL", "https://internal.example.ts.net/.."),
            ],
            vec!["INTERNAL_BASE_URL", "/.."],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                (
                    "INTERNAL_BASE_URL",
                    "https://internal.example.ts.net:000443",
                ),
            ],
            vec!["INTERNAL_BASE_URL", "000443"],
        ),
        // Nothing serves TLS on port 0.
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("MEDIA_ALLOWED_PREFIXES", "https://host.example:0/"),
            ],
            vec!["MEDIA_ALLOWED_PREFIXES", "https://host.example:0/"],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("INTERNAL_BASE_URL", "https://internal.example.ts.net:0"),
            ],
            vec!["INTERNAL_BASE_URL", ":0"],
        ),
        // An empty suffix would make every Referer internal; refused in both modes.
        (
            vec![("INTERNAL_REFERER_SUFFIX", "")],
            vec!["INTERNAL_REFERER_SUFFIX"],
        ),
        (
            vec![
                ("MEDIA_MODE", "redirect"),
                ("INTERNAL_REFERER_SUFFIX", "  "),
            ],
            vec!["INTERNAL_REFERER_SUFFIX"],
        ),
    ] {
        let misskey = unique_path("misskey");
        let listen = unique_path("egress");
        let mut child = proxy_command(&listen, &misskey, &env)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn misskey-egress-proxy");

        // A process that is still running after the deadline did start, which
        // is the failure being tested for: report it instead of waiting on a
        // server that will never exit.
        let deadline = Instant::now() + Duration::from_secs(10);
        let exit_status = loop {
            if let Some(status) = child.try_wait().expect("poll the proxy") {
                break Some(status);
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        let _ = child
            .stderr
            .take()
            .expect("piped stderr")
            .read_to_string(&mut stderr);
        let started = exit_status.is_none();
        let _ = std::fs::remove_file(&listen);

        assert!(
            exit_status.is_some_and(|status| !status.success()),
            "{env:?} should refuse to start (it {})",
            if started { "kept running" } else { "exited 0" }
        );
        for needle in expected_in_stderr {
            assert!(
                stderr.contains(needle),
                "{env:?}: {needle:?} not in {stderr}"
            );
        }
    }
}

/// In `proxy` mode `MEDIA_ALLOWED_PREFIXES` is ignored, not validated: a value
/// that would stop a redirect-mode process does not stop this one, and media
/// is relayed as before.
#[test]
fn proxy_mode_ignores_the_allowed_prefixes() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy_with(
        &listen,
        &misskey,
        &[("MEDIA_ALLOWED_PREFIXES", "http://not-even-valid")],
    );
    wait_until_connectable(&listen);

    let response = request(
        &listen,
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
        "",
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{response}"
    );
    assert_eq!(upstream.load(Ordering::SeqCst), 1);

    cleanup(&mut proxy, &listen, &misskey);
}

/// Runs the proxy with `RUST_LOG=warn` until it listens, stops it, and returns
/// everything it wrote to stdout and stderr (`tracing_subscriber::fmt` logs to
/// stdout).
fn startup_log_at_warn(extra_env: &[(&str, &str)]) -> String {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    spawn_mock_misskey(&misskey);
    let mut env = vec![("RUST_LOG", "warn")];
    env.extend_from_slice(extra_env);
    let mut proxy = proxy_command(&listen, &misskey, &env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn misskey-egress-proxy");
    wait_until_connectable(&listen);
    let _ = proxy.kill();
    let _ = proxy.wait();

    let mut log = String::new();
    let _ = proxy
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_string(&mut log);
    let _ = proxy
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut log);
    let _ = std::fs::remove_file(&listen);
    let _ = std::fs::remove_file(&misskey);
    log
}

/// The README and `docs/routes.md` promise a `warn` when `MEDIA_ALLOWED_PREFIXES`
/// is set in `proxy` mode, where it does nothing. It appears then, and not when
/// the variable is unset, blank, or actually in use.
#[test]
fn proxy_mode_warns_only_when_the_allowed_prefixes_are_set_to_something() {
    const WARNING: &str = "MEDIA_ALLOWED_PREFIXES is ignored";
    let set = ("MEDIA_ALLOWED_PREFIXES", "https://s3.example.com/bucket/");

    let log = startup_log_at_warn(&[set]);
    assert!(log.contains(WARNING), "no warning in {log:?}");

    for (case, env) in [
        ("unset", vec![]),
        ("blank", vec![("MEDIA_ALLOWED_PREFIXES", " ")]),
        ("redirect mode", vec![("MEDIA_MODE", "redirect"), set]),
    ] {
        let log = startup_log_at_warn(&env);
        assert!(!log.contains(WARNING), "{case}: {log:?}");
    }
}

/// `INTERNAL_REFERER_SUFFIX` is trimmed and lowercased when it is read: the
/// host of a `Referer` reaches the matcher lowercased, so a capital letter or a
/// space around the value would otherwise switch the internal redirect off
/// without a word. And a suffix written without its leading dot matches on a
/// dot boundary. Redirect mode, so a request that is not internal is a 404
/// and the upstream is never dialled.
#[test]
fn the_referer_suffix_is_normalised_when_read_and_matched_on_a_dot_boundary() {
    for (suffix, referer, internal) in [
        (
            " .INTERNAL.Example.TS.net\t",
            "https://misskey.internal.example.ts.net/",
            true,
        ),
        (
            "Internal.Example.ts.NET",
            "https://misskey.internal.example.ts.net/",
            true,
        ),
        (
            "internal.example.ts.net",
            "https://internal.example.ts.net/",
            true,
        ),
        (
            "internal.example.ts.net",
            "https://notinternal.example.ts.net/",
            false,
        ),
        (
            ".internal.example.ts.net",
            "https://notinternal.example.ts.net/",
            false,
        ),
    ] {
        let misskey = unique_path("misskey");
        let listen = unique_path("egress");
        let upstream = spawn_counting_mock_misskey(&misskey);
        let mut proxy = spawn_proxy_with(
            &listen,
            &misskey,
            &[
                ("MEDIA_MODE", "redirect"),
                ("INTERNAL_REFERER_SUFFIX", suffix),
            ],
        );
        wait_until_connectable(&listen);

        let response = request(&listen, "/files/x", &format!("Referer: {referer}\r\n"));
        let expected = if internal {
            "HTTP/1.1 302"
        } else {
            "HTTP/1.1 404"
        };
        assert!(
            status_line(&response).starts_with(expected),
            "suffix {suffix:?}, Referer {referer}: {response}"
        );
        if internal {
            assert_eq!(
                header(&response, "location"),
                Some("https://internal.example.ts.net/files/x"),
                "suffix {suffix:?}: {response}"
            );
        }
        assert_eq!(upstream.load(Ordering::SeqCst), 0);

        cleanup(&mut proxy, &listen, &misskey);
    }
}

/// W6: redirect mode with no `MEDIA_ALLOWED_PREFIXES` at all starts, refuses
/// every original URL and every `/files/*` without an internal Referer, and
/// still redirects an internal one. The upstream is never dialled.
#[test]
fn redirect_mode_without_an_allowlist_starts_and_refuses_everything_external() {
    let misskey = unique_path("misskey");
    let listen = unique_path("egress");
    let upstream = spawn_counting_mock_misskey(&misskey);
    let mut proxy = spawn_proxy_with(&listen, &misskey, &[("MEDIA_MODE", "redirect")]);
    wait_until_connectable(&listen);

    for target in [
        "/files/x",
        "/proxy/x?url=https%3A%2F%2Fs3.example.com%2Fbucket%2Fa.png",
    ] {
        let response = request(&listen, target, "");
        assert!(
            status_line(&response).starts_with("HTTP/1.1 404"),
            "{target}: {response}"
        );
        assert_eq!(header(&response, "location"), None, "{target}");
        // Over the wire too: a shared cache in front must not keep it.
        assert_eq!(
            header(&response, "cache-control"),
            Some("no-store"),
            "{target}: {response}"
        );
    }

    let response = request(&listen, "/files/x", INTERNAL_REFERER);
    assert!(
        status_line(&response).starts_with("HTTP/1.1 302"),
        "{response}"
    );
    assert_eq!(
        header(&response, "location"),
        Some("https://internal.example.ts.net/files/x"),
        "{response}"
    );
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(upstream.load(Ordering::SeqCst), 0);

    cleanup(&mut proxy, &listen, &misskey);
}
