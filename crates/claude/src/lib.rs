//! giverny-claude: Claude Code integration.
//!
//! Watches Claude Code's local state (`sessions/<pid>.json` registry,
//! `.claude.json` usage cache), installs/relays hooks, discovers account
//! profiles (`CLAUDE_CONFIG_DIR` dirs), and reads transcripts for session
//! titles. Never reads credentials, never calls the network.

pub mod agent_eta;
pub mod attach;
pub mod continuation;
pub mod feed;
pub mod hooks;
pub mod jobs;
pub mod lineage;
pub mod manage;
pub mod manage_history;
pub mod manage_inbox;
pub mod manage_nudge;
pub mod manage_run;
pub mod plugin;
pub mod plugin_hook;
pub mod profiles;
pub mod registry;
pub mod resources;
pub mod run_live;
pub mod session_use;
pub mod subagents;
pub mod tokens;
pub mod transcript;
pub mod usage;
pub mod use_reading;
pub mod worker_log;
pub mod worker_pids;
pub mod wsl;
