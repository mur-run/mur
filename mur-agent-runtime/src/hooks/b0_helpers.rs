//! Pure helpers for B0SafetyHook rule branches.
//!
//! Each helper is a free function with no IO and no Tauri/runtime
//! state, so unit tests can construct fixtures directly. The helpers
//! are imported by `mur-agent-runtime/src/hooks/b0.rs` from the rule
//! branches that need them.

use std::path::Path;

/// Returns `true` when `candidate` is inside `confine_to` (after
/// canonicalization). A `candidate` that does NOT exist is checked
/// against the parent's canonical path — useful for fs.write where
/// the file may be about to be created.
///
/// Symlinks ARE followed (`canonicalize` resolves them) so this is a
/// real-world confinement check, not a string-prefix match.
pub fn path_confined_to(candidate: &Path, confine_to: &Path) -> bool {
    let confine_canonical = match std::fs::canonicalize(confine_to) {
        Ok(p) => p,
        Err(_) => return false, // confine_to missing — fail closed
    };
    let candidate_canonical = match std::fs::canonicalize(candidate) {
        Ok(p) => p,
        Err(_) => {
            // Not yet created. Check the parent.
            match candidate.parent() {
                Some(parent) => match std::fs::canonicalize(parent) {
                    Ok(p) => p,
                    Err(_) => return false,
                },
                None => return false,
            }
        }
    };
    candidate_canonical.starts_with(&confine_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn confined_path_is_inside() {
        let dir = TempDir::new().unwrap();
        let inner = dir.path().join("inside.txt");
        std::fs::write(&inner, "x").unwrap();
        assert!(path_confined_to(&inner, dir.path()));
    }

    #[test]
    fn outside_path_rejected() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let foreign = other.path().join("file.txt");
        std::fs::write(&foreign, "x").unwrap();
        assert!(!path_confined_to(&foreign, dir.path()));
    }

    #[test]
    fn nonexistent_file_uses_parent_for_check() {
        let dir = TempDir::new().unwrap();
        let new_file = dir.path().join("doesnt-exist-yet.txt");
        // Parent (dir) exists and IS the confine root.
        assert!(path_confined_to(&new_file, dir.path()));
    }

    #[test]
    fn nonexistent_parent_fails_closed() {
        let dir = TempDir::new().unwrap();
        let two_deep = dir.path().join("ghost-dir/file.txt");
        assert!(!path_confined_to(&two_deep, dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_rejected() {
        let confine = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let target = other.path().join("real.txt");
        std::fs::write(&target, "x").unwrap();
        let link = confine.path().join("escape.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // Symlink resolves outside confine_to → reject.
        assert!(!path_confined_to(&link, confine.path()));
    }
}

/// Match a host string against an allowlist.
///
/// Delegates to [`crate::sandbox::reqwest_guard::host_matches_pattern`] so
/// both the B0 gate and the HostGuard DNS resolver share a single wildcard
/// interpretation.  The canonical form is `*.example.com`; the legacy
/// `.example.com` leading-dot form is also accepted.
pub fn host_is_allowlisted(host: &str, allow: &[String]) -> bool {
    allow
        .iter()
        .any(|p| crate::sandbox::reqwest_guard::host_matches_pattern(host, p))
}

#[cfg(test)]
mod allowlist_tests {
    use super::*;

    #[test]
    fn exact_match_allowed() {
        assert!(host_is_allowlisted(
            "api.openai.com",
            &["api.openai.com".into()]
        ));
    }

    #[test]
    fn case_insensitive() {
        assert!(host_is_allowlisted(
            "API.OpenAI.com",
            &["api.openai.com".into()]
        ));
    }

    #[test]
    fn dot_prefix_matches_subdomain() {
        let allow = vec![".openai.com".into()];
        assert!(host_is_allowlisted("api.openai.com", &allow));
        assert!(host_is_allowlisted("openai.com", &allow));
    }

    #[test]
    fn unrelated_host_rejected() {
        let allow = vec![".openai.com".into()];
        assert!(!host_is_allowlisted("evil.com", &allow));
        assert!(!host_is_allowlisted("notopenai.com", &allow));
    }

    #[test]
    fn empty_allowlist_rejects_everything() {
        assert!(!host_is_allowlisted("api.openai.com", &[]));
    }
}

/// Scan body for known credential/secret patterns. Returns the FIRST
/// match's classification (or `None` if clean). Delegates to
/// `mur_common::skill::scan::secrets` so the pattern list is a single
/// source of truth.
pub fn scan_for_secrets(body: &str) -> Option<&'static str> {
    mur_common::skill::scan::scan_secrets(body)
        .into_iter()
        .next()
        .map(|f| f.label)
}

/// Replace every match of the credential pattern set with
/// `[REDACTED:<label>]`. Used at the telemetry write boundary
/// (B0 rule 9 / M8.1) to scrub free-form strings before they
/// land on disk in `~/.mur/agents/<name>/telemetry/<date>.jsonl`.
///
/// Returns `Cow::Borrowed` when nothing matched so the common
/// hot path (no secrets present) avoids any allocation.
/// Replace credential-shaped substrings with `[REDACTED:<kind>]`.
///
/// Thin forwarder: the implementation moved to `mur_common::redact` so the
/// CLI's capture queue writer can share it (#979). B0 rule 9 is named
/// "telemetry sink redaction" and used to cover only this crate's writer.
pub fn redact_secrets(input: &str) -> std::borrow::Cow<'_, str> {
    mur_common::redact::redact_secrets(input)
}

/// Replace home-directory-style absolute paths with `~/`.
///
/// Thin forwarder — see `redact_secrets`.
pub fn redact_home_path(input: &str) -> std::borrow::Cow<'_, str> {
    mur_common::redact::redact_home_path(input)
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    #[test]
    fn detects_openai_key() {
        assert_eq!(
            scan_for_secrets("here is my key: sk-abcd1234567890efghij1234"),
            Some("openai_key"),
        );
    }

    #[test]
    fn detects_anthropic_key() {
        assert!(scan_for_secrets("sk-ant-abcdefghijklmnopqrst-1234").is_some());
    }

    #[test]
    fn detects_aws_access_key() {
        assert_eq!(
            scan_for_secrets("AKIAIOSFODNN7EXAMPLE"),
            Some("aws_access_key"),
        );
    }

    #[test]
    fn detects_github_pat() {
        assert_eq!(
            scan_for_secrets("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Some("github_pat"),
        );
    }

    #[test]
    fn detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(scan_for_secrets(jwt), Some("jwt"));
    }

    #[test]
    fn detects_pem() {
        assert_eq!(
            scan_for_secrets("-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END..."),
            Some("pem_private_key"),
        );
    }

    #[test]
    fn detects_env_assignment() {
        assert_eq!(
            scan_for_secrets("api_key=abcdefghij1234567890"),
            Some("env_assignment"),
        );
    }

    #[test]
    fn clean_text_returns_none() {
        assert_eq!(scan_for_secrets("the model is gpt-4o today"), None);
        assert_eq!(scan_for_secrets("this is a normal message"), None);
    }
}

#[cfg(test)]
mod telemetry_redact_tests {
    use super::*;

    #[test]
    fn redact_secrets_replaces_openai_key() {
        let out = redact_secrets("oops sk-abcd1234567890efghij1234 leaked");
        assert!(out.contains("[REDACTED:openai_key]"), "got {out:?}");
        assert!(!out.contains("sk-abcd"));
    }

    #[test]
    fn redact_secrets_replaces_anthropic_and_aws_in_one_string() {
        let s = "key1 sk-ant-abcdefghijklmnop-9999 and aws AKIAIOSFODNN7EXAMPL2";
        let out = redact_secrets(s);
        assert!(out.contains("[REDACTED:anthropic_key]"));
        assert!(out.contains("[REDACTED:aws_access_key]"));
    }

    #[test]
    fn redact_secrets_clean_text_borrows() {
        // No allocation when input is clean.
        let s = "all good here, no secrets";
        let out = redact_secrets(s);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn redact_home_path_collapses_macos() {
        let out = redact_home_path("failed to read /Users/alice/secret.txt: nope");
        assert_eq!(out, "failed to read ~/secret.txt: nope");
    }

    #[test]
    fn redact_home_path_collapses_linux() {
        let out = redact_home_path("ENOENT at /home/bob/.ssh/id_rsa");
        assert_eq!(out, "ENOENT at ~/.ssh/id_rsa");
    }

    #[test]
    fn redact_home_path_collapses_windows() {
        let out = redact_home_path(r"open C:\Users\Carol\Desktop\notes.md failed");
        assert!(out.contains(r"~\Desktop\notes.md"), "got {out:?}");
    }

    #[test]
    fn redact_home_path_clean_text_borrows() {
        let s = "no path here";
        let out = redact_home_path(s);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn redact_secrets_handles_pem_block() {
        let out = redact_secrets("-----BEGIN RSA PRIVATE KEY-----\nMIIE...");
        assert!(out.contains("[REDACTED:pem_private_key]"), "got {out:?}");
    }
}

/// Redact common PII patterns in `body`. Returns the redacted text;
/// the redaction is permissive (catches obvious patterns; defers to
/// the user for ambiguous cases).
///
/// Replaces matched spans with `<REDACTED:label>`.
pub fn redact_pii(body: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // Email
            (
                Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").unwrap(),
                "email",
            ),
            // US SSN
            (Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(), "ssn"),
            // Credit card (very loose: 13-19 digits in groups)
            (Regex::new(r"\b(?:\d{4}[- ]?){3,4}\d{1,4}\b").unwrap(), "cc"),
            // Phone (international or US-style)
            (
                Regex::new(r"\b\+?\d{1,3}[- ]?\(?\d{3}\)?[- ]?\d{3}[- ]?\d{4}\b").unwrap(),
                "phone",
            ),
        ]
    });

    let mut out = body.to_string();
    for (rx, label) in patterns {
        out = rx
            .replace_all(&out, format!("<REDACTED:{label}>"))
            .to_string();
    }
    out
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    #[test]
    fn redacts_email() {
        assert_eq!(
            redact_pii("contact alex@example.com"),
            "contact <REDACTED:email>"
        );
    }

    #[test]
    fn redacts_ssn() {
        assert_eq!(redact_pii("ssn 123-45-6789"), "ssn <REDACTED:ssn>");
    }

    #[test]
    fn redacts_credit_card() {
        let red = redact_pii("card 4111-1111-1111-1111");
        assert!(red.contains("<REDACTED:cc>"), "got {red}");
    }

    #[test]
    fn redacts_phone() {
        let red = redact_pii("call +1-555-123-4567");
        assert!(red.contains("<REDACTED:phone>"), "got {red}");
    }

    #[test]
    fn clean_text_unchanged() {
        let clean = "the project will ship next week.";
        assert_eq!(redact_pii(clean), clean);
    }
}

/// Ask `wintrust.dll` whether `path` carries a valid Authenticode signature.
///
/// Returns the raw `WinVerifyTrust` status so the policy — which statuses
/// refuse a startup — stays in [`wintrust_verdict`], a pure function that
/// compiles and is tested on every platform. Keeping the FFI this thin is
/// deliberate: code only a Windows CI runner can execute is code nobody reads
/// a test failure for.
#[cfg(target_os = "windows")]
fn wintrust_verify(path: &std::path::Path) -> i32 {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_SAFER_FLAG, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    data.dwUIChoice = WTD_UI_NONE;
    // No revocation check: it reaches the network, and a boot that hangs
    // because a CRL endpoint is slow is its own outage.
    data.fdwRevocationChecks = WTD_REVOKE_NONE;
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous = WINTRUST_DATA_0 {
        pFile: &mut file_info,
    };
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    data.dwProvFlags = WTD_SAFER_FLAG;

    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    // VERIFY allocates state that CLOSE frees; skipping it leaks per call.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
    }
    status
}

/// Which `WinVerifyTrust` statuses refuse a startup.
///
/// **Presence and integrity, not trust chain** — the same question the macOS
/// branch asks. `codesign -dv` reports whether a signature is *there*; it does
/// not demand that the chain validate on this machine. Windows now matches:
/// no signature at all, or a signature that does not match the bytes, refuses
/// the startup; an expired certificate or a root this machine does not trust
/// does not.
///
/// That asymmetry is the point rather than an oversight. Rule 11 exists to
/// catch a binary that was swapped, and a swapped binary fails the digest.
/// An expired cert is not something the operator of the agent can fix — the
/// publisher has to reissue — and this session's field report is what a
/// startup gate on an unsatisfiable condition costs: the agent never runs
/// again and the error names a fix that does not exist. See
/// `docs/architecture/mcp-supply-chain.md`.
///
/// Defined on every platform so its policy is unit-tested everywhere, not only
/// where it runs.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn wintrust_verdict(status: i32, path: &std::path::Path) -> Result<(), String> {
    // HRESULTs from wintrust; `as i32` because they are negative when signed.
    const S_OK: i32 = 0;
    const TRUST_E_NOSIGNATURE: i32 = 0x800B_0100u32 as i32;
    const TRUST_E_BAD_DIGEST: i32 = 0x8009_6010u32 as i32;
    const TRUST_E_EXPLICIT_DISTRUST: i32 = 0x800B_0111u32 as i32;
    const TRUST_E_SUBJECT_FORM_UNKNOWN: i32 = 0x800B_0003u32 as i32;
    const CERT_E_EXPIRED: i32 = 0x800B_0101u32 as i32;
    const CERT_E_UNTRUSTEDROOT: i32 = 0x800B_0109u32 as i32;

    match status {
        S_OK => Ok(()),
        TRUST_E_NOSIGNATURE => Err(format!("Windows binary not signed: {}", path.display())),
        TRUST_E_BAD_DIGEST => Err(format!(
            "Windows binary's signature does not match its contents — the file changed after it was signed: {}",
            path.display()
        )),
        TRUST_E_EXPLICIT_DISTRUST => Err(format!(
            "Windows binary is explicitly distrusted by this machine's policy: {}",
            path.display()
        )),
        // Signed, but the chain does not validate here. Not the operator's to
        // fix, and not what rule 11 is looking for.
        CERT_E_EXPIRED | CERT_E_UNTRUSTEDROOT => Ok(()),
        // Not a form Authenticode knows how to check. `is_native_image` already
        // filtered scripts out; anything still landing here is a native image
        // this machine cannot answer for, which is not evidence of tampering.
        TRUST_E_SUBJECT_FORM_UNKNOWN => Ok(()),
        other => Err(format!(
            "Windows signature check failed (0x{:08X}): {}",
            other as u32,
            path.display()
        )),
    }
}

#[cfg(test)]
mod wintrust_tests {
    /// The policy half of rule 11 on Windows, exercised on every platform —
    /// the point of keeping it out of the FFI.
    #[test]
    fn wintrust_refuses_only_missing_or_broken_signatures() {
        use super::wintrust_verdict;
        let p = std::path::Path::new("C:\\srv.exe");

        assert!(wintrust_verdict(0, p).is_ok(), "S_OK verifies");

        let unsigned = wintrust_verdict(0x800B_0100u32 as i32, p).unwrap_err();
        assert!(unsigned.contains("not signed"), "{unsigned}");

        // The one rule 11 actually exists for: bytes changed after signing.
        let tampered = wintrust_verdict(0x8009_6010u32 as i32, p).unwrap_err();
        assert!(
            tampered.contains("does not match its contents"),
            "{tampered}"
        );

        let distrusted = wintrust_verdict(0x800B_0111u32 as i32, p).unwrap_err();
        assert!(distrusted.contains("distrusted"), "{distrusted}");

        // Signed, chain does not validate here. Not the operator's to fix, so
        // not a startup refusal — the same question macOS's `codesign -dv`
        // asks, and the lesson of the npx brick.
        assert!(
            wintrust_verdict(0x800B_0101u32 as i32, p).is_ok(),
            "an expired certificate must not brick an agent"
        );
        assert!(
            wintrust_verdict(0x800B_0109u32 as i32, p).is_ok(),
            "an untrusted root must not brick an agent"
        );
        assert!(
            wintrust_verdict(0x800B_0003u32 as i32, p).is_ok(),
            "a form Authenticode cannot check is not evidence of tampering"
        );

        // Anything unrecognised still refuses, and names the code so the
        // operator can look it up rather than guess.
        let odd = wintrust_verdict(0x8009_6004u32 as i32, p).unwrap_err();
        assert!(odd.contains("0x80096004"), "{odd}");
    }
}

/// True when `path` is a native executable image the platform code-signing
/// tools can actually verify: Mach-O (thin or fat) on macOS, PE on Windows.
///
/// Anything else — a JavaScript file, a `#!` wrapper, a `.py` entry point — is
/// a *script*, and no amount of `codesign` will ever say yes to one. What runs
/// it is an interpreter resolved at exec time, which the entry's pin does not
/// cover either (see `docs/architecture/mcp-supply-chain.md`,
/// "Interpreter-launched entries are reported, not enforced").
///
/// Detected from the file header rather than from a list of interpreter names,
/// because such a list is exactly the thing that goes stale: `npx` today,
/// `bunx`/`pnpm dlx`/`uvx` tomorrow. The header is the property that matters.
///
/// Unreadable → `false`: rule 11 treats a binary it cannot read as a soft
/// failure, and a hard refusal here would resurrect the brick this guards.
fn is_native_image(path: &std::path::Path) -> bool {
    use std::io::Read as _;
    let mut head = [0u8; 4];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    if cfg!(windows) {
        return n >= 2 && &head[..2] == b"MZ";
    }
    if n < 4 {
        return false;
    }
    // Mach-O MH_MAGIC / MH_CIGAM (32- and 64-bit) and FAT_MAGIC / FAT_CIGAM.
    matches!(
        u32::from_be_bytes(head),
        0xFEED_FACE | 0xCEFA_EDFE | 0xFEED_FACF | 0xCFFA_EDFE | 0xCAFE_BABE | 0xBEBA_FECA
    )
}

/// Returns Ok(()) if the binary at `path` is signed (or sig-checks
/// don't apply on this platform). Returns Err with a user-actionable
/// reason on macOS/Windows when the signature is missing or invalid.
///
/// **Scripts are out of scope, and saying so is the point.** Resolving an MCP
/// entry's `command` follows symlinks, so `npx` lands on `npm/bin/npx-cli.js`;
/// demanding a signature there refused startup for a condition the user cannot
/// fix at all, with a hint telling them to run `codesign` on a `.js` file. An
/// agent given two such entries could not boot again (field report,
/// 2026-09-15). What protects an interpreter-launched entry is `mur agent mcp
/// vendor` — a MUR-owned install whose lockfile rule 6 then enforces — not a
/// signature that cannot exist.
pub fn verify_signed(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("binary missing: {}", path.display()));
    }
    if !is_native_image(path) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(path)
            .output()
            .map_err(|e| format!("codesign spawn: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "macOS binary not signed: {} (run `codesign -dv --verbose=4 {0}` for details)",
                path.display()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        wintrust_verdict(wintrust_verify(path), path)
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        // Linux: signing is not standard for native binaries.
        // Spec calls this out as macOS/Windows only.
        let _ = path;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────
// B0 rule 6 / M9.3 — MCP install-time pin verification.
// ─────────────────────────────────────────────────────────────────

/// Why a pinned MCP entry failed re-verification at startup. Used to
/// drive the user-facing recovery prompt (`mur agent mcp inspect
/// <name>`) so the message can specifically say "the binary changed"
/// vs. "the description changed" rather than a generic mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinDriftReason {
    /// The pinned binary path could not be resolved on disk. The user
    /// either uninstalled the MCP without removing it from
    /// profile.yaml or the binary moved. Treated as a soft fail (warn,
    /// don't block) so the supervisor can still start.
    BinaryMissing { path: String, io_error: String },
    /// The pinned binary's SHA-256 did not match the recorded hash.
    /// Hard fail — the binary on disk is not what was approved at
    /// install time.
    BinaryDrift { expected: String, actual: String },
    /// I/O error while reading the binary (permissions / disk).
    /// Treated as soft fail; rule 11 codesign check would have caught
    /// the equivalent permission issue first.
    BinaryReadError { path: String, io_error: String },
}

impl std::fmt::Display for PinDriftReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryMissing { path, io_error } => {
                write!(f, "binary at `{path}` cannot be located ({io_error})")
            }
            Self::BinaryDrift { expected, actual } => {
                let exp_short = expected.chars().take(16).collect::<String>();
                let act_short = actual.chars().take(16).collect::<String>();
                write!(
                    f,
                    "binary SHA-256 changed (pinned: {exp_short}…, current: {act_short}…)"
                )
            }
            Self::BinaryReadError { path, io_error } => {
                write!(f, "binary at `{path}` could not be read ({io_error})")
            }
        }
    }
}

/// Re-compute the SHA-256 of `path` and compare with `expected`. Used
/// by `B0SafetyHook::on_startup` to enforce the rule-6 install-time
/// pin on every supervisor start.
///
/// Mirrors the chunked stream-hash in `mur-core::cmd::agent_mcp_pin`
/// so the install-side and verify-side outputs are byte-identical.
/// Duplicated rather than imported to avoid the `mur-agent-runtime ←
/// mur-core` dependency cycle this hook already takes pains to avoid.
/// SHA-256 (lowercase hex) of a binary, in exactly the form a `binary_sha256`
/// pin records.
///
/// Shared by the verifier below and by the supervisor's re-pin of MUR's own
/// bundled MCP server, so the hash that gets written and the hash that gets
/// checked can never disagree about algorithm or encoding.
pub fn binary_sha256(path: &std::path::Path) -> Result<String, PinDriftReason> {
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use std::io::Read;

    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            // ENOENT vs other I/O — distinguish so the user sees a
            // clear "uninstalled?" hint rather than a generic error.
            if e.kind() == std::io::ErrorKind::NotFound {
                return Err(PinDriftReason::BinaryMissing {
                    path: path.display().to_string(),
                    io_error: e.to_string(),
                });
            }
            return Err(PinDriftReason::BinaryReadError {
                path: path.display().to_string(),
                io_error: e.to_string(),
            });
        }
    };

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(PinDriftReason::BinaryReadError {
                    path: path.display().to_string(),
                    io_error: e.to_string(),
                });
            }
        };
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn verify_mcp_binary_hash(
    expected: &str,
    path: &std::path::Path,
) -> Result<(), PinDriftReason> {
    let actual = binary_sha256(path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(PinDriftReason::BinaryDrift {
            expected: expected.to_string(),
            actual,
        })
    }
}

#[cfg(test)]
mod pin_verify_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Same SHA-256 fixture as `mur-core::cmd::agent_mcp_pin` so the
    /// two halves of M9 stay in lockstep.
    const HELLO_SHA: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

    #[test]
    fn matches_pinned_hash_returns_ok() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello\n").unwrap();
        assert!(verify_mcp_binary_hash(HELLO_SHA, f.path()).is_ok());
    }

    #[test]
    fn case_insensitive_hash_match() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello\n").unwrap();
        let upper: String = HELLO_SHA.chars().map(|c| c.to_ascii_uppercase()).collect();
        assert!(verify_mcp_binary_hash(&upper, f.path()).is_ok());
    }

    #[test]
    fn drift_returns_specific_reason() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"world\n").unwrap();
        let err = verify_mcp_binary_hash(HELLO_SHA, f.path()).unwrap_err();
        match err {
            PinDriftReason::BinaryDrift { expected, actual } => {
                assert_eq!(expected, HELLO_SHA);
                assert_ne!(actual, HELLO_SHA);
                assert_eq!(actual.len(), 64);
            }
            other => panic!("expected BinaryDrift, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_distinguished_from_read_error() {
        let err = verify_mcp_binary_hash(HELLO_SHA, std::path::Path::new("/no/such/binary-xyz"))
            .unwrap_err();
        assert!(
            matches!(err, PinDriftReason::BinaryMissing { .. }),
            "got {err:?}",
        );
    }

    #[test]
    fn drift_display_includes_short_hash_prefixes() {
        let drift = PinDriftReason::BinaryDrift {
            expected: "deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb".into(),
            actual: "cafebabe00112233445566778899aabbccddeeff00112233445566778899aabb".into(),
        };
        let s = drift.to_string();
        assert!(s.contains("deadbeef00112233"), "got {s}");
        assert!(s.contains("cafebabe00112233"), "got {s}");
        assert!(!s.contains("deadbeef00112233445566"), "no full hash leak");
    }
}
