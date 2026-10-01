//! giverny-claude: Claude Code integration.
//!
//! Watches Claude Code's local state (`sessions/<pid>.json` registry,
//! `.claude.json` usage cache), installs/relays hooks, discovers account
//! profiles (`CLAUDE_CONFIG_DIR` dirs), and reads transcripts for session
//! titles. Never reads credentials, never calls the network.

pub mod feed;
pub mod hooks;
pub mod jobs;
pub mod pass;
pub mod pass_history;
pub mod pass_nudge;
pub mod plugin;
pub mod profiles;
pub mod registry;
pub mod subagents;
pub mod tokens;
pub mod transcript;
pub mod usage;
pub mod worker_log;
pub mod wsl;
