//! Loads everything a human-owned channel creation needs (B3, D-2).
//!
//! `ensure_enrolled()?` first: a call site that needs the human's key fails
//! loudly rather than falling back to an unowned channel. Inside an agent's
//! sandbox `keys/.human/` is unreadable, so [`HumanSigner::load`] errors with
//! the "run it in your own terminal" hint instead of minting a legacy channel.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_channel::service::{ChannelOwner, RouterSeal};
use mur_common::identity::AgentIdentity;

use crate::channel_writer::{ROUTER_AGENT, read_key_version};
use crate::identity_enroll::{HumanSigner, ensure_enrolled};

/// The human signer and the router key that seals the head, loaded once.
pub struct HumanOwner {
    signer: HumanSigner,
    router: AgentIdentity,
    router_kv: u32,
    router_home: PathBuf,
}

impl HumanOwner {
    pub fn load(mur_home: &Path) -> Result<Self> {
        ensure_enrolled(mur_home).context("a human-owned channel needs the human key")?;
        let signer = HumanSigner::load(mur_home)?;
        let router_home = mur_home.join("agents").join(ROUTER_AGENT);
        let router = AgentIdentity::load(&router_home).with_context(|| {
            format!(
                "a human-owned channel's head is sealed by the router key under {}",
                router_home.display()
            )
        })?;
        Ok(Self {
            signer,
            router,
            router_kv: read_key_version(&router_home),
            router_home,
        })
    }

    pub fn owner(&self) -> ChannelOwner<'_> {
        ChannelOwner::Human {
            signer: &self.signer,
            router: RouterSeal {
                identity: &self.router,
                kv: self.router_kv,
                key_home: &self.router_home,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_writer::plant_writer_identity;
    use tempfile::TempDir;

    /// D-2: no human key means no channel, never an unowned fallback.
    #[test]
    fn mur_core_create_fails_loudly_without_key() {
        let home = TempDir::new().unwrap();
        plant_writer_identity(home.path());
        // Enrollment mints a missing key, so make it unrecoverable instead:
        // a public half with no private half.
        crate::identity_enroll::enroll(home.path()).unwrap();
        std::fs::remove_file(mur_common::identity::human::human_private_key_path(
            home.path(),
        ))
        .unwrap();
        let err = HumanOwner::load(home.path()).err().expect("must refuse");
        assert!(
            format!("{err:#}").contains("private key"),
            "names the missing key: {err:#}"
        );
        let svc = mur_channel::ChannelService::open(home.path()).unwrap();
        assert!(svc.list(10).unwrap().is_empty(), "no channel was created");
    }

    #[cfg(unix)]
    #[test]
    fn sandboxed_mur_human_create_errors_with_terminal_hint() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let home = TempDir::new().unwrap();
        plant_writer_identity(home.path());
        crate::identity_enroll::enroll(home.path()).unwrap();
        let keys = mur_common::identity::human::human_key_dir(home.path());
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = HumanOwner::load(home.path());
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        let err = format!("{:#}", got.err().expect("must refuse"));
        assert!(
            err.contains("refusing to enroll") || err.contains("your own terminal"),
            "{err}"
        );
    }

    #[test]
    fn owned_channel_is_created_end_to_end() {
        let home = TempDir::new().unwrap();
        plant_writer_identity(home.path());
        let owner = HumanOwner::load(home.path()).unwrap();
        let svc = mur_channel::ChannelService::open(home.path()).unwrap();
        let ch = svc.create_for_agent("qa", owner.owner()).unwrap();
        let evs = svc.load_events(&ch.id).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(
            evs[0].kind,
            mur_common::channel::EventKind::OwnerKeyEnrolled
        );
    }
}
