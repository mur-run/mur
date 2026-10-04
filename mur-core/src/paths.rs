//! Crate-level path helpers.
//!
//! `mur_root` is the `.mur` data directory with an optional explicit
//! override; without one it delegates to `mur_common::home`, the single
//! resolver every MUR surface shares (#1696).

use std::path::PathBuf;

pub fn mur_root(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    mur_common::home::mur_home()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mur_root_uses_explicit_override_first() {
        let p = mur_root(Some("/tmp/fake-mur"));
        assert_eq!(p, PathBuf::from("/tmp/fake-mur"));
    }

    #[test]
    fn mur_root_honors_mur_home_env_when_no_override() {
        // Serialize via the crate-local env-mutex so this test does not race
        // against `conversations` tests that also mutate MUR_HOME.
        let mut envg = mur_common::test_env::EnvGuard::hold();
        envg.set_var("MUR_HOME", "/tmp/via-env");
        assert_eq!(mur_root(None), PathBuf::from("/tmp/via-env"));
        envg.unset_var("MUR_HOME");
    }

    #[test]
    fn mur_root_falls_back_to_home_dir_when_env_empty() {
        let mut envg = mur_common::test_env::EnvGuard::hold();
        envg.set_var("MUR_HOME", "");
        let p = mur_root(None);
        assert!(p.ends_with(".mur"), "expected .../.mur, got: {p:?}");
        envg.unset_var("MUR_HOME");
    }
}
