//! GUI-free core logic for Anveg Code.
//!
//! Nothing in this crate depends on GPUI. Everything here is unit-tested.

pub mod atomic;
pub mod chime;
pub mod claude_env;
pub mod config;
pub mod files;
pub mod fswatch;
pub mod gh;
pub mod git;
pub mod git_activity;
pub mod github_api;
pub mod github_auth;
pub mod github_publish;
pub mod launcher;
pub mod notes;
pub mod paths;
pub mod procinfo;
pub mod process;
pub mod session_index;
pub mod side_diff;
pub mod status;
pub mod time;
pub mod transcripts;
pub mod watcher;
