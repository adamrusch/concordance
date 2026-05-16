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
async fn server_initializes_and_lists_v0_2_tool_catalog() {
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
        "get_proposal",
        "list_proposals",
        "list_votes",
        "render_proposal_markdown",
    ];
    assert_eq!(names, expected, "v0.2 MVP tool catalog");

    // Every read tool must be readOnly + idempotent.
    for read_tool in [
        "auth_status",
        "list_votes",
        "list_proposals",
        "get_proposal",
        "render_proposal_markdown",
        "fetch_proposal_thread",
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

    // Graceful shutdown.
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
