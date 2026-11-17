//! Write grants that must never reach the router's signed channel heads
//! (channel-log-integrity spec decision E2, #1875, parent #1770).
//!
//! `<mur_home>/channel-heads/` holds the commitment the seal exists to
//! provide. A sandboxed process that can write there can forge or delete it,
//! and a handoff child (sandboxed, borrowing the router identity) deliberately
//! does not seal, so nothing else stands in the way.
//!
//! The check is on the finished policy, after every runtime-added grant, and
//! it is a refusal, not a clamp: a profile that reaches the heads directory
//! does not start. Decisions made where the issue is silent (flag in review):
//! * **Overlap in either direction.** A grant on a parent (`~/.mur`, `~`, `/`)
//!   covers the directory; a grant inside it (`channel-heads/x`) is write
//!   access to the protected area too.
//! * **`fs_deny` is ignored.** Landlock cannot express deny-within-allow, so a
//!   deny of the heads directory beside a broad grant is not a guarantee on
//!   Linux. Counting it on macOS only would make one profile mean two things.
//! * **Case-insensitive.** The default macOS and Windows filesystems fold
//!   case, so `~/.MUR` reaches the same directory. The cost is a false refusal
//!   for two directories that differ only by case on a case-sensitive Linux
//!   filesystem, which is a refusal and not a hole.
//! * **Not tied to `fail_closed_on_sandbox_error`.** That flag turns a failed
//!   `apply` into "run with no sandbox at all". Refusing here must not degrade
//!   into that, so [`is_refusal`] lets the caller tell this error apart.

use std::path::{Component, Path, PathBuf};

use mur_channel::anchor::HEADS_DIR;

use super::policy::SandboxPolicy;

/// A write grant that overlaps the protected heads directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "sandbox profile refused: write grant {} reaches {}, which holds the router's \
     signed channel heads (E2, #1875). Narrow the grant so it does not include \
     that directory; a deny entry does not make up for it.",
    grant.display(),
    protected.display()
)]
pub struct ProtectedPathWritable {
    pub grant: PathBuf,
    pub protected: PathBuf,
}

/// Whether `err` is a [`ProtectedPathWritable`] refusal, through any amount of
/// `anyhow` context.
pub fn is_refusal(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ProtectedPathWritable>().is_some()
}

/// Refuse a policy whose write grants overlap `<mur_home>/channel-heads`.
pub fn check_protected_writes(
    policy: &SandboxPolicy,
    mur_home: &Path,
) -> Result<(), ProtectedPathWritable> {
    let mut protected = vec![mur_home.join(HEADS_DIR)];
    // The kernel judges the resolved path, so a symlinked `mur_home` is
    // checked under both names.
    if let Ok(real) = std::fs::canonicalize(mur_home) {
        let resolved = real.join(HEADS_DIR);
        if !protected.contains(&resolved) {
            protected.push(resolved);
        }
    }
    for grant in &policy.fs_write {
        // Likewise a grant written through a symlink (macOS `/var` is
        // `/private/var`) is judged under the name the kernel will see.
        let resolved = std::fs::canonicalize(grant).ok();
        for target in &protected {
            if overlaps(grant, target) || resolved.as_deref().is_some_and(|r| overlaps(r, target)) {
                return Err(ProtectedPathWritable {
                    grant: grant.clone(),
                    protected: target.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Why the sandbox would refuse or drop `grant` as a read+write entitlement
/// for the agent rooted at `agent_home`, or `None` when it would install it.
///
/// For callers that offer a grant before the agent seals (murmur's cwd
/// consent, #2448): the same rules [`super::apply`] enforces, so a prompt can
/// never offer a path the next start refuses or silently narrows.
/// * [`check_protected_writes`] — a refusal; the agent does not start.
/// * [`LaunchChain::partition_grants`](super::launch_chain::LaunchChain::partition_grants)
///   — the write-side launch chain (pins, broker, other agents, launch
///   artifacts, autostart). Landlock drops an overlapping grant whole; SBPL
///   denies writes inside it. Judged by the Landlock rule on every platform,
///   as the read side is, so one offer means one thing everywhere.
/// * [`LaunchChain::partition_read_grants`](super::launch_chain::LaunchChain::partition_read_grants)
///   — the credential store; the whole grant is dropped.
pub fn grant_refusal(agent_home: &Path, grant: &Path) -> Option<String> {
    // Same `<mur_home>` derivation as `apply` and `policy/build.rs`.
    let mur_home = agent_home
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(mur_common::home::mur_home);
    let policy = SandboxPolicy {
        fs_write: vec![grant.to_path_buf()],
        ..SandboxPolicy::default()
    };
    if let Err(e) = check_protected_writes(&policy, &mur_home) {
        return Some(e.to_string());
    }
    let chain = super::launch_chain::LaunchChain::new(agent_home);
    let grants = [grant.to_path_buf()];
    if !chain.partition_grants(&grants).1.is_empty() {
        let why = chain
            .protects_write(grant)
            .unwrap_or("it contains a launch-chain path no agent may write");
        return Some(format!(
            "write grant {} is launch-chain protected: {why}",
            grant.display()
        ));
    }
    let (_, dropped) = chain.partition_read_grants(&grants);
    (!dropped.is_empty()).then(|| {
        format!(
            "read grant {} reaches the credential store or a sibling signing key; \
             the sandbox drops it whole",
            grant.display()
        )
    })
}

/// One path is a component-wise prefix of the other (so `channel-heads-old`
/// does not overlap `channel-heads`), compared without regard to case.
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (folded(a), folded(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn folded(p: &Path) -> Vec<String> {
    p.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement,
        NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };

    fn entitlements(write: Vec<String>, deny: Vec<String>) -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Restricted,
                    allow_hosts: vec!["api.anthropic.com".to_string()],
                    allow_ports: vec![],
                    protocols: vec!["tcp".to_string()],
                    resolve_dns: Default::default(),
                },
            },
            filesystem: FilesystemEntitlement {
                read: vec![],
                write,
                deny,
            },
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement {
                    mode: SpawnMode::Allowlist,
                    allowed: vec![],
                    allowed_dirs: vec![],
                },
            },
            syscalls: Default::default(),
            ssh: Default::default(),
            limits: Default::default(),
            llm: Default::default(),
            tools: vec![],
            fail_closed_on_sandbox_error: true,
        }
    }

    /// A fake `<mur_home>` with one agent, and the policy for `write` grants.
    struct Fixture {
        _tmp: tempfile::TempDir,
        mur_home: PathBuf,
        agent_home: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path().join(".mur");
        let agent_home = mur_home.join("agents").join("w1");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(mur_home.join("channels")).unwrap();
        Fixture {
            _tmp: tmp,
            mur_home,
            agent_home,
        }
    }

    impl Fixture {
        fn policy(&self, write: &[&Path], deny: &[&Path]) -> SandboxPolicy {
            let s = |p: &&Path| p.to_string_lossy().into_owned();
            SandboxPolicy::from_entitlements(
                &entitlements(write.iter().map(s).collect(), deny.iter().map(s).collect()),
                &self.agent_home,
            )
        }

        fn check(&self, policy: &SandboxPolicy) -> Result<(), ProtectedPathWritable> {
            check_protected_writes(policy, &self.mur_home)
        }
    }

    #[test]
    fn a_profile_with_no_extra_grants_is_accepted() {
        let f = fixture();
        assert_eq!(f.check(&f.policy(&[], &[])), Ok(()));
    }

    #[test]
    fn granting_mur_home_is_refused() {
        let f = fixture();
        let err = f.check(&f.policy(&[&f.mur_home], &[])).unwrap_err();
        assert_eq!(err.grant, f.mur_home);
        assert!(err.protected.ends_with(HEADS_DIR));
    }

    #[test]
    fn granting_a_trailing_slash_mur_home_is_refused() {
        // The real `pm` profile writes `~/.mur/`.
        let f = fixture();
        let with_slash = PathBuf::from(format!("{}/", f.mur_home.display()));
        assert!(f.check(&f.policy(&[&with_slash], &[])).is_err());
    }

    #[test]
    fn granting_a_grandparent_of_mur_home_is_refused() {
        let f = fixture();
        let home = f.mur_home.parent().unwrap();
        assert!(f.check(&f.policy(&[home], &[])).is_err());
        // The filesystem root of this platform (`/`, or `C:\` on Windows).
        let root = f.mur_home.ancestors().last().unwrap();
        assert!(f.check(&f.policy(&[root], &[])).is_err());
    }

    #[test]
    fn granting_the_heads_directory_itself_is_refused() {
        let f = fixture();
        let heads = f.mur_home.join(HEADS_DIR);
        std::fs::create_dir_all(&heads).unwrap();
        assert!(f.check(&f.policy(&[&heads], &[])).is_err());
    }

    #[test]
    fn granting_inside_the_heads_directory_is_refused() {
        let f = fixture();
        let inside = f.mur_home.join(HEADS_DIR).join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(f.check(&f.policy(&[&inside], &[])).is_err());
    }

    #[test]
    fn granting_channels_alone_is_accepted() {
        let f = fixture();
        let channels = f.mur_home.join("channels");
        assert_eq!(f.check(&f.policy(&[&channels], &[])), Ok(()));
    }

    #[test]
    fn a_sibling_that_only_shares_the_name_prefix_is_accepted() {
        let f = fixture();
        let sibling = f.mur_home.join("channel-heads-old");
        std::fs::create_dir_all(&sibling).unwrap();
        assert_eq!(f.check(&f.policy(&[&sibling], &[])), Ok(()));
    }

    #[test]
    fn a_deny_beside_a_broad_grant_does_not_excuse_it() {
        let f = fixture();
        let heads = f.mur_home.join(HEADS_DIR);
        // Build the grant directly: on Linux `from_entitlements` already drops
        // a user grant overlapping a user deny (#2160), which would hide what
        // this test pins — the check itself ignores `fs_deny`.
        let mut policy = f.policy(&[], &[&heads]);
        policy.fs_write.push(f.mur_home.clone());
        assert!(
            f.check(&policy).is_err(),
            "Landlock cannot express deny-within-allow"
        );
    }

    /// #2160: on Linux the overlapping user grant never reaches the policy, so
    /// the heads directory stays unwritable without needing the refusal.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_a_broad_grant_beside_a_heads_deny_is_dropped() {
        let f = fixture();
        let heads = f.mur_home.join(HEADS_DIR);
        let policy = f.policy(&[&f.mur_home], &[&heads]);
        assert!(!policy.fs_write.contains(&f.mur_home));
        assert!(policy.dropped.iter().any(|d| d.verb == "write"));
        assert_eq!(f.check(&policy), Ok(()));
    }

    #[test]
    fn a_case_variant_of_the_grant_is_refused() {
        let f = fixture();
        let shouted = PathBuf::from(f.mur_home.to_string_lossy().to_uppercase());
        // Build the policy directly: the upper-cased path does not exist on a
        // case-sensitive filesystem and `from_entitlements` would drop it.
        let mut policy = f.policy(&[], &[]);
        policy.fs_write.push(shouted);
        assert!(f.check(&policy).is_err());
    }

    #[test]
    fn a_runtime_added_write_path_is_judged_too() {
        let f = fixture();
        let mut policy = f.policy(&[], &[]);
        policy.allow_extra_write_paths(std::slice::from_ref(&f.mur_home));
        assert!(f.check(&policy).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_mur_home_is_checked_under_its_real_name() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-mur");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link-mur");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut policy = SandboxPolicy::from_entitlements(
            &entitlements(vec![], vec![]),
            &link.join("agents").join("w1"),
        );
        policy.fs_write.push(real.clone());
        assert!(check_protected_writes(&policy, &link).is_err());
    }

    #[test]
    fn apply_refuses_before_installing_anything_and_the_error_is_recognisable() {
        // `apply` returns on refusal before `apply_policy`, so this does not
        // seal the test process.
        let f = fixture();
        let ent = entitlements(vec![f.mur_home.to_string_lossy().into_owned()], vec![]);
        let err = crate::sandbox::apply(&ent, &f.agent_home, &[], &[], &[]).unwrap_err();
        assert!(is_refusal(&err), "{err:#}");
        assert!(is_refusal(&err.context("outer context")));
    }

    #[test]
    fn apply_judges_the_runtime_added_write_paths_not_only_the_profile() {
        // The profile is clean; the caller's extra path is what reaches the
        // heads. Checking before `allow_extra_write_paths` would pass this.
        let f = fixture();
        let ent = entitlements(vec![], vec![]);
        let extra = [f.mur_home.clone()];
        let err = crate::sandbox::apply(&ent, &f.agent_home, &[], &[], &extra).unwrap_err();
        assert!(is_refusal(&err), "{err:#}");
    }

    #[test]
    fn grant_refusal_matches_the_seal_time_write_check() {
        // #2448: the cwd prompt asks this before offering; it must refuse
        // exactly what `check_protected_writes` refuses.
        let f = fixture();
        let refused = grant_refusal(&f.agent_home, &f.mur_home).expect("mur_home is refused");
        assert!(refused.contains("channel heads"), "{refused}");
        assert!(f.check(&f.policy(&[&f.mur_home], &[])).is_err());
        let heads = f.mur_home.join(HEADS_DIR);
        assert!(grant_refusal(&f.agent_home, &heads).is_some());
    }

    #[test]
    fn grant_refusal_accepts_a_narrow_dir_under_mur_home() {
        let f = fixture();
        let narrow = f.mur_home.join("artifacts").join("mur").join("run");
        std::fs::create_dir_all(&narrow).unwrap();
        assert_eq!(grant_refusal(&f.agent_home, &narrow), None);
        assert!(f.check(&f.policy(&[&narrow], &[])).is_ok());
    }

    #[test]
    fn grant_refusal_refuses_a_write_protected_but_readable_dir() {
        // #2452 review: the entitlement pins pass both the heads check and the
        // read partition, yet the write partition drops them (Linux) and SBPL
        // denies writes there (macOS). The prompt must not offer read+write.
        let f = fixture();
        let pins = f.mur_home.join(mur_common::entitlements_pin::PINS_DIR);
        std::fs::create_dir_all(&pins).unwrap();
        assert!(f.check(&f.policy(&[&pins], &[])).is_ok());
        let chain = super::super::launch_chain::LaunchChain::new(&f.agent_home);
        assert!(
            chain
                .partition_read_grants(std::slice::from_ref(&pins))
                .1
                .is_empty()
        );
        let refused = grant_refusal(&f.agent_home, &pins).expect("pins are write-protected");
        assert!(refused.contains("entitlement pins"), "{refused}");
    }

    #[test]
    fn other_errors_are_not_mistaken_for_a_refusal() {
        assert!(!is_refusal(&anyhow::anyhow!("landlock failed")));
    }
}
