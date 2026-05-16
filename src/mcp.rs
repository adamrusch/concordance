//! MCP (Model Context Protocol) server for Concordance.
//!
//! Exposes the Ekklesia governance API to LLM agents via the MCP standard.
//! The tool catalog and design rationale live in `docs/mcp-tool-surface.md`.
//!
//! Transport: stdio (the only transport MCP clients spawn as a subprocess).
//! Entry point: `concordance mcp` subcommand → [`run_stdio`].

use chrono::{DateTime, Utc};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    api::Vote,
    auth::{inspect_jwt, require_valid_jwt},
    client::EkklesiaClient,
    error::Error,
    render::render_proposal_md,
    store::Store,
};

const JWT_ENV_VAR: &str = "CONCORDANCE_JWT";

/// Concordance MCP server. One instance handles the lifetime of one stdio
/// connection — typically spawned as a subprocess by Claude Code, Cursor,
/// or another MCP-aware client.
#[derive(Clone)]
pub struct ConcordanceServer {
    store: std::sync::Arc<Store>,
    // Used by the rmcp macros (#[tool_router] / #[tool_handler]); the
    // compiler can't see those references and would otherwise warn.
    #[allow(dead_code)]
    tool_router: ToolRouter<ConcordanceServer>,
}

impl ConcordanceServer {
    pub fn new(store: Store) -> Self {
        Self {
            store: std::sync::Arc::new(store),
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve the effective instance: argument-provided value wins, else the
    /// configured default.
    fn resolve_instance(&self, requested: Option<String>) -> Result<String, ErrorData> {
        match requested {
            Some(name) => Ok(name),
            None => self
                .store
                .default_instance()
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None)),
        }
    }

    /// Build an authenticated HTTP client for `instance`. JWT source order:
    /// 1. `CONCORDANCE_JWT` env var (lets an agent inject per-session).
    /// 2. The sled store entry for the instance.
    fn make_client(&self, instance: &str) -> Result<EkklesiaClient, ErrorData> {
        let config = self
            .store
            .get_instance(instance)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let jwt = match std::env::var(JWT_ENV_VAR) {
            Ok(v) if !v.is_empty() => v,
            _ => self
                .store
                .get_token(instance)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
        };
        require_valid_jwt(&jwt, instance)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        EkklesiaClient::new(&config.url, &jwt)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

/// Serialize a JSON value into MCP `text` content.
fn json_result(value: Value) -> Result<CallToolResult, ErrorData> {
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

/// Build a `{start, end, is_open, time_remaining_seconds}` object for a
/// time-bounded window. `None` start/end produce nulls in the corresponding
/// fields, and `is_open` is `false` whenever either endpoint is missing.
fn window_state(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Value {
    let is_open = matches!((start, end), (Some(s), Some(e)) if now >= s && now < e);
    let time_remaining_seconds = end.map(|e| (e - now).num_seconds());
    serde_json::json!({
        "start": start.map(|d| d.to_rfc3339()),
        "end": end.map(|d| d.to_rfc3339()),
        "is_open": is_open,
        "time_remaining_seconds": time_remaining_seconds,
    })
}

/// Render a `Vote` as the agent-facing object documented in
/// `docs/mcp-tool-surface.md`: identity + computed window state + flags.
fn render_vote(v: &Vote, now: DateTime<Utc>) -> Value {
    serde_json::json!({
        "id": v.id,
        "slug": v.slug,
        "title": v.title,
        "description": v.description,
        "comments_enabled": v.comments_enabled,
        "feedback_window": window_state(v.feedback_start_date, v.feedback_end_date, now),
        "voting_window": window_state(v.voting_start_date, v.voting_end_date, now),
    })
}

// ── Tool argument schemas ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuthStatusArgs {
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListVotesArgs {
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
    /// 1-indexed page number.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Page size (1-100).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListProposalsArgs {
    /// Vote-cycle id (24-hex). Find one with `list_votes`.
    pub vote_id: String,
    /// Status filter. `live` (default), `withdrawn`, or `all`. `draft`
    /// proposals are admin/owner-only and cannot be retrieved by other users.
    #[serde(default)]
    pub status: Option<String>,
    /// 1-indexed page number.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Page size (1-100).
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetProposalArgs {
    /// Proposal id (24-hex).
    pub proposal_id: String,
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenderProposalMarkdownArgs {
    /// Proposal id (24-hex).
    pub proposal_id: String,
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

fn default_page() -> u32 {
    1
}
fn default_limit() -> u32 {
    20
}

// ── Tools ────────────────────────────────────────────────────────────────────

#[tool_router]
impl ConcordanceServer {
    /// Report whether the stored JWT for an instance is valid and how long
    /// until it expires. Local-only — no network call. Use before any write
    /// to avoid the failure mode of submitting a comment with an expired
    /// token.
    #[tool(
        name = "auth_status",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn auth_status(
        &self,
        Parameters(args): Parameters<AuthStatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let report = match self.store.get_token(&instance) {
            Err(Error::NoToken(_)) => serde_json::json!({
                "instance": instance,
                "valid": false,
                "reason": "no token configured for this instance",
            }),
            Err(e) => return Err(ErrorData::internal_error(e.to_string(), None)),
            Ok(jwt) => match inspect_jwt(&jwt) {
                Ok(info) => serde_json::json!({
                    "instance": instance,
                    "valid": !info.is_expired,
                    "expires_at": info.expires_at.to_rfc3339(),
                    "seconds_remaining": info.seconds_remaining,
                }),
                Err(e) => serde_json::json!({
                    "instance": instance,
                    "valid": false,
                    "reason": format!("token parse error: {e}"),
                }),
            },
        };
        json_result(report)
    }

    /// List the vote cycles configured on the instance. Each entry includes
    /// computed `feedback_window` and `voting_window` state so the agent can
    /// reason about open/closed comment and voting periods without doing its
    /// own date math.
    #[tool(
        name = "list_votes",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn list_votes(
        &self,
        Parameters(args): Parameters<ListVotesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;
        let page = client
            .list_votes(args.page, args.limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let now = Utc::now();
        let data: Vec<Value> = page.data.iter().map(|v| render_vote(v, now)).collect();
        json_result(serde_json::json!({
            "data": data,
            "meta": page.meta,
        }))
    }

    /// List proposals within a vote cycle. Status defaults to `live`. Pass
    /// `all` to include withdrawn proposals; `draft` is admin/owner-only and
    /// rejected by the server for non-privileged callers.
    #[tool(
        name = "list_proposals",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn list_proposals(
        &self,
        Parameters(args): Parameters<ListProposalsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;

        let status_filter = match args.status.as_deref() {
            None | Some("live") => Some("live"),
            Some("withdrawn") => Some("withdrawn"),
            Some("all") => None,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!(
                        "unknown status {other:?}; expected one of: live, withdrawn, all"
                    ),
                    None,
                ));
            }
        };

        let page = client
            .list_proposals(&args.vote_id, status_filter, args.page, args.limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        json_result(serde_json::to_value(&page).unwrap_or(Value::Null))
    }

    /// Fetch a single proposal by id, including its full `meta_data` blob.
    /// Use `render_proposal_markdown` if you want a human-readable view; this
    /// tool returns the raw API object for structured inspection.
    #[tool(
        name = "get_proposal",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn get_proposal(
        &self,
        Parameters(args): Parameters<GetProposalArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;
        let proposal = client
            .get_proposal(&args.proposal_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        json_result(serde_json::to_value(&proposal).unwrap_or(Value::Null))
    }

    /// Render a proposal as Markdown — same format as the `proposals get` CLI
    /// command. Returns `{markdown, frontmatter}` where `frontmatter` is the
    /// raw `meta_data` for programmatic access.
    #[tool(
        name = "render_proposal_markdown",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn render_proposal_markdown(
        &self,
        Parameters(args): Parameters<RenderProposalMarkdownArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;
        let proposal = client
            .get_proposal(&args.proposal_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let markdown = render_proposal_md(&proposal);
        json_result(serde_json::json!({
            "proposal_id": proposal.id,
            "title": proposal.title,
            "markdown": markdown,
            "frontmatter": proposal.meta_data,
        }))
    }
}

#[tool_handler]
impl ServerHandler for ConcordanceServer {}

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub async fn run_stdio(store: Store) -> anyhow::Result<()> {
    // Logs go to stderr; stdout is reserved for MCP messages.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let server = ConcordanceServer::new(store);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_state_open_in_middle() {
        let now = "2026-05-16T12:00:00Z".parse().unwrap();
        let start: DateTime<Utc> = "2026-05-01T00:00:00Z".parse().unwrap();
        let end: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        let w = window_state(Some(start), Some(end), now);
        assert_eq!(w["is_open"], serde_json::json!(true));
        assert!(w["time_remaining_seconds"].as_i64().unwrap() > 0);
    }

    #[test]
    fn window_state_closed_after_end() {
        let now = "2026-07-01T00:00:00Z".parse().unwrap();
        let start: DateTime<Utc> = "2026-05-01T00:00:00Z".parse().unwrap();
        let end: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        let w = window_state(Some(start), Some(end), now);
        assert_eq!(w["is_open"], serde_json::json!(false));
        assert!(w["time_remaining_seconds"].as_i64().unwrap() < 0);
    }

    #[test]
    fn window_state_not_yet_open() {
        let now = "2026-04-01T00:00:00Z".parse().unwrap();
        let start: DateTime<Utc> = "2026-05-01T00:00:00Z".parse().unwrap();
        let end: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        let w = window_state(Some(start), Some(end), now);
        assert_eq!(w["is_open"], serde_json::json!(false));
        assert!(w["time_remaining_seconds"].as_i64().unwrap() > 0);
    }

    #[test]
    fn window_state_missing_endpoints_is_closed() {
        let now = "2026-05-16T12:00:00Z".parse().unwrap();
        let start: DateTime<Utc> = "2026-05-01T00:00:00Z".parse().unwrap();
        // missing end
        let w = window_state(Some(start), None, now);
        assert_eq!(w["is_open"], serde_json::json!(false));
        assert!(w["time_remaining_seconds"].is_null());
        // missing start
        let end: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        let w = window_state(None, Some(end), now);
        assert_eq!(w["is_open"], serde_json::json!(false));
    }
}
