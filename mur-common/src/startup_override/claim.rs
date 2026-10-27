//! Taking the live override file. The runtime (consume) and the CLI / Hub
//! (withdraw) both take it with one `rename` to their own name. A rename of
//! one source succeeds once, so exactly one side holds the file; the loser
//! sees `ENOENT`. Nobody reads or deletes the live name in place.

use std::io;
use std::path::{Path, PathBuf};

use crate::agent_name::validate_agent_name;
use crate::hitl::approval_token::SECRETS_DIR;

/// Under [`SECRETS_DIR`], which a sealed agent can neither read nor write.
pub const OVERRIDES_SUBDIR: &str = "startup-overrides";

const LIVE_SUFFIX: &str = ".json";
const CONSUMED_SUFFIX: &str = ".consumed.json";
const WITHDRAWN_SUFFIX: &str = ".withdrawn.json";

/// `<mur_home>/secrets/startup-overrides`
pub fn overrides_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(SECRETS_DIR).join(OVERRIDES_SUBDIR)
}

/// `<agent>.json` — published by a writer, waiting for a boot.
pub fn live_path(dir: &Path, agent: &str) -> PathBuf {
    dir.join(format!("{agent}{LIVE_SUFFIX}"))
}

/// `<agent>.consumed.json` — claimed by a boot.
pub fn consumed_path(dir: &Path, agent: &str) -> PathBuf {
    dir.join(format!("{agent}{CONSUMED_SUFFIX}"))
}

/// `<agent>.withdrawn.json` — taken back by the CLI or Hub.
pub fn withdrawn_path(dir: &Path, agent: &str) -> PathBuf {
    dir.join(format!("{agent}{WITHDRAWN_SUFFIX}"))
}

/// Which side is claiming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAs {
    /// The booting runtime.
    Consume,
    /// The CLI or Hub, sweeping an orphan or giving up on an acknowledgement.
    Withdraw,
}

#[derive(Debug)]
pub enum Claimed {
    /// The caller now holds the file at this path.
    Took(PathBuf),
    /// No live file: never published, or the other side took it first.
    Absent,
    /// The rename failed for another reason. Fail closed: treat as not taken,
    /// and never as `Absent`.
    Failed(io::Error),
}

/// One `rename` from the live name to the caller's name, nothing else.
pub fn claim(dir: &Path, agent: &str, who: ClaimAs) -> Claimed {
    if let Err(e) = validate_agent_name(agent) {
        return Claimed::Failed(io::Error::new(io::ErrorKind::InvalidInput, e));
    }
    let dest = match who {
        ClaimAs::Consume => consumed_path(dir, agent),
        ClaimAs::Withdraw => withdrawn_path(dir, agent),
    };
    match std::fs::rename(live_path(dir, agent), &dest) {
        Ok(()) => Claimed::Took(dest),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Claimed::Absent,
        Err(e) => Claimed::Failed(e),
    }
}
