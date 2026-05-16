//! Integration smoke test for the MCP server.
//!
//! Spawns the `concordance mcp` binary in a subprocess with an isolated
//! HOME directory (so it uses a fresh empty sled store, no real credentials),
//! sends MCP protocol messages over stdio, and asserts the v0.2 tool catalog
//! is exactly what `docs/mcp-tool-surface.md` promises.
//!
//! No network access — the test only exercises `initialize` and `tools/list`.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(5);

async fn send(stdin: &mut ChildStdin, msg: Value) {
    let mut line = serde_json::to_vec(&msg).expect("serialize message");
    line.push(b'\n');
    stdin.write_all(&line).await.expect("write to server stdin");
    stdin.flush().await.expect("flush server stdin");
}

async fn recv(stdout: &mut BufReader<ChildStdout>) -> Value {
    let mut line = String::new();
    let read_fut = stdout.read_line(&mut line);
    let n = timeout(TIMEOUT, read_fut)
        .await
        .expect("server response within 5s")
        .expect("read line from server");
    assert!(n > 0, "server closed stdout unexpectedly");
    serde_json::from_str(&line).expect("server reply is valid JSON")
}

#[tokio::test]
async fn server_initializes_and_lists_v0_3_tool_catalog() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .arg("mcp")
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn concordance mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // initialize
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "smoke", "version": "0.1"}
            }
        }),
    )
    .await;
    let init = recv(&mut stdout).await;
    assert_eq!(init["id"], json!(1));
    assert!(init["result"]["serverInfo"]["name"].is_string());
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // initialized notification (no reply expected)
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await;

    // tools/list
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    let expected = vec![
        "auth_status",
        "create_comment",
        "fetch_proposal_thread",
        "get_identity",
        "get_proposal",
        "get_signature",
        "get_verification_post",
        "link_stake_address",
        "list_proposals",
        "list_votes",
        "render_proposal_markdown",
        "set_identity",
    ];
    assert_eq!(names, expected, "v0.3 tool catalog");

    // Every read tool must be readOnly + idempotent.
    for read_tool in [
        "auth_status",
        "list_votes",
        "list_proposals",
        "get_proposal",
        "render_proposal_markdown",
        "fetch_proposal_thread",
        "get_identity",
        "get_signature",
        "get_verification_post",
    ] {
        let t = tools.iter().find(|t| t["name"] == read_tool).unwrap();
        let ann = &t["annotations"];
        assert_eq!(
            ann["readOnlyHint"],
            json!(true),
            "{read_tool} should be readOnly"
        );
        assert_eq!(
            ann["idempotentHint"],
            json!(true),
            "{read_tool} should be idempotent"
        );
    }

    // Identity-write tools change local state but are not "destructive" in
    // the MCP sense — the user can re-run them. They must NOT be marked
    // destructive (else MCP clients would prompt twice per onboarding).
    for write_tool in ["set_identity", "link_stake_address"] {
        let t = tools.iter().find(|t| t["name"] == write_tool).unwrap();
        let ann = &t["annotations"];
        assert_eq!(
            ann["readOnlyHint"],
            json!(false),
            "{write_tool} writes state"
        );
        assert_eq!(
            ann.get("destructiveHint").unwrap_or(&json!(false)),
            &json!(false),
            "{write_tool} should NOT be destructive"
        );
        assert_eq!(
            ann["idempotentHint"],
            json!(true),
            "{write_tool} should be idempotent"
        );
    }

    // The single write tool must carry the destructive hint and not claim
    // idempotency. This is the safety contract Claude Code and other MCP
    // clients rely on to prompt the user before invocation.
    let cc = tools
        .iter()
        .find(|t| t["name"] == "create_comment")
        .unwrap();
    let ann = &cc["annotations"];
    assert_eq!(ann["readOnlyHint"], json!(false));
    assert_eq!(ann["destructiveHint"], json!(true));
    assert_eq!(ann["idempotentHint"], json!(false));
    assert_eq!(ann["openWorldHint"], json!(true));

    // Each tool exposes a JSON Schema for its arguments.
    for t in tools {
        let schema = &t["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "{} inputSchema should be an object",
            t["name"]
        );
    }

    // Tools that take a proposal_id must declare it as required.
    for need_proposal in [
        "get_proposal",
        "render_proposal_markdown",
        "fetch_proposal_thread",
        "create_comment",
    ] {
        let t = tools.iter().find(|t| t["name"] == need_proposal).unwrap();
        let required = t["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{need_proposal} should have required[]"));
        assert!(
            required.iter().any(|v| v == "proposal_id"),
            "{need_proposal} should require proposal_id, got {required:?}"
        );
    }

    // set_identity must require all three identity fields — these are the
    // contract for the on-disk identity.toml format.
    let set_id = tools.iter().find(|t| t["name"] == "set_identity").unwrap();
    let required = set_id["inputSchema"]["required"]
        .as_array()
        .expect("set_identity should have required[]");
    let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    for field in ["name", "x_handle", "cardano_forum_name"] {
        assert!(
            required_names.contains(&field),
            "set_identity should require {field}, got {required_names:?}"
        );
    }

    // Graceful shutdown.
    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
async fn identity_lifecycle_round_trip() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .arg("mcp")
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn concordance mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},
                      "clientInfo":{"name":"smoke","version":"0.1"}}
        }),
    )
    .await;
    recv(&mut stdout).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .await;

    // get_identity before set should error.
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"get_identity","arguments":{}}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    assert!(
        resp.get("error").is_some(),
        "get_identity should error when nothing's saved, got {resp:?}"
    );

    // set_identity with a leading @ should strip it.
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"set_identity","arguments":{
                "name":"Test User","x_handle":"@testhandle","cardano_forum_name":"forum_user"
            }}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    let body: serde_json::Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["identity"]["x_handle"], "testhandle");

    // get_identity now returns the saved values + a signature.
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"get_identity","arguments":{}}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    let body: serde_json::Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let sig = body["signature"].as_str().expect("signature should be a string");
    assert!(sig.contains("Test User"));
    assert!(sig.contains("@testhandle"));
    assert!(sig.contains("forum_user"));
    assert!(sig.contains("via Concordance Feedback Tool"));

    // get_verification_post must fail when stake_address isn't linked.
    // (No instance configured here — invalid_params for missing default.)
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"get_verification_post","arguments":{}}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    assert!(
        resp.get("error").is_some(),
        "get_verification_post should error before link, got {resp:?}"
    );

    // create_comment must refuse to run without identity? — actually
    // identity is set above, so this would try to hit the network. We
    // exercised the "no identity" path in the no-identity sub-test below.

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
async fn create_comment_refuses_without_identity() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .arg("mcp")
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn concordance mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},
                      "clientInfo":{"name":"smoke","version":"0.1"}}
        }),
    )
    .await;
    recv(&mut stdout).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .await;

    // No identity, no instance, no JWT — but the identity check is first
    // in the validation order, so we should see the "no identity" error
    // before any of the other guard rails.
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"create_comment","arguments":{
                "proposal_id":"ffffffffffffffffffffffff",
                "content":"hello"
            }}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    let msg = resp["error"]["message"]
        .as_str()
        .expect("expected error.message");
    assert!(
        msg.contains("identity"),
        "expected identity-related error, got {msg:?}"
    );

    drop(stdin);
    let _ = child.wait().await;
}

#[tokio::test]
async fn auth_status_works_against_empty_store() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_concordance"))
        .arg("mcp")
        .env_clear()
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn concordance mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},
                      "clientInfo":{"name":"smoke","version":"0.1"}}
        }),
    )
    .await;
    recv(&mut stdout).await;
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .await;

    // With no instance configured, the call should return a JSON-RPC error
    // (invalid_params: no default instance), not a panic or hang.
    send(
        &mut stdin,
        json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"auth_status","arguments":{}}
        }),
    )
    .await;
    let resp = recv(&mut stdout).await;
    assert!(resp.get("error").is_some(), "expected error, got {resp:?}");

    drop(stdin);
    let _ = child.wait().await;
}
