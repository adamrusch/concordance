# Getting started with Concordance (via Claude Code)

This guide is for people who want to review and comment on Ekklesia proposals
(Cardano governance: CC votes, budget proposals, treasury withdrawals) without
having to be a CLI user. You drive the workflow by talking to Claude Code;
Claude runs all the terminal commands and calls the right MCP tools.

If you're comfortable in a terminal, see the [README](../README.md) instead —
this guide just wraps the same commands in a Claude-mediated flow.

## Two things require you to leave Claude

1. **Sign in to the voting platform with your Cardano wallet** — only you can
   sign the authentication challenge.
2. **Copy one cookie from your browser** — Claude can't read your browser
   cookies, so you have to paste the value into chat.

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

## Step 2 — Sign in to Hydra-Voting with a Cardano wallet

Open <https://hydra-voting.intersectmbo.org> in your browser. Sign in with
**the wallet you want to be publicly identified with** — its stake address
will be visible on every comment you post, and other community members will
use it to verify your identity claim from Step 1.

> You can sign in either as a **DRep** or as a **regular Ada holder**.
> Concordance works with both. The Ekklesia API accepts feedback comments
> from any authenticated user regardless of DRep status.

Once you see your stake address in the corner of the page, you're in.

Tell Claude: *"I'm signed in to Hydra-Voting."*

## Step 3 — Copy your `token` cookie

The cookie is `HttpOnly`, so the JavaScript console can't read it. You need
DevTools' storage view.

**Chrome / Edge / Brave / Arc:**

1. Press `Cmd+Option+I` (macOS) or `Ctrl+Shift+I` (Windows/Linux).
2. Click the **Application** tab. (Use the `»` overflow menu if it's hidden.)
3. Left sidebar: **Storage → Cookies → `https://hydra-voting.intersectmbo.org`**.
4. Find the row where **Name** is `token`. Double-click its **Value** cell —
   it expands to a long string with three dot-separated segments.
5. `Cmd+A`, `Cmd+C`. Expect roughly 300–1500 characters.

**Firefox:**

1. `Cmd+Option+I` → **Storage** tab → **Cookies → hydra-voting.intersectmbo.org**.
2. Click the `token` row; right-click the value → **Copy**.

**Safari:**

1. Enable Develop menu: **Safari → Settings → Advanced → "Show features for web developers"**.
2. `Cmd+Option+I` → **Storage** → **Cookies → hydra-voting.intersectmbo.org**.
3. Copy the **Value** column for the `token` row.

### Security note

The `token` is bearer-equivalent — anyone who has it can act as you on the
platform until it expires (typically ~24 hours). Two safer options if you
don't want to paste it directly into chat:

- **Type it into a local file yourself.** Ask Claude: *"I'd rather not
  paste my JWT in chat — set up a local file route."* Claude will tell you
  exactly what to write where, so the value never enters this conversation.
- **Rotate after.** Log out of hydra-voting.intersectmbo.org when you're
  done. That invalidates the token.

If you're fine pasting it for this session, send it to Claude as your next
message.

## Step 4 — Claude configures Concordance

Once you've pasted the token, Claude will:

1. Build Concordance if it's not already built (`cargo build --release` —
   first build takes a few minutes; subsequent runs are instant).
2. Register the Intersect Hydra Voting instance.
3. Store your token.
4. Verify the token is valid (`auth_status` shows time-to-expiry and the
   stake address that signed it).
5. **Link the stake address** to your identity file from Step 1
   (`link_stake_address`).
6. Run a read smoke test (`list_votes` returns the active vote cycles
   with computed feedback-window state).

## Step 5 — Post a verification message on X or the Cardano Forum

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

## Step 6 — Review and comment

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

**`401 JWT rejected`** — your token expired. Re-do Step 3 to grab a fresh one.

**`no identity configured`** — you skipped Step 1. Ask Claude: *"Set up
my Concordance identity."*

**`stake address not yet linked`** when generating the verification post —
you skipped the linking step. Ask Claude: *"Link my stake address."*

**The cookie isn't visible in DevTools** — make sure you're signed in (not
just on the landing page) and that you've clicked the cookie scope for
`https://hydra-voting.intersectmbo.org` specifically.

**Token is short / only one segment when pasted** — DevTools sometimes
truncates when you single-click instead of double-click the cell.
Double-click to expand, then `Cmd+A` to select the whole value.

## What's in the repo for this flow

- [`scripts/probe-auth.sh`](../scripts/probe-auth.sh) — non-destructive
  auth probe. Reads `EKKLESIA_JWT` from env or accepts JWT as `$1`.
- [`docs/mcp-tool-surface.md`](mcp-tool-surface.md) — full MCP tool
  catalog with annotations and the signature contract.
- [`docs/getting-started-with-claude.md`](getting-started-with-claude.md) —
  this file.
