//! murmur agent runtime — BusyBox-style multi-call binary.
//!
//! Each symlink named `mur_agent_<name>` dispatches to a profile at
//! `~/.mur/agents/<name>/` and runs an A2A v0.3 agent.

#![allow(dead_code)]

pub mod bridge;
pub mod communication_policy;
pub mod companion;
/// B0 M10 — redacted crashlog writer (panic hook + secret/home-path
/// redaction). Closes the gap acknowledged in
/// `docs/release/privacy-statement.md` §4.
pub mod crashlog;
pub mod durable;
pub mod entitlements;
mod exec_dirs;
pub mod export;
pub mod expression;
pub mod federation;
pub mod hitl;
pub mod hooks;
pub mod idle_scheduler;
pub mod import;
pub mod llm;
pub mod lock_file;
pub mod mcp;
pub mod mcp_repin;
pub mod multi_call;
pub mod multimodal;
pub mod oauth;
pub mod profile;
pub mod protocol;
pub mod retry;
pub mod sandbox;
pub mod scheduler;
pub mod skills;
pub mod socket_path;
pub mod subcommand;
pub mod supervisor;
pub mod supervisor_runner;
pub mod task_runner;
pub mod telemetry_writer;
pub mod tools;
pub mod transport;
pub mod turn_ledger;
pub mod voice;
pub mod watch_scheduler;
