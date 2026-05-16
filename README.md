# Concordance

**LLM-mediated client for the Ekklesia governance API** (Cardano Hydra-Voting and related instances). Concordance lets you review proposals, post comments, and submit proposals by talking to an LLM agent — no terminal required for the common workflow.

> Concordance is a soft fork of [exegesis](https://github.com/nixedge/exegesis). The Rust core, CLI commands, and proposal-markdown format are inherited as-is. The fork exists to add agent-facing surfaces (MCP server, machine-readable output, deterministic ordering) without disturbing exegesis's terminal-user experience.

## Status

| Layer | Status |
|-------|--------|
| Rust core (API client, parser, store) | ✅ inherited from exegesis |
| CLI (`concordance ...`) | ✅ same subcommands as exegesis |
| Claude Code workflow (tutorial + auth probe) | ✅ [docs/getting-started-with-claude.md](docs/getting-started-with-claude.md) |
| **MCP server (`concordance mcp`)** | ✅ **v0.2 — 7 tools, live-verified on hydra-voting.intersectmbo.org** |
| MCP tool stretch surface (likes, reply lists, comment edit, search filter) | 🚧 v0.2.1 |
| Generated tool descriptors (OpenAI / Gemini schemas exported from MCP) | 🚧 v0.3 |
| Proposal authoring tools over MCP (submit / withdraw) | 🚧 v0.3 |

## Quick start (Claude Code)

1. **Build:** `cargo build --release` (or `nix build`).
2. **One-time config** of your instance and JWT — see [docs/getting-started-with-claude.md](docs/getting-started-with-claude.md). Claude walks you through it.
3. **Wire Concordance into Claude Code's MCP registry**:

   ```sh
   claude mcp add concordance \
     /absolute/path/to/concordance/target/release/concordance mcp
   ```

   Or equivalently, add to your MCP config file:

   ```json
   {
     "mcpServers": {
       "concordance": {
         "command": "/absolute/path/to/concordance",
         "args": ["mcp"],
         "env": { "CONCORDANCE_JWT": "optional override" }
       }
     }
   }
   ```

4. **Use it from chat.** Ask Claude things like *"show me the open Cardano Budget 2026 proposals"*, *"render proposal `<id>` and the comment thread"*, *"draft a comment on the third one — I'll OK the text before you submit."* Claude calls the right MCP tools; `create_comment` always prompts for your approval (it's marked `destructiveHint: true`).

## The v0.2 tool catalog

7 tools, all live-verified. Full spec with rationale at [docs/mcp-tool-surface.md](docs/mcp-tool-surface.md).

| Tool | Kind | What it does |
|---|---|---|
| `auth_status` | read, local | Is the stored JWT valid? How long until it expires? |
| `list_votes` | read | Lists vote cycles with computed `feedback_window` and `voting_window` state (is_open + time_remaining_seconds) |
| `list_proposals` | read | Lists proposals in a cycle, filterable by status (`live` / `withdrawn` / `all`) |
| `get_proposal` | read | Full proposal object including `meta_data` |
| `render_proposal_markdown` | read | Proposal as human-readable Markdown + frontmatter blob |
| `fetch_proposal_thread` | read, composite | One-call review payload: proposal markdown + feedback window + full comment tree |
| `create_comment` | **destructive** | Post a public comment. MCP clients prompt before invocation. |

## Direct CLI use

```sh
./target/release/concordance --help
```

Subcommands inherited from exegesis: `instances`, `auth`, `votes`, `proposals`, `comments`. The new `mcp` subcommand runs the server over stdio (used by MCP clients; you don't invoke it manually). See [exegesis's README](https://github.com/nixedge/exegesis/blob/master/README.md) for the proposal-markdown format and the full command set.

## Why fork?

Exegesis is a tight, well-tested CLI aimed at people who live in a terminal. Concordance has a different audience: governance participants (DReps, CC members, community reviewers) who want to interact with Hydra-Voting through an LLM. The two goals push the codebase in different directions:

| | exegesis | Concordance |
|---|----------|-------------|
| Primary user | terminal-comfortable developer | LLM-mediated participant |
| Output format | human-readable | both human-readable and machine-parseable (via MCP) |
| Distribution | Nix flake / cargo install | the above + MCP server + tool descriptors |
| Scope of new features | CLI ergonomics | agent affordances (idempotency, deterministic output, schema export) |

Where a change benefits both, it should flow back upstream. The Claude-mediated tutorial and `scripts/probe-auth.sh` are candidates.

## Testing

```sh
cargo test --release
```

94 tests across 5 suites: unit tests for parsing/auth/store, property tests, integration round-trip tests, and an MCP smoke test that spawns the binary, drives the MCP protocol over stdio, and asserts the v0.2 catalog + annotation contract.

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Concordance © 2026 Adam Rusch; derived from exegesis © 2026 NixEdge, LLC.
