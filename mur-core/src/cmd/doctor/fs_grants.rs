//! Filesystem-grant checks for `mur agent doctor`: grants the sandbox drops
//! because the path is missing, or whole because they overlap the launch chain,
//! plus the empty-deny-list advisory.

use super::Check;

/// Report filesystem grants the kernel will silently drop.
///
/// A filesystem entitlement only reaches the sandbox if its path EXISTS when
/// the agent starts. A grant for a path that is not there is accepted by the
/// profile, dropped when the policy is sealed, and then shows up as a bare
/// errno in some later, unrelated operation — `perm show` lists it, so the
/// profile and the kernel disagree with nobody to say so.
///
/// `perm allow-read` / `allow-write` refuse a missing path at grant time, but
/// that cannot help a grant made before the check existed, or one whose
/// directory was removed afterwards. This is the reporting half.
///
/// `deny` entries are not checked: denying a path that does not exist costs
/// nothing and is a reasonable thing to write ahead of time.
/// Report write grants the sandbox drops WHOLE for overlapping the launch chain.
///
/// Distinct from `check_filesystem_entitlements`, which reports grants dropped
/// because their path does not exist. This one is about grants whose path is
/// fine but whose SCOPE is not: Landlock is a pure allow-list, so a protected
/// path inside a grant cannot be carved out — the grant is dropped entire,
/// fail-closed.
///
/// `SandboxPolicy::dropped_grants` has recorded exactly this since the launch
/// chain landed, with a comment saying it is "recorded for the runtime-doctor".
/// Nothing ever read it. So an agent could be granted a directory, silently
/// lose the whole grant on Linux, and find out from an errno.
pub(super) fn check_dropped_launch_chain_grants(
    fs: &mur_common::agent::FilesystemEntitlement,
    agent_home: &std::path::Path,
) -> Check {
    let chain = mur_agent_runtime::sandbox::launch_chain::LaunchChain::new(agent_home);
    let expand = |v: &Vec<String>| -> Vec<std::path::PathBuf> {
        v.iter()
            .map(|s| mur_agent_runtime::sandbox::policy::expand_entitlement_path(s))
            .collect()
    };
    let writes = expand(&fs.write);
    let reads = expand(&fs.read);
    let (_kept, dropped) = chain.partition_grants(&writes);
    // Reads are partitioned against a DIFFERENT protected set (the credential
    // store and sibling signing keys, not the launch chain), so a path may be
    // fine to write-grant and refused to read-grant. Reported together because
    // the user-visible consequence is identical: the grant silently vanishes.
    let (_kept_r, dropped_reads) = chain.partition_read_grants(&reads);

    let (ok, detail) = grant_scope_verdict(
        &dropped,
        &dropped_reads,
        (writes.len(), reads.len()),
        cfg!(target_os = "linux"),
    );
    Check::new("grant_scope", ok, detail)
}

/// Verdict and wording for overlapping grants.
///
/// `landlock` is the platform difference, taken as a parameter rather than read
/// from `cfg!` so both answers are reachable from a test on either host. The
/// bug this replaces was a single sentence asserting Linux's behaviour
/// everywhere, and no test could see it from macOS.
pub(super) fn grant_scope_verdict(
    dropped: &[std::path::PathBuf],
    dropped_reads: &[std::path::PathBuf],
    (writes, reads): (usize, usize),
    landlock: bool,
) -> (bool, String) {
    // The all-clear used to live in the caller as an early return, which left
    // this function returning `(false, "")` when handed two empty lists — a
    // failed check with nothing written beside it. Unreachable through that
    // caller, and exactly the blank #1095 removed from schedules: a verdict
    // must always arrive with its reason. Owning the whole decision here means
    // there is no invariant to enforce elsewhere and forget (#1105).
    if dropped.is_empty() && dropped_reads.is_empty() {
        return (
            true,
            format!("{writes} write + {reads} read grant(s), none overlap a protected path"),
        );
    }
    let mut parts: Vec<String> = Vec::new();
    if !dropped.is_empty() {
        let names: Vec<String> = dropped.iter().map(|p| p.display().to_string()).collect();
        // The two halves do not share a fate. Landlock has no deny rule, so it
        // discards an overlapping write grant whole; macOS installs the grant
        // and re-closes the launch-chain paths inside it with `deny` clauses
        // emitted after the allows. Telling a macOS user their grant was
        // dropped sends them to narrow one that is working.
        parts.push(if landlock {
            format!(
                "{} write grant(s) contain a launch-chain path and are DROPPED WHOLE (Landlock \
                 cannot carve one out) — grant the specific subdirectory instead: {}",
                dropped.len(),
                names.join(", ")
            )
        } else {
            format!(
                "{} write grant(s) contain a launch-chain path, so those paths stay denied inside \
                 them and the grant covers less than it reads as: {}",
                dropped.len(),
                names.join(", ")
            )
        });
    }
    if !dropped_reads.is_empty() {
        let names: Vec<String> = dropped_reads
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        // The read half IS dropped whole on both platforms: `from_entitlements`
        // keeps only the partition's surviving reads, unlike the write side
        // where the kept set is discarded.
        parts.push(format!(
            "{} read grant(s) contain the credential store or a sibling signing key and are \
             DROPPED WHOLE: {}",
            dropped_reads.len(),
            names.join(", ")
        ));
    }
    // A write-only overlap is not a failure where the grant is installed.
    // Reporting it as one is how a check earns its way into the part of the
    // screen people stop reading.
    // Reached only with something dropped, so this says what it means: a read
    // overlap fails everywhere, a write overlap fails only where Landlock
    // discards it.
    let ok = dropped_reads.is_empty() && !landlock;
    (ok, parts.join("; "))
}

pub(super) fn check_filesystem_entitlements(
    fs: &mur_common::agent::FilesystemEntitlement,
) -> Check {
    let mut missing: Vec<String> = Vec::new();
    let mut granted = 0usize;

    for (kind, paths) in [("read", &fs.read), ("write", &fs.write)] {
        for raw in paths {
            granted += 1;
            let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw);
            // `metadata`, which FOLLOWS symlinks — the same call
            // `reject_dead_grant` uses at grant time (`cmd/agent/perm.rs`).
            // The two halves must agree on what "exists" means, or `perm`
            // accepts a path this then reports as broken. Following also
            // catches a dangling symlink, whose link exists but whose target
            // does not, and which the sandbox can no more grant than a path
            // that was never there.
            if std::fs::metadata(&p).is_err() {
                missing.push(format!("{kind} {}", p.display()));
            }
        }
    }

    if missing.is_empty() {
        return Check::new(
            "entitlements",
            true,
            format!("{granted} filesystem grant(s), all present"),
        );
    }
    Check::new(
        "entitlements",
        false,
        format!(
            "{} of {granted} filesystem grant(s) will be DROPPED at start — the path does not exist: {}. Create them (mkdir -p) and restart the agent.",
            missing.len(),
            missing.join(", ")
        ),
    )
}

/// Advisory note for a profile with no `filesystem.deny` entries (#1748).
///
/// Not a security gap: the launch chain hard-denies
/// [`mur_common::agent::DEFAULT_DENY_PATHS`] for every agent regardless of the
/// profile (#1747). But an empty list means nothing beyond those defaults is
/// fenced off, and the profile no longer documents what the sandbox actually
/// refuses. `ok: true` on purpose, same reasoning as `sandbox_scope`: nothing
/// is broken, and a permanent red would train users to skip the report.
pub(super) fn check_deny_list(fs: &mur_common::agent::FilesystemEntitlement, name: &str) -> Check {
    if !fs.deny.is_empty() {
        return Check::new(
            "deny_list",
            true,
            format!("{} filesystem deny entr(ies)", fs.deny.len()),
        );
    }
    let defaults = mur_common::agent::DEFAULT_DENY_PATHS;
    Check::new(
        "deny_list",
        true,
        format!(
            "advisory: filesystem.deny is empty. {} stay denied anyway (launch-chain hard \
             deny), but nothing else is fenced off. To record them in the profile: {}",
            defaults.join(", "),
            defaults
                .iter()
                .map(|d| format!("mur agent perm deny-path {name} {d}"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )
}
