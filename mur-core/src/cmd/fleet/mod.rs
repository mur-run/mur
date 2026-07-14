//! `mur fleet` command module.

pub mod cherry_cmd;
pub mod compare;
pub mod concurrent_cmd;
pub mod control;
pub mod create;
pub mod delete;
pub mod export;
pub mod import;
pub mod jobs;
pub mod judge_cmd;
pub mod list;
pub mod loop_run;
pub mod partition_cmd;
pub mod plan;
// TODO(T2-T4): remove once loop_run/panel wire this in — pure data + persistence
// module with no consumer yet in this task.
#[allow(dead_code)]
pub mod progress;
pub mod roster;
pub mod run;
pub mod settings;
pub mod show;
pub mod store;
