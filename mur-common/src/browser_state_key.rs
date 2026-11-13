//! Handing the browser profile state key to a sealed live-browser MCP child
//! (#2369).
//!
//! `mur browser record --mode live --profile <site>` decrypts the saved
//! profile at startup and re-encrypts the rotated session at exit, both with
//! the age identity kept in the OS Keychain. Spawned by an agent, that child
//! inherits the agent's sandbox, where the Keychain is unreachable
//! (`UNIX[Operation not permitted]`). The runtime therefore reads the key
//! before it seals — the same reason provider secrets are cached there — and
//! passes it to the matching child through an inherited pipe whose fd number
//! is named by [`FD_ENV`]. A pipe, not an env var: the value then never sits
//! in the child's environment block, which other same-user processes can read.
//!
//! One key for all profiles, by design: the identity at
//! [`KEYCHAIN_ACCOUNT`] encrypts every saved profile, so a trusted live
//! browser that is handed it can decrypt any of them, not only the one it was
//! started for. The trust boundary is who receives the key (only the `mur`
//! CLI in MUR's install dir), not which profile it opens.
//!
//! The Keychain coordinates and the env name are duplicated in `mur-browser`
//! (which does not depend on this crate); a `mur-core` test pins them equal.

use crate::mcp_first_party::MUR_CLI_NAMES;
use crate::secret::SecretRef;
use std::path::Path;

/// Keychain service holding the key. Same service as every agent secret.
pub const KEYCHAIN_SERVICE: &str = "mur-agent";
/// Keychain account holding the age identity for all browser profiles.
pub const KEYCHAIN_ACCOUNT: &str = "browser/state-key";
/// Env var naming the inherited read end of the pipe carrying the key.
pub const FD_ENV: &str = "MUR_BROWSER_STATE_KEY_FD";
/// Name used for the "a Keychain prompt is still pending" marker
/// (`secret::keychain_pending`), so a crash loop asks only once.
pub const PENDING_NAME: &str = "BROWSER_STATE_KEY";

/// The Keychain reference the runtime resolves before sealing.
pub fn keychain_ref() -> SecretRef {
    SecretRef::Keychain {
        service: KEYCHAIN_SERVICE.to_string(),
        account: KEYCHAIN_ACCOUNT.to_string(),
    }
}

/// Whether an MCP entry with these args is an agent-spawned live browser
/// session (`browser record … --mode live`), with or without a profile.
///
/// This, not [`wants_state_key`], decides whether the runtime reads the key
/// before sealing: adding `--profile` to an existing live entry is a hot edit
/// (`mur-core` `live_effect`), so the next browser may need a key the agent
/// did not need when it started — and after the seal it can no longer read it.
///
/// `browser record` must be the first two args, exactly as
/// `mur agent browser live` writes them — not merely appear somewhere, where
/// any server's argv could carry it as a value. The argv alone is still not
/// enough to be handed the key: see [`is_installed_cli`].
pub fn is_live_browser(args: &[String]) -> bool {
    let is_record =
        args.first().is_some_and(|a| a == "browser") && args.get(1).is_some_and(|a| a == "record");
    let is_live = args.windows(2).any(|w| w[0] == "--mode" && w[1] == "live")
        || args.iter().any(|a| a == "--mode=live");
    is_record && is_live
}

/// Whether an MCP entry with these args is a live browser session bound to at
/// least one saved profile — the only kind handed the key at spawn. Matches
/// the argv `mur agent browser live` writes (`browser record … --mode live
/// --profile <site>`), in any order after `browser record`.
pub fn wants_state_key(args: &[String]) -> bool {
    is_live_browser(args)
        && args
            .iter()
            .any(|a| a == "--profile" || a.starts_with("--profile="))
}

/// Whether `program` — the file the spawn will exec, already resolved — is
/// the `mur` CLI installed in `install_dir`, the directory MUR's own binaries
/// live in. Only then is the key handed over (#2369 review): an argv shaped
/// like a live browser proves nothing about the binary that receives it.
/// Same trust anchor as the first-party re-pin (`mcp_first_party`): the exact
/// CLI name, judged on the canonical file, in the canonical install dir.
pub fn is_installed_cli(program: &Path, install_dir: &Path) -> bool {
    let Ok(file) = program.canonicalize() else {
        return false;
    };
    file.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| MUR_CLI_NAMES.contains(&n))
        && file.parent() == Some(install_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_a_live_record_with_a_profile_wants_the_key() {
        let live = v(&[
            "browser",
            "record",
            "--run",
            "live-a",
            "--mode",
            "live",
            "--profile",
            "gitea",
        ]);
        assert!(wants_state_key(&live));
        assert!(wants_state_key(&v(&[
            "browser",
            "record",
            "--mode=live",
            "--profile=gitea"
        ])));
        // Public-pages live session: nothing to decrypt at spawn — but still a
        // live browser, so the key is read before the seal in case a profile
        // is added later without a restart.
        assert!(!wants_state_key(&v(&[
            "browser", "record", "--mode", "live"
        ])));
        assert!(is_live_browser(&v(&[
            "browser", "record", "--mode", "live"
        ])));
        // Recording modes are not agent-spawned live sessions.
        assert!(!wants_state_key(&v(&[
            "browser",
            "record",
            "--mode",
            "test",
            "--profile",
            "gitea"
        ])));
        assert!(!is_live_browser(&v(&[
            "browser", "record", "--mode", "test"
        ])));
        // `browser record` buried in another server's argv is not a browser.
        assert!(!is_live_browser(&v(&[
            "serve", "--note", "browser", "record", "--mode", "live"
        ])));
        assert!(!wants_state_key(&v(&[
            "x",
            "browser",
            "record",
            "--mode",
            "live",
            "--profile",
            "g"
        ])));
        // Any other server that happens to take a --profile flag.
        assert!(!wants_state_key(&v(&[
            "serve",
            "--mode",
            "live",
            "--profile",
            "x"
        ])));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_mur_cli_in_the_install_dir_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().canonicalize().unwrap();
        let mur = install.join("mur");
        std::fs::write(&mur, "").unwrap();
        assert!(is_installed_cli(&mur, &install));
        // Right name, wrong directory.
        let other = tempfile::tempdir().unwrap();
        let stray = other.path().join("mur");
        std::fs::write(&stray, "").unwrap();
        assert!(!is_installed_cli(&stray, &install));
        // Right directory, a sibling that is not the CLI.
        let sib = install.join("mur-mcp-server");
        std::fs::write(&sib, "").unwrap();
        assert!(!is_installed_cli(&sib, &install));
        // A `mur` symlink to a stranger is judged by the file it names.
        let evil = install.join("evil");
        std::fs::write(&evil, "").unwrap();
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("mur");
        std::os::unix::fs::symlink(&evil, &link).unwrap();
        assert!(!is_installed_cli(&link, &install));
        // An unresolved bare name never passes.
        assert!(!is_installed_cli(Path::new("mur"), &install));
    }
}
