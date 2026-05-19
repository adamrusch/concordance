//! `concordance auth login` — localhost-mediated wallet authentication.
//!
//! Replaces the v0.3 DevTools-cookie-scraping onboarding with a flow that
//! mirrors the OAuth PKCE loopback pattern:
//!
//! 1. CLI binds a one-shot HTTP server on `127.0.0.1:<os-assigned-port>`.
//! 2. CLI opens the user's browser to `http://localhost:<port>/auth?k=<token>`.
//! 3. The helper page (HTML+JS, served from `/auth`) talks to the local
//!    server only — never to Ekklesia. The CIP-30 wallet API
//!    (`window.cardano.*`) requires a browser context.
//! 4. The CLI calls Ekklesia's `POST /session` and `PUT /session` over
//!    its own `reqwest` client. The wallet's role is purely to sign the
//!    nonce returned by `POST /session`.
//! 5. CLI stores the resulting JWT via the existing `store.set_token`
//!    codepath, then shuts down the listener.
//!
//! ## Why this works without server-side cooperation
//!
//! Ekklesia's `PUT /api/v0/session` returns the JWT in the response body,
//! not only via the `Set-Cookie: token=<jwt>; HttpOnly` header. CORS is
//! a browser-only enforcement — a native `reqwest::Client` doesn't
//! preflight, doesn't read the `Access-Control-Allow-*` headers, and
//! happily reads the body. So the wallet step has to happen in a
//! browser (CIP-30 lives there), but everything else stays in the CLI.
//!
//! ## What this commit (1/3) ships
//!
//! Just the listener plumbing: subcommand, OS-assigned port, random
//! session token, stub HTML at `/auth`, `POST /done` to shut the
//! listener down, browser-launch, 5-minute deadline. No wallet wiring
//! yet — commits 2 and 3 layer the CIP-30 connect + the Ekklesia
//! session calls on top of this scaffold.

use std::io::{Cursor, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rand::Rng;
use rand::distributions::Alphanumeric;
use tiny_http::{Method, Request, Response, Server, StatusCode};

use crate::store::Store;

/// Length of the session-token random string. 32 alphanumeric chars =
/// ~190 bits of entropy from `[A-Za-z0-9]`, which is well clear of
/// brute-force concern over the lifetime of the listener (≤5 minutes).
const SESSION_TOKEN_LEN: usize = 32;

/// Default deadline for the whole flow. Five minutes matches Ekklesia's
/// nonce TTL (the `POST /session` response is only valid for 5 minutes
/// per `docs/upstream/proposals-openapi.yaml`), so any value beyond
/// that buys nothing.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(300);

/// HTML served from `/auth`. The wallet-connect UI: enumerates the
/// CIP-30 wallets on `window.cardano.*`, calls `enable()`, reads the
/// first reward (stake) address, converts it to bech32, and POSTs it
/// to `/init`. Commit 3 will add the signing step on top of the same
/// page; for now the page POSTs `/done` immediately after `/init`.
///
/// The token placeholder `__SESSION_TOKEN__` is rewritten per-request
/// so the page can echo it back to the listener on /init and /done
/// for verification.
const LOGIN_PAGE_HTML: &str = include_str!("login_page.html");

/// Configuration knobs used by [`run`]. Plumbed as a struct so the
/// integration test can hand in a short deadline without touching the
/// default 5-minute one.
pub struct LoginOptions {
    /// Total time the flow is allowed to take from listener-bound to
    /// either shutdown or timeout. Defaults to 5 minutes.
    pub deadline: Duration,
    /// Whether to actually open the user's default browser. Tests
    /// disable this and hit the listener directly with curl/reqwest.
    pub open_browser: bool,
    /// Optional override of the bound port (0 ⇒ OS-assigned). Only
    /// used by tests that need a fixed port; in production, always 0.
    pub fixed_port: Option<u16>,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            deadline: DEFAULT_DEADLINE,
            open_browser: true,
            fixed_port: None,
        }
    }
}

/// Outcome of a single `auth login` run. The CLI prints a friendly
/// summary; tests assert on the variants directly.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The helper page successfully POSTed `/done` with a matching
    /// session token. In commit 1 this carried no payload; commit 2
    /// also reports the stake address (and optional wallet name) the
    /// user picked, so the caller can surface it. Commit 3 will add a
    /// `user_id` field carrying the JWT subject.
    Completed {
        /// Bech32 stake address the user confirmed in step 2 (the
        /// "use this address" button on the helper page). `None` only
        /// if the page reached `/done` without going through `/init`
        /// — shouldn't happen in normal use, but we don't trap it
        /// here so a future variation (e.g. a "skip wallet" path) can
        /// reuse the outcome.
        stake_addr: Option<String>,
        /// Display name of the wallet that signed the connect request
        /// (e.g. `"Lace"`, `"Eternl"`). `None` if the page chose not
        /// to send it. Surfaces in the CLI's friendly success message
        /// but isn't load-bearing for any downstream operation.
        wallet_name: Option<String>,
    },
    /// Deadline elapsed before `/done` was reached. The listener shuts
    /// down cleanly and the user gets an actionable message.
    TimedOut,
}

/// Shared mutable state between the request handlers and the main
/// supervisor loop. Wrapped in [`Mutex`] because tiny_http dispatches
/// one OS thread per request; multiple handlers can race on the same
/// `Arc<LoginState>` (e.g. a stale browser tab POSTing `/init` while
/// a fresh tab races to `/done`). All fields are short strings, so
/// holding the mutex across the whole handler body is fine.
#[derive(Default)]
struct LoginState {
    /// Bech32 stake address from `/init`. `None` until the page has
    /// successfully posted a wallet-confirmed address.
    stake_addr: Option<String>,
    /// Wallet display name from `/init` (e.g. `"Lace"`).
    wallet_name: Option<String>,
}

/// Entry point invoked by `main.rs`. Binds the listener, opens the
/// browser (unless disabled), services requests until either `/done`
/// fires or the deadline elapses, then returns. The `_store` argument
/// is unused in commits 1 and 2 — it's threaded through so commit 3
/// can call `store.set_token` without changing the signature.
pub fn run(_store: &Store, _instance: &str, options: LoginOptions) -> anyhow::Result<LoginOutcome> {
    let port = options.fixed_port.unwrap_or(0);
    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    // We pre-bind via std::net::TcpListener so we can learn the assigned
    // port before constructing the helper URL. `tiny_http::Server::from_listener`
    // adopts the existing socket.
    let listener = TcpListener::bind(bind_addr)
        .map_err(|e| anyhow::anyhow!("failed to bind 127.0.0.1: {e}"))?;
    let bound_addr = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("failed to read bound address: {e}"))?;
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("failed to start localhost listener: {e}"))?;

    let session_token = random_session_token();
    let helper_url = format!(
        "http://localhost:{}/auth?k={}",
        bound_addr.port(),
        session_token
    );

    // Best-effort writes — if the parent process closed stderr (e.g.
    // a test harness has already drained the URL line), the EPIPE
    // would otherwise panic and surface as a generic 500 inside the
    // handler thread. See the `writeln!` use in `/init` for the same
    // pattern.
    let _ = writeln!(
        std::io::stderr(),
        "concordance: listening on http://localhost:{port}\n  \
         If the browser didn't open, paste this URL:\n  \
         {url}",
        port = bound_addr.port(),
        url = helper_url,
    );

    if options.open_browser {
        // `open` is best-effort: if it fails (headless CI, no browser
        // configured) we don't error out — the user can still paste
        // the URL into a browser manually. The writeln above tells
        // them how.
        if let Err(e) = open::that(&helper_url) {
            let _ = writeln!(std::io::stderr(), "  (couldn't auto-open the browser: {e})");
        }
    }

    let outcome = serve_until_done(server, &session_token, bound_addr, options.deadline);
    Ok(outcome)
}

/// Drain HTTP requests off `server` until either:
/// - the helper page POSTs `/done` with a matching session token
///   (returns [`LoginOutcome::Completed`]), or
/// - `deadline` elapses (returns [`LoginOutcome::TimedOut`]).
///
/// The handler threads share an [`Arc<Mutex<LoginState>>`] so the
/// stake address captured at `/init` survives until the supervisor
/// reads it on success.
///
/// All inbound `Host:` headers are validated against the bound port —
/// anything else is rejected as a defense against DNS-rebinding attacks
/// (an attacker-controlled DNS name resolving to 127.0.0.1 would
/// otherwise be able to drive this listener).
fn serve_until_done(
    server: Server,
    session_token: &str,
    bound_addr: SocketAddr,
    deadline: Duration,
) -> LoginOutcome {
    let started = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(LoginState::default()));

    // tiny_http's `recv_timeout` lets the main thread sleep until either
    // a request arrives or the polling interval expires. We pick 500ms:
    // small enough that the deadline check kicks in promptly, large
    // enough that an idle listener barely touches the CPU.
    loop {
        if started.elapsed() >= deadline {
            return LoginOutcome::TimedOut;
        }
        if done.load(Ordering::Acquire) {
            return outcome_from_state(&state);
        }
        match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(req)) => {
                let done_clone = Arc::clone(&done);
                let state_clone = Arc::clone(&state);
                let token = session_token.to_string();
                // Each request gets a short-lived worker thread. The
                // worker mutates `done`/`state` if the request was
                // `/init` or `/done` with a matching session token;
                // the main loop notices on its next tick.
                thread::spawn(move || {
                    handle_request(
                        req,
                        &token,
                        bound_addr.port(),
                        &done_clone,
                        &state_clone,
                    );
                });
            }
            Ok(None) => {
                // recv_timeout returns Ok(None) on the poll-timeout
                // path — no request, just loop back to check the
                // deadline.
            }
            Err(_) => {
                // tiny_http's recv loop only errors when the listener
                // is closed (e.g. by `Server::unblock`); treat that as
                // a graceful shutdown trigger.
                return if done.load(Ordering::Acquire) {
                    outcome_from_state(&state)
                } else {
                    LoginOutcome::TimedOut
                };
            }
        }
    }
}

/// Snapshot the captured stake address + wallet name out of the shared
/// state and build a [`LoginOutcome::Completed`] payload. Kept as a
/// helper because the `done`-then-return path is reachable from two
/// branches in [`serve_until_done`].
fn outcome_from_state(state: &Arc<Mutex<LoginState>>) -> LoginOutcome {
    let guard = state.lock().expect("login state mutex poisoned");
    LoginOutcome::Completed {
        stake_addr: guard.stake_addr.clone(),
        wallet_name: guard.wallet_name.clone(),
    }
}

/// Service a single inbound HTTP request. The router is tiny:
///
/// - `GET /auth?k=<token>` → serve the helper HTML page (with the
///   session token interpolated into the JS).
/// - `POST /init` → if the body's session token matches, record the
///   reported `stakeAddr` (and optional `walletName`) into the shared
///   state. Doesn't shut the listener down — `/done` does that.
/// - `POST /done` → if the body contains the matching token, set the
///   `done` flag (signalling the main loop to return).
/// - everything else → 404.
///
/// `Host:` headers are validated against the listener's bound port to
/// block DNS-rebinding-style attacks. Anything that doesn't look like
/// `localhost:<port>` or `127.0.0.1:<port>` returns 400.
fn handle_request(
    mut req: Request,
    expected_token: &str,
    bound_port: u16,
    done: &Arc<AtomicBool>,
    state: &Arc<Mutex<LoginState>>,
) {
    if !host_header_is_loopback(&req, bound_port) {
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    }

    let method = req.method().clone();
    let url = req.url().to_string();
    let (path, query) = split_path_and_query(&url);

    match (&method, path.as_str()) {
        (Method::Get, "/auth") => {
            // Verify the query-string `k` matches the session token.
            // We don't gate the HTML body itself behind it (an attacker
            // who can reach localhost can also read the page from
            // their own browser), but we do gate the JS that knows
            // about the token: if the URL token doesn't match, the
            // page renders without the embedded token and any
            // subsequent POSTs from it will be rejected.
            let token_in_url = parse_query_param(&query, "k").unwrap_or_default();
            let rendered = if subtle_eq(token_in_url.as_bytes(), expected_token.as_bytes()) {
                LOGIN_PAGE_HTML.replace("__SESSION_TOKEN__", expected_token)
            } else {
                LOGIN_PAGE_HTML.replace("__SESSION_TOKEN__", "")
            };
            let body = rendered.into_bytes();
            let len = body.len();
            let response = Response::new(
                StatusCode(200),
                vec![
                    tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"text/html; charset=utf-8"[..],
                    )
                    .unwrap(),
                    // No-store: the page is short-lived per session and
                    // its embedded session token must not be cached by
                    // a shared proxy or by the browser disk cache.
                    tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
                ],
                Cursor::new(body),
                Some(len),
                None,
            );
            let _ = req.respond(response);
        }
        (Method::Post, "/init") => {
            // The page POSTs JSON: `{ sessionToken, stakeAddr, walletName }`.
            // Validate the token first, then capture the stake address
            // into shared state. Both checks happen here (not at /done)
            // so the user sees an immediate error in their browser if
            // anything goes wrong rather than discovering the failure
            // post-shutdown.
            let mut buf = String::new();
            if std::io::Read::read_to_string(req.as_reader(), &mut buf).is_err() {
                let _ = req.respond(Response::empty(StatusCode(400)));
                return;
            }
            let init_payload: serde_json::Value = match serde_json::from_str(&buf) {
                Ok(v) => v,
                Err(_) => {
                    let _ = req.respond(Response::empty(StatusCode(400)));
                    return;
                }
            };
            let body_token = init_payload
                .get("sessionToken")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !subtle_eq(body_token.as_bytes(), expected_token.as_bytes()) {
                let _ = req.respond(Response::empty(StatusCode(403)));
                return;
            }
            let stake_addr = init_payload
                .get("stakeAddr")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // Reject obviously-malformed stake addresses up front so the
            // user gets a 400 in the browser instead of an opaque
            // failure later. Bech32 enforcement happens on the wire
            // (Ekklesia parses it); we just gate basic shape here.
            if !stake_addr
                .as_deref()
                .map(is_plausible_stake_address)
                .unwrap_or(false)
            {
                let _ = req.respond(Response::empty(StatusCode(400)));
                return;
            }
            let wallet_name = init_payload
                .get("walletName")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // Capture the stake address into shared state, then build
            // a one-line summary for the terminal *outside* the lock
            // so we never hold the mutex while doing I/O. eprintln!
            // can SIGPIPE/panic if the parent has closed stderr (as
            // the integration test does after scraping the helper URL),
            // and tiny_http would surface that panic as a generic 500
            // — confusing because the user-facing request is fine.
            let log_line = {
                let mut guard = state.lock().expect("login state mutex poisoned");
                guard.stake_addr = stake_addr;
                guard.wallet_name = wallet_name;
                format!(
                    "concordance: received stake address {} (wallet: {})",
                    guard.stake_addr.as_deref().unwrap_or("?"),
                    guard.wallet_name.as_deref().unwrap_or("?"),
                )
            };
            // Best-effort write to stderr — if the parent has dropped
            // the pipe (e.g. a test that already drained the line it
            // cared about), we'd panic on the EPIPE. `writeln!` returns
            // a Result we discard, which is the panic-free equivalent
            // of `eprintln!` here.
            let _ = writeln!(std::io::stderr(), "{log_line}");
            let _ = req.respond(Response::from_string("ok"));
        }
        (Method::Post, "/done") => {
            let mut buf = String::new();
            if std::io::Read::read_to_string(req.as_reader(), &mut buf).is_err() {
                let _ = req.respond(Response::empty(StatusCode(400)));
                return;
            }
            // Body format: `{ "sessionToken": "<value>" }`. We accept
            // either JSON or the same value form-urlencoded, so the
            // stub HTML stays simple — both forms parse via a single
            // helper.
            let body_token = extract_session_token(&buf).unwrap_or_default();
            if !subtle_eq(body_token.as_bytes(), expected_token.as_bytes()) {
                let _ = req.respond(Response::empty(StatusCode(403)));
                return;
            }
            done.store(true, Ordering::Release);
            let _ = req.respond(Response::from_string("ok"));
        }
        _ => {
            let _ = req.respond(Response::empty(StatusCode(404)));
        }
    }
}

/// Cheap shape check on a candidate stake address. We don't decode the
/// bech32 here — Ekklesia does that on the wire and gives a clearer
/// error than a CLI-side validator. We just want to reject obvious
/// junk (empty, no `stake` prefix, suspiciously long, etc.) before
/// state is mutated.
///
/// Cardano stake addresses are bech32:
///   - HRP `stake` (mainnet) or `stake_test` (preview/testnet)
///   - Followed by `1` separator and ~53 chars of data + checksum
///   - Total length 59 (mainnet) or 64 (testnet) chars typical
fn is_plausible_stake_address(addr: &str) -> bool {
    if !(addr.starts_with("stake1") || addr.starts_with("stake_test1")) {
        return false;
    }
    // Bech32 chars only: lowercase letters + digits, minus 1/b/i/o (the
    // disallowed set from BIP-173). We don't enforce the exact set
    // here — we just refuse anything outside `[a-z0-9_]` so an attacker
    // can't inject CR/LF or other control characters into log output.
    if !addr
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return false;
    }
    // Bech32 max length is 90 chars per BIP-173. Cardano stake
    // addresses are well within that; clip the sanity ceiling at 128
    // for headroom.
    let len = addr.len();
    (12..=128).contains(&len)
}

/// Generate a CSPRNG-backed alphanumeric token. Restricted to
/// `[A-Za-z0-9]` so it survives a URL query-string without encoding
/// (the helper page interpolates it into `window.location.search`).
fn random_session_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(SESSION_TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Split `url` into (`path`, `query`) on the first `?`. `tiny_http` gives
/// us `req.url()` as a relative URL string (no scheme/host), so we just
/// need a cheap splitter — no full URL parsing required.
fn split_path_and_query(url: &str) -> (String, String) {
    match url.find('?') {
        Some(i) => (url[..i].to_string(), url[i + 1..].to_string()),
        None => (url.to_string(), String::new()),
    }
}

/// Find a single value for `key` in a query string. Returns the first
/// match; bare `?k` or `?k=` map to an empty string. Good enough for
/// the two parameters this listener cares about (`k` and nothing else).
fn parse_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = match pair.find('=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, ""),
        };
        if k == key {
            // Percent-decode is intentionally skipped here because the
            // session token is restricted to `[A-Za-z0-9]` — none of
            // those characters need encoding. If we ever broaden the
            // alphabet, swap this for `urlencoding::decode`.
            return Some(v.to_string());
        }
    }
    None
}

/// Pull the session-token value out of `/done`'s request body. Accepts
/// either:
///
/// - JSON: `{"sessionToken": "..."}` (commit 2+ uses this).
/// - Form-encoded: `sessionToken=...` (commit 1's stub HTML uses this).
///
/// The looseness is on purpose — the listener is intentionally lenient
/// about the body shape so the helper page can evolve without breaking
/// the integration test.
fn extract_session_token(body: &str) -> Option<String> {
    // Try JSON first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(s) = v.get("sessionToken").and_then(|s| s.as_str()) {
            return Some(s.to_string());
        }
    }
    // Then form-encoded.
    for pair in body.split('&') {
        let (k, v) = match pair.find('=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, ""),
        };
        if k == "sessionToken" {
            return Some(v.to_string());
        }
    }
    None
}

/// Constant-time byte-slice comparison. The session-token check
/// shouldn't leak length info via timing; while this listener is
/// almost entirely on localhost, a buggy app on the same machine
/// could otherwise probe the token byte-by-byte. Cheap insurance.
fn subtle_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Validate the `Host:` header on inbound requests. Two forms are
/// allowed: `localhost:<port>` and `127.0.0.1:<port>`, both with the
/// exact bound port. Anything else gets 400.
///
/// This is the DNS-rebinding hardening: an attacker who points
/// `evil.example.com` at `127.0.0.1` could otherwise have a browser
/// session on `evil.example.com` POST to our listener. The browser
/// sends `Host: evil.example.com`, which fails this check.
fn host_header_is_loopback(req: &Request, bound_port: u16) -> bool {
    let host_header = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string());
    let Some(host) = host_header else {
        // tiny_http inserts a `Host` field on every request; missing
        // means we never want to honour it.
        return false;
    };
    let expected_lo = format!("localhost:{bound_port}");
    let expected_ip = format!("127.0.0.1:{bound_port}");
    host == expected_lo || host == expected_ip
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_token_is_alphanumeric_and_long() {
        let t = random_session_token();
        assert_eq!(t.len(), SESSION_TOKEN_LEN);
        assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn session_token_changes_each_call() {
        let a = random_session_token();
        let b = random_session_token();
        // 32 alphanumeric chars = ~190 bits; the odds of collision in
        // the same test run are astronomically low. If this ever fires
        // it's a real bug, not bad luck.
        assert_ne!(a, b);
    }

    #[test]
    fn split_path_and_query_handles_both_shapes() {
        assert_eq!(
            split_path_and_query("/auth?k=abc"),
            ("/auth".to_string(), "k=abc".to_string())
        );
        assert_eq!(
            split_path_and_query("/auth"),
            ("/auth".to_string(), String::new())
        );
        assert_eq!(
            split_path_and_query("/done?"),
            ("/done".to_string(), String::new())
        );
    }

    #[test]
    fn parse_query_param_finds_value() {
        assert_eq!(parse_query_param("k=abc&x=y", "k"), Some("abc".to_string()));
        assert_eq!(parse_query_param("x=y", "k"), None);
        assert_eq!(parse_query_param("k=", "k"), Some(String::new()));
        assert_eq!(parse_query_param("k", "k"), Some(String::new()));
    }

    #[test]
    fn extract_session_token_handles_json() {
        let body = r#"{"sessionToken":"abcd1234"}"#;
        assert_eq!(extract_session_token(body), Some("abcd1234".to_string()));
    }

    #[test]
    fn extract_session_token_handles_form() {
        assert_eq!(
            extract_session_token("sessionToken=zxcv"),
            Some("zxcv".to_string())
        );
        assert_eq!(
            extract_session_token("foo=bar&sessionToken=zxcv"),
            Some("zxcv".to_string())
        );
    }

    #[test]
    fn extract_session_token_returns_none_when_missing() {
        assert_eq!(extract_session_token("{}"), None);
        assert_eq!(extract_session_token("foo=bar"), None);
    }

    #[test]
    fn subtle_eq_matches_basic_cases() {
        assert!(subtle_eq(b"", b""));
        assert!(subtle_eq(b"abc", b"abc"));
        assert!(!subtle_eq(b"abc", b"abd"));
        assert!(!subtle_eq(b"abc", b"abcd"));
        assert!(!subtle_eq(b"abc", b"ab"));
    }

    #[test]
    fn plausible_stake_address_accepts_real_shapes() {
        // Real bech32 stake addresses pulled from the integration test
        // fixtures in `auth::tests` — these are well-formed mainnet
        // and testnet shapes the API definitely accepts.
        assert!(is_plausible_stake_address(
            "stake1u8td6l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt"
        ));
        assert!(is_plausible_stake_address(
            "stake_test1uq5l5sakfcpm6uz85v942xu5f76kzj9qz33c7986d0dxc3sxnvt"
        ));
    }

    #[test]
    fn plausible_stake_address_rejects_obvious_junk() {
        assert!(!is_plausible_stake_address(""));
        assert!(!is_plausible_stake_address("addr1q9random"));
        assert!(!is_plausible_stake_address("Stake1uXYZ"), "case-sensitive");
        // CR/LF injection — the log line that includes this string
        // must not be allowed to inject ANSI escape or newlines into
        // the user's terminal.
        assert!(!is_plausible_stake_address("stake1u\nINJECT"));
        assert!(!is_plausible_stake_address("stake1u\x1b[31mred"));
        // Too short
        assert!(!is_plausible_stake_address("stake1"));
        // Too long
        assert!(!is_plausible_stake_address(&format!(
            "stake1{}",
            "a".repeat(200)
        )));
    }
}
