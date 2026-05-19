//! Integration smoke test for `concordance auth login` (commit 1 / v0.4).
//!
//! Drives the listener end-to-end **without** the wallet step:
//!
//! 1. Spawn the `concordance auth login` subprocess with `HOME` pointed
//!    at a tempdir, so no real on-disk state is touched.
//! 2. Set `CONCORDANCE_LOGIN_NO_BROWSER=1` so the subprocess doesn't
//!    actually launch a browser (CI / headless dev box hostility).
//! 3. Set `CONCORDANCE_LOGIN_FIXED_PORT` to a freshly-bound-then-released
//!    port we agree on out-of-band, so the test knows where to hit.
//! 4. Wait for the listener to come up (probe with TcpStream::connect).
//! 5. Scrape the session token from `/auth?k=<token>` — the listener
//!    redirects us to it on stderr, so we read stderr line-by-line.
//! 6. POST `/done` with the token. Assert the subprocess exits 0.
//!
//! No CIP-30, no Ekklesia API, no real JWT — just the listener +
//! browser-launch + shutdown plumbing the commit 1 PR ships.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bind ephemerally, capture the port, then drop the listener so the
/// subprocess can re-bind it. There's an inherent TOCTOU window here
/// (another process could grab the port between drop and re-bind);
/// for a single-machine test it's vanishingly unlikely and the cost
/// of a port collision is one flake — acceptable.
fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = l.local_addr().expect("local_addr").port();
    drop(l);
    port
}

/// Probe `127.0.0.1:<port>` until either it accepts a TCP connection
/// or `deadline` elapses. Returns true on connect, false on timeout.
fn wait_for_listener(port: u16, deadline: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.shutdown(Shutdown::Both);
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Spin until the subprocess's stderr emits a line containing
/// `http://localhost:<port>/auth?k=...` (the helper URL). Returns the
/// session token from the query string.
fn read_session_token_from_stderr<R: Read + Send + 'static>(
    stderr: R,
    deadline: Duration,
) -> Option<String> {
    use std::sync::{Arc, Mutex};
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(|r| r.ok()) {
            // Match the helper URL line. Look for "k=" so this is robust
            // to wording changes in the surrounding stderr text.
            if let Some(idx) = line.find("/auth?k=") {
                let after = &line[idx + "/auth?k=".len()..];
                // Token is alphanumeric (see `random_session_token`); take
                // the leading run of [A-Za-z0-9] so trailing chars like
                // `"`, whitespace, or punctuation don't leak in.
                let token: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                if !token.is_empty() {
                    *captured_clone.lock().unwrap() = Some(token);
                    return;
                }
            }
        }
    });
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Some(t) = captured.lock().unwrap().clone() {
            return Some(t);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[test]
fn auth_login_listener_handshake_round_trips() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        // Test-only env vars; documented next to `LoginOptions` so a
        // contributor wiring a new option knows the override surface.
        .env("CONCORDANCE_LOGIN_NO_BROWSER", "1")
        .env("CONCORDANCE_LOGIN_FIXED_PORT", port.to_string())
        // Cap the deadline so a wedged test fails fast instead of
        // chewing 5 real minutes of CI time. The flow itself is the
        // unit under test, not the timeout's accuracy.
        .env("CONCORDANCE_LOGIN_DEADLINE_SECS", "10")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concordance auth login");

    let stderr = child.stderr.take().expect("stderr piped");
    // Wait for the listener to actually accept connections before we
    // try to scrape the URL — otherwise we race the bind.
    assert!(
        wait_for_listener(port, Duration::from_secs(5)),
        "listener never bound on port {port}"
    );

    let token = read_session_token_from_stderr(stderr, Duration::from_secs(3))
        .expect("session token in stderr");
    assert_eq!(token.len(), 32, "session token wrong length: {token:?}");
    assert!(
        token.chars().all(|c| c.is_ascii_alphanumeric()),
        "session token has non-alphanumeric chars: {token:?}"
    );

    // Hit /auth first as a smoke test on the GET path: the page must
    // render and embed the same token in its body.
    let body = http_get(port, &format!("/auth?k={token}")).expect("GET /auth");
    assert!(
        body.contains(&token),
        "rendered page should embed session token, body excerpt: {:?}",
        &body[..body.len().min(160)]
    );
    assert!(
        body.contains("Concordance"),
        "rendered page should mention Concordance, body excerpt: {:?}",
        &body[..body.len().min(160)]
    );

    // The /auth path with a wrong token should still serve the page but
    // strip the embedded token (so subsequent /done from that page is
    // refused). The page bytes themselves are public — there's no secret
    // in the HTML.
    let stripped_body = http_get(port, "/auth?k=wrong-token").expect("GET /auth?k=wrong");
    assert!(
        !stripped_body.contains(&token),
        "page served with wrong token must not embed the real one"
    );

    // POST /done with a bogus token must be refused.
    let (status, _body) = http_post_json(
        port,
        "/done",
        r#"{"sessionToken":"definitely-not-the-real-one"}"#,
    )
    .expect("POST /done with bad token");
    assert_eq!(status, 403, "bad session token should be rejected with 403");

    // Now POST /done with the correct token. The listener should accept
    // it and the subprocess should exit cleanly.
    let (status, _body) =
        http_post_json(port, "/done", &format!(r#"{{"sessionToken":"{token}"}}"#))
            .expect("POST /done with good token");
    assert_eq!(status, 200, "valid /done should return 200");

    // The subprocess should now exit promptly.
    let exit = child
        .wait_timeout_or_kill(Duration::from_secs(5))
        .expect("subprocess should exit after /done");
    assert!(
        exit.success(),
        "subprocess exit status was {exit:?}, expected success"
    );
}

#[test]
fn auth_login_init_then_done_surfaces_stake_address_in_stdout() {
    // Commit 2 adds the `/init` route, which captures the stake address
    // the helper page reads from the wallet. The CLI then echoes it on
    // stdout once the listener shuts down. This test simulates the
    // page's behaviour: POST /init with a valid mainnet stake address,
    // then POST /done, then assert the CLI's stdout mentions the
    // address and wallet name.
    let tmp = tempfile::tempdir().expect("create tempdir");
    let port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .env("CONCORDANCE_LOGIN_NO_BROWSER", "1")
        .env("CONCORDANCE_LOGIN_FIXED_PORT", port.to_string())
        .env("CONCORDANCE_LOGIN_DEADLINE_SECS", "10")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concordance auth login");

    // The stderr reader thread (in `read_session_token_from_stderr`)
    // already drains stderr. We need a second strategy if we want to
    // inspect stderr lines later — for this test we don't, so it's
    // safe to read everything in one consumer.
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout = child.stdout.take().expect("stdout piped");
    assert!(
        wait_for_listener(port, Duration::from_secs(5)),
        "listener never bound on port {port}"
    );
    let token = read_session_token_from_stderr(stderr, Duration::from_secs(3))
        .expect("session token in stderr");

    // Use the same fixture address as the JWT decoder test — it's a
    // real well-formed mainnet stake address (length, prefix, allowed
    // chars all correct). The CLI only does shape validation.
    let stake = "stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt";

    // POST /init with the wrong session token → 403.
    let (status, _body) = http_post_json(
        port,
        "/init",
        &format!(r#"{{"sessionToken":"nope","stakeAddr":"{stake}","walletName":"Lace"}}"#),
    )
    .expect("POST /init bad token");
    assert_eq!(status, 403, "bad token on /init must be 403");

    // POST /init with a structurally-broken stake address → 400.
    let (status, _body) = http_post_json(
        port,
        "/init",
        &format!(r#"{{"sessionToken":"{token}","stakeAddr":"garbage"}}"#),
    )
    .expect("POST /init bad stake addr");
    assert_eq!(status, 400, "non-bech32 stake addr must be 400");

    // Real POST /init.
    let (status, body) = http_post_json(
        port,
        "/init",
        &format!(r#"{{"sessionToken":"{token}","stakeAddr":"{stake}","walletName":"Lace"}}"#),
    )
    .expect("POST /init success");
    assert_eq!(
        status, 200,
        "valid /init must be 200, got {status} (body: {body:?})"
    );

    // Now /done — the CLI should print the captured stake addr on stdout
    // before exiting.
    let (status, _body) =
        http_post_json(port, "/done", &format!(r#"{{"sessionToken":"{token}"}}"#))
            .expect("POST /done");
    assert_eq!(status, 200);

    let exit = child
        .wait_timeout_or_kill(Duration::from_secs(5))
        .expect("subprocess should exit");
    assert!(exit.success(), "exit {exit:?}");

    // Drain stdout — the success message must mention the address +
    // wallet name. Reading after wait is fine because the pipe is
    // buffered.
    let mut stdout_buf = String::new();
    use std::io::Read as _;
    let mut stdout = stdout;
    stdout
        .read_to_string(&mut stdout_buf)
        .expect("read stdout");
    assert!(
        stdout_buf.contains(stake),
        "stdout should mention the stake address, got: {stdout_buf:?}"
    );
    assert!(
        stdout_buf.contains("Lace"),
        "stdout should mention the wallet name, got: {stdout_buf:?}"
    );
}

#[test]
fn auth_login_rejects_non_loopback_host_header() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .env("CONCORDANCE_LOGIN_NO_BROWSER", "1")
        .env("CONCORDANCE_LOGIN_FIXED_PORT", port.to_string())
        .env("CONCORDANCE_LOGIN_DEADLINE_SECS", "10")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concordance auth login");

    let stderr = child.stderr.take().expect("stderr piped");
    assert!(
        wait_for_listener(port, Duration::from_secs(5)),
        "listener never bound on port {port}"
    );
    let token = read_session_token_from_stderr(stderr, Duration::from_secs(3))
        .expect("session token in stderr");

    // DNS-rebinding defense: a request with `Host: evil.example.com`
    // (the kind a rebinding attacker would land) must be rejected
    // before any per-route logic runs.
    let (status, _body) = http_post_raw(
        port,
        "POST /done HTTP/1.1\r\n\
         Host: evil.example.com\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {LEN}\r\n\
         Connection: close\r\n\
         \r\n\
         {BODY}",
        &format!(r#"{{"sessionToken":"{token}"}}"#),
    )
    .expect("rebind-shaped POST /done");
    assert_eq!(
        status, 400,
        "non-loopback Host header should be rejected with 400"
    );

    // Now the legitimate Host header — same token — succeeds.
    let (status, _body) =
        http_post_json(port, "/done", &format!(r#"{{"sessionToken":"{token}"}}"#))
            .expect("POST /done with good Host");
    assert_eq!(status, 200);

    let exit = child
        .wait_timeout_or_kill(Duration::from_secs(5))
        .expect("subprocess should exit after /done");
    assert!(exit.success(), "subprocess exit status was {exit:?}");
}

// ── Minimal HTTP client helpers ──────────────────────────────────────────
//
// We can't use reqwest here cleanly — the binary under test already pulls
// in reqwest via its lib, but adding it as a dev-dep for the test would
// be redundant. The listener handles HTTP/1.1 with `Connection: close`,
// so a 30-line socket client is enough to drive every test we need.

fn http_get(port: u16, path: &str) -> std::io::Result<String> {
    let req = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let resp = send_raw(port, req.as_bytes())?;
    Ok(split_body(&resp).1)
}

fn http_post_json(port: u16, path: &str, body: &str) -> std::io::Result<(u16, String)> {
    let req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len()
    );
    let resp = send_raw(port, req.as_bytes())?;
    let (status, body) = split_body(&resp);
    Ok((status, body))
}

/// Send a raw request with placeholders for Content-Length / body. Used
/// for the Host-header test where we need exact control over the
/// request bytes.
fn http_post_raw(port: u16, template: &str, body: &str) -> std::io::Result<(u16, String)> {
    let req = template
        .replace("{LEN}", &body.len().to_string())
        .replace("{BODY}", body);
    let resp = send_raw(port, req.as_bytes())?;
    Ok(split_body(&resp))
}

fn send_raw(port: u16, bytes: &[u8]) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(bytes)?;
    let mut buf = String::new();
    // Tolerate a timeout-on-read: the server may keep the TCP socket
    // open after writing its response if it's reading more than we
    // sent (HTTP/1.1 keepalive default). What we have so far is the
    // response we asked for; return it rather than erroring out.
    match stream.read_to_string(&mut buf) {
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => {}
        Err(e) => return Err(e),
    }
    Ok(buf)
}

/// Parse a raw HTTP/1.1 response into (status, body). Doesn't try to
/// be a real parser — the listener emits well-formed `\r\n\r\n` blank
/// lines and uses `Content-Length` consistently.
fn split_body(resp: &str) -> (u16, String) {
    let mut lines = resp.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = match resp.find("\r\n\r\n") {
        Some(i) => resp[i + 4..].to_string(),
        None => String::new(),
    };
    (status, body)
}

// ── std::process::Child timeout helper ───────────────────────────────────
//
// `std::process::Child::wait` blocks forever; we want "wait up to N
// seconds, then kill". The `wait-timeout` crate exists but adds a dep
// for one method — implement it inline against the existing Child API.

trait ChildWaitTimeout {
    fn wait_timeout_or_kill(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<std::process::ExitStatus>;
}

impl ChildWaitTimeout for std::process::Child {
    fn wait_timeout_or_kill(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<std::process::ExitStatus> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            if started.elapsed() >= timeout {
                self.kill()?;
                return self.wait();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
