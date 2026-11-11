use crate::sandbox::launch_chain::LaunchChain;
use std::path::PathBuf;

// The Landlock/seccomp application path is Linux-only; the module compiles
// everywhere so `partition_write_grants` (shared with sandbox::policy, and tested on
// macOS) is not cfg-gated.
#[cfg(target_os = "linux")]
use super::{SandboxPolicy, SandboxStatus};
#[cfg(target_os = "linux")]
use anyhow::Context;
#[cfg(target_os = "linux")]
use landlock::{
    ABI, Access, AccessFs, AccessNet, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, make_bitflags, path_beneath_rules,
};

#[cfg(target_os = "linux")]
pub fn apply_linux(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let abi = ABI::V4;

    // Build the FS portion of the ruleset (always enabled).
    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .context("handle AccessFs")?;

    // Only install Landlock network rules when we need to restrict outbound TCP.
    // CRITICAL: calling handle_access(AccessNet::from_all(abi)) with ZERO NetPort rules
    // blocks ALL outbound TCP — so we must guard this on whether ports are restricted.
    if policy.net_allow_ports.is_some() {
        ruleset = ruleset
            .handle_access(AccessNet::from_all(abi))
            .context("handle AccessNet")?;
    }

    let mut created = ruleset.create().context("create Landlock ruleset")?;

    // FS read-only paths. path_beneath_rules() silently skips non-existent paths.
    // Already partitioned at profile-build time against the credential store
    // and sibling signing keys (#850) — and partitioned THERE rather than here
    // because only the user-declared grants may be dropped; the system read
    // paths in this list are chosen by the builder and must always survive.
    //
    // Every rule source below carries `Execute` (`from_read` and `from_all`
    // include it), and Landlock grants are additive, so each one goes through
    // the same `mur` CLI carve-out (#2317) — excluding it from `fs_exec` alone
    // would leave a read or write grant above it still executable.
    let deny = policy.landlock_exec_denies();
    // Strict (#2343): read and write grants carry no `Execute` at all, so
    // the only exec rules are `exec_grants()` below — otherwise a readable
    // `/usr` or `/lib`, or a writable project dir, would stay executable.
    let rw_mask = if policy.landlock_read_write_exec() {
        AccessFs::from_all(abi)
    } else {
        AccessFs::from_all(abi) & !AccessFs::Execute
    };
    created = add_exec_split_rules(
        created,
        &policy.fs_read,
        AccessFs::from_read(abi) & rw_mask,
        &deny,
        "fs_read",
    )?;

    // FS read+write paths (superset of read). Landlock is a pure allow-list,
    // so a grant that contains a protected launch-chain path cannot be carved
    // (no deny rule exists to re-close it) — it is dropped whole, fail-closed.
    // `policy.dropped_grants` already recorded the same computation at profile
    // build time; this local recomputation also covers paths added after that
    // (e.g. the media-state extra grants) and keeps the kernel and the report
    // reading from the same code path.
    let (writable, _dropped) = partition_write_grants(&policy.fs_write, &policy.launch_chain);
    created = add_exec_split_rules(created, &writable, rw_mask, &deny, "fs_write")?;

    // FS exec paths (execute + read file + read dir). Under Strict this is
    // the resolved literals and prefixes, not the system exec dirs (#2343).
    created = add_exec_split_rules(
        created,
        &policy.exec_grants(),
        make_bitflags!(AccessFs::{ Execute | ReadFile | ReadDir }),
        &deny,
        "fs_exec",
    )?;

    // Network port rules — only when the mode restricts outbound TCP. The
    // helper merges the general allow-list with the loopback carve-outs
    // (see policy::connect_tcp_ports). Off/Unrestricted → empty → no rules
    // (Landlock default-deny for handled accesses).
    for port in super::policy::connect_tcp_ports(policy) {
        created = created
            .add_rule(NetPort::new(port, AccessNet::ConnectTcp))
            .context("add net port rule")?;
    }

    let status = created.restrict_self().context("restrict_self")?;

    // Apply the seccomp BPF denylist for high-risk syscalls.
    apply_seccomp_denylist()?;

    let enforcing = matches!(
        status.ruleset,
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced
    );

    Ok(SandboxStatus {
        platform: "linux-landlock-v4".to_string(),
        effective_abi: None, // landlock 0.4 does not expose effective ABI in RestrictionStatus
        enforcing,
        dropped: Vec::new(),
    })
}

/// Install one rule source with `access`, except that a grant above a denied
/// `mur` CLI file gets `access` minus `Execute`, and `Execute` alone goes to
/// that grant carved around the denied files. See
/// [`super::policy::split_exec_grants`]. When `access` has no `Execute` there
/// is nothing to carve and the grants go in unsplit.
#[cfg(target_os = "linux")]
fn add_exec_split_rules(
    mut created: landlock::RulesetCreated,
    grants: &[PathBuf],
    access: landlock::BitFlags<AccessFs>,
    deny: &[PathBuf],
    what: &str,
) -> anyhow::Result<landlock::RulesetCreated> {
    // No `Execute` to carve (a Strict read/write grant, #2343): install the
    // grants as-is. Splitting would hand `exec_only` an `Execute` the caller
    // never asked for, reopening every sibling of the denied file.
    if !access.contains(AccessFs::Execute) {
        if grants.is_empty() || access.is_empty() {
            return Ok(created);
        }
        return created
            .add_rules(path_beneath_rules(grants.iter(), access))
            .with_context(|| format!("add {what} rules"));
    }
    let split = super::policy::split_exec_grants(grants, deny);
    for (paths, access) in [
        (&split.clean, access),
        (&split.no_exec, access & !AccessFs::Execute),
        (&split.exec_only, AccessFs::Execute.into()),
    ] {
        if paths.is_empty() || access.is_empty() {
            continue;
        }
        created = created
            .add_rules(path_beneath_rules(paths.iter(), access))
            .with_context(|| format!("add {what} rules"))?;
    }
    Ok(created)
}

/// Install a seccomp BPF denylist for syscalls that have no legitimate use in an agent process.
/// Default action is Allow; the listed syscalls return EPERM.
///
/// To deny a syscall unconditionally (no condition matching), map it to an EMPTY
/// `Vec<SeccompRule>`. Mapping to a non-empty vec means "deny when a rule matches";
/// mapping to `vec![]` means "always deny" (matches regardless of arguments).
#[cfg(target_os = "linux")]
fn apply_seccomp_denylist() -> anyhow::Result<()> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};
    use std::collections::BTreeMap;

    let arch = if cfg!(target_arch = "x86_64") {
        TargetArch::x86_64
    } else if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        // Unsupported arch — skip seccomp silently.  Landlock still applies.
        return Ok(());
    };

    let deny_action = SeccompAction::Errno(libc::EPERM as u32);

    // Map each denied syscall number to an empty rule vector.
    // An empty rule vector means the syscall always triggers the match_action (deny).
    // i64::from() widens c_long (i32 on 32-bit, i64 on 64-bit) to i64.
    // On x86_64 the conversion is a no-op; allow the lint for cross-arch portability.
    #[allow(clippy::useless_conversion)]
    let syscalls: &[i64] = &[
        i64::from(libc::SYS_ptrace),
        i64::from(libc::SYS_mount),
        i64::from(libc::SYS_kexec_load),
        i64::from(libc::SYS_bpf),
        i64::from(libc::SYS_unshare),
        i64::from(libc::SYS_pivot_root),
    ];

    let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
        syscalls.iter().map(|&nr| (nr, vec![])).collect();

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow, // mismatch_action: allow all unmatched syscalls
        deny_action,          // match_action: deny the listed syscalls
        arch,
    )
    .context("build seccomp filter")?;

    let prog: BpfProgram = filter.try_into().context("compile BPF program")?;
    seccompiler::apply_filter(&prog).context("apply seccomp filter")?;

    Ok(())
}

/// Split write grants into those Landlock can safely install and those it
/// cannot. Landlock has no deny rule, so a grant that contains a protected
/// path cannot be carved — it is dropped whole. Fail-closed on purpose: the
/// alternative is installing a rule that hands over the launch chain.
///
pub(crate) fn partition_write_grants(
    grants: &[PathBuf],
    chain: &LaunchChain,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    chain.partition_grants(grants)
}

/// Read counterpart of [`partition_write_grants`]. Same shape, different
/// protected set — see `LaunchChain::partition_read_grants`.
pub(crate) fn partition_read_grants(
    grants: &[PathBuf],
    chain: &LaunchChain,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    chain.partition_read_grants(grants)
}

#[cfg(all(test, target_os = "linux"))]
mod strict_exec_probe;
#[cfg(all(test, target_os = "linux"))]
mod unix_connect_probe;

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(tmp: &std::path::Path) -> LaunchChain {
        LaunchChain::for_test(
            &tmp.join("agents").join("mur"),
            &tmp.join("bin"),
            &tmp.join("home"),
        )
    }

    #[test]
    fn a_write_grant_containing_a_protected_path_is_dropped_not_carved() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents/mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let skills = mur_home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();

        let (kept, dropped) = partition_write_grants(
            &[mur_home.to_path_buf(), skills.clone()],
            &chain(tmp.path()),
        );

        // `<mur_home>` contains `<mur_home>/agents`, and Landlock cannot carve it out.
        assert_eq!(dropped, vec![mur_home.to_path_buf()]);
        // Negative control: a grant that contains nothing protected survives intact.
        assert_eq!(kept, vec![skills]);
    }

    #[test]
    fn a_write_grant_on_a_sibling_agent_dir_is_dropped_but_own_home_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents/mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let skills = mur_home.join("skills");
        std::fs::create_dir_all(&skills).unwrap();

        let (kept, dropped) = partition_write_grants(
            &[
                mur_home.join("agents/pm"),
                agent_home.clone(),
                skills.clone(),
            ],
            &chain(tmp.path()),
        );

        // A sibling's home is another agent's launch entry and cannot be
        // carved — dropped whole.
        assert_eq!(dropped, vec![mur_home.join("agents/pm")]);
        // Negative controls: the own-home force-grant and an unrelated grant
        // survive, so the drop is the launch chain and not a blanket prune.
        assert_eq!(kept, vec![agent_home, skills]);
    }
}
