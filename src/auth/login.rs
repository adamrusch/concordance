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
//! ## Listener routes
//!
//! - `GET  /auth?k=<token>` — serve the helper HTML page with the
//!   per-run session token baked in.
//! - `POST /init`           — page hands the CLI the chosen stake
//!   address. CLI replies once it has the challenge ready.
//! - `GET  /challenge`      — page polls this; once the CLI has called
//!   Ekklesia's `POST /session`, the response carries the `dataHex`
//!   the wallet needs to sign.
//! - `POST /signed`         — page returns the wallet's
//!   `{signature, key}`. CLI calls `PUT /session`, persists the JWT.
//! - `POST /done`           — page tells the CLI to shut the listener
//!   down. The CLI also auto-sets `done` once `PUT /session` succeeds,
//!   so a wedged client can't keep the listener alive past its job.
//!
//! ## Signature wire format for `PUT /session`
//!
//! CIP-30's `signData(addr, dataHex)` returns
//! `DataSignature = { signature: hex, key: hex }`, both CBOR-encoded
//! COSE_Sign1 / COSE_Key structures. Ekklesia's spec (see
//! `docs/upstream/proposals-openapi.yaml`) only says "Hex-encoded
//! signature produced by the wallet". Empirically the API accepts
//! `signature.signature` (the COSE_Sign1 hex) as the `signature`
//! field; the `key` is sent alongside on a separate field per the
//! reference implementation. We mirror that contract here.

use std::io::{Cursor, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rand::Rng;
use rand::distributions::Alphanumeric;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde_json::json;
use tiny_http::{Method, Request, Response, Server, StatusCode};
use tokio::runtime::Handle;

use crate::auth::inspect_jwt;
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
    /// The full handshake completed: stake address captured, challenge
    /// signed, JWT persisted via [`Store::set_token`]. The carried
    /// fields are surfaced to the user so the CLI's friendly success
    /// line can be specific (`Signed in as Lace / stake1u...`).
    Completed {
        /// Bech32 stake address the user signed with. `None` only if
        /// the page reached `/done` without going through `/init` —
        /// shouldn't happen in normal use, but we don't trap it here.
        stake_addr: Option<String>,
        /// Display name of the wallet that signed (`"Lace"`, etc.).
        /// `None` if the page chose not to send it.
        wallet_name: Option<String>,
        /// `userId` claim from the issued JWT (Ekklesia's internal
        /// identifier for the authenticated voter). `None` until
        /// `PUT /session` has succeeded.
        user_id: Option<String>,
    },
    /// Deadline elapsed before the flow completed. The listener shuts
    /// down cleanly and the user gets an actionable message.
    TimedOut,
}

/// Where the wallet-signing handshake currently is. The page polls
/// `/challenge` to read this — empty steady-state until `/init` has
/// triggered the `POST /session` call, then `ChallengeReady` once the
/// CLI has the `dataHex` to hand to `signData`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum FlowStage {
    /// Waiting for the user to pick a wallet + stake address.
    #[default]
    AwaitingInit,
    /// `POST /session` is in flight; the page should keep polling.
    FetchingChallenge,
    /// `POST /session` failed; the page should surface the message and
    /// either offer a retry or instruct the user to re-run the CLI.
    ChallengeError(String),
    /// Challenge is ready — page can call `signData(addr, dataHex)`.
    ChallengeReady { data_hex: String },
    /// `PUT /session` is in flight after the page POSTed `/signed`.
    Verifying,
    /// `PUT /session` failed; page surfaces the error.
    VerifyError(String),
    /// Token stored. Page should POST `/done`.
    Verified,
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
    /// `userId` from the issued JWT, set on `PUT /session` success.
    user_id: Option<String>,
    /// Tracks where the wallet-signing handshake is — drives the
    /// `/challenge` polling endpoint's responses.
    stage: FlowStage,
}

/// Context handed to every spawned request worker. Bundles every
/// immutable input the handler needs (base URL of the Ekklesia
/// instance to authenticate against, the runtime handle to run async
/// reqwest calls, etc.) plus the shared mutable state and shutdown
/// flag. Threading these as one struct keeps the per-request thread
/// spawn simple and makes adding new fields a one-line change at the
/// call site.
struct LoginContext {
    session_token: String,
    bound_port: u16,
    base_url: String,
    instance_name: String,
    rt_handle: Handle,
    store: Store,
}

/// Entry point invoked by `main.rs`. Resolves the instance's base
/// URL, binds the listener, opens the browser (unless disabled),
/// services requests until either `/done` fires or the deadline
/// elapses, then returns.
///
/// Must be called from within a tokio runtime — the request handlers
/// run on plain OS threads (tiny_http is blocking) but each one needs
/// to drive `reqwest` async calls against Ekklesia, which they do via
/// the `tokio::runtime::Handle` captured here.
pub fn run(store: &Store, instance: &str, options: LoginOptions) -> anyhow::Result<LoginOutcome> {
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

    // Resolve the instance config up front so we surface "no such
    // instance" before opening a browser. The caller (handle_auth)
    // also runs `get_instance` for the same reason; the double-check
    // is cheap.
    let config = store.get_instance(instance)?;
    // Test-only override so the integration test can swap the
    // hydra-voting URL for a localhost mock. Not advertised in --help
    // because real users have no reason to set it.
    let base_url = std::env::var("CONCORDANCE_LOGIN_OVERRIDE_BASE_URL").unwrap_or(config.url);

    // Capture the runtime handle this function is called from. The
    // OS threads tiny_http spawns aren't tokio-aware on their own,
    // but they can `handle.block_on(future)` to drive a single
    // reqwest future to completion.
    let rt_handle = Handle::try_current().map_err(|_| {
        anyhow::anyhow!(
            "auth login must be called from within a tokio runtime \
             (main.rs uses #[tokio::main] — if this changes, plumb \
             a Handle into LoginOptions instead)"
        )
    })?;

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

    let ctx = LoginContext {
        session_token,
        bound_port: bound_addr.port(),
        base_url,
        instance_name: instance.to_string(),
        rt_handle,
        store: store.clone(),
    };
    let outcome = serve_until_done(server, ctx, options.deadline);
    Ok(outcome)
}

/// Drain HTTP requests off `server` until either:
/// - the wallet-driven flow finishes (page POSTs `/done`, or the CLI
///   auto-completes after persisting the JWT) — returns
///   [`LoginOutcome::Completed`] with whatever state was captured, or
/// - `deadline` elapses — returns [`LoginOutcome::TimedOut`].
///
/// The handler threads share an [`Arc<Mutex<LoginState>>`] so the
/// stake address captured at `/init` and the dataHex captured by the
/// background `POST /session` survive until the page polls
/// `/challenge` for them.
///
/// All inbound `Host:` headers are validated against the bound port —
/// anything else is rejected as a defense against DNS-rebinding attacks
/// (an attacker-controlled DNS name resolving to 127.0.0.1 would
/// otherwise be able to drive this listener).
fn serve_until_done(server: Server, ctx: LoginContext, deadline: Duration) -> LoginOutcome {
    let started = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(LoginState::default()));
    let ctx = Arc::new(ctx);

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
                let ctx_clone = Arc::clone(&ctx);
                // Each request gets a short-lived worker thread. The
                // worker mutates `done`/`state` per-route; the main
                // loop notices on its next tick.
                thread::spawn(move || {
                    handle_request(req, &ctx_clone, &done_clone, &state_clone);
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

/// Snapshot the captured stake address, wallet name, and user-id out
/// of the shared state and build a [`LoginOutcome::Completed`]
/// payload. Kept as a helper because the `done`-then-return path is
/// reachable from two branches in [`serve_until_done`].
fn outcome_from_state(state: &Arc<Mutex<LoginState>>) -> LoginOutcome {
    let guard = state.lock().expect("login state mutex poisoned");
    LoginOutcome::Completed {
        stake_addr: guard.stake_addr.clone(),
        wallet_name: guard.wallet_name.clone(),
        user_id: guard.user_id.clone(),
    }
}

/// Service a single inbound HTTP request. The router is small:
///
/// - `GET  /auth?k=<token>` — serve the helper HTML.
/// - `POST /init` — capture the chosen stake address, then kick off
///   the background `POST /session` call. Returns 202 immediately so
///   the page can pivot to polling `/challenge`.
/// - `GET  /challenge` — page polls; returns 204 while the flow is
///   still pending, 200 + `{dataHex}` once `POST /session` resolves,
///   or 500 + `{error}` if Ekklesia rejected the request.
/// - `POST /signed` — page returns the wallet's `{signature, key}`.
///   Handler calls `PUT /session`, persists the JWT, and stores the
///   `userId` claim on the shared state.
/// - `POST /done` — page tells the CLI to shut the listener down.
/// - everything else — 404.
///
/// `Host:` headers are validated against the listener's bound port to
/// block DNS-rebinding-style attacks. Anything that doesn't look like
/// `localhost:<port>` or `127.0.0.1:<port>` returns 400.
fn handle_request(
    req: Request,
    ctx: &Arc<LoginContext>,
    done: &Arc<AtomicBool>,
    state: &Arc<Mutex<LoginState>>,
) {
    if !host_header_is_loopback(&req, ctx.bound_port) {
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    }

    let method = req.method().clone();
    let url = req.url().to_string();
    let (path, query) = split_path_and_query(&url);

    match (&method, path.as_str()) {
        (Method::Get, "/auth") => serve_helper_page(req, &ctx.session_token, &query),
        (Method::Post, "/init") => handle_init(req, ctx, state),
        (Method::Get, "/challenge") => handle_challenge(req, ctx, state),
        (Method::Post, "/signed") => handle_signed(req, ctx, state, done),
        (Method::Post, "/done") => handle_done(req, &ctx.session_token, done),
        _ => {
            let _ = req.respond(Response::empty(StatusCode(404)));
        }
    }
}

/// `GET /auth?k=<token>` — render `login_page.html` with the per-run
/// session token interpolated. The token gate is enforced at the
/// JS-embed step, not at the HTML-body step: an attacker who can
/// reach localhost can also load the static markup from their own
/// browser. The token only matters for the `/init`, `/signed`,
/// `/done` POSTs the page later makes.
fn serve_helper_page(req: Request, expected_token: &str, query: &str) {
    let token_in_url = parse_query_param(query, "k").unwrap_or_default();
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
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
            // No-store: the page is short-lived per session and its
            // embedded session token must not be cached by a shared
            // proxy or by the browser disk cache.
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        ],
        Cursor::new(body),
        Some(len),
        None,
    );
    let _ = req.respond(response);
}

/// `POST /init` — the page hands us the user-confirmed stake address.
/// We capture it into shared state, flip the stage to
/// [`FlowStage::FetchingChallenge`], and spawn a tokio task that
/// drives Ekklesia's `POST /session`. Returns 200 immediately; the
/// page polls `/challenge` for the dataHex (or any error).
fn handle_init(mut req: Request, ctx: &Arc<LoginContext>, state: &Arc<Mutex<LoginState>>) {
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
    if !subtle_eq(body_token.as_bytes(), ctx.session_token.as_bytes()) {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }
    let stake_addr = init_payload
        .get("stakeAddr")
        .and_then(|v| v.as_str())
        .map(str::to_string);
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
    let stake_for_async = stake_addr.clone().expect("validated above");

    let log_line = {
        let mut guard = state.lock().expect("login state mutex poisoned");
        guard.stake_addr = stake_addr;
        guard.wallet_name = wallet_name;
        guard.stage = FlowStage::FetchingChallenge;
        format!(
            "concordance: received stake address {} (wallet: {})",
            guard.stake_addr.as_deref().unwrap_or("?"),
            guard.wallet_name.as_deref().unwrap_or("?"),
        )
    };
    let _ = writeln!(std::io::stderr(), "{log_line}");

    // Drive `POST /session` on the existing tokio runtime. We
    // intentionally don't await it here — the handler must return
    // promptly so the page can pivot to polling /challenge. The task
    // writes the result back into the shared state once it resolves.
    let base_url = ctx.base_url.clone();
    let state_clone = Arc::clone(state);
    ctx.rt_handle.spawn(async move {
        match post_session(&base_url, &stake_for_async).await {
            Ok(data_hex) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "concordance: nonce received from {base_url} ({} chars)",
                    data_hex.len()
                );
                let mut guard = state_clone.lock().expect("login state mutex poisoned");
                guard.stage = FlowStage::ChallengeReady { data_hex };
            }
            Err(e) => {
                let msg = format!("POST /session failed: {e}");
                let _ = writeln!(std::io::stderr(), "concordance: {msg}");
                let mut guard = state_clone.lock().expect("login state mutex poisoned");
                guard.stage = FlowStage::ChallengeError(msg);
            }
        }
    });

    let _ = req.respond(Response::from_string("ok"));
}

/// `GET /challenge` — the page polls this until the CLI has a dataHex
/// to hand to `signData`. Returns:
///
/// - 204 No Content while we're still in `AwaitingInit` or
///   `FetchingChallenge`.
/// - 200 OK + `{"dataHex": "..."}` once `POST /session` has resolved.
/// - 500 + `{"error": "..."}` if Ekklesia rejected the request — the
///   page surfaces this so the user can re-try or re-run the CLI.
///
/// Body is irrelevant on a GET; we don't need a mutable Request.
fn handle_challenge(req: Request, _ctx: &Arc<LoginContext>, state: &Arc<Mutex<LoginState>>) {
    let snapshot = state.lock().expect("login state mutex poisoned").stage.clone();
    match snapshot {
        FlowStage::AwaitingInit | FlowStage::FetchingChallenge => {
            let _ = req.respond(Response::empty(StatusCode(204)));
        }
        FlowStage::ChallengeReady { data_hex } => {
            let payload = json!({ "dataHex": data_hex }).to_string();
            let _ = respond_json(req, StatusCode(200), &payload);
        }
        FlowStage::ChallengeError(ref msg) => {
            let payload = json!({ "error": msg }).to_string();
            let _ = respond_json(req, StatusCode(500), &payload);
        }
        FlowStage::Verifying => {
            // `/signed` already received; page should be on its own
            // polling track at this point but we return 200 with a
            // hint so a confused poll doesn't loop indefinitely.
            let payload = json!({ "stage": "verifying" }).to_string();
            let _ = respond_json(req, StatusCode(200), &payload);
        }
        FlowStage::VerifyError(ref msg) => {
            let payload = json!({ "error": msg }).to_string();
            let _ = respond_json(req, StatusCode(500), &payload);
        }
        FlowStage::Verified => {
            let payload = json!({ "stage": "verified" }).to_string();
            let _ = respond_json(req, StatusCode(200), &payload);
        }
    }
}

/// `POST /signed` — the page returns the wallet's COSE_Sign1
/// signature + key. We call `PUT /session`, persist the JWT, and
/// flip the stage to `Verified`. The page (or the CLI on its own)
/// then closes the listener via `/done`.
fn handle_signed(
    mut req: Request,
    ctx: &Arc<LoginContext>,
    state: &Arc<Mutex<LoginState>>,
    done: &Arc<AtomicBool>,
) {
    let mut buf = String::new();
    if std::io::Read::read_to_string(req.as_reader(), &mut buf).is_err() {
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    }
    let payload: serde_json::Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(_) => {
            let _ = req.respond(Response::empty(StatusCode(400)));
            return;
        }
    };
    let body_token = payload
        .get("sessionToken")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !subtle_eq(body_token.as_bytes(), ctx.session_token.as_bytes()) {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }
    let signature = payload
        .get("signature")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let key = payload
        .get("key")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if signature.is_empty() {
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    }
    let stake_addr = {
        let mut guard = state.lock().expect("login state mutex poisoned");
        guard.stage = FlowStage::Verifying;
        guard.stake_addr.clone()
    };
    let Some(stake_addr) = stake_addr else {
        // /init never landed — the page got out of order. Return
        // 400 so the page can surface a "start over" hint.
        let mut guard = state.lock().expect("login state mutex poisoned");
        guard.stage = FlowStage::VerifyError("no stake address recorded".to_string());
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    };

    // Drive `PUT /session` synchronously — the page is waiting on
    // this response, and we want the JWT in hand before responding.
    let result = ctx
        .rt_handle
        .block_on(put_session(&ctx.base_url, &stake_addr, &signature, key.as_deref()));

    match result {
        Ok(auth) => {
            // Validate the JWT before storing it: same `inspect_jwt`
            // contract `auth set` uses (3 dot-separated parts, decodable
            // payload, present `exp`). If the API misbehaves we want
            // to surface that as a session error, not a corrupt store.
            match inspect_jwt(&auth.token) {
                Ok(info) => {
                    if let Err(e) = ctx.store.set_token(&ctx.instance_name, &auth.token) {
                        let msg = format!("failed to persist JWT: {e}");
                        let mut guard = state.lock().expect("login state mutex poisoned");
                        guard.stage = FlowStage::VerifyError(msg.clone());
                        let _ = respond_json(
                            req,
                            StatusCode(500),
                            &json!({ "error": msg }).to_string(),
                        );
                        return;
                    }
                    let status_line = info.status_line();
                    let user_id = info.user_id.or(Some(auth.user_id.clone()));
                    let _ = writeln!(
                        std::io::stderr(),
                        "concordance: signed in as {} ({})",
                        user_id.as_deref().unwrap_or(&auth.user_id),
                        status_line,
                    );
                    {
                        let mut guard = state.lock().expect("login state mutex poisoned");
                        guard.user_id = user_id.clone();
                        guard.stage = FlowStage::Verified;
                    }
                    // Auto-shutdown once we've persisted the JWT. The
                    // page will also POST /done on success, but
                    // setting `done` here means a misbehaving page
                    // (or a tab the user closed before the success
                    // step) can't keep the listener alive.
                    done.store(true, Ordering::Release);
                    let _ = respond_json(
                        req,
                        StatusCode(200),
                        &json!({ "userId": user_id }).to_string(),
                    );
                }
                Err(e) => {
                    let msg = format!("API returned a malformed JWT: {e}");
                    let mut guard = state.lock().expect("login state mutex poisoned");
                    guard.stage = FlowStage::VerifyError(msg.clone());
                    let _ = respond_json(
                        req,
                        StatusCode(500),
                        &json!({ "error": msg }).to_string(),
                    );
                }
            }
        }
        Err(e) => {
            let msg = format!("PUT /session failed: {e}");
            let _ = writeln!(std::io::stderr(), "concordance: {msg}");
            let mut guard = state.lock().expect("login state mutex poisoned");
            guard.stage = FlowStage::VerifyError(msg.clone());
            let _ = respond_json(
                req,
                StatusCode(500),
                &json!({ "error": msg }).to_string(),
            );
        }
    }
}

/// `POST /done` — page tells the CLI it can shut the listener down.
/// Idempotent: the CLI's own success path (after `PUT /session`)
/// also sets `done`, so a page that's slow to POST /done won't keep
/// the listener alive.
fn handle_done(mut req: Request, expected_token: &str, done: &Arc<AtomicBool>) {
    let mut buf = String::new();
    if std::io::Read::read_to_string(req.as_reader(), &mut buf).is_err() {
        let _ = req.respond(Response::empty(StatusCode(400)));
        return;
    }
    let body_token = extract_session_token(&buf).unwrap_or_default();
    if !subtle_eq(body_token.as_bytes(), expected_token.as_bytes()) {
        let _ = req.respond(Response::empty(StatusCode(403)));
        return;
    }
    done.store(true, Ordering::Release);
    let _ = req.respond(Response::from_string("ok"));
}

/// Build a JSON response with the given status + body. We use this
/// instead of `Response::from_string` because the latter sets
/// `Content-Type: text/plain`, which makes the page treat the body
/// as opaque.
fn respond_json(req: Request, status: StatusCode, body: &str) -> std::io::Result<()> {
    let bytes = body.as_bytes().to_vec();
    let len = bytes.len();
    let response = Response::new(
        status,
        vec![
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]).unwrap(),
        ],
        Cursor::new(bytes),
        Some(len),
        None,
    );
    req.respond(response)
}

// ── Ekklesia session API client ──────────────────────────────────────────
//
// These two functions wrap the unauthenticated `POST /session` and
// `PUT /session` endpoints in `docs/upstream/proposals-openapi.yaml`.
// They don't live on `EkklesiaClient` because that struct's default
// headers always carry a Bearer token — and the whole point of these
// endpoints is to mint one.

/// Result of a successful `PUT /session`. Maps to the `AuthToken`
/// schema in the OpenAPI spec.
#[derive(Debug)]
struct AuthToken {
    token: String,
    /// Echoed from the API, but we don't ship it anywhere — the JWT's
    /// own `exp` claim is authoritative and is what `inspect_jwt` reads.
    #[allow(dead_code)]
    expires_in: String,
    user_id: String,
}

/// `POST /api/v0/session` — request a nonce for the given stake
/// address. Returns the hex-encoded `dataHex` the wallet must sign.
async fn post_session(base_url: &str, stake_addr: &str) -> anyhow::Result<String> {
    let url = format!("{}/api/v0/session", base_url.trim_end_matches('/'));
    let body = json!({
        "signerAddress": stake_addr,
        "signType": "cip-8",
    });
    let resp = unauthenticated_client(base_url)?
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("connect to {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("POST {url} returned {status}: {body_text}");
    }
    let parsed: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| anyhow::anyhow!("POST /session response was not JSON: {e}"))?;
    parsed
        .get("dataHex")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("POST /session response missing dataHex"))
}

/// `PUT /api/v0/session` — verify the wallet signature and return a
/// JWT. The `signature` arg is the hex-encoded COSE_Sign1 payload
/// returned by CIP-30's `signData` (i.e. `result.signature`); `key`
/// is the COSE_Key from the same call, sent as a sibling field per
/// the Hydra Voting reference implementation. The spec accepts the
/// `key` as optional but in practice all production instances expect
/// it for non-Ed25519 stake-key signatures.
async fn put_session(
    base_url: &str,
    stake_addr: &str,
    signature: &str,
    key: Option<&str>,
) -> anyhow::Result<AuthToken> {
    let url = format!("{}/api/v0/session", base_url.trim_end_matches('/'));
    let mut body = json!({
        "signerAddress": stake_addr,
        "signType": "cip-8",
        "signature": signature,
    });
    if let Some(k) = key {
        body["key"] = json!(k);
    }
    let resp = unauthenticated_client(base_url)?
        .put(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("connect to {url}: {e}"))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("PUT {url} returned {status}: {body_text}");
    }
    let parsed: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| anyhow::anyhow!("PUT /session response was not JSON: {e}"))?;
    let token = parsed
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("PUT /session response missing token"))?
        .to_string();
    let expires_in = parsed
        .get("expiresIn")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = parsed
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(AuthToken {
        token,
        expires_in,
        user_id,
    })
}

/// Build an unauthenticated `reqwest::Client` for the two session
/// endpoints. We can't reuse [`EkklesiaClient`] because its default
/// headers carry the Bearer token we're about to mint. The `Origin`
/// header matches the existing client's behaviour (Ekklesia enforces
/// it in some deployments).
fn unauthenticated_client(base_url: &str) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ORIGIN,
        HeaderValue::from_str(base_url.trim_end_matches('/'))
            .map_err(|e| anyhow::anyhow!("invalid Origin header from base_url: {e}"))?,
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client build: {e}"))
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
