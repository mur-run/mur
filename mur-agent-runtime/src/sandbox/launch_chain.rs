//! MUR's own launch chain: the files that decide what starts next and with
//! what authority.
//!
//! A sandbox that can be edited from inside it is not a boundary, it is a
//! delay. The set guarded here is deliberately NOT "dangerous paths" — that
//! set is open-ended (`.zshenv`, autostart, git hooks, cron) and unwinnable.
//! It is the closed set MUR owns: what triggers a start, what gets exec'd,
//! what entitlements the started process carries, and what identity it signs
//! with. Every member is derivable from `mur_home`, `bin_dir` and `$HOME`.
//!
//! This is a predicate, not a path list, on purpose: a list built at seal
//! time cannot cover `<mur_home>/agents/<name>` for a name that did not exist
//! yet, and creating that directory is exactly the escape.

use std::path::{Path, PathBuf};

/// Written by MUR itself under `bin_dir`; exec'd before any sandbox applies.
const RUNTIME_BINARY: &str = "mur-agent-runtime";
/// BusyBox-style per-agent symlinks to `RUNTIME_BINARY`.
const AGENT_SYMLINK_PREFIX: &str = "mur_agent_";

#[derive(Clone, Debug)]
pub struct LaunchChain {
    mur_home: PathBuf,
    agent_home: PathBuf,
    bin_dir: PathBuf,
    autostart: Vec<PathBuf>,
}

impl LaunchChain {
    /// Derive the protected set for the agent rooted at `agent_home`.
    pub fn new(agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let bin_dir = std::env::var_os("MUR_AGENT_BIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        Self::build(agent_home, &bin_dir, &home)
    }

    /// Construct with explicit roots. Tests use this; `new` is the real path.
    pub fn for_test(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        Self::build(agent_home, bin_dir, home)
    }

    /// A chain rooted outside any real path, so it never fires.
    ///
    /// For tests that exercise something else and just need to construct a
    /// tool. Tests that exercise the chain build their own with `for_test`.
    #[cfg(test)]
    pub fn inert() -> Self {
        Self::default()
    }

    fn build(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        // `<mur_home>/agents/<name>` — the same derivation policy.rs uses for
        // the channels and open-items force-grants.
        let mur_home = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| agent_home.to_path_buf());
        Self {
            mur_home,
            agent_home: agent_home.to_path_buf(),
            bin_dir: bin_dir.to_path_buf(),
            autostart: autostart_dirs(home),
        }
    }

    pub fn agent_self_home(&self) -> &Path {
        &self.agent_home
    }

    /// Why `path` may never be written, or `None` if it is not in the set.
    ///
    /// Returns the reason rather than a bare `bool` because the tool gate is
    /// the only layer that can explain itself — the kernel returns an EPERM
    /// that reads identically to "not granted".
    pub fn protects_write(&self, path: &Path) -> Option<&'static str> {
        if self.is_other_agent(path) {
            return Some(
                "another agent's directory — its profile.yaml is that agent's \
                 entitlements and its identity.key is that agent's signing authority",
            );
        }
        if self.is_launch_artifact(path) {
            return Some(
                "MUR's runtime binary or a per-agent symlink — exec'd before \
                 the sandbox applies, so replacing it escapes every sandbox",
            );
        }
        if self.autostart.iter().any(|d| path.starts_with(d)) {
            return Some("an OS autostart directory — entries here run outside any sandbox");
        }
        None
    }

    /// Why `path` may never be read. Only sibling signing keys: whoever reads
    /// one can forge that agent's channel events with no write at all, and
    /// verify-on-fold accepts them because the key on disk is untouched.
    pub fn protects_read(&self, path: &Path) -> Option<&'static str> {
        if self.is_other_agent(path) && path.file_name().is_some_and(|n| n == "identity.key") {
            return Some(
                "another agent's signing key — reading it is enough to forge \
                 that agent's signed channel events",
            );
        }
        None
    }

    /// Concrete paths for backends that need a list rather than a predicate
    /// (SBPL). `<mur_home>/agents` is included as a whole; the caller is
    /// responsible for re-allowing `agent_self_home()` after it.
    pub fn deny_paths(&self) -> Vec<PathBuf> {
        let mut out = vec![self.mur_home.join("agents")];
        out.push(self.bin_dir.join(RUNTIME_BINARY));
        out.extend(self.existing_agent_symlinks());
        out.extend(self.autostart.iter().cloned());
        out
    }

    /// Under `<mur_home>/agents` but not under this agent's own home.
    fn is_other_agent(&self, path: &Path) -> bool {
        path.starts_with(self.mur_home.join("agents")) && !path.starts_with(&self.agent_home)
    }

    fn is_launch_artifact(&self, path: &Path) -> bool {
        if path.parent() != Some(self.bin_dir.as_path()) {
            return false;
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == RUNTIME_BINARY || n.starts_with(AGENT_SYMLINK_PREFIX))
    }

    /// Symlinks present now. One created later is not listed, which is
    /// acceptable: a new symlink only matters if something starts it, and
    /// that needs a profile (denied) or an autostart entry (denied) or a human.
    fn existing_agent_symlinks(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.bin_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(AGENT_SYMLINK_PREFIX))
            })
            .map(|e| e.path())
            .collect()
    }
}

impl Default for LaunchChain {
    /// Rooted outside any real path, so it never fires. Only reachable via
    /// `SandboxPolicy::default()` (tests and policy-less contexts); every
    /// real policy is built by `from_entitlements`, which constructs the
    /// actual chain from `agent_home`.
    fn default() -> Self {
        let root = Path::new("/nonexistent-launch-chain-root");
        Self::build(&root.join("agents/none"), &root.join("bin"), root)
    }
}

#[cfg(target_os = "macos")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/LaunchAgents"),
        home.join("Library/LaunchDaemons"),
    ]
}

#[cfg(target_os = "linux")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/systemd/user"),
        home.join(".config/autostart"),
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn autostart_dirs(_home: &Path) -> Vec<PathBuf> {
    Vec::new()
}

/// A grant root so broad that granting it is equivalent to no sandbox.
///
/// Unifies the judgement previously duplicated in `access.rs::is_overbroad_root`
/// (cwd consent) and `policy.rs::is_guarded_prefix` (spawn prefixes). Those two
/// disagreed: one knew about `/usr` and `/opt`, the other about depth. This is
/// the union.
pub fn is_overbroad_grant_root(path: &Path, home: &Path) -> bool {
    if path == Path::new("/")
        || path == home
        || path == Path::new("/usr")
        || path == Path::new("/opt")
        || path == Path::new("/opt/homebrew")
    {
        return true;
    }
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        return rest.components().count() <= 1;
    }
    path.components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count()
        < 2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<tmp>/agents/mur` as agent_home, so mur_home is `<tmp>`.
    fn chain(tmp: &Path) -> LaunchChain {
        LaunchChain::for_test(
            &tmp.join("agents").join("mur"),
            &tmp.join("bin"),
            &tmp.join("home"),
        )
    }

    #[test]
    fn sibling_agent_files_are_write_protected_but_own_are_left_to_self_protect() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let agents = tmp.path().join("agents");

        assert!(c.protects_write(&agents.join("pm/profile.yaml")).is_some());
        assert!(c.protects_write(&agents.join("pm/identity.key")).is_some());
        assert!(c.protects_write(&agents.join("pm/anything/else")).is_some());

        // Negative control: the agent's own home stays writable. Without this,
        // a predicate that returned Some() for everything would still pass.
        assert!(c.protects_write(&agents.join("mur/running.lock")).is_none());
        assert!(c.protects_write(&agents.join("mur/skills/x.yaml")).is_none());
    }

    #[test]
    fn protects_agents_created_after_the_policy_was_built() {
        // The regression a path list cannot catch: this directory does not
        // exist, and never existed when any list would have been built.
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let unborn = tmp.path().join("agents/not-created-yet/profile.yaml");
        assert!(!unborn.exists());
        assert!(c.protects_write(&unborn).is_some());
    }

    #[test]
    fn only_murs_own_launch_artifacts_in_bin_dir_are_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let bin = tmp.path().join("bin");

        assert!(c.protects_write(&bin.join("mur-agent-runtime")).is_some());
        assert!(c.protects_write(&bin.join("mur_agent_pm")).is_some());

        // Negative control: an agent installing a tool for itself still works.
        assert!(c.protects_write(&bin.join("ripgrep")).is_none());
        assert!(c.protects_write(&bin.join("murmur-notes")).is_none());
    }

    #[test]
    fn sibling_identity_key_is_read_protected_and_nothing_else_is() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let agents = tmp.path().join("agents");

        assert!(c.protects_read(&agents.join("pm/identity.key")).is_some());

        // Negative controls: reads are otherwise untouched by this module.
        assert!(c.protects_read(&agents.join("pm/profile.yaml")).is_none());
        assert!(c.protects_read(&agents.join("mur/identity.key")).is_none());
        assert!(c.protects_read(&tmp.path().join("skills/x.yaml")).is_none());
    }

    #[test]
    fn overbroad_roots_are_rejected_and_normal_project_dirs_are_not() {
        let home = PathBuf::from("/Users/someone");
        assert!(is_overbroad_grant_root(Path::new("/"), &home));
        assert!(is_overbroad_grant_root(&home, &home));
        assert!(is_overbroad_grant_root(Path::new("/Users"), &home));
        assert!(is_overbroad_grant_root(Path::new("/usr"), &home));
        assert!(is_overbroad_grant_root(Path::new("/opt/homebrew"), &home));
        assert!(is_overbroad_grant_root(Path::new("/Volumes/Disk"), &home));

        // Negative controls: real grants people legitimately make.
        assert!(!is_overbroad_grant_root(&home.join("Projects/app"), &home));
        assert!(!is_overbroad_grant_root(
            Path::new("/Volumes/Disk/Projects/app"),
            &home
        ));
    }
}
