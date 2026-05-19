# Getting started with Concordance (via Claude Code)

This guide is for people who want to review and comment on Ekklesia proposals
(Cardano governance: CC votes, budget proposals, treasury withdrawals) without
having to be a CLI user. You drive the workflow by talking to Claude Code;
Claude runs all the terminal commands and calls the right MCP tools.

If you're comfortable in a terminal, see the [README](../README.md) instead —
this guide just wraps the same commands in a Claude-mediated flow.

## One thing requires you to leave Claude

**Sign in to the voting platform with your Cardano wallet** — only you can
approve the wallet's signature prompt. Concordance opens a localhost helper
page in your browser that connects your CIP-30 wallet and walks you through
the three steps; everything else stays in chat.

Everything else — collecting your identity, installing Concordance,
configuring it, posting comments with your signature — Claude does for you.

## Prerequisites

- A Cardano wallet (Lace, Eternl, Yoroi, Nami, etc.) loaded in your browser.
- Claude Code running on your machine. (See
  [claude.com/claude-code](https://claude.com/claude-code).)
- Rust toolchain (`rustc` ≥ 1.75) or Nix. If you have neither, ask Claude:
  *"install the prerequisites for Concordance."*

## Step 1 — Tell Claude who you are in the Cardano community

Before any wallet step, Concordance records the identity you go by in the
community. This becomes the signature on every comment you post.

Tell Claude:

> *"Set up my Concordance identity."*

Claude will ask for three things:

- **Name** — the identity you go by in the Cardano community (e.g. your
  preferred display name; doesn't have to be your legal name).
- **X handle** — your X (Twitter) handle without the leading `@`. Use
  `none` if you have no X account you want to associate.
- **Cardano Forum username** — your username on
  [forum.cardano.org](https://forum.cardano.org). Use `none` if you don't
  have one.

These three fields establish your **proof of identity** — once you've also
linked a wallet (Step 3), you'll make a public verification post on X or the
Cardano Forum so other community members can link the signature back to a
real human.

The values are stored in `~/.config/concordance/identity.toml` (macOS:
`~/Library/Application Support/concordance/identity.toml`). You can read or
edit this file directly at any time.

## Step 2 — Sign in with `concordance auth login`

Ask Claude:

> *"Sign me in to Concordance."*

Claude runs `concordance auth login`. The CLI:

1. Opens a one-shot HTTP server on `127.0.0.1` at a random port.
2. Launches your default browser at `http://localhost:<port>/auth?k=<token>`.
3. Detects every CIP-30 wallet you have installed (Lace, Eternl, Nami,
   Yoroi, Vespr, etc.). You pick the wallet whose stake address you want
   to be publicly identified with — that address will be visible on every
   comment you post.
4. Asks the wallet to sign a one-time challenge from Hydra Voting (no
   on-chain activity; the signature only proves you control the key).
5. Submits the signature to `PUT /api/v0/session`, receives a JWT,
   stores it for you. No DevTools, no cookie-scraping.

> You can sign in either as a **DRep** or as a **regular Ada holder**.
> Concordance works with both. The Ekklesia API accepts feedback comments
> from any authenticated user regardless of DRep status.

When the helper page says **"Signed in"**, you're done — close the tab.
The CLI prints `concordance: signed in as stake1u...` and exits.

> **If the browser doesn't auto-open** (headless box, no `$BROWSER`),
> the CLI prints the URL on stderr — paste it into any browser on the
> same machine. The listener only accepts requests from `127.0.0.1` /
> `localhost`, so the helper page can't be loaded remotely.

### Manual / scripting fallback: `auth set`

If you can't run a browser on the same machine as Concordance (CI
runners, remote dev boxes you SSH into, etc.), the v0.3 `auth set`
path still works: paste a JWT in via stdin, a file, or `$CONCORDANCE_JWT`.
See `concordance auth set --help` for the full source list.

`auth set` is also useful if you already have a long-lived JWT from
another tool — e.g. a CI secrets store — and don't want to re-prompt
your wallet every day.

## Step 3 — Post a verification message on X or the Cardano Forum

Ask Claude:

> *"Show me my verification post."*

Claude calls `get_verification_post` and prints a short message containing
your stake address and a link to the voting portal. Copy that exactly,
then post it:

- **on X** under the handle you set in Step 1, **or**
- **on Cardano Forum** under the username you set in Step 1.

Either one is enough. The presence of a public post under your claimed
handle that mentions your stake address is what lets readers verify that
the signature on a Hydra-Voting comment links to the same person who runs
that X / Forum account.

You only need to do this once per identity (or whenever you switch to a
different wallet).

## Step 4 — Review and comment

You're set up. Ask Claude things like:

- *"Show me the open vote cycles and which ones are still in their
  feedback window."*
- *"List the proposals in `cardano-budget-2026`. Anything with
  `treasury withdrawal` in the title?"*
- *"Render proposal `<id>` and the comment thread so I can read it."*
- *"Help me draft a comment on `<id>`. Quote the executive summary at
  the top and leave space for my response."*
- *"Submit the comment I just drafted."*

The agent's drafts go through `create_comment`, which:

- **Always appends your signature** (name, X handle, Forum name, "via
  Concordance Feedback Tool"). The signature applies to replies too — your
  identity stays attached even deep in a thread.
- **Prompts you to approve** before each invocation (it's marked
  `destructiveHint: true`). The "agent drafts, you approve, agent submits"
  loop is the intended workflow.
- **Errors clearly** if your identity isn't set up, your JWT is expired,
  or the combined `content + signature` would exceed the 2000-char server
  limit.

## Troubleshooting

**`401 JWT rejected`** — your token expired. Re-run
`concordance auth login` (or ask Claude *"Sign me in to Concordance again."*)
to mint a fresh one.

**`no identity configured`** — you skipped Step 1. Ask Claude: *"Set up
my Concordance identity."*

**`stake address not yet linked`** when generating the verification post —
you skipped the linking step. Ask Claude: *"Link my stake address."*

**No CIP-30 wallet detected on the helper page** — install Lace, Eternl,
Yoroi, Nami, or any other CIP-30-compatible browser extension, then
reload the page (or re-run `concordance auth login`). Wallets inject
their API into `window.cardano.*` only after the extension has finished
loading.

**Browser didn't open** — the CLI prints `If the browser didn't open,
paste this URL` followed by a `http://localhost:<port>/auth?k=...`
link on stderr. Paste that into any browser running on the same
machine. The listener binds `127.0.0.1` only, so a browser on a
different machine can't reach it (and a `Host:` header from a public
DNS name resolving to 127.0.0.1 is rejected too).

**Wallet declined to sign** — re-run `concordance auth login`. The
helper page surfaces wallet error messages inline so you can see why
the prompt was rejected (locked wallet, wrong network, etc.).

**Manual / scripting paths** — if you have a JWT from another source
or can't run a wallet on this machine, `concordance auth set --jwt -`
(stdin), `--jwt-file <path>`, or `$CONCORDANCE_JWT` all still work
and won't trigger the browser flow.

**`error: store error: ... WouldBlock`** — fixed in v0.3.2; if you still
see it, you're on an older build. Pull, `cargo build --release`, retry.

**`error: concordance is already running with the database open`** — also
fixed in v0.3.2 (CLI and MCP server share the on-disk store cleanly). If
you see this, you're on a build between v0.3.1 and v0.3.2: quit Claude
Code or `pkill -f 'concordance mcp'` and retry while you upgrade.

## What's in the repo for this flow

- [`scripts/probe-auth.sh`](../scripts/probe-auth.sh) — non-destructive
  auth probe. Reads `EKKLESIA_JWT` from env or accepts JWT as `$1`.
- [`docs/mcp-tool-surface.md`](mcp-tool-surface.md) — full MCP tool
  catalog with annotations and the signature contract.
- [`docs/getting-started-with-claude.md`](getting-started-with-claude.md) —
  this file.
