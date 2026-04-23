//! Crate-level path helpers.
//!
//! Use `mur_root` when you need the `.mur` data directory from code outside
//! `conversations/`. Respects `MUR_HOME` as an authoritative override — on
//! Windows, `dirs::home_dir()` calls `SHGetKnownFolderPath` and ignores
//! `HOME`/`USERPROFILE` env overrides, so tests that redirect via env vars
//! need this escape hatch.
//!
//! Semantics are identical to `conversations::paths::mur_root`; Phase 2 of
//! the Windows CI hardening effort will sweep the ~35
//! `dirs::home_dir().join(".mur")` direct callers in the crate to use this
//! one-line helper.

use std::path::PathBuf;

pub fn mur_root(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("MUR_HOME")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("no home dir").join(".mur")
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
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "/tmp/via-env") };
        assert_eq!(mur_root(None), PathBuf::from("/tmp/via-env"));
        unsafe { std::env::remove_var("MUR_HOME") };
    }

    #[test]
    fn mur_root_falls_back_to_home_dir_when_env_empty() {
        let _g = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_HOME", "") };
        let p = mur_root(None);
        assert!(p.ends_with(".mur"), "expected .../.mur, got: {p:?}");
        unsafe { std::env::remove_var("MUR_HOME") };
    }
}
