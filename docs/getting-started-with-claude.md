# Getting started with Concordance (via Claude Code)

This guide is for people who want to review and comment on Ekklesia proposals
(Cardano governance: CC votes, budget proposals, treasury withdrawals) without
having to be a CLI user. You drive the workflow by talking to Claude Code;
Claude runs all the terminal commands.

If you're comfortable in a terminal, see the [README](../README.md) instead —
this guide just wraps the same commands in a Claude-mediated flow.

## What you'll do

Two things require you to leave Claude:

1. **Sign in to the voting platform with your Cardano wallet** — only you can
   sign the authentication challenge.
2. **Copy one cookie from your browser** — Claude can't read your browser
   cookies, so you have to paste the value into chat.

Everything else — installing Concordance, configuring it, verifying it works,
listing proposals, posting comments — Claude does for you.

## Prerequisites

- A Cardano wallet (Lace, Eternl, Yoroi, Nami, etc.) loaded in your browser.
- Claude Code running on your machine. (See
  [claude.com/claude-code](https://claude.com/claude-code).)
- Rust toolchain (`rustc` ≥ 1.75) or Nix. If you have neither, ask Claude:
  *"install the prerequisites for Concordance."*

## Step 1 — Sign in with your wallet

Open <https://hydra-voting.intersectmbo.org> in your browser and sign in with
your wallet. You'll be asked to sign a message — that's the platform's CIP-8
authentication. Once you see your stake address in the corner of the page,
you're in.

Tell Claude: *"I'm signed in."*

## Step 2 — Copy your `token` cookie

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

1. `Cmd+Option+I` (macOS) → **Storage** tab → **Cookies → hydra-voting.intersectmbo.org**.
2. Click the `token` row; right-click the value → **Copy**.

**Safari:**

1. Enable Develop menu: **Safari → Settings → Advanced → "Show features for web developers"**.
2. `Cmd+Option+I` → **Storage** → **Cookies → hydra-voting.intersectmbo.org**.
3. Copy the **Value** column for the `token` row.

### Security note

The `token` is bearer-equivalent — anyone who has it can act as you on the
platform until it expires (typically ~24 hours). Two safer options if you don't
want to paste it directly into chat:

- **Type it into a local file yourself.** Ask Claude: *"I'd rather not paste
  my JWT in chat — set up a local file route."* Claude will tell you exactly
  what to write where, so the value never enters this conversation.
- **Rotate after.** Log out of hydra-voting.intersectmbo.org when you're done.
  That invalidates the token.

If you're fine pasting it for this session, send it to Claude as your next
message.

## Step 3 — Claude configures Concordance

Once you've pasted the token, Claude will:

1. Build Concordance if it's not already built (`cargo build --release` — first
   build takes a few minutes; subsequent runs are instant).
2. Register the Intersect Hydra Voting instance.
3. Store your token.
4. Verify the token is valid (`auth status` shows time-to-expiry).
5. Run a read smoke test (`votes` lists the active vote cycles).
6. Run a non-destructive **auth probe** to confirm the API will accept your
   comments. See [`scripts/probe-auth.sh`](../scripts/probe-auth.sh) for what
   this does — it posts to `/api/v0/comments` with a deliberately fake
   proposal ID, so no real comment is ever created. The HTTP status tells
   Claude whether your account can comment:

   | Status | Meaning |
   |--------|---------|
   | 404 | Auth + role passed; only the proposal lookup failed → you can comment |
   | 400 / 422 | Body validation failed before any role check; auth still passed |
   | 403 | Role-gated; this account can't comment via the API |
   | 401 | JWT rejected (expired or wrong value) |

If the probe returns 404 or 4xx-with-validation-error, you're good to go.

## Step 4 — Review and comment

Ask Claude things like:

- *"Show me the open vote cycles."*
- *"List the proposals in cardano-budget-2026."*
- *"Render proposal `<id>` as markdown so I can read it here."*
- *"Show me the comment thread on proposal `<id>`."*
- *"I want to draft a comment on `<id>`. Quote the executive summary at the top
  and leave space for my response."*
- *"Submit the comment I just drafted (dry-run first)."*

Claude knows the feedback windows (when the comment period closes for each
vote cycle) and will warn you if a window is closing soon.

## Troubleshooting

**`401 JWT rejected`** — your token expired. Re-do Step 2 to grab a fresh one.

**`403` on the probe** — the API has gated your account. This is rare on the
public instance; ask Claude to inspect the response body for the reason.

**`auth status` says expired** — the token's `exp` claim is in the past.
Sign in again and re-copy the cookie.

**The cookie isn't visible in DevTools** — make sure you're signed in (not
just on the landing page) and that you've clicked the cookie scope for
`https://hydra-voting.intersectmbo.org` specifically, not just "Cookies".

**Token is short / only one segment when pasted** — DevTools sometimes truncates
when you single-click instead of double-click the cell. Double-click to expand,
then `Cmd+A` to select the whole value.

## What's in the repo for this flow

- [`scripts/probe-auth.sh`](../scripts/probe-auth.sh) — non-destructive auth
  probe. Reads `EKKLESIA_JWT` from env or accepts JWT as `$1`. Defaults to the
  Intersect instance; override with `EKKLESIA_BASE`.
- [`docs/getting-started-with-claude.md`](getting-started-with-claude.md) —
  this file.
