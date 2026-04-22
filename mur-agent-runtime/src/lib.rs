//! murmur agent runtime — BusyBox-style multi-call binary.
//!
//! Each symlink named `mur_agent_<name>` dispatches to a profile at
//! `~/.mur/agents/<name>/` and runs an A2A v0.3 agent.

#![allow(dead_code)]

pub mod multi_call;
pub mod profile;
pub mod entitlements;
pub mod lock_file;
pub mod socket_path;
pub mod subcommand;
pub mod supervisor;
pub mod telemetry_writer;
pub mod communication_policy;
pub mod retry;
pub mod llm;
pub mod task_runner;
pub mod protocol;
pub mod transport;
pub mod export;
pub mod import;
