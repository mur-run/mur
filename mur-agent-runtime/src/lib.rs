//! murmur agent runtime — BusyBox-style multi-call binary.
//!
//! Each symlink named `mur_agent_<name>` dispatches to a profile at
//! `~/.mur/agents/<name>/` and runs an A2A v0.3 agent.

#![allow(dead_code)]

pub mod bridge;
pub mod communication_policy;
pub mod companion;
pub mod durable;
pub mod entitlements;
pub mod export;
pub mod hooks;
pub mod import;
pub mod llm;
pub mod lock_file;
pub mod multi_call;
pub mod multimodal;
pub mod profile;
pub mod protocol;
pub mod retry;
pub mod socket_path;
pub mod subcommand;
pub mod supervisor;
pub mod task_runner;
pub mod telemetry_writer;
pub mod transport;
