//! **exegesis** — Ekklesia voting platform CLI for Cardano governance.
//!
//! Provides typed API access, local credential storage, proposal submission
//! from markdown files, and bulk proposal fetching with markdown rendering.

pub mod api;
pub mod auth;
pub mod client;
pub mod error;
pub mod proposal;
pub mod render;
pub mod store;
