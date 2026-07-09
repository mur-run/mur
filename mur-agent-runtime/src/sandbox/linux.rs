use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use landlock::{
    ABI, Access, AccessFs, AccessNet, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, make_bitflags, path_beneath_rules,
};

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
    if !policy.fs_read.is_empty() {
        let read_rules = path_beneath_rules(policy.fs_read.iter(), AccessFs::from_read(abi));
        created = created.add_rules(read_rules).context("add fs_read rules")?;
    }

    // FS read+write paths (superset of read).
    if !policy.fs_write.is_empty() {
        let write_rules = path_beneath_rules(policy.fs_write.iter(), AccessFs::from_all(abi));
        created = created
            .add_rules(write_rules)
            .context("add fs_write rules")?;
    }

    // FS exec paths (execute + read file + read dir).
    if !policy.fs_exec.is_empty() {
        let exec_access = make_bitflags!(AccessFs::{ Execute | ReadFile | ReadDir });
        let exec_rules = path_beneath_rules(policy.fs_exec.iter(), exec_access);
        created = created.add_rules(exec_rules).context("add fs_exec rules")?;
    }

    // Network port rules — only when the mode restricts outbound TCP.
    // An empty `ports` list (mode = Off) means no ConnectTcp rules are added,
    // which blocks all outbound TCP (Landlock default-deny for handled accesses).
    if let Some(ports) = &policy.net_allow_ports {
        for port in ports.iter().chain(policy.net_allow_loopback_ports.iter()) {
            created = created
                .add_rule(NetPort::new(*port, AccessNet::ConnectTcp))
                .context("add net port rule")?;
        }
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
    })
}

/// Install a seccomp BPF denylist for syscalls that have no legitimate use in an agent process.
/// Default action is Allow; the listed syscalls return EPERM.
///
/// To deny a syscall unconditionally (no condition matching), map it to an EMPTY
/// `Vec<SeccompRule>`. Mapping to a non-empty vec means "deny when a rule matches";
/// mapping to `vec![]` means "always deny" (matches regardless of arguments).
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
