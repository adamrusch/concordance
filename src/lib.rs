//! **concordance** — LLM-mediated client for the Ekklesia governance API.
//!
//! Soft fork of [exegesis](https://github.com/nixedge/exegesis); same Rust
//! core (typed API access, local credential storage, proposal submission
//! from markdown files, bulk fetch with markdown rendering), but the binary
//! is shaped for agent use: deterministic output, idempotent operations,
//! and an MCP server (`concordance mcp`). The CLI surface remains
//! compatible with the upstream subcommands.

pub mod api;
pub mod auth;
pub mod client;
pub mod error;
pub mod identity;
pub mod mcp;
pub mod proposal;
pub mod render;
pub mod store;

/// First-boot banner shown above the CLI's `--help` output and printed to
/// stderr when the MCP server starts. Stdout in MCP mode is reserved for
/// the JSON-RPC stream, so the banner must go to stderr there.
pub const BANNER: &str = r"
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

   LLM-mediated feedback for Cardano governance
   https://github.com/adamrusch/concordance
";
