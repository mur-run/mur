//! Credential-store stamps: how MUR notices that an owning CLI refreshed a
//! token underneath it. Moved out of `login.rs` for CLAUDE.md §4's 800-line
//! rule. Pure movement: verbatim, with paths one level deeper.

use super::*;

/// An opaque marker for "the credential store as it stood at some moment".
/// Compared for equality only — never parsed, never displayed, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStamp(pub(super) String);

/// Arguments for the metadata read. Split out so a test can assert that `-w`
/// — the flag that would print the password itself — is never present.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(
        dead_code,
        reason = "the only non-test caller is the macOS-only `keychain_stamp`, \
                  which is not compiled here"
    )
)]
pub fn keychain_stamp_args() -> Vec<&'static str> {
    vec!["find-generic-password", "-s", CLAUDE_KEYCHAIN_SERVICE]
}

// These two helpers just join the per-provider suffix onto an already-resolved
// home, so `store_stamp_in` (below) can be driven with a temp-dir home in
// tests. `dirs::home_dir` is the resolver production hands in — the crate
// mur-core already depends on `dirs`, and this is the same resolution the
// runtime sandbox uses (see `cli/access.rs`). NOT `directories::BaseDirs`,
// which this crate does not depend on.
pub(super) fn claude_credentials_path(home: &Path) -> PathBuf {
    home.join(CLAUDE_CREDENTIALS_REL)
}

pub(super) fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(CODEX_AUTH_REL)
}

/// mtime of a credential file, as an opaque stamp.
pub(super) fn file_stamp(p: &Path) -> Option<StoreStamp> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(StoreStamp(format!("{}.{}", d.as_secs(), d.subsec_nanos())))
}

/// The keychain item's `mdat` line, verbatim. `security` prints it without
/// `-w`, so no secret is read.
#[cfg(target_os = "macos")]
pub(super) fn keychain_stamp() -> Option<StoreStamp> {
    let out = std::process::Command::new("security")
        .args(keychain_stamp_args())
        .output()
        .ok()?;
    // `security` writes the attribute dump to stderr.
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("\"mdat\""))
        .map(|l| StoreStamp(l.trim().to_string()))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn keychain_stamp() -> Option<StoreStamp> {
    None
}

/// The whole of [`store_stamp`]'s behaviour, with its two real-world inputs
/// — the home directory and the (macOS-only, machine-global) keychain probe —
/// injected **as thunks**, not as values.
///
/// Thunks, because laziness is part of the routing and therefore has to live
/// on this side of the seam:
///
/// * the Chatgpt arm must never run the keychain probe (that shells out to
///   `security`), and
/// * a keychain hit must not need the home directory resolved at all, so a
///   box where `home_dir()` fails still gets a stamp.
///
/// An earlier version composed those two rules in the production wrapper
/// instead, which left the wrapper passing `keychain: None` unconditionally:
/// the `keychain` arm here became dead in production, the test that pinned it
/// pinned a path production never took, and `store_stamp` itself — the thing
/// actually called — had no test at all. Everything that decides *which store
/// a provider reads* now lives in this one `match`, under test
/// (`store_stamp_reads_each_providers_own_file_not_the_others` reddens if the
/// arms are swapped; `the_seam_resolves_only_what_it_needs` reddens if either
/// thunk is forced eagerly). What is left in `store_stamp` is a two-argument
/// delegation whose arguments cannot be transposed — they have different
/// return types.
///
/// The keychain is forced here rather than shelled out to for real in tests:
/// the real keychain is machine-global state a test cannot control (and, on a
/// box that has ever logged into Claude Code, would make the Anthropic arm
/// return a real stamp regardless of `home`, masking exactly the bug this seam
/// exists to catch).
pub(super) fn store_stamp_in(
    p: Provider,
    home: impl FnOnce() -> Option<PathBuf>,
    keychain: impl FnOnce() -> Option<StoreStamp>,
) -> Option<StoreStamp> {
    match p {
        // macOS keeps it in the keychain; Linux/Windows installs write a file.
        Provider::Anthropic => {
            keychain().or_else(move || file_stamp(&claude_credentials_path(&home()?)))
        }
        Provider::Chatgpt => file_stamp(&codex_auth_path(&home()?)),
    }
}

/// Current stamp for a provider's credential store, or `None` when there is
/// no store to stamp.
///
/// **Known gap.** On Linux the credential can live in a keychain (the gateway
/// reads it through `keyring`'s linux-native backend), and murmur has no way
/// to stamp that without taking a dependency on the secret store itself. There
/// `store_stamp` returns `None`, so rung 2 can never report
/// `RefreshedByProbe`. That degrades correctly rather than lying —
/// `classify_repair` falls through to the owner CLI's own health report — but
/// it is a real limitation, not an oversight, and `no_stamp_degrades_to_the_cli_report`
/// pins the degradation.
pub fn store_stamp(p: Provider) -> Option<StoreStamp> {
    store_stamp_in(p, dirs::home_dir, keychain_stamp)
}
