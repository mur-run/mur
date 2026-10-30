//! Agent lifecycle commands: create / list / status / stop / remove / rename.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::agent::*;
use mur_common::identity::AgentIdentity;
use mur_common::{AgentProfile as _AgentProfile, LockFile};

use super::{
    pid_alive, refuse_if_running, resolve_bin_dir, resolve_mur_home, resolve_runtime_target,
    write_atomic,
};

mod create;
mod list_status;
mod manage;

pub use create::*;
pub use list_status::*;
pub use manage::*;

#[cfg(test)]
use create::{near_registry_key, resolve_model_ref_for_create};
#[cfg(test)]
use list_status::describe_unloadable;
#[cfg(test)]
use manage::{count_unread_companion_inbox, purge_agent_state};

mod status_override;
mod status_refusal;
mod status_ssh_proxy;

#[cfg(test)]
mod tests_resolve_model_ref_for_create;

#[cfg(test)]
mod tests_near_registry_key;

#[cfg(test)]
mod tests_remove_unread_guard;

#[cfg(test)]
mod tests_purge_removes_private_key;

#[cfg(test)]
mod tests_unloadable_agent_dirs;
