//! Where the human owner's identity lives (at-mention phase 2, D-1, D-8).
//!
//! Split in two on purpose, like an agent's:
//!
//! - `<mur_home>/identity/` holds the PUBLIC half (`identity.pub`,
//!   `rotations.jsonl`). Every agent may read it — verifying a human-signed
//!   event needs it — and no agent may write it: the rotation chain there
//!   decides which key speaks for the human.
//! - `<mur_home>/keys/.human/` holds the private key. It sits under `keys/`,
//!   which the sandbox already denies for read and write as a subtree, and
//!   `.human` can never collide with an agent because `.` is not a valid
//!   agent-name character (`agent_name::validate_agent_name`).

use std::path::{Path, PathBuf};

/// Directory under `<mur_home>` holding the human's public key material.
pub const HUMAN_IDENTITY_DIR: &str = "identity";

/// Directory under `<mur_home>/keys` holding the human's private key.
pub const HUMAN_KEY_DIR: &str = ".human";

/// Directory under `<mur_home>` that holds every private signing key.
pub const KEYS_DIR: &str = "keys";

/// `<mur_home>/identity` — public, agent-readable, never agent-writable.
pub fn human_identity_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(HUMAN_IDENTITY_DIR)
}

/// `<mur_home>/keys/.human` — private, denied to every agent.
pub fn human_key_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(KEYS_DIR).join(HUMAN_KEY_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_dir_name_is_not_a_valid_agent_name() {
        assert!(crate::agent_name::validate_agent_name(HUMAN_KEY_DIR).is_err());
    }

    #[test]
    fn the_two_halves_never_share_a_directory() {
        let home = Path::new("/m");
        let public = human_identity_dir(home);
        let private = human_key_dir(home);
        assert!(!private.starts_with(&public) && !public.starts_with(&private));
    }
}
