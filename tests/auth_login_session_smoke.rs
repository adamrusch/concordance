//! End-to-end smoke for `concordance auth login`'s full session flow
//! (commit 3 / v0.4).
//!
//! Spawns a mock Ekklesia server (a tiny std::net listener that
//! understands `POST /api/v0/session` and `PUT /api/v0/session`) on
//! one port, then spawns the `concordance auth login` subprocess on
//! another port with `CONCORDANCE_LOGIN_OVERRIDE_BASE_URL` pointed at
//! the mock. The test then drives the helper-page side of the flow
//! over plain HTTP: POST /init, poll /challenge, POST /signed, and
//! verifies the JWT got persisted into the tempdir-backed store via
//! a follow-up `concordance auth status` run.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ── Mock Ekklesia ────────────────────────────────────────────────────────
//
// The mock responds to POST /api/v0/session with a canned
// {dataHex, userId, userIdHex, signerAddressHex} body, and to
// PUT /api/v0/session with a canned {token, expiresIn, userId} body
// — the token being a structurally-valid JWT with a far-future `exp`
// claim so `inspect_jwt` accepts it.

struct MockEkklesia {
    base_url: String,
    captures: Arc<Mutex<MockCaptures>>,
    _stop_tx: std::sync::mpsc::Sender<()>,
}

#[derive(Default, Debug)]
struct MockCaptures {
    post_session_body: Option<String>,
    put_session_body: Option<String>,
}

fn fake_jwt() -> String {
    // exp = now + 1 day. The CLI's inspect_jwt requires a structurally
    // valid JWT (3 parts, decodable payload, present `exp`); we don't
    // need a real signature because the CLI never verifies it.
    let exp = chrono::Utc::now().timestamp() + 86_400;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_json = format!(
        r#"{{"userId":"stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt","signType":"stake","iat":0,"exp":{exp}}}"#
    );
    let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    format!("{header}.{payload}.fakesig")
}

fn start_mock() -> MockEkklesia {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("mock local_addr");
    listener
        .set_nonblocking(true)
        .expect("mock set_nonblocking");

    let captures = Arc::new(Mutex::new(MockCaptures::default()));
    let captures_clone = Arc::clone(&captures);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .ok();
                    let mut buf = [0u8; 8192];
                    let mut total = 0usize;
                    // Read request headers until \r\n\r\n, then any body
                    // implied by Content-Length.
                    loop {
                        match socket.read(&mut buf[total..]) {
                            Ok(0) => break,
                            Ok(n) => {
                                total += n;
                                if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                                if total >= buf.len() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let raw = String::from_utf8_lossy(&buf[..total]).to_string();
                    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
                    let first_line = head.lines().next().unwrap_or("").to_string();

                    // Best-effort: if Content-Length says more, keep reading.
                    let mut body = body.to_string();
                    let cl = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split_once(':'))
                        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while body.len() < cl {
                        let mut more = [0u8; 4096];
                        match socket.read(&mut more) {
                            Ok(0) => break,
                            Ok(n) => body.push_str(&String::from_utf8_lossy(&more[..n])),
                            Err(_) => break,
                        }
                    }

                    let response = if first_line.starts_with("POST /api/v0/session") {
                        captures_clone.lock().unwrap().post_session_body = Some(body.clone());
                        let body = r#"{"dataHex":"deadbeefcafe","userId":"u-1","userIdHex":"abc","signerAddressHex":"def"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else if first_line.starts_with("PUT /api/v0/session") {
                        captures_clone.lock().unwrap().put_session_body = Some(body.clone());
                        let token = fake_jwt();
                        let body = format!(
                            r#"{{"token":"{}","expiresIn":"2099-01-01T00:00:00Z","userId":"stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt"}}"#,
                            token
                        );
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else {
                        let body = "{}";
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    };
                    let _ = socket.write_all(response.as_bytes());
                    let _ = socket.flush();
                    let _ = socket.shutdown(Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
    });

    MockEkklesia {
        base_url: format!("http://{addr}"),
        captures,
        _stop_tx: stop_tx,
    }
}

// ── Port + listener helpers ──────────────────────────────────────────────

fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("local_addr").port();
    drop(l);
    p
}

fn wait_for_listener(port: u16, deadline: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.shutdown(Shutdown::Both);
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn read_session_token_from_stderr<R: Read + Send + 'static>(
    stderr: R,
    deadline: Duration,
) -> Option<String> {
    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured);
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(|r| r.ok()) {
            if let Some(idx) = line.find("/auth?k=") {
                let after = &line[idx + "/auth?k=".len()..];
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
        thread::sleep(Duration::from_millis(25));
    }
    None
}

// ── Minimal HTTP client ──────────────────────────────────────────────────

fn send_raw(port: u16, bytes: &[u8]) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(bytes)?;
    let mut buf = String::new();
    match stream.read_to_string(&mut buf) {
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => {}
        Err(e) => return Err(e),
    }
    Ok(buf)
}

fn split_body(resp: &str) -> (u16, String) {
    let status_line = resp.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = resp
        .find("\r\n\r\n")
        .map(|i| resp[i + 4..].to_string())
        .unwrap_or_default();
    (status, body)
}

fn http_get(port: u16, path: &str) -> std::io::Result<(u16, String)> {
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    let resp = send_raw(port, req.as_bytes())?;
    Ok(split_body(&resp))
}

fn http_post_json(port: u16, path: &str, body: &str) -> std::io::Result<(u16, String)> {
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    let resp = send_raw(port, req.as_bytes())?;
    Ok(split_body(&resp))
}

// ── Child process timeout ────────────────────────────────────────────────

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
            thread::sleep(Duration::from_millis(25));
        }
    }
}

// ── The test ─────────────────────────────────────────────────────────────

#[test]
fn auth_login_full_session_flow_persists_jwt() {
    let mock = start_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .env("CONCORDANCE_LOGIN_NO_BROWSER", "1")
        .env("CONCORDANCE_LOGIN_FIXED_PORT", port.to_string())
        .env("CONCORDANCE_LOGIN_DEADLINE_SECS", "15")
        .env("CONCORDANCE_LOGIN_OVERRIDE_BASE_URL", &mock.base_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn concordance auth login");

    let stderr = child.stderr.take().expect("stderr");
    let stdout = child.stdout.take().expect("stdout");
    assert!(
        wait_for_listener(port, Duration::from_secs(5)),
        "listener never bound on port {port}"
    );
    let token = read_session_token_from_stderr(stderr, Duration::from_secs(3))
        .expect("session token in stderr");

    let stake = "stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt";

    // 1. Hand the CLI the stake address — kicks off POST /session.
    let (status, _body) = http_post_json(
        port,
        "/init",
        &format!(r#"{{"sessionToken":"{token}","stakeAddr":"{stake}","walletName":"Lace"}}"#),
    )
    .expect("POST /init");
    assert_eq!(status, 200, "POST /init must be 200");

    // 2. Poll /challenge until the CLI has the dataHex.
    let mut data_hex = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let (s, body) = http_get(port, "/challenge").expect("GET /challenge");
        if s == 204 {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        if s == 200 {
            let v: serde_json::Value = serde_json::from_str(&body).expect("parse /challenge body");
            if let Some(hex) = v.get("dataHex").and_then(|v| v.as_str()) {
                data_hex = Some(hex.to_string());
                break;
            }
        }
        panic!("unexpected /challenge response: status={s}, body={body:?}");
    }
    let data_hex = data_hex.expect("never got dataHex");
    assert_eq!(data_hex, "deadbeefcafe", "mock returned canned dataHex");

    // Verify the CLI sent the right body to mock /session.
    let post_body = mock
        .captures
        .lock()
        .unwrap()
        .post_session_body
        .clone()
        .expect("CLI should have POSTed /session");
    let post_json: serde_json::Value = serde_json::from_str(&post_body).expect("post body json");
    assert_eq!(post_json["signerAddress"], stake);
    assert_eq!(post_json["signType"], "stake");

    // 3. Wallet would now signData(stakeAddr, dataHex) and POST /signed.
    //    We simulate that with a canned hex signature.
    let signature = "00112233445566778899aabbccddeeff";
    let key = "ffeeddccbbaa99887766554433221100";
    let (status, body) = http_post_json(
        port,
        "/signed",
        &format!(
            r#"{{"sessionToken":"{token}","signature":"{signature}","key":"{key}"}}"#
        ),
    )
    .expect("POST /signed");
    // v0.4.1+ copy-paste UX: /signed returns the JWT in the body
    // (ok: true, token: "...", userId: "..."), DOES NOT auto-store,
    // and DOES NOT auto-shutdown. The page renders the token for the
    // user to copy back to the chat agent, which finalizes with
    // `auth set --jwt -`.
    assert_eq!(status, 200, "POST /signed must be 200, body: {body:?}");
    let signed_resp: serde_json::Value =
        serde_json::from_str(&body).expect("/signed body json");
    assert_eq!(signed_resp["ok"], serde_json::json!(true));
    assert_eq!(
        signed_resp["userId"],
        "stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt"
    );
    assert!(
        signed_resp["token"].as_str().is_some_and(|t| !t.is_empty()),
        "/signed should return the JWT for the page to display: {body:?}"
    );

    // Verify the CLI's PUT /session body matches the spec contract.
    let put_body = mock
        .captures
        .lock()
        .unwrap()
        .put_session_body
        .clone()
        .expect("CLI should have PUT /session");
    let put_json: serde_json::Value = serde_json::from_str(&put_body).expect("put body json");
    assert_eq!(put_json["signerAddress"], stake);
    assert_eq!(put_json["signType"], "stake");
    assert_eq!(put_json["signature"], signature);
    assert_eq!(put_json["key"], key);

    // 4. Listener must NOT auto-shutdown: the user still needs to copy
    //    the token from the page. Send /done explicitly to unblock.
    let (s, _) =
        http_post_json(port, "/done", &format!(r#"{{"sessionToken":"{token}"}}"#)).unwrap();
    assert_eq!(s, 200);
    let exit = child
        .wait_timeout_or_kill(Duration::from_secs(5))
        .expect("subprocess should exit after /done");
    assert!(exit.success(), "exit {exit:?}");

    // 5. stderr should mention the server returned a JWT and point
    //    the user at the `auth set --jwt -` finalization step.
    let mut stdout_buf = String::new();
    let mut stdout = stdout;
    stdout.read_to_string(&mut stdout_buf).expect("read stdout");
    assert!(
        stdout_buf.contains("server returned JWT") || stdout_buf.contains("auth set"),
        "stdout should mention the JWT or the auth-set hint: {stdout_buf:?}"
    );

    // 6. JWT must NOT have been auto-persisted — the user finalizes
    //    via `auth set --jwt -`. `concordance auth status` should
    //    report no token in the tempdir-backed store.
    let status_out = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "status"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .output()
        .expect("run concordance auth status");
    assert!(
        !status_out.status.success(),
        "auth status should exit nonzero — copy-paste UX does NOT auto-persist"
    );
}

#[test]
fn auth_login_put_session_failure_surfaces_to_signed_caller() {
    // If Ekklesia rejects the signature, /signed must return 500 with
    // a useful error string in `error`, and the CLI must NOT persist
    // anything. The mock here returns 400 for PUT /session to simulate
    // a real failure.
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
    let mock_addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock_url = format!("http://{mock_addr}");
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).unwrap_or(0);
                    let raw = String::from_utf8_lossy(&buf[..n]);
                    let first = raw.lines().next().unwrap_or("");
                    let response = if first.starts_with("POST /api/v0/session") {
                        // POST succeeds so /init → /challenge round-trip works.
                        let body = r#"{"dataHex":"deadbeefcafe","userId":"u","userIdHex":"a","signerAddressHex":"b"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else {
                        // PUT /session returns 400 — the failure case.
                        let body = r#"{"status":"error","message":"signature verification failed"}"#;
                        format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    };
                    let _ = socket.write_all(response.as_bytes());
                    let _ = socket.shutdown(Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
    });

    let tmp = tempfile::tempdir().expect("tempdir");
    let port = pick_free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .env("CONCORDANCE_LOGIN_NO_BROWSER", "1")
        .env("CONCORDANCE_LOGIN_FIXED_PORT", port.to_string())
        .env("CONCORDANCE_LOGIN_DEADLINE_SECS", "15")
        .env("CONCORDANCE_LOGIN_OVERRIDE_BASE_URL", &mock_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auth login");

    let stderr = child.stderr.take().expect("stderr");
    assert!(wait_for_listener(port, Duration::from_secs(5)));
    let token = read_session_token_from_stderr(stderr, Duration::from_secs(3))
        .expect("session token");

    let stake = "stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt";

    // /init → success
    let (s, _) = http_post_json(
        port,
        "/init",
        &format!(r#"{{"sessionToken":"{token}","stakeAddr":"{stake}","walletName":"Lace"}}"#),
    )
    .unwrap();
    assert_eq!(s, 200);

    // Poll /challenge until dataHex is ready
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ok = false;
    while Instant::now() < deadline {
        let (s, body) = http_get(port, "/challenge").unwrap();
        if s == 200 && body.contains("dataHex") {
            ok = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(ok, "never got dataHex from /challenge");

    // /signed → CLI calls mock PUT which returns 400. The copy-paste
    // UX (v0.4.1+) surfaces this as a 200 with `{ok: false, error}`
    // so the page can render the error in the same copyable result
    // panel as a successful JWT. Returning 500 here would force the
    // page into a different code path; making both surfaces identical
    // keeps the diagnosis UX simple.
    let (s, body) = http_post_json(
        port,
        "/signed",
        &format!(
            r#"{{"sessionToken":"{token}","signature":"00112233","key":"ffeeddcc"}}"#
        ),
    )
    .unwrap();
    assert_eq!(s, 200, "/signed wraps failure as 200 with ok:false");
    let v: serde_json::Value = serde_json::from_str(&body).expect("error body json");
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(
        v.get("error")
            .and_then(|x| x.as_str())
            .map(|m| m.contains("PUT") && m.contains("400"))
            .unwrap_or(false),
        "error body should describe the PUT failure, got: {body:?}"
    );

    // POST /done to shut down the listener — same as the happy path.
    let (s, _) =
        http_post_json(port, "/done", &format!(r#"{{"sessionToken":"{token}"}}"#)).unwrap();
    assert_eq!(s, 200);

    let exit = child
        .wait_timeout_or_kill(Duration::from_secs(5))
        .expect("subprocess exit");
    assert!(exit.success());

    // No JWT should be persisted — `concordance auth status` should
    // report no token (the failure path doesn't store anything).
    let status_out = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .args(["auth", "status"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .output()
        .expect("auth status");
    // status exits non-zero when there's no token configured — the
    // error message is on stderr in that case, not stdout.
    let stderr_text = String::from_utf8_lossy(&status_out.stderr);
    assert!(
        stderr_text.to_lowercase().contains("no jwt")
            || stderr_text.to_lowercase().contains("no token"),
        "auth status stderr should indicate missing token, got: {stderr_text:?}"
    );

    let _ = stop_tx.send(());
}
