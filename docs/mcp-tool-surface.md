# MCP tool surface

This document describes the Model Context Protocol tools that Concordance
exposes (and will expose) to LLM agents. The catalog is the contract between
Concordance and any agent — Claude Code, Cursor, Continue, OpenAI-compatible
clients via MCP bridges, etc. Tools added here should be reviewed against the
[ship tiers](#ship-tiers) below; tools removed are a breaking change.

For the design rationale behind individual tools, see the inline **Why**
field. For the user-facing setup flow, see
[getting-started-with-claude.md](getting-started-with-claude.md).

## Conventions

- **Names** are `snake_case`, verb-prefix, no namespace prefix (the server's
  identity already namespaces them).
- **Annotations** use MCP spec terms: `readOnlyHint`, `destructiveHint`,
  `idempotentHint`, `openWorldHint`.
- **Args** are listed with `?` for optional. Default values noted where
  relevant.
- **`instance?`** appears on every API-touching tool. When omitted, falls
  back to the configured default instance (matches the CLI's `-i` flag).
  As of v0.3.1 the default resolves to a built-in fallback
  (`hydra-voting.intersectmbo.org`) when no DB entry exists, so a fresh
  install reaches Intersect MBO's Hydra Voting deployment with zero
  `instances add` calls. The fallback lives in code, not the DB —
  changing it is a release-level change, and users can't accidentally
  remove it.

## Ship tiers

| Tier | Status | Scope |
|------|--------|-------|
| v0.2 MVP | ✅ shipped | 7 tools — review proposals and post a comment via an agent |
| **v0.3 identity & signature** | ✅ shipped | **5 identity tools + signature injection in `create_comment`** |
| v0.2.1 stretch | planned | 4 tools — fills out read/comment surface (likes, replies-only list, comment edit, search filter) |
| v0.4 authoring | planned | 4 tools — proposal submission/withdrawal (proposers only) |
| local/config | partial | 2 tools — multi-instance helpers |

## Identity & signature contract

Every comment posted through Concordance carries a signature block — name,
X handle, Cardano Forum username, "via Concordance Feedback Tool" — so any
reader can attribute the comment to a human via the X/Forum platforms.

The user records their community identity *before* the wallet step, then
links their stake address afterward. The agent should walk a new user
through this sequence:

1. `set_identity { name, x_handle, cardano_forum_name }` — write the three
   identity fields to `$XDG_CONFIG_HOME/concordance/identity.toml`.
   `"none"` is the documented sentinel for users without an X or Forum
   account.
2. (User configures their Hydra-Voting instance and JWT through the
   existing setup flow — see [getting-started-with-claude.md](getting-started-with-claude.md).)
3. `link_stake_address` — read the `userId` claim from the stored JWT and
   record it as the identity's `stake_address`.
4. `get_verification_post` — return a public post the user copy-pastes to
   X or the Cardano Forum; readers verify the signature's claimed handle
   matches the stake address that signs the verification post.
5. From here on, every `create_comment` automatically appends the
   signature; calls without a configured identity fail before any I/O.

## v0.3 identity tools

### `set_identity`

| | |
|---|---|
| **Args** | `name`, `x_handle`, `cardano_forum_name` |
| **Annotations** | `readOnlyHint: false`, `idempotentHint: true`, `openWorldHint: false` |
| **Returns** | `{saved_to, identity, signature_preview}` |
| **Why** | Captures the community identity before the wallet step. Strips a leading `@` from `x_handle`. Empty fields rejected; `"none"` accepted. Preserves a previously linked `stake_address` across re-runs. |

### `get_identity`

| | |
|---|---|
| **Args** | — |
| **Annotations** | `readOnlyHint`, `idempotentHint`, `openWorldHint: false` |
| **Returns** | `{identity, saved_to, signature}` |
| **Why** | Lets the agent confirm an identity is configured before drafting a post. Errors (with the config-file path) if not set. |

### `link_stake_address`

| | |
|---|---|
| **Args** | `instance?` |
| **Annotations** | `readOnlyHint: false`, `idempotentHint: true`, `openWorldHint: false` |
| **Returns** | `{instance, stake_address, sign_type, identity}` |
| **Why** | Pulls the `userId` claim from the stored JWT and writes it into the identity file. Local-only — no network. Errors if no identity is configured yet, or if the JWT lacks a `userId` claim. |

### `get_verification_post`

| | |
|---|---|
| **Args** | `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint`, `openWorldHint: false` |
| **Returns** | `{post_text, stake_address, portal_url}` |
| **Why** | The text the user pastes to X / Cardano Forum so other community members can verify the stake address claimed in the signature. Errors if the stake address isn't linked yet. |

### `get_signature`

| | |
|---|---|
| **Args** | — |
| **Annotations** | `readOnlyHint`, `idempotentHint`, `openWorldHint: false` |
| **Returns** | `{signature, identity}` |
| **Why** | Lets the agent preview the exact signature block with the user before posting. |

## v0.2 MVP

### `list_votes`

| | |
|---|---|
| **Args** | `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint` |
| **Returns** | array of `{id, slug, title, feedback_window: {start, end, is_open, time_remaining}, voting_window, comments_enabled}` |
| **Why** | Entry point: the agent has to know what cycles exist before it can do anything else. Pre-computes feedback-window state so the agent doesn't need a clock + a date library. |

### `list_proposals`

| | |
|---|---|
| **Args** | `vote_id`, `status?` (`live\|withdrawn\|all`, default `live`), `page?` (default `1`), `limit?` (default `20`), `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint` |
| **Returns** | `{data: [{id, title, summary, status, proposer, version, comment_count, submitted_at}], meta: {page, limit, total, total_pages, has_next_page}}` |
| **Why** | Bulk browse with a status filter — useful for cycles with many proposals (Budget 2026 has 69). `draft` is admin/owner-only on the server and is omitted from the user-facing enum. |

> **Deferred to v0.2.1:** the `search?` param (substring match) and the `category?` filter (which maps to vote-specific `metaData.strategyFramework.pillars` and would let an agent filter for, e.g., a specific strategic pillar). Empirical probing showed the obvious `?search=` and `?query=` URL params don't filter on hydra-voting; the search transport needs more investigation before we expose it as a tool arg.

### `get_proposal`

| | |
|---|---|
| **Args** | `proposal_id`, `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint` |
| **Returns** | full proposal object including raw `meta_data` |
| **Why** | Raw object access for when the agent needs to act on structured fields (budget totals, work-package items, status enums). Sibling to `render_proposal_markdown`. |

### `render_proposal_markdown`

| | |
|---|---|
| **Args** | `proposal_id`, `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint` |
| **Returns** | `{markdown, frontmatter}` |
| **Why** | LLMs reason better over markdown than nested JSON. Same code path as the existing `proposals get` CLI command. |

### `fetch_proposal_thread`

| | |
|---|---|
| **Args** | `proposal_id`, `include_replies?` (default `true`), `max_depth?` (default `unlimited`), `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint` |
| **Returns** | `{proposal_markdown, vote_window, comments: [{id, author: {name, type}, content, created_at, replies: [recursive]}]}` |
| **Why** | The headline workflow tool. One call returns everything needed to review a proposal. Without it the agent makes 5–10 calls (get vote, get proposal, list comments, list replies-per-comment×N), each filling context with intermediate JSON. The composite costs one round-trip and gives the LLM a single coherent payload to reason over. Primitives are still available for cases where the agent needs precision. |

### `create_comment`

| | |
|---|---|
| **Args** | `proposal_id`, `content`, `parent_id?`, `omit_signature?` (default `false`), `instance?` |
| **Annotations** | **`destructiveHint`**, *not* `idempotentHint`, `openWorldHint` |
| **Returns** | created comment object |
| **Why** | The reason this project exists. Public and irreversible by non-admins → marked destructive so Claude Code prompts the user before each invocation. The agent should draft in chat, get explicit user OK, then call this tool. |

**Signature injection:** the user's identity signature is appended to every
post unless `omit_signature: true`. The combined `content + signature` must
fit the 2000-char server limit; the tool rejects over-length submissions
with a precise hint about how much to trim. Calls without a configured
identity fail with a pointer at [`set_identity`](#set_identity). The
signature applies to replies too — provenance stays clear in deep threads.

### `auth_status`

| | |
|---|---|
| **Args** | `instance?` |
| **Annotations** | `readOnlyHint`, `idempotentHint`, local-only (no network) |
| **Returns** | `{instance, valid, expires_at, seconds_remaining, user_id, sign_type}` |
| **Why** | Lets the agent proactively check token freshness before attempting a write. Avoids the failure mode where the agent submits a comment, hits 401, and the user has to re-explain what they wanted. |

## v0.2.1 stretch — not in v0.2

`list_comments` (focused thread reads with `user_type` filter), `list_comment_replies` (drill-down), `toggle_comment_like` (mildly destructive, non-idempotent), `update_comment` (15-min server-side edit window only, destructive).

## v0.3 authoring — not yet scoped

`submit_proposal` (wraps `proposals submit`, `dry_run` defaulting to `true`),
`update_proposal`, `withdraw_proposal`, `delete_proposal_draft`. All destructive.
Relevant only when authoring via LLM (not the current user goal).

## Tools we deliberately did not add

- **`draft_comment_from_proposal`** — pre-fills a markdown template quoting the
  proposal's summary. Belongs client-side: any LLM can produce this from
  `render_proposal_markdown` output. Adding a server tool here just constrains
  the prompt.
- **`describe_feedback_window`** — folded into `list_votes` and
  `fetch_proposal_thread` as computed `is_open` / `time_remaining` fields.
- **`summarize_thread`** — pure LLM task. The server shouldn't summarize;
  the agent should. Anything the server pre-summarizes is a place we'd be
  unable to tune for the user's specific question.

## Confirmation UX

Only `create_comment` is `destructiveHint: true` in v0.2. Claude Code (and
similar MCP clients) will prompt the user before invoking. The agent should
always draft the comment in chat, get the user's explicit OK, then call
`create_comment` — the destructive prompt is a *second* safety net, not the
primary one.

Future versions may add a `confirmed_by_user: true` parameter for
session-trust mode, where the user can grant ongoing approval after the first
prompt. Not in v0.2.

## Authentication

In MCP mode, the JWT is sourced in this order:

1. `CONCORDANCE_JWT` environment variable (lets an agent inject the token
   per-session without writing it to disk).
2. The local store entry for the requested instance (plain TOML at
   `$XDG_DATA_HOME/concordance/tokens.toml`, mode 0600 on POSIX). As of
   v0.3.2 the store is plain TOML files rather than an embedded sled DB
   so the CLI and the MCP server can share state without lock conflict.

If neither resolves a valid token, tool calls return an error pointing the
agent at the local-only `auth_status` tool and the
[getting-started doc](getting-started-with-claude.md).
