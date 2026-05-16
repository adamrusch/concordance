//! MCP (Model Context Protocol) server for Concordance.
//!
//! Exposes the Ekklesia governance API to LLM agents via the MCP standard.
//! The tool catalog and design rationale live in `docs/mcp-tool-surface.md`.
//!
//! Transport: stdio (the only transport MCP clients spawn as a subprocess).
//! Entry point: `concordance mcp` subcommand → [`run_stdio`].

use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{auth::inspect_jwt, error::Error, store::Store};

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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuthStatusArgs {
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

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

        let body = serde_json::to_string_pretty(&report)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(body)]))
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
