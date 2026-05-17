# Concordance

**A bridge between an AI assistant and Hydra Voting — the platform Intersect MBO uses to coordinate Cardano governance proposals before they go on-chain.** Concordance lets you ask Claude (or any other capable LLM) to read proposals, summarize comment threads, and help you draft and submit feedback on the [Intersect Hydra Voting](https://hydra-voting.intersectmbo.org) portal — without ever opening the web interface.

```
   ____                              _
  / ___|___  _ __   ___ ___  _ __ __| | __ _ _ __   ___ ___
 | |   / _ \| '_ \ / __/ _ \| '__/ _` |/ _` | '_ \ / __/ _ \
 | |__| (_) | | | | (_| (_) | | | (_| | (_| | | | | (_|  __/
  \____\___/|_| |_|\___\___/|_|  \__,_|\__,_|_| |_|\___\___|
   _____              _ _                _      _____           _
  |  ___|__  ___   __| | |__   __ _  ___| | __ |_   _|__   ___ | |
  | |_ / _ \/ _ \ / _` | '_ \ / _` |/ __| |/ /   | |/ _ \ / _ \| |
  |  _|  __/  __/| (_| | |_) | (_| | (__|   <    | | (_) | (_) | |
  |_|  \___|\___| \__,_|_.__/ \__,_|\___|_|\_\   |_|\___/ \___/|_|
```

## What is this?

Hydra Voting hosts dozens of proposals at a time, each with its own document, comment thread, and metadata. Reading carefully through them — let alone drafting considered feedback — is friction-heavy work in a web UI: scroll the list, open a proposal, read the full document, scroll the comments, switch tabs to take notes, then come back to draft a reply.

Concordance is a client tool that puts that same work in front of an AI assistant. You ask Claude — or another capable LLM — to pull the proposals you care about, help you analyze them, surface the points worth responding to, and draft a reply you can edit before sending. When you've approved the draft, the assistant submits it through Concordance. Every comment carries an automatic signature linking it to the X and Cardano Forum identities you established during onboarding, so other community members can verify who you are.

Concordance is just the input layer: it does not decide policy, it stores your credentials and identity only on your local machine, and it doesn't replace the on-chain governance vote that comes after this consensus-building phase. Hydra Voting remains the system of record; Concordance is a way to participate in it more comfortably.

Concordance changes the input method. Instead of clicking through the web UI, you **talk to an AI assistant**. The assistant uses Concordance to fetch proposals, render comment threads in a form it can summarize, and — when you OK the draft — submit your comment on your behalf.

Concrete examples of what you'd say in chat:

> *"Show me the open Cardano Budget 2026 proposals and tell me which ones are budget-line items for treasury withdrawals."*
>
> *"Render proposal 69fdc9b2… as markdown so I can read it, plus the full comment thread."*
>
> *"Draft a comment on that proposal arguing that the milestone schedule is unrealistic. I'll review your draft before you post it."*
>
> *"Post it."*

The AI handles the mechanics; you stay in chat the whole time.

## Who is this for?

- **DReps** drafting public votes and the reasoning behind them.
- **Constitutional Committee members** publishing rationales for their positions.
- **Community reviewers** weighing in on Budget 2026, treasury withdrawals, and similar proposals.
- Anyone who'd rather think in conversation than click around a portal.

You do not need to be a developer. You do not need to be comfortable in a terminal. You **do** need a Cardano wallet (Lace, Eternl, Yoroi, Nami, etc.) so the platform can authenticate you, and **either** Claude Code installed locally **or** another LLM that supports the MCP standard.

## How it works (the short version)

Concordance is a small program that exposes Hydra-Voting's operations as **tools** an AI assistant can call. The standard for this is called [MCP](https://modelcontextprotocol.io) (Model Context Protocol). Claude Code natively speaks MCP, so registering Concordance with Claude Code is a one-time setup; from then on, every conversation can use it.

Every comment Concordance posts on your behalf is automatically signed with a block that says:

```
--
<your name>
X Handle: @<your X handle>
Cardano Forum: <your forum username>
via Concordance Feedback Tool
```

You also make a one-time **verification post** on X or the Cardano Forum that mentions your Cardano stake address. That public post lets anyone who sees one of your Hydra-Voting comments verify that the claimed handle in the signature really is you. (Details below.)

## Get started with Claude Code

You'll need [Claude Code](https://claude.com/claude-code) installed and a working Rust toolchain (or Nix). If you have neither Rust nor Nix, ask Claude *"install the prerequisites for Concordance"* and it'll walk you through it.

### Step 1 — Get the code and build it

```sh
git clone https://github.com/adamrusch/concordance.git
cd concordance
cargo build --release
```

That produces a binary at `target/release/concordance`. Note the absolute path — you'll need it in the next step.

### Step 2 — Tell Claude Code where Concordance lives

Concordance is an MCP "server." Claude Code talks to it like any other tool. Register it once with:

```sh
claude mcp add concordance /absolute/path/to/concordance/target/release/concordance mcp
```

Replace `/absolute/path/to/...` with the real path on your machine (the output of `pwd` inside the cloned directory, followed by `/target/release/concordance`).

If you prefer editing your Claude Code MCP config file directly instead of running the CLI, add this block:

```json
{
  "mcpServers": {
    "concordance": {
      "command": "/absolute/path/to/concordance/target/release/concordance",
      "args": ["mcp"]
    }
  }
}
```

That's the entire integration. There's no URL to fill in, no API key to manage on Claude Code's side — Concordance lives on your machine and Claude Code launches it as a subprocess when needed.

### Step 3 — Open a new Claude Code chat and onboard

Start with:

> *"Help me set up Concordance."*

Claude will walk you through:

1. **Your community identity.** Three questions: the name you go by in the Cardano community, your X handle (without the `@`), and your Cardano Forum username. Use `none` for X or Forum if you don't have an account there.
2. **Wallet sign-in.** You'll log in to <https://hydra-voting.intersectmbo.org> with your wallet (DReps and regular Ada holders both work). Then you'll copy one cookie value out of your browser's DevTools and paste it in chat. Claude stores it locally; it never leaves your machine.
3. **Stake address linking.** Claude reads the stake address out of your authentication token and adds it to your local identity file.
4. **Verification post.** Claude shows you a short message to copy-paste publicly to X or the Cardano Forum. This is what lets other community members confirm the signature on your comments is really you. You only do this once per wallet.

The complete walkthrough lives at [docs/getting-started-with-claude.md](docs/getting-started-with-claude.md) — but most users won't need to read it directly; Claude leads.

### Step 4 — Use it

After onboarding, anything related to Hydra-Voting can flow through chat. Ask Claude what's open for feedback, get a proposal rendered as Markdown, summarize a thread, draft a comment, submit it. Claude always prompts you to approve before actually posting (the `create_comment` action is marked destructive in MCP terms, which means MCP clients require explicit confirmation).

## Using Concordance with other LLMs (OpenAI, Gemini, Grok, …)

Concordance is built around MCP because that's the cleanest cross-LLM standard right now. **Claude Code is the only client we develop against and test with.** But the design is intentionally portable:

- The MCP server emits **standard JSON-RPC over stdio** — anything that speaks MCP can connect.
- Tool argument schemas are emitted as **standard JSON Schema** — easy to translate into OpenAI function specs, Gemini function declarations, or any similar format.
- The signature contract, the verification post, the identity file format — all of it is described in plain text in [`docs/mcp-tool-surface.md`](docs/mcp-tool-surface.md).

If you'd like to use Concordance with ChatGPT, Codex, Gemini, Grok, or another assistant, **ask that assistant to read the tool surface doc and write a small adapter for itself**. Most current models can do this in a single conversation. Generated descriptors that work specifically with OpenAI and Gemini are planned for a future release (see "Roadmap" below).

> **Disclaimer.** Cross-LLM use is **not officially supported** by the maintainer. We don't test against other models, we don't guarantee a port will work, and we won't be the first responder if it doesn't. Treat Claude Code as the reference experience; treat any other client as a community effort.

## What every Concordance-posted comment looks like

Plain language: every comment you post through Concordance has a four-line signature attached. There's no way to make Concordance post unsigned in normal use; the only escape hatch is a flag intended for testing.

Example:

> Looking at Work Package 2 specifically — the 12-week milestone for "production-ready dApp integrations" assumes a level of partner cooperation that isn't documented anywhere in the proposal. I'd want to see commitment letters or at least an LOI from one of the named partners before this milestone is approved.
>
> \--
> Adam Rusch
> X Handle: @adamrusch
> Cardano Forum: adam_rusch
> via Concordance Feedback Tool

Anyone reading that comment on Hydra-Voting can:

1. See it was posted from your Cardano stake address (that's a property of the platform, independent of Concordance).
2. See the signature claiming you go by "Adam Rusch" on X (`@adamrusch`) and on the Cardano Forum (`adam_rusch`).
3. Visit your X profile (or Cardano Forum profile) to find your verification post — a public message you made that names the same stake address.

This three-way link is your **proof of identity**. The stake address proves cryptographically that the comment came from a particular wallet. The verification post on X/Forum proves that wallet belongs to the person who runs that account. Together, comments are attributable, and the trust does not require Concordance, the maintainer, or any third party.

## What's shipped today

| Capability | Status |
|---|---|
| Read votes, proposals, and comment threads | ✅ |
| Render proposals as Markdown (LLM-friendly) | ✅ |
| Post comments with auto-signature | ✅ |
| Identity & verification-post flow | ✅ (v0.3) |
| MCP server over stdio (Claude Code, Cursor, Continue, …) | ✅ |
| First-boot banner + clear `--help` output | ✅ |
| `like`, edit-your-own-comment, focused thread reads | 🚧 v0.2.1 |
| Generated tool descriptors for OpenAI / Gemini | 🚧 v0.4 |
| Submit / withdraw proposals (proposer flow) | 🚧 v0.4 |

The current tool catalog has 12 tools. The full spec — with arguments, return shapes, and design rationale — is at [docs/mcp-tool-surface.md](docs/mcp-tool-surface.md).

## Roadmap

- **v0.2.1** — round out the read & write surface: comment likes, comment editing (15-min window on the server), focused thread reads by author type, and the proposal `search` filter (needs more API spelunking to find the right wire format).
- **v0.4** — proposal-authoring tools for DReps and CC members submitting their own proposals; generated tool descriptors so OpenAI, Gemini, and Grok can use Concordance natively without per-LLM adapters.

If you have an opinion on what should land next, open an issue.

## For developers

Concordance is a soft fork of [exegesis](https://github.com/nixedge/exegesis) (also Apache-2.0). The Rust core, the CLI subcommands, and the proposal-markdown format are inherited as-is; the fork's purpose is to add the agent-facing surface (MCP server, identity & signature, machine-readable output) without disturbing exegesis's terminal-user experience.

Direct CLI use (no LLM in the loop) is the same as exegesis: `concordance --help` lists `instances`, `auth`, `votes`, `proposals`, `comments`, plus the new `mcp` subcommand. The proposal-markdown format and the submission flow are documented in the [upstream README](https://github.com/nixedge/exegesis/blob/master/README.md).

```sh
cargo test --release
```

…runs 126 tests across 5 suites: unit tests for parsing, auth, store, identity; property tests; integration round-trip tests; and an MCP smoke test that spawns the binary, drives the MCP protocol over stdio, and asserts the v0.3 tool catalog + annotation contract.

## License

Apache 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Concordance © 2026 Adam Rusch; derived from exegesis © 2026 NixEdge, LLC.
