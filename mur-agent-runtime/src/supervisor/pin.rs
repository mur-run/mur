//! Refuse to start on entitlements that differ from their pin (#712).
//!
//! Runs after `Profile::load`, before anything is sealed. See
//! `mur_common::entitlements_pin` for why the pin exists and what it covers.

use std::path::Path;

use mur_common::entitlements_pin::{self, PinCheck};
use tracing::{debug, warn};

use crate::profile::Profile;

/// Outcome the caller acts on. `Refuse` carries the message to print.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Start,
    Refuse(String),
}

pub(super) fn verify(profile: &Profile, agent_home: &Path, mur_home: &Path) -> Verdict {
    let name = profile.inner.name.as_str();
    // Only an installed agent has a pin: embedded / exported binaries run from
    // a cache dir outside `<mur_home>/agents`, with no trusted writer to pin.
    if agent_home != mur_home.join("agents").join(name) {
        debug!(home = %agent_home.display(), "agent not under mur_home; entitlement pin not applied");
        return Verdict::Start;
    }
    // Compare what is on disk (unexpanded), the same form writers pin.
    let path = agent_home.join("profile.yaml");
    let on_disk = match entitlements_pin::entitlements_from_yaml(&profile.raw_yaml, &path) {
        Ok(e) => e,
        Err(e) => return Verdict::Refuse(format!("error[profile_invalid]: {e}")),
    };
    match entitlements_pin::check(mur_home, name, &on_disk) {
        Ok(PinCheck::Match) => Verdict::Start,
        Ok(PinCheck::Missing) => {
            // Trust on first use: installs from before #712 have no pin.
            match entitlements_pin::write_pin(mur_home, name, &on_disk) {
                Ok(()) => warn!(
                    agent = name,
                    "no entitlement pin; pinned the current entitlements (first start since #712)"
                ),
                Err(e) => warn!(agent = name, error = %e, "could not write entitlement pin"),
            }
            Verdict::Start
        }
        Ok(PinCheck::Mismatch { changed }) => Verdict::Refuse(format!(
            "error[entitlements_unpinned]: {name}'s profile.yaml entitlements changed \
             outside MUR ({}). If you made this change, accept it with \
             `mur agent perm reseal {name}`; otherwise the agent may have edited \
             its own profile — inspect it before resealing.",
            changed.join(", ")
        )),
        Err(e) => Verdict::Refuse(format!(
            "error[entitlements_pin]: {e}. Run `mur agent perm reseal {name}` to re-pin."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(yaml_edit: impl Fn(String) -> String) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agents").join("bridge_test");
        std::fs::create_dir_all(&home).unwrap();
        let yaml = yaml_edit(
            include_str!("../../../mur-common/tests/fixtures/minimal_profile.yaml").to_string(),
        );
        std::fs::write(home.join("profile.yaml"), yaml).unwrap();
        (tmp, home)
    }

    fn load(home: &Path) -> Profile {
        Profile::load(home).unwrap()
    }

    #[test]
    fn first_start_pins_then_starts() {
        let (tmp, home) = setup(|y| y);
        assert_eq!(verify(&load(&home), &home, tmp.path()), Verdict::Start);
        assert!(entitlements_pin::pin_path(tmp.path(), "bridge_test").exists());
        assert_eq!(verify(&load(&home), &home, tmp.path()), Verdict::Start);
    }

    #[test]
    fn self_widened_profile_is_refused() {
        let (tmp, home) = setup(|y| y);
        assert_eq!(verify(&load(&home), &home, tmp.path()), Verdict::Start);
        // What the agent's bash tool can do on Linux: rewrite its own profile.
        let p = home.join("profile.yaml");
        let y = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, y.replace("write: []", "write: [\"/\"]")).unwrap();
        let Verdict::Refuse(msg) = verify(&load(&home), &home, tmp.path()) else {
            panic!("widened entitlements must not start");
        };
        assert!(msg.contains("filesystem") && msg.contains("perm reseal bridge_test"));
    }

    #[test]
    fn non_entitlement_edit_still_starts() {
        let (tmp, home) = setup(|y| y);
        assert_eq!(verify(&load(&home), &home, tmp.path()), Verdict::Start);
        let p = home.join("profile.yaml");
        let y = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, y.replace("name: \"m\"", "name: \"other\"")).unwrap();
        assert_eq!(verify(&load(&home), &home, tmp.path()), Verdict::Start);
    }

    #[test]
    fn agent_outside_mur_home_is_not_pinned() {
        let (tmp, home) = setup(|y| y);
        let other = tempfile::tempdir().unwrap();
        assert_eq!(verify(&load(&home), &home, other.path()), Verdict::Start);
        assert!(!entitlements_pin::pin_path(tmp.path(), "bridge_test").exists());
    }
}
