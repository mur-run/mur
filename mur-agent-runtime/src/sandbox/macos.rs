use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use mur_common::agent::SpawnMode;
use std::ffi::CString;
use std::path::PathBuf;

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

/// Standard macOS binary locations a process must be able to exec in order
/// to run the shell interpreter and coreutils (per-agent spawn allowlists
/// enumerate arbitrary tool binaries, but the shell itself and basic
/// coreutils it relies on live here). Mirrors `system_exec_paths` in
/// `policy.rs`'s narrower set — this is deliberately the coarse system
/// locations only; anything else must be explicitly allowlisted via
/// `spawn_allowed_paths`.
///
/// **Exemption semantic (intentional, not a bug) — `Allowlist`/`None` only:**
/// under `SpawnMode::Allowlist` (and the exec-deny baseline `None` shares),
/// every binary under these three roots — `mkdir`, `cat`, `cp`, `git`,
/// etc. — is exec'able regardless of `spawn_allowed_paths`, because the
/// shell tool (`bash`) needs a working coreutils environment to be usable
/// at all (see `hooks/b0.rs`'s coarse spawn gate, which defers per-binary
/// enforcement to this layer). That allowlist therefore bounds the real
/// threat surface — downloaded, Homebrew-installed, and project-local
/// binaries outside the system roots — not an executable already trusted
/// enough to ship with the OS.
///
/// `SpawnMode::Strict` does NOT get this exemption (see `build_sbpl_profile`):
/// it skips this whole path list, so only the resolved shell binary the
/// `bash` tool spawns (auto-seeded into `spawn_allowed_paths` by
/// `SandboxPolicy::from_entitlements`) plus the profile's own
/// `spawn_allowed_paths`/`spawn_allowed_prefixes` remain exec'able — no
/// other system binary, including coreutils, is implied.
const MACOS_SYSTEM_EXEC_PATHS: &[&str] = &["/bin", "/usr/bin", "/usr/lib"];

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

/// Resolve MUR_HOME the same way the supervisor does (`supervisor.rs`'s
/// startup resolution): the `MUR_HOME` env var if set, else `$HOME/.mur`.
/// Needed so the per-agent `agents/` directory (peer `agent.sock` files,
/// dialed for A2A) can be subpath-allowed for unix-socket network-outbound
/// below — mirrors how `SandboxPolicy::from_entitlements` derives home-based
/// paths, but falls back to `/tmp` instead of panicking (this runs inside
/// the already-sandboxing process, where a hard `.expect` would be fatal).
fn resolved_mur_home() -> PathBuf {
    std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".mur")
        })
}

/// AF_UNIX socket paths the restricted network profile must not blanket-deny.
/// Unix-socket `connect` is itself a `network-outbound` operation under SBPL,
/// so the `(deny network-outbound)` baseline (see below) would otherwise
/// reject any process- or test-owned domain socket. Scoped to: the macOS
/// per-user temp root and `/tmp` (where test/tool sockets land — same dirs
/// `system_read_paths` re-allows for reads in `policy.rs`), and this agent's
/// own `<mur_home>/agents` directory (peer `agent.sock` files dialed for
/// A2A). Subpaths, not path-literals, since exact socket filenames vary.
fn unix_socket_allow_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/private/var/folders"),
        PathBuf::from("/private/tmp"),
        resolved_mur_home().join("agents"),
    ]
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

    // Re-allow writes for the standard macOS system-write locations so the
    // runtime (dyld, temp, stdio) keeps working under the deny baseline.
    for p in MACOS_SYSTEM_WRITE_PATHS {
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Re-allow writes to the policy's explicitly allowed write paths. These
    // come after the baseline so they win the last-match-wins evaluation.
    for path in &policy.fs_write {
        let p = sbpl_escape(&path.to_string_lossy());
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Deny reads (and keep an explicit write-deny) on sensitive paths.
    // Emitted AFTER the write allows: SBPL is last-match-wins, so a denied
    // path nested inside a granted write subtree (e.g. the agent's own
    // profile.yaml under agent_home — issue #712) only stays denied if the
    // deny comes later. fs_deny overriding fs_read/fs_write is the field's
    // documented contract (see `SandboxPolicy::fs_deny`).
    for path in &policy.fs_deny {
        let p = sbpl_escape(&path.to_string_lossy());
        lines.push(format!("(deny file-read* (subpath \"{p}\"))"));
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
    }

    // Process-exec restrictions. Under `(allow default)` any binary is
    // spawnable unless explicitly denied. When spawn_mode is not `Any`
    // (i.e. Allowlist, None, or Strict), deny all exec by default and
    // re-allow: the policy's own fs_exec dirs, each individually-resolved
    // spawn_allowed_paths binary by exact path-literal, and
    // spawn_allowed_prefixes subpaths. `Allowlist` and `None` additionally
    // re-allow the standard system exec locations (shell interpreter +
    // coreutils, needed for MCP spawn / shell tools to keep functioning) --
    // `Strict` deliberately skips that exemption (see the
    // `MACOS_SYSTEM_EXEC_PATHS` doc comment): only the shell binary
    // auto-seeded into `spawn_allowed_paths` by
    // `SandboxPolicy::from_entitlements` plus the profile's own allowlist
    // remain exec'able. `Any` emits no exec clauses at all, falling through
    // to the top-level `(allow default)`.
    if policy.spawn_mode != SpawnMode::Any {
        lines.push("(deny process-exec* (subpath \"/\"))".to_string());

        if policy.spawn_mode != SpawnMode::Strict {
            for p in MACOS_SYSTEM_EXEC_PATHS {
                lines.push(format!("(allow process-exec* (subpath \"{p}\"))"));
            }
        }

        for path in &policy.fs_exec {
            let p = sbpl_escape(&path.to_string_lossy());
            lines.push(format!("(allow process-exec* (subpath \"{p}\"))"));
        }

        for path in &policy.spawn_allowed_paths {
            let p = sbpl_escape(&path.to_string_lossy());
            lines.push(format!("(allow process-exec* (path-literal \"{p}\"))"));
        }

        // Issue 17: a resolved spawn_allowed_paths literal is often just one
        // entry point into a directory tree of siblings the tool needs at
        // runtime (rustup toolchain `bin/` siblings like `cargo`/`rustc`
        // invoked via shims, a Homebrew keg's `libexec/git-core` helpers, or
        // the Xcode Command Line Tools `usr/bin` tree). spawn_allowed_prefixes
        // grants exec on the whole containing directory (package/toolchain
        // root, or immediate parent when that root would be too broad) so
        // those siblings work without allowlisting every binary individually.
        for path in &policy.spawn_allowed_prefixes {
            let p = sbpl_escape(&path.to_string_lossy());
            lines.push(format!("(allow process-exec* (subpath \"{p}\"))"));
        }
    }

    // Network restrictions. NOTE: macOS SBPL `remote tcp` only accepts `*` or
    // `localhost` as the host — a hostname like "api.anthropic.com:443" is a
    // hard parse error that fails sandbox_init for the WHOLE profile (silent
    // fail-open). So we restrict by PORT here (host `*`) and delegate hostname
    // allowlisting to the HostGuard reqwest layer.
    //
    // macOS resolves hostnames by talking to mDNSResponder over this UNIX
    // socket, which is itself a `network-outbound` op. Under a blanket deny it
    // must be re-allowed or ALL name resolution fails — every external host
    // errors with "Could not resolve host", so only loopback IPs (which need
    // no DNS) are reachable. HostGuard still gates which hostnames the
    // runtime client resolves; this restores resolution only, not arbitrary
    // egress (TCP stays port-gated). Hoisted so both the empty-ports
    // (ProxyOnly/Off) and non-empty arms below can reference it.
    const MDNSRESPONDER_SOCKET: &str = "/private/var/run/mDNSResponder";
    match &policy.net_allow_ports {
        None => { /* Unrestricted: (allow default) covers outbound. */ }
        Some(ports) if ports.is_empty() => {
            lines.push("(deny network-outbound)".to_string());
            // ProxyOnly / loopback-only: no general `*:port`, but still allow
            // name resolution + the loopback carve-outs (cc-proxy LLM + egress
            // proxy) so the worker can reach its proxies. Mirrors the loopback
            // part of the `Some(non-empty)` arm below. When there are no
            // loopback ports either, this is true Off: deny all outbound TCP.
            if !policy.net_allow_loopback_ports.is_empty() {
                lines.push(format!(
                    "(allow network-outbound (remote unix-socket (path-literal \"{MDNSRESPONDER_SOCKET}\")))"
                ));
                for p in unix_socket_allow_paths() {
                    let p = sbpl_escape(&p.to_string_lossy());
                    lines.push(format!(
                        "(allow network-outbound (remote unix-socket (subpath \"{p}\")))"
                    ));
                }
                for port in &policy.net_allow_loopback_ports {
                    lines.push(format!(
                        "(allow network-outbound (remote tcp \"localhost:{port}\"))"
                    ));
                }
            }
        }
        Some(ports) => {
            lines.push("(deny network-outbound)".to_string());
            lines.push(format!(
                "(allow network-outbound (remote unix-socket (path-literal \"{MDNSRESPONDER_SOCKET}\")))"
            ));
            // Scoped AF_UNIX carve-out (see unix_socket_allow_paths doc): test/tool
            // sockets under the temp dirs, and peer agent.sock dialing under
            // <mur_home>/agents. Does NOT widen general network-outbound access —
            // TCP stays port-gated by the loop below.
            for p in unix_socket_allow_paths() {
                let p = sbpl_escape(&p.to_string_lossy());
                lines.push(format!(
                    "(allow network-outbound (remote unix-socket (subpath \"{p}\")))"
                ));
            }
            for port in ports {
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"*:{port}\"))"
                ));
            }
            // Loopback-only carve-outs (egress proxy listener): SBPL's
            // `remote tcp` accepts `localhost` as the host, so this does NOT
            // widen general egress — only dials to 127.0.0.1/::1 on the port.
            for port in &policy.net_allow_loopback_ports {
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"localhost:{port}\"))"
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
    fn deny_path_wins_over_overlapping_write_grant() {
        // Issue #712: a denied file nested inside a granted write subtree
        // (e.g. the agent's own profile.yaml under agent_home) must be
        // emitted AFTER the allow so it wins the last-match-wins evaluation.
        let sbpl = build_sbpl_profile(&policy_with(
            vec![PathBuf::from("/data/agent")],
            vec![PathBuf::from("/data/agent/profile.yaml")],
        ));
        let allow = sbpl
            .find("(allow file-write* (subpath \"/data/agent\"))")
            .expect("write grant must be emitted");
        let deny = sbpl
            .find("(deny file-write* (subpath \"/data/agent/profile.yaml\"))")
            .expect("deny must be emitted");
        assert!(
            deny > allow,
            "deny must follow the overlapping allow (last-match-wins):\n{sbpl}"
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

    #[test]
    fn restricted_allows_scoped_unix_sockets() {
        // AF_UNIX connect is itself `network-outbound` under SBPL, so the
        // `(deny network-outbound)` baseline would otherwise break any
        // domain socket (test sockets, peer agent.sock dialing). These three
        // subpath carve-outs must be present without widening general
        // network access (TCP stays port-gated — see the next test).
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![443]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains(
                "(allow network-outbound (remote unix-socket (subpath \"/private/var/folders\")))"
            ),
            "restricted profile must allow unix sockets under macOS per-user temp:\n{sbpl}"
        );
        assert!(
            sbpl.contains(
                "(allow network-outbound (remote unix-socket (subpath \"/private/tmp\")))"
            ),
            "restricted profile must allow unix sockets under /private/tmp:\n{sbpl}"
        );
        let agents_dir = resolved_mur_home().join("agents");
        let agents_dir = sbpl_escape(&agents_dir.to_string_lossy());
        assert!(
            sbpl.contains(&format!(
                "(allow network-outbound (remote unix-socket (subpath \"{agents_dir}\")))"
            )),
            "restricted profile must allow unix sockets under <mur_home>/agents (A2A agent.sock):\n{sbpl}"
        );
    }

    #[test]
    fn off_mode_does_not_allow_unix_sockets() {
        // Off (deny-all) must not get the scoped AF_UNIX carve-out either —
        // it stays fully air-gapped, matching off_mode_denies_network.
        let mut policy = policy_with(vec![], vec![]);
        policy.net_allow_ports = Some(vec![]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            !sbpl.contains("(allow network-outbound"),
            "Off mode must not allow any outbound, including unix sockets:\n{sbpl}"
        );
    }

    #[test]
    fn allowlist_mode_emits_exec_deny_baseline_and_system_reallows() {
        let mut policy = policy_with(vec![], vec![]);
        policy.spawn_mode = SpawnMode::Allowlist;
        policy.spawn_allowed_paths = vec![PathBuf::from("/usr/bin/env")];
        let sbpl = build_sbpl_profile(&policy);

        assert!(
            sbpl.contains("(deny process-exec* (subpath \"/\"))"),
            "missing default-deny-exec baseline:\n{sbpl}"
        );
        for p in MACOS_SYSTEM_EXEC_PATHS {
            assert!(
                sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
                "missing system exec re-allow for {p}:\n{sbpl}"
            );
        }
        for p in &policy.fs_exec {
            let p = sbpl_escape(&p.to_string_lossy());
            assert!(
                sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
                "missing fs_exec re-allow for {p}:\n{sbpl}"
            );
        }
        assert!(
            sbpl.contains("(allow process-exec* (path-literal \"/usr/bin/env\"))"),
            "missing path-literal allow for the resolved spawn binary:\n{sbpl}"
        );
    }

    #[test]
    fn any_mode_emits_no_process_exec_clauses() {
        let mut policy = policy_with(vec![], vec![]);
        policy.spawn_mode = SpawnMode::Any;
        policy.spawn_allowed_paths = vec![PathBuf::from("/usr/bin/env")];
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            !sbpl.contains("process-exec*"),
            "Any mode must fall through to the top-level allow default, no exec clauses:\n{sbpl}"
        );
    }

    #[test]
    fn strict_mode_denies_system_exec_paths_but_allows_shell_literal() {
        let mut policy = policy_with(vec![], vec![]);
        policy.spawn_mode = SpawnMode::Strict;
        // Simulate what `SandboxPolicy::from_entitlements` would have
        // produced: the auto-seeded shell literal plus a fake
        // profile-declared allowlist entry and prefix.
        policy.spawn_allowed_paths =
            vec![PathBuf::from("/bin/bash"), PathBuf::from("/opt/fake/tool")];
        policy.spawn_allowed_prefixes = vec![PathBuf::from("/opt/fake")];
        let sbpl = build_sbpl_profile(&policy);

        assert!(
            sbpl.contains("(deny process-exec* (subpath \"/\"))"),
            "missing default-deny-exec baseline:\n{sbpl}"
        );
        for p in MACOS_SYSTEM_EXEC_PATHS {
            assert!(
                !sbpl.contains(&format!("(allow process-exec* (subpath \"{p}\"))")),
                "strict mode must NOT re-allow system exec path {p}:\n{sbpl}"
            );
        }
        assert!(
            sbpl.contains("(allow process-exec* (path-literal \"/bin/bash\"))"),
            "missing path-literal allow for the auto-seeded shell binary:\n{sbpl}"
        );
        assert!(
            sbpl.contains("(allow process-exec* (path-literal \"/opt/fake/tool\"))"),
            "missing path-literal allow for the profile's own spawn_allowed_paths entry:\n{sbpl}"
        );
        assert!(
            sbpl.contains("(allow process-exec* (subpath \"/opt/fake\"))"),
            "missing subpath allow for spawn_allowed_prefixes:\n{sbpl}"
        );
    }

    #[test]
    fn loopback_port_carveout_is_localhost_scoped() {
        let mut policy = SandboxPolicy {
            net_allow_ports: Some(vec![80, 443]),
            ..Default::default()
        };
        policy.allow_loopback_ports(&[54321]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains("(allow network-outbound (remote tcp \"localhost:54321\"))"),
            "proxy port must be loopback-scoped: {sbpl}"
        );
        assert!(
            !sbpl.contains("\"*:54321\""),
            "proxy port must NOT be wildcard-host: {sbpl}"
        );
    }

    #[test]
    fn restricted_loopback_only_policy_has_no_wildcard_tcp_allow() {
        // A worker whose egress is ONLY via loopback proxy: deny all general TCP outbound
        // and rely on loopback-only access. The airtight guarantee assumes no general
        // `(remote tcp "*:PORT")` allow exists (that would be a direct-egress escape hatch).
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(Vec::new()); // deny all general TCP outbound
        policy.net_allow_loopback_ports = vec![58999];
        let sbpl = build_sbpl_profile(&policy);

        // When all general TCP is denied, the deny network-outbound is present.
        assert!(sbpl.contains("(deny network-outbound)"));
        // The critical invariant: NO wildcard-host TCP allow (the escape hatch).
        assert!(
            !sbpl.contains("(remote tcp \"*:"),
            "restricted worker must not emit a wildcard-host tcp allow:\n{sbpl}"
        );
    }

    #[test]
    fn proxy_only_sbpl_allows_loopback_and_dns_but_no_wildcard() {
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(Vec::new()); // deny general TCP
        policy.net_allow_loopback_ports = vec![8088, 54321]; // cc-proxy + egress proxy
        let sbpl = build_sbpl_profile(&policy);

        assert!(sbpl.contains("(deny network-outbound)"));
        // loopback carve-outs present…
        assert!(sbpl.contains("(remote tcp \"localhost:8088\")"));
        assert!(sbpl.contains("(remote tcp \"localhost:54321\")"));
        // …name resolution restored (loopback host resolution)…
        assert!(sbpl.contains("/private/var/run/mDNSResponder"));
        // …and NO wildcard-host tcp allow (the escape hatch).
        assert!(
            !sbpl.contains("(remote tcp \"*:"),
            "no wildcard tcp allow:\n{sbpl}"
        );
    }
}
