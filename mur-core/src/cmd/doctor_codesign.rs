//! `codesign-identity` doctor check and its `--fix` (#1587).
//!
//! Distinct from the `apple-signing` check in [`crate::cmd::doctor`]: that one
//! is about **release signing** (`MUR_APPLE_DEVELOPER_ID` / notarization for
//! `mur agent export`). This one is about **local re-signing on `mur update`**.
//! When `update.codesign_identity` is unset, `mur update` falls back to ad-hoc
//! signing; an ad-hoc signature has no certificate, so macOS Keychain grants
//! bind to the binary's CDHash and die on every rebuild (#866).
//!
//! The fix creates a self-signed code-signing certificate and writes it to
//! `update.codesign_identity`. It deliberately does **not** run
//! `security add-trusted-cert`: that raises an admin authorization dialog, and
//! a measured test confirmed `codesign -s` + `codesign -v` both succeed on an
//! imported-but-untrusted identity. We need a *stable* identity, not a
//! Gatekeeper-*trusted* one, so the fix stays non-interactive apart from the
//! keychain password prompt `security import` may raise.

use anyhow::{Context as _, Result, bail};
use std::process::Command;

/// Common Name of the certificate the fix creates. Also the identity string
/// written to `update.codesign_identity`, because `codesign -s` accepts a CN
/// substring match and a CN is readable where a SHA-1 hash is not.
pub(crate) const SELF_SIGNED_CN: &str = "MUR Local Signing";

/// Validity of the generated certificate, in days. Long enough that no user
/// hits the expiry in practice; the check reports an expired certificate as a
/// failure, so a stale one is visible rather than silent.
const CERT_VALIDITY_DAYS: u32 = 3650;

/// What `mur doctor` should say about local re-signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodesignVerdict {
    /// Not macOS — `mur update` does not re-sign anything.
    Skipped,
    /// `update.codesign_identity` is set and the identity exists.
    Configured(String),
    /// Configured but absent from the keychain (or expired). Pointing at an
    /// identity that does not exist is worse than not configuring one: the
    /// re-sign fails mid-update instead of falling back cleanly.
    Dangling(String),
    /// Unset, but a usable identity exists — suggest it instead of minting a
    /// new certificate.
    UnsetWithCandidate(String),
    /// Unset and no code-signing identity at all — `--fix` can create one.
    UnsetNoIdentity,
}

impl CodesignVerdict {
    /// Non-ok verdicts are the ones `--fix` considers.
    pub(crate) fn needs_attention(&self) -> bool {
        !matches!(self, Self::Skipped | Self::Configured(_))
    }
}

/// Decide the verdict from the two inputs, with no I/O of its own so the
/// decision table is unit-testable.
///
/// `identities` is the list of code-signing identity names available in the
/// keychain, most-preferred first.
pub(crate) fn verdict(configured: Option<&str>, identities: &[String]) -> CodesignVerdict {
    if !cfg!(target_os = "macos") {
        return CodesignVerdict::Skipped;
    }
    match configured {
        Some(id) if identity_present(id, identities) => CodesignVerdict::Configured(id.to_string()),
        Some(id) => CodesignVerdict::Dangling(id.to_string()),
        None => match preferred_candidate(identities) {
            Some(c) => CodesignVerdict::UnsetWithCandidate(c),
            None => CodesignVerdict::UnsetNoIdentity,
        },
    }
}

/// `codesign -s` matches an identity by substring, so the check has to as well:
/// a configured CN like `MUR Local Signing` must count as present when the
/// keychain reports the full subject line.
fn identity_present(configured: &str, identities: &[String]) -> bool {
    let needle = configured.trim();
    if needle.is_empty() {
        return false;
    }
    identities.iter().any(|i| i.contains(needle) || i == needle)
}

/// Rank available identities for a *local re-sign*: a Developer ID beats an
/// Apple Development certificate, which beats anything else (including a
/// previously generated `MUR Local Signing`). All three work for the purpose;
/// the order just avoids minting a certificate when a real one is on hand.
fn preferred_candidate(identities: &[String]) -> Option<String> {
    let rank = |i: &String| -> u8 {
        if i.contains("Developer ID Application") {
            0
        } else if i.contains("Apple Development") {
            1
        } else {
            2
        }
    };
    identities.iter().min_by_key(|i| rank(i)).cloned()
}

/// Code-signing identity names from `security find-identity -v -p codesigning`.
///
/// `-v` lists only valid (unexpired, complete key pair) identities, which is
/// what makes an expired certificate show up as [`CodesignVerdict::Dangling`]
/// without parsing dates here. Any failure yields an empty list: treating an
/// unreadable keychain as "no identities" makes the check advisory rather than
/// wrong.
pub(crate) fn available_identities() -> Vec<String> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Ok(out) = Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
    else {
        return Vec::new();
    };
    parse_identities(&String::from_utf8_lossy(&out.stdout))
}

/// Pull the quoted subject names out of `security find-identity` output, whose
/// lines look like `  1) A1B2C3... "Apple Development: you (TEAM)"`.
fn parse_identities(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| {
            let start = l.find('"')?;
            let rest = &l[start + 1..];
            let end = rest.rfind('"')?;
            let name = rest[..end].trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Print the `codesign-identity` section of `mur doctor`.
///
/// Returns the verdict so `--fix` can act on it without probing twice.
pub(crate) fn report(configured: Option<&str>) -> CodesignVerdict {
    let v = verdict(configured, &available_identities());
    match &v {
        CodesignVerdict::Skipped => {}
        CodesignVerdict::Configured(id) => {
            println!("✅ Codesign identity: '{id}' — `mur update` re-signs with a stable identity");
        }
        CodesignVerdict::Dangling(id) => {
            println!("❌ Codesign identity: '{id}' is configured but not in the keychain");
            println!("     `mur update` will fail to re-sign. Remove or correct");
            println!("     `update.codesign_identity` in ~/.mur/config.yaml, or run");
            println!("     `mur doctor --fix` to create a local signing certificate.");
        }
        CodesignVerdict::UnsetWithCandidate(c) => {
            println!("⚠️  Codesign identity: not set — `mur update` will sign ad-hoc (#866)");
            println!("     Keychain grants bind to the binary hash, so they die on each upgrade.");
            println!("     You already have a usable identity: '{c}'");
            println!("     Run `mur doctor --fix` to use it, or set");
            println!("     `update.codesign_identity` in ~/.mur/config.yaml.");
        }
        CodesignVerdict::UnsetNoIdentity => {
            println!("⚠️  Codesign identity: not set — `mur update` will sign ad-hoc (#866)");
            println!("     Keychain grants bind to the binary hash, so they die on each upgrade.");
            println!("     No code-signing certificate found. `mur doctor --fix` creates a");
            println!("     self-signed one (no admin dialog; the keychain may ask for your");
            println!("     login password).");
        }
    }
    v
}

/// One-line description of what `--fix` would do, shown before it runs.
pub(crate) fn fix_description(v: &CodesignVerdict) -> Option<String> {
    match v {
        CodesignVerdict::UnsetWithCandidate(c) => {
            Some(format!("set update.codesign_identity to '{c}'"))
        }
        CodesignVerdict::UnsetNoIdentity | CodesignVerdict::Dangling(_) => Some(format!(
            "create a self-signed '{SELF_SIGNED_CN}' certificate and set update.codesign_identity"
        )),
        _ => None,
    }
}

/// Apply the fix for `v`. Config is only written after a real
/// `codesign -s` + `codesign -v` round-trip succeeds, so a green line here
/// means `mur update` will actually work.
pub(crate) fn apply_fix(v: &CodesignVerdict) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("codesign identities only exist on macOS");
    }
    let identity = match v {
        CodesignVerdict::UnsetWithCandidate(c) => c.clone(),
        CodesignVerdict::UnsetNoIdentity | CodesignVerdict::Dangling(_) => {
            create_self_signed_identity()?
        }
        _ => bail!("nothing to fix"),
    };

    verify_identity_signs(&identity)
        .with_context(|| format!("'{identity}' cannot sign — leaving config untouched"))?;

    let mut config = crate::store::config::load_config()?;
    config.update.codesign_identity = Some(identity.clone());
    crate::store::config::save_config(&config)?;
    println!("✓ update.codesign_identity = '{identity}' (verified by signing a test file)");
    Ok(())
}

/// Mint a self-signed code-signing certificate and import it into the login
/// keychain, returning the identity string to configure.
///
/// openssl rather than `certtool`, because `certtool` cannot set the
/// `codeSigning` extended key usage that `codesign` requires. The PKCS#12 is
/// written with the legacy SHA-1/3DES algorithms: openssl 3 defaults to
/// AES-256-CBC + PBKDF2, which macOS `security import` rejects outright.
fn create_self_signed_identity() -> Result<String> {
    if identity_present(SELF_SIGNED_CN, &available_identities()) {
        // Idempotent: a previous run already created it.
        return Ok(SELF_SIGNED_CN.to_string());
    }

    let dir = tempfile::tempdir().context("create temp dir for certificate material")?;
    let key = dir.path().join("key.pem");
    let cert = dir.path().join("cert.pem");
    let p12 = dir.path().join("identity.p12");
    // Transport password for the PKCS#12 only: it never leaves this temp dir
    // and the keychain holds the key afterwards.
    let pass = format!("pass:{}", uuid::Uuid::new_v4());

    let cnf = dir.path().join("openssl.cnf");
    std::fs::write(
        &cnf,
        format!(
            "[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n\
             [dn]\nCN={SELF_SIGNED_CN}\n\
             [v3]\nbasicConstraints=critical,CA:false\n\
             keyUsage=critical,digitalSignature\n\
             extendedKeyUsage=critical,codeSigning\n"
        ),
    )
    .context("write openssl config")?;

    run(
        "/usr/bin/openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            &CERT_VALIDITY_DAYS.to_string(),
            "-keyout",
            &key.to_string_lossy(),
            "-out",
            &cert.to_string_lossy(),
            "-config",
            &cnf.to_string_lossy(),
        ],
    )
    .context("generate self-signed code-signing certificate")?;

    run(
        "/usr/bin/openssl",
        &[
            "pkcs12",
            "-export",
            "-inkey",
            &key.to_string_lossy(),
            "-in",
            &cert.to_string_lossy(),
            "-out",
            &p12.to_string_lossy(),
            "-passout",
            &pass,
            "-name",
            SELF_SIGNED_CN,
            "-macalg",
            "sha1",
            "-keypbe",
            "PBE-SHA1-3DES",
            "-certpbe",
            "PBE-SHA1-3DES",
        ],
    )
    .context("bundle certificate as PKCS#12")?;

    let keychain = login_keychain().context("locate the login keychain")?;
    // `-T /usr/bin/codesign` pre-authorizes codesign so signing does not raise
    // a per-use "allow access" dialog. No `add-trusted-cert`: see module docs.
    run(
        "/usr/bin/security",
        &[
            "import",
            &p12.to_string_lossy(),
            "-k",
            &keychain,
            "-P",
            pass.trim_start_matches("pass:"),
            "-T",
            "/usr/bin/codesign",
        ],
    )
    .context("import certificate into the login keychain")?;

    if !identity_present(SELF_SIGNED_CN, &available_identities()) {
        bail!(
            "certificate imported but '{SELF_SIGNED_CN}' is not listed as a codesigning identity"
        );
    }
    println!("✓ created self-signed certificate '{SELF_SIGNED_CN}' in {keychain}");
    Ok(SELF_SIGNED_CN.to_string())
}

/// Path of the login keychain, as `security` wants it spelled.
fn login_keychain() -> Option<String> {
    let home = dirs::home_dir()?;
    let db = home.join("Library/Keychains/login.keychain-db");
    let legacy = home.join("Library/Keychains/login.keychain");
    let chosen = if db.exists() { db } else { legacy };
    Some(chosen.to_string_lossy().into_owned())
}

/// Sign a throwaway copy of a real Mach-O and verify it. This is the only
/// evidence that matters: an identity that lists but cannot sign (missing
/// private key, denied keychain access) must not reach `config.yaml`.
fn verify_identity_signs(identity: &str) -> Result<()> {
    let dir = tempfile::tempdir().context("create temp dir for signing probe")?;
    let target = dir.path().join("probe");
    std::fs::copy("/bin/echo", &target).context("copy probe binary")?;
    run(
        "/usr/bin/codesign",
        &["-f", "-s", identity, &target.to_string_lossy()],
    )
    .context("sign the probe binary")?;
    run("/usr/bin/codesign", &["-v", &target.to_string_lossy()])
        .context("verify the probe signature")?;
    Ok(())
}

/// Run a command, failing with its stderr so the user sees what `security` or
/// `openssl` actually said rather than a bare exit code.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute {program}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "{program} failed ({}): {}",
            out.status,
            if err.is_empty() { "no output" } else { &err }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_find_identity_output() {
        let out = "  1) AAAA \"Apple Development: me (TEAM1)\"\n  \
                   2) BBBB \"Developer ID Application: Co (TEAM2)\"\n     \
                   2 valid identities found\n";
        assert_eq!(
            parse_identities(out),
            ids(&[
                "Apple Development: me (TEAM1)",
                "Developer ID Application: Co (TEAM2)"
            ])
        );
    }

    #[test]
    fn empty_output_yields_no_identities() {
        assert!(parse_identities("  0 valid identities found\n").is_empty());
    }

    #[test]
    fn developer_id_preferred_over_apple_development() {
        let got = preferred_candidate(&ids(&[
            "Apple Development: me (T1)",
            "Developer ID Application: Co (T2)",
        ]));
        assert_eq!(got.as_deref(), Some("Developer ID Application: Co (T2)"));
    }

    #[test]
    fn self_signed_is_last_resort_candidate() {
        let got = preferred_candidate(&ids(&[SELF_SIGNED_CN, "Apple Development: me (T1)"]));
        assert_eq!(got.as_deref(), Some("Apple Development: me (T1)"));
    }

    #[test]
    fn configured_matches_by_substring() {
        assert!(identity_present(
            "Developer ID Application",
            &ids(&["Developer ID Application: Co (T2)"])
        ));
        assert!(!identity_present(
            "Nope",
            &ids(&["Apple Development: me (T1)"])
        ));
        assert!(!identity_present(
            "  ",
            &ids(&["Apple Development: me (T1)"])
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn verdict_table() {
        assert_eq!(
            verdict(
                Some("Apple Development: me (T1)"),
                &ids(&["Apple Development: me (T1)"])
            ),
            CodesignVerdict::Configured("Apple Development: me (T1)".into())
        );
        assert_eq!(
            verdict(Some("Gone: x"), &ids(&["Apple Development: me (T1)"])),
            CodesignVerdict::Dangling("Gone: x".into())
        );
        assert_eq!(
            verdict(None, &ids(&["Apple Development: me (T1)"])),
            CodesignVerdict::UnsetWithCandidate("Apple Development: me (T1)".into())
        );
        assert_eq!(verdict(None, &[]), CodesignVerdict::UnsetNoIdentity);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_always_skips() {
        assert_eq!(verdict(None, &[]), CodesignVerdict::Skipped);
        assert_eq!(verdict(Some("x"), &[]), CodesignVerdict::Skipped);
    }

    #[test]
    fn only_actionable_verdicts_carry_a_fix() {
        assert!(fix_description(&CodesignVerdict::UnsetNoIdentity).is_some());
        assert!(fix_description(&CodesignVerdict::Dangling("x".into())).is_some());
        assert!(fix_description(&CodesignVerdict::UnsetWithCandidate("x".into())).is_some());
        assert!(fix_description(&CodesignVerdict::Skipped).is_none());
        assert!(fix_description(&CodesignVerdict::Configured("x".into())).is_none());
    }

    #[test]
    fn needs_attention_excludes_ok_and_skipped() {
        assert!(!CodesignVerdict::Skipped.needs_attention());
        assert!(!CodesignVerdict::Configured("x".into()).needs_attention());
        assert!(CodesignVerdict::UnsetNoIdentity.needs_attention());
    }
}
