# Concordance

**LLM-mediated client for the Ekklesia governance API** (Cardano Hydra-Voting and related instances). Concordance lets you review proposals, post comments, and submit proposals by talking to an LLM agent — no terminal required for the common workflow.

> Concordance is a soft fork of [exegesis](https://github.com/nixedge/exegesis). The Rust core, CLI commands, and proposal-markdown format are inherited as-is. The fork exists to add agent-facing surfaces (MCP server, machine-readable output, deterministic ordering) without disturbing exegesis's terminal-user experience.

## Status

| Layer | Status |
|-------|--------|
| Rust core (API client, parser, store) | ✅ inherited from exegesis |
| CLI (`concordance ...`) | ✅ works; matches exegesis subcommands |
| Claude Code workflow (tutorial + auth probe) | ✅ [docs/getting-started-with-claude.md](docs/getting-started-with-claude.md) |
| MCP server (`concordance mcp`) | 🚧 planned for v0.2 |
| `--json` output across CLI | 🚧 planned for v0.2 |
| Generated tool descriptors (OpenAI / Gemini) | 🚧 planned for v0.3 |

## Two ways to use it

### Through an LLM agent (Claude Code, Cursor, etc.)

The recommended path. See [Getting Started with Claude](docs/getting-started-with-claude.md). You stay in your chat client; the agent runs Concordance for you.

Once the MCP server is built (v0.2), any MCP-aware agent — Claude Code, Cursor, Continue, and via bridges the OpenAI / Gemini / Grok ecosystems — will be able to use the Hydra-Voting tools natively.

### Directly from a terminal

```sh
cargo build --release
./target/release/concordance --help
```

CLI subcommands match exegesis verbatim: `instances`, `auth`, `votes`, `proposals`, `comments`. See [exegesis's README](https://github.com/nixedge/exegesis/blob/master/README.md) for the proposal-markdown format and the full command set — they apply unchanged.

## Why fork?

Exegesis is a tight, well-tested CLI aimed at people who live in a terminal. Concordance has a different audience: governance participants (DReps, CC members, community reviewers) who want to interact with Hydra-Voting through an LLM. The two goals push the codebase in different directions:

| | exegesis | Concordance |
|---|----------|-------------|
| Primary user | terminal-comfortable developer | LLM-mediated participant |
| Output format | human-readable | both human-readable and machine-parseable |
| Distribution | Nix flake / cargo install | the above + MCP server + tool descriptors |
| Scope of new features | CLI ergonomics | agent affordances (idempotency, deterministic output, schema export) |

Where a change benefits both, it should flow back upstream. The Claude-mediated tutorial and the `scripts/probe-auth.sh` helper are good candidates.

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Concordance © 2026 Adam Rusch; derived from exegesis © 2026 NixEdge, LLC.
