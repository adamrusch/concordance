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
    api::{Comment, CreateCommentRequest, Vote},
    auth::{inspect_jwt, require_valid_jwt},
    client::EkklesiaClient,
    error::Error,
    identity::{Identity, prepare_comment_content},
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
pub struct SetIdentityArgs {
    /// The name the user goes by in the Cardano community. This appears on
    /// every comment they post via Concordance.
    pub name: String,
    /// X (Twitter) handle without the leading `@`. Use the literal string
    /// "none" if the user has no X account they want to associate.
    pub x_handle: String,
    /// Cardano Forum username. Use "none" if no Forum account.
    pub cardano_forum_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceOnlyArgs {
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateCommentArgs {
    /// Proposal id (24-hex) to comment on. The proposal must be live and
    /// within its vote cycle's feedback window.
    pub proposal_id: String,
    /// Comment body. Markdown is accepted. The Concordance signature is
    /// appended automatically (see `omit_signature` to opt out); the
    /// combined `content + signature` must fit the 2000-char server limit.
    pub content: String,
    /// Optional id of a comment to reply to. Omit for a top-level comment.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Suppress the Concordance signature. Defaults to `false` — every
    /// post normally carries the signature for community traceability.
    /// Only set `true` in unusual circumstances (e.g. testing) and tell
    /// the user explicitly that their identity will not be attached.
    #[serde(default)]
    pub omit_signature: bool,
    /// Instance name. Omit to use the configured default instance.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchProposalThreadArgs {
    /// Proposal id (24-hex).
    pub proposal_id: String,
    /// Whether to recursively fetch replies under each top-level comment.
    /// Default `true`. Set to `false` for a flat top-level-only view.
    #[serde(default = "default_true")]
    pub include_replies: bool,
    /// Maximum reply depth. 0 = top-level only (same as `include_replies = false`).
    /// 1 = top-level + direct replies. Default `2`.
    /// Capped at 10 to avoid pathological fanout.
    #[serde(default = "default_depth")]
    pub max_depth: u32,
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
fn default_true() -> bool {
    true
}
fn default_depth() -> u32 {
    2
}

/// Render a comment + its (already-fetched) child replies as a JSON node.
fn render_comment_node(c: &Comment, replies: Vec<Value>) -> Value {
    serde_json::json!({
        "id": c.id,
        "parent_id": c.parent_id,
        "author": c.author.as_ref().map(|a| serde_json::json!({
            "name": a.name,
            "type": a.author_type,
        })),
        "content": c.content,
        "created_at": c.created_at.map(|d| d.to_rfc3339()),
        "reply_count": c.reply_count,
        "like_count": c.like_count,
        "replies": replies,
    })
}

/// Recursively fetch reply tree under a comment, stopping at `max_depth`.
/// Returns a `Vec<Value>` ready to splice into a parent's `replies` field.
/// Boxed because the future is self-referential through recursion.
fn fetch_replies_recursive<'a>(
    client: &'a EkklesiaClient,
    parent_id: String,
    depth: u32,
    max_depth: u32,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<Value>, Error>> + Send + 'a>,
> {
    Box::pin(async move {
        if depth >= max_depth {
            return Ok(vec![]);
        }
        let page = client.list_comment_replies(&parent_id, 1, 100).await?;
        let mut out = Vec::with_capacity(page.data.len());
        for reply in &page.data {
            let kids = if reply.reply_count.unwrap_or(0) > 0 {
                fetch_replies_recursive(client, reply.id.clone(), depth + 1, max_depth).await?
            } else {
                Vec::new()
            };
            out.push(render_comment_node(reply, kids));
        }
        Ok(out)
    })
}

// ── Tools ────────────────────────────────────────────────────────────────────

#[tool_router]
impl ConcordanceServer {
    /// Report whether the stored JWT for an instance is valid and how long
    /// until it expires. Local-only — no network call. Surfaces the
    /// `userId` (stake address) and `signType` (`stake` or `drep`) so the
    /// agent can confirm which wallet/identity is in use.
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
                    "user_id": info.user_id,
                    "sign_type": info.sign_type,
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

    /// Store the user's Cardano community identity — name, X handle, and
    /// Cardano Forum username — to the local identity file. Call this
    /// before any wallet step, so the signature can be built before the
    /// first comment is posted.
    ///
    /// Existing fields are overwritten; the stake address (if previously
    /// linked) is preserved. Use the string "none" for x_handle or
    /// cardano_forum_name if the user has no account on that platform.
    #[tool(
        name = "set_identity",
        annotations(read_only_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn set_identity(
        &self,
        Parameters(args): Parameters<SetIdentityArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        for (label, value) in [
            ("name", &args.name),
            ("x_handle", &args.x_handle),
            ("cardano_forum_name", &args.cardano_forum_name),
        ] {
            if value.trim().is_empty() {
                return Err(ErrorData::invalid_params(
                    format!("{label} cannot be empty; use \"none\" if not applicable"),
                    None,
                ));
            }
        }

        // Preserve stake_address across re-runs of set_identity.
        let prior_stake = Identity::load().ok().and_then(|i| i.stake_address);
        let id = Identity {
            name: args.name.trim().to_string(),
            x_handle: args.x_handle.trim().trim_start_matches('@').to_string(),
            cardano_forum_name: args.cardano_forum_name.trim().to_string(),
            stake_address: prior_stake,
        };
        id.save()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        json_result(serde_json::json!({
            "saved_to": Identity::default_path().display().to_string(),
            "identity": &id,
            "signature_preview": id.signature(),
        }))
    }

    /// Return the user's saved identity. Errors with a configuration hint if
    /// no identity is stored.
    #[tool(
        name = "get_identity",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_identity(&self) -> Result<CallToolResult, ErrorData> {
        let id = Identity::load().map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        json_result(serde_json::json!({
            "identity": &id,
            "saved_to": Identity::default_path().display().to_string(),
            "signature": id.signature(),
        }))
    }

    /// Extract the stake address (`userId`) from the configured JWT and write
    /// it into the local identity file. Run this after the user logs in to
    /// Hydra-Voting with their wallet and `auth set --jwt <token>` (or the
    /// MCP-equivalent has stored the token).
    ///
    /// Local-only: reads the sled store, doesn't hit the network. Errors if
    /// no identity is configured yet or if the JWT lacks a `userId` claim.
    #[tool(
        name = "link_stake_address",
        annotations(read_only_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn link_stake_address(
        &self,
        Parameters(args): Parameters<InstanceOnlyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let mut id = Identity::load()
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let jwt = self
            .store
            .get_token(&instance)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let info =
            inspect_jwt(&jwt).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let stake = info.user_id.ok_or_else(|| {
            ErrorData::invalid_params(
                "JWT has no userId claim; cannot derive stake address".to_string(),
                None,
            )
        })?;
        id.stake_address = Some(stake.clone());
        id.save()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        json_result(serde_json::json!({
            "instance": instance,
            "stake_address": stake,
            "sign_type": info.sign_type,
            "identity": &id,
        }))
    }

    /// Return the suggested public verification post — text the user
    /// copy-pastes to X or the Cardano Forum so other community members can
    /// link the Concordance signature back to a real human. The
    /// `{stake_address}` and `{Hydra Voting Portal URL}` placeholders are
    /// substituted with the user's stake address and the instance base URL.
    ///
    /// Errors if the stake address has not been linked yet (the user must
    /// run `link_stake_address` first) or if no identity is configured.
    #[tool(
        name = "get_verification_post",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_verification_post(
        &self,
        Parameters(args): Parameters<InstanceOnlyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let config = self
            .store
            .get_instance(&instance)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let id =
            Identity::load().map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let post = id
            .verification_post(&config.url)
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        json_result(serde_json::json!({
            "post_text": post,
            "stake_address": id.stake_address,
            "portal_url": config.url,
        }))
    }

    /// Return the signature block that will be appended to every comment.
    /// Local-only, no I/O beyond reading the identity file. Useful for
    /// previewing the signature with the user before they post.
    #[tool(
        name = "get_signature",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn get_signature(&self) -> Result<CallToolResult, ErrorData> {
        let id = Identity::load().map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        json_result(serde_json::json!({
            "signature": id.signature(),
            "identity": &id,
        }))
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

    /// Composite read: fetch a proposal as Markdown, its vote-cycle feedback
    /// window, and the full comment thread (top-level + nested replies up to
    /// `max_depth`). Costs one tool call instead of the 5–10 a pure-primitive
    /// agent would make. Designed for "give me everything I need to review."
    #[tool(
        name = "fetch_proposal_thread",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn fetch_proposal_thread(
        &self,
        Parameters(args): Parameters<FetchProposalThreadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;

        let max_depth = if !args.include_replies {
            0
        } else {
            args.max_depth.min(10)
        };

        let proposal = client
            .get_proposal(&args.proposal_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let vote_window = if let Some(vote_id) = &proposal.vote_id {
            match client.get_vote(vote_id).await {
                Ok(v) => Some(window_state(
                    v.feedback_start_date,
                    v.feedback_end_date,
                    Utc::now(),
                )),
                Err(_) => None,
            }
        } else {
            None
        };

        let top_level = client
            .list_comments(&proposal.id, 1, 100)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let mut comments = Vec::with_capacity(top_level.data.len());
        for c in &top_level.data {
            let kids = if max_depth > 0 && c.reply_count.unwrap_or(0) > 0 {
                fetch_replies_recursive(&client, c.id.clone(), 0, max_depth)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            } else {
                Vec::new()
            };
            comments.push(render_comment_node(c, kids));
        }

        json_result(serde_json::json!({
            "proposal_id": proposal.id,
            "title": proposal.title,
            "status": proposal.status,
            "proposal_markdown": render_proposal_md(&proposal),
            "feedback_window": vote_window,
            "comment_count_total": top_level.meta.total,
            "comment_count_fetched": comments.len(),
            "comments": comments,
        }))
    }

    /// Post a comment on a proposal. The comment is public and irreversible
    /// by non-admin users (the 15-minute server-side edit window allows typo
    /// fixes via `update_comment`, but withdraw is admin-only).
    ///
    /// **The Concordance signature is automatically appended to every
    /// comment** (name, X handle, Cardano Forum name, "via Concordance
    /// Feedback Tool"). The user must call `set_identity` once before their
    /// first post — `create_comment` errors if no identity is configured.
    /// The signature applies to replies too, not just top-level comments,
    /// so provenance is unambiguous in deep threads. Set `omit_signature:
    /// true` only in unusual circumstances.
    ///
    /// Server constraints:
    ///   - The proposal must be live and within its vote cycle's
    ///     `feedbackEndDate`.
    ///   - `content + signature` max 2000 chars; duplicates of recent
    ///     comments are rejected.
    ///   - Rate limit: 5 / min, 20 / hour per user.
    ///
    /// The agent should draft the comment in chat, get the user's explicit
    /// approval, and then call this tool. The `destructiveHint` annotation
    /// causes MCP clients (e.g. Claude Code) to prompt the user before
    /// invocation as a second safety net.
    #[tool(
        name = "create_comment",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true,
        )
    )]
    async fn create_comment(
        &self,
        Parameters(args): Parameters<CreateCommentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Build the final content the server will see — with signature
        // unless explicitly suppressed. Shared with the `comments add` CLI
        // path via `prepare_comment_content` so both honor the same contract.
        let final_content = prepare_comment_content(
            &args.content,
            args.omit_signature,
            "omit_signature: true",
        )
        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;

        let instance = self.resolve_instance(args.instance)?;
        let client = self.make_client(&instance)?;

        let req = CreateCommentRequest {
            proposal_id: args.proposal_id,
            content: final_content,
            parent_id: args.parent_id,
        };
        let result = client
            .create_comment(&req)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        json_result(result)
    }
}

#[tool_handler]
impl ServerHandler for ConcordanceServer {}

/// Run the MCP server over stdio. Blocks until the client disconnects.
pub async fn run_stdio(store: Store) -> anyhow::Result<()> {
    // Banner + logs go to stderr; stdout is reserved for MCP JSON-RPC.
    eprintln!("{}", crate::BANNER);

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
