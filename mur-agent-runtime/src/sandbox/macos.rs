use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use std::ffi::CString;

pub fn apply_macos(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let profile = build_sbpl_profile(policy);
    let profile_c = CString::new(profile).context("SBPL profile to CString")?;

    let mut error_buf: *mut libc::c_char = std::ptr::null_mut();
    // parameters is a null-terminated array of (key, value, ..., NULL) pairs.
    // We pass no template parameters.
    let params: [*const libc::c_char; 1] = [std::ptr::null()];

    let rc = unsafe {
        sandbox_init_with_parameters(
            profile_c.as_ptr(),
            0, // flags — always 0
            params.as_ptr(),
            &mut error_buf,
        )
    };

    if rc != 0 {
        let msg = if error_buf.is_null() {
            "unknown SBPL error".to_string()
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(error_buf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error_buf) };
            s
        };
        tracing::warn!(error = %msg, "macOS sandbox_init failed; running advisory-only");
        return Ok(SandboxStatus {
            platform: "macos-sbpl-failed".to_string(),
            effective_abi: None,
            enforcing: false,
        });
    }

    Ok(SandboxStatus {
        platform: "macos-sbpl".to_string(),
        effective_abi: None,
        enforcing: true,
    })
}

/// Standard macOS locations a process must be able to write to in order to
/// function (per-user temp + cache used by dyld, confstr, NSURLSession, etc.).
/// Under the default-deny-write baseline these are re-allowed so confinement
/// blocks user data (Documents, etc.) without breaking the runtime.
const MACOS_SYSTEM_WRITE_PATHS: &[&str] = &[
    "/private/var/folders", // per-user temp + dyld closures + confstr dirs
    "/private/tmp",         // /tmp symlinks here
    "/dev/null",
    "/dev/stdout",
    "/dev/stderr",
];

/// Escape a string for safe inclusion in an SBPL double-quoted literal.
///
/// SBPL (a TinyScheme dialect) honors `\\` and `\"` inside string literals.
/// Without escaping, a path or host containing `"` / `)` could break out of
/// the literal and rewrite the policy — and if the resulting profile fails to
/// parse, `apply_macos` falls back to advisory-only (no sandbox). Since paths
/// and hosts originate from `profile.yaml` (attacker-influenced for imported
/// `.muragent` packages), escaping is a trust boundary, not cosmetics.
fn sbpl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // Drop control characters that could corrupt the profile text.
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Build an SBPL profile string from the policy.
///
/// Strategy: default-allow for reads/exec/network (so the process can load
/// system libraries and run), but **default-deny for filesystem writes** —
/// nothing is writable except the agent's own declared write paths plus the
/// standard macOS system-write locations. This matches the Linux Landlock
/// posture for writes without the fragility of a full `(deny default)` that
/// would also block dyld and system access. All interpolated paths/hosts are
/// escaped to prevent s-expression injection via a malicious profile.
pub fn build_sbpl_profile(policy: &SandboxPolicy) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
        // Baseline: deny ALL writes. Specific paths are re-allowed below.
        "(deny file-write* (subpath \"/\"))".to_string(),
    ];

    // Deny reads (and keep an explicit write-deny) on sensitive paths.
    for path in &policy.fs_deny {
        let p = sbpl_escape(&path.to_string_lossy());
        lines.push(format!("(deny file-read* (subpath \"{p}\"))"));
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
    }

    // Re-allow writes for the standard macOS system-write locations so the
    // runtime (dyld, temp, stdio) keeps working under the deny baseline.
    for p in MACOS_SYSTEM_WRITE_PATHS {
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Re-allow writes to the policy's explicitly allowed write paths. These
    // come last so they win the last-match-wins evaluation over the baseline.
    for path in &policy.fs_write {
        let p = sbpl_escape(&path.to_string_lossy());
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Network restrictions. NOTE: macOS SBPL `remote tcp` only accepts `*` or
    // `localhost` as the host — a hostname like "api.anthropic.com:443" is a
    // hard parse error that fails sandbox_init for the WHOLE profile (silent
    // fail-open). So we restrict by PORT here (host `*`) and delegate hostname
    // allowlisting to the HostGuard reqwest layer.
    match &policy.net_allow_ports {
        None => { /* Unrestricted: (allow default) covers outbound. */ }
        Some(ports) if ports.is_empty() => {
            // Off: deny all outbound TCP.
            lines.push("(deny network-outbound)".to_string());
        }
        Some(ports) => {
            lines.push("(deny network-outbound)".to_string());
            // macOS resolves hostnames by talking to mDNSResponder over this
            // UNIX socket, which is itself a `network-outbound` op. Under the
            // blanket deny it must be re-allowed or ALL name resolution fails —
            // every external host errors with "Could not resolve host", so only
            // loopback IPs (which need no DNS) are reachable. HostGuard still
            // gates which hostnames the runtime client resolves; this restores
            // resolution only, not arbitrary egress (TCP stays port-gated).
            const MDNSRESPONDER_SOCKET: &str = "/private/var/run/mDNSResponder";
            lines.push(format!(
                "(allow network-outbound (remote unix-socket (path-literal \"{MDNSRESPONDER_SOCKET}\")))"
            ));
            for port in ports {
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"*:{port}\"))"
                ));
            }
        }
    }

    lines.join("\n")
}

unsafe extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const libc::c_char,
        flags: u64,
        parameters: *const *const libc::c_char,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;

    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn policy_with(write: Vec<PathBuf>, deny: Vec<PathBuf>) -> SandboxPolicy {
        SandboxPolicy {
            fs_write: write,
            fs_deny: deny,
            ..Default::default()
        }
    }

    #[test]
    fn writes_are_default_deny_with_baseline() {
        let sbpl = build_sbpl_profile(&policy_with(vec![], vec![]));
        assert!(
            sbpl.contains("(deny file-write* (subpath \"/\"))"),
            "missing default-deny-write baseline:\n{sbpl}"
        );
    }

    #[test]
    fn system_write_paths_are_reallowed() {
        let sbpl = build_sbpl_profile(&policy_with(vec![], vec![]));
        for p in MACOS_SYSTEM_WRITE_PATHS {
            assert!(
                sbpl.contains(&format!("(allow file-write* (subpath \"{p}\"))")),
                "missing system write allow for {p}:\n{sbpl}"
            );
        }
    }

    #[test]
    fn declared_write_path_is_allowed_after_baseline() {
        let sbpl = build_sbpl_profile(&policy_with(vec![PathBuf::from("/data/agent")], vec![]));
        let baseline = sbpl.find("(deny file-write* (subpath \"/\"))").unwrap();
        let allow = sbpl
            .find("(allow file-write* (subpath \"/data/agent\"))")
            .expect("declared write path must be allowed");
        assert!(
            allow > baseline,
            "allow must follow the deny baseline (last-match-wins)"
        );
    }

    #[test]
    fn malicious_path_is_escaped_not_injected() {
        // A path crafted to break out of the SBPL string literal must be
        // neutralized — the raw injection payload must not appear verbatim.
        let evil = PathBuf::from("x\") (allow file-write* (subpath \"/");
        let sbpl = build_sbpl_profile(&policy_with(vec![evil], vec![]));
        assert!(
            !sbpl.contains("x\") (allow file-write* (subpath \"/\"))"),
            "unescaped injection payload leaked into profile:\n{sbpl}"
        );
        assert!(
            sbpl.contains("\\\""),
            "quote should be backslash-escaped:\n{sbpl}"
        );
    }

    #[test]
    fn off_mode_denies_network() {
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(sbpl.contains("(deny network-outbound)"));
        assert!(
            !sbpl.contains("(allow network-outbound"),
            "Off mode must not allow any outbound:\n{sbpl}"
        );
    }

    #[test]
    fn restricted_uses_port_wildcard_not_hostname() {
        // Regression: hostname-based `remote tcp` is invalid SBPL and fails
        // sandbox_init for the whole profile (silent fail-open). Restricted
        // mode must emit `*:<port>` rules only.
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![443, 80]);
        policy.net_allow_hosts = Some(vec!["api.anthropic.com".to_string()]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:443\"))"));
        assert!(sbpl.contains("(allow network-outbound (remote tcp \"*:80\"))"));
        assert!(
            !sbpl.contains("api.anthropic.com"),
            "SBPL must not contain hostnames (invalid `remote tcp` host):\n{sbpl}"
        );
    }

    #[test]
    fn restricted_allows_dns_resolution() {
        // Without the mDNSResponder socket allowance the `(deny network-outbound)`
        // baseline blocks macOS name resolution, so no external host resolves
        // and only loopback IPs work. Regression guard for that gap.
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![443]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains(
                "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))"
            ),
            "restricted profile must permit DNS via the mDNSResponder socket:\n{sbpl}"
        );
    }

    #[test]
    fn off_mode_still_blocks_dns() {
        // Off (deny-all) must NOT get the DNS exception — it stays air-gapped.
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            !sbpl.contains("mDNSResponder"),
            "Off mode must not allow DNS:\n{sbpl}"
        );
    }

    #[test]
    fn unrestricted_emits_no_network_rules() {
        let policy = policy_with(vec![], vec![]); // net_allow_ports defaults to None
        let sbpl = build_sbpl_profile(&policy);
        assert!(!sbpl.contains("network-outbound"));
    }
}
