//! The agent's dedicated key as setup sees it: whether `plan` offers one, and
//! the one seam onto `mur_ssh_request::keystore`, which is unix-only. Off unix
//! there is no kernel seal, so a key is never offered and every call here
//! says so.

use std::path::{Path, PathBuf};

use mur_agent_runtime::sandbox::policy::ExecBackend;
use mur_common::agent::Entitlements;

use super::plan::{Dedicated, DedicatedOff, sealed_every_run};

pub use sys::*;

/// Whether `plan` offers the dedicated key (K3, K4). `generate_key` is the
/// `ssh_setup.generate_key` switch, read by the caller so tests pass it.
///
/// The sweep of an interrupted generation runs only when a key is offered:
/// with the switch off or the seal not enforced, nothing under `secrets/` is
/// touched.
pub fn dedicated_for(
    mur_home: &Path,
    agent: &str,
    ent: &Entitlements,
    generate_key: bool,
    backend: Option<ExecBackend>,
) -> Dedicated {
    if !generate_key {
        return Dedicated::Off(DedicatedOff::SwitchOff);
    }
    if !sealed_every_run(ent, backend) {
        return Dedicated::Off(DedicatedOff::NotSealedEveryRun);
    }
    sweep(mur_home, agent);
    match stored(mur_home, agent) {
        Ok(Some(k)) => Dedicated::Stored {
            fingerprint: k.fingerprint,
        },
        Ok(None) => Dedicated::Offer,
        Err(e) => Dedicated::Off(DedicatedOff::Unusable(e)),
    }
}

/// The comment field of a `<type> <base64> [comment]` line.
pub fn comment_of(public_line: &str) -> String {
    public_line
        .splitn(3, ' ')
        .nth(2)
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(unix)]
mod sys {
    use super::*;
    use mur_ssh_request::keystore::{self, Removal};

    pub use mur_ssh_request::keystore::StoredKey;

    pub fn stored(mur_home: &Path, agent: &str) -> Result<Option<StoredKey>, String> {
        keystore::stored(mur_home, agent).map_err(|e| e.to_string())
    }

    pub fn generate(mur_home: &Path, agent: &str) -> Result<StoredKey, String> {
        keystore::generate(mur_home, agent).map_err(|e| e.to_string())
    }

    /// Add the key to the user's ssh-agent.
    pub fn load(key: &StoredKey) -> Result<(), String> {
        keystore::load(key).map_err(|e| e.to_string())
    }

    pub fn sweep(mur_home: &Path, agent: &str) {
        keystore::sweep_partial(mur_home, agent);
    }

    /// The agent's key directory under `secrets/`, which no sealed agent can
    /// read or write.
    pub fn key_dir(mur_home: &Path, agent: &str) -> Option<PathBuf> {
        keystore::key_dir(mur_home, agent).ok()
    }

    /// One public key line of the type this store generates.
    pub fn is_key_line(line: &str) -> bool {
        keystore::parse_pub(line).is_some()
    }

    /// Drop the key from ssh-agent and delete it. `Ok(Some(_))` is a note for
    /// the user; `Err` names the directory to delete by hand.
    pub fn remove(mur_home: &Path, agent: &str) -> Result<Option<String>, String> {
        match keystore::unload_and_delete(mur_home, agent) {
            Ok(Removal::Absent) => Ok(None),
            Ok(Removal::Removed { unloaded: true }) => {
                Ok(Some("Removed the agent's dedicated ssh key".into()))
            }
            Ok(Removal::Removed { unloaded: false }) => Ok(Some(
                "Removed the agent's dedicated ssh key file. ssh-agent could not be told to \
                 drop it; if it is still listed by `ssh-add -l`, it stays loaded until you log \
                 out or run `ssh-add -D`."
                    .into(),
            )),
            Err(e) => Err(by_hand(mur_home, agent, &e.to_string())),
        }
    }

    /// Move the key with a renamed agent.
    pub fn rename(mur_home: &Path, old: &str, new: &str) -> Result<bool, String> {
        keystore::rename(mur_home, old, new).map_err(|e| {
            format!(
                "{e}; '{new}' has no dedicated ssh key until {} is moved to its name",
                shown_dir(mur_home, old)
            )
        })
    }

    fn by_hand(mur_home: &Path, agent: &str, why: &str) -> String {
        format!(
            "could not remove the dedicated ssh key ({why}); delete {} by hand",
            shown_dir(mur_home, agent)
        )
    }

    fn shown_dir(mur_home: &Path, agent: &str) -> String {
        match key_dir(mur_home, agent) {
            Some(d) => d.display().to_string(),
            None => "the agent's directory under secrets/ssh".into(),
        }
    }
}

#[cfg(not(unix))]
mod sys {
    use super::*;

    const UNIX_ONLY: &str = "a dedicated ssh key needs a sandboxed agent, which this platform \
                             does not have";

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StoredKey {
        pub private: PathBuf,
        pub public_line: String,
        pub fingerprint: String,
    }

    pub fn stored(_: &Path, _: &str) -> Result<Option<StoredKey>, String> {
        Ok(None)
    }
    pub fn generate(_: &Path, _: &str) -> Result<StoredKey, String> {
        Err(UNIX_ONLY.into())
    }
    pub fn load(_: &StoredKey) -> Result<(), String> {
        Err(UNIX_ONLY.into())
    }
    pub fn sweep(_: &Path, _: &str) {}
    pub fn key_dir(_: &Path, _: &str) -> Option<PathBuf> {
        None
    }
    pub fn is_key_line(_: &str) -> bool {
        false
    }
    pub fn remove(_: &Path, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    pub fn rename(_: &Path, _: &str, _: &str) -> Result<bool, String> {
        Ok(false)
    }
}
