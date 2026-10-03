//! Which MCP server entries run a binary MUR shipped itself.
//!
//! One definition, two consumers: the agent runtime re-pins these on every
//! start (`mur-agent-runtime/src/mcp_repin.rs`), and `mur doctor` has to know
//! the same thing to avoid reporting a routine upgrade as a refuse-to-start.
//! It lives here because `mur-agent-runtime` must not depend on `mur-core`.
//!
//! The trust anchor and why both halves of the test are load-bearing are
//! documented in `mcp_repin.rs` and `docs/architecture/mcp-supply-chain.md`.

use std::path::{Path, PathBuf};

/// The name prefix MUR's own shipped binaries carry (`mur-mcp-server`,
/// `mur-research-gateway`, …). Half of the first-party test; matching on the
/// directory alone would exempt unrelated binaries in a crowded `~/.local/bin`.
pub const MUR_BINARY_PREFIX: &str = "mur-";

/// The first-party binary `command` resolves to, or `None` when the entry is
/// third-party and rule 6 must keep enforcing its install-time hash.
///
/// `bundled` is MUR's own copy of `mur-mcp-server` under `~/.mur/mcp-servers/`
/// when the caller treats it as current. `install_dir` is the canonical
/// directory MUR's binaries are installed in (the runtime's own directory).
pub fn first_party_target(
    command: &str,
    bundled: Option<&Path>,
    install_dir: &Path,
) -> Option<PathBuf> {
    if let Some(bundled) = bundled {
        let c = Path::new(command);
        if c == bundled || c.file_name() == bundled.file_name() {
            return Some(bundled.to_path_buf());
        }
    }

    // Resolve exactly as the spawn does, so the file judged is the file that
    // will run. Resolution canonicalizes, so a symlinked install (Homebrew's
    // `bin` into its Cellar) compares equal to the canonical install dir.
    let prog = command.split_whitespace().next()?;
    if !Path::new(prog)
        .file_name()?
        .to_str()?
        .starts_with(MUR_BINARY_PREFIX)
    {
        return None;
    }
    let resolved =
        crate::exec::resolve_command_in(&crate::exec::augmented_path_var(), prog).ok()?;
    (resolved.parent()? == install_dir).then_some(resolved)
}

/// The canonical directory holding the running executable — the install dir
/// [`first_party_target`] compares against. `None` when it cannot be resolved.
pub fn current_install_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .canonicalize()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        (dir, canon)
    }

    #[test]
    fn a_mur_binary_in_the_install_dir_is_first_party() {
        let (_d, dir) = install_dir();
        let gw = dir.join("mur-research-gateway");
        std::fs::write(&gw, b"gw").unwrap();
        assert_eq!(
            first_party_target(gw.to_str().unwrap(), None, &dir),
            Some(gw)
        );
    }

    #[test]
    fn a_third_party_binary_in_the_install_dir_is_not() {
        let (_d, dir) = install_dir();
        let other = dir.join("some-mcp");
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(
            first_party_target(other.to_str().unwrap(), None, &dir),
            None
        );
    }

    #[test]
    fn a_mur_named_binary_elsewhere_is_not() {
        let (_d, dir) = install_dir();
        let (_e, elsewhere) = install_dir();
        let gw = elsewhere.join("mur-research-gateway");
        std::fs::write(&gw, b"gw").unwrap();
        assert_eq!(first_party_target(gw.to_str().unwrap(), None, &dir), None);
    }

    #[test]
    fn the_bundled_server_matches_by_name() {
        let (_d, dir) = install_dir();
        let bundled = PathBuf::from("/x/.mur/mcp-servers/mur-mcp-server");
        assert_eq!(
            first_party_target("mur-mcp-server", Some(&bundled), &dir),
            Some(bundled)
        );
    }
}
