use super::*;

#[test]
fn agent_home_always_in_write() {
    let home = PathBuf::from("/tmp/agent_home");
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
    assert!(policy.fs_write.contains(&home));
}

#[test]
fn own_profile_and_identity_key_always_denied() {
    // Issue #712: even a broad write grant covering the agents root (or
    // the agent's own dir) must not make the agent's profile.yaml /
    // identity.key writable — the self-escalation loop (self-edit +
    // self-restart) stays closed regardless of entitlements.
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let mut ent = minimal_entitlements();
    ent.filesystem.write = vec![
        mur_home.join("agents").to_string_lossy().into_owned(),
        agent_home.to_string_lossy().into_owned(),
    ];
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
    // Pinned by name, not only by iterating the constant: dropping the
    // public key material from the list would let the router agent sign
    // its own HITL approvals, and a loop over the list cannot notice that.
    for f in ["identity.pub", "rotations.jsonl"] {
        assert!(
            SELF_PROTECTED_AGENT_FILES.contains(&f),
            "{f} must stay self-protected"
        );
    }
    for f in SELF_PROTECTED_AGENT_FILES {
        assert!(
            policy.fs_deny.contains(&agent_home.join(f)),
            "{f} must always be in fs_deny"
        );
    }
    // Only those files are denied — the rest of agent_home
    // (running.lock, running.sentinel, stderr.log) stays writable.
    assert!(policy.fs_write.contains(&agent_home));
}

/// The runtime reads `config.yaml` unconditionally (model switch, skills,
/// remember, fleet_run). On Linux Landlock is deny-by-default, so without a
/// read grant `Config::load_or_default` silently returns DEFAULTS and the
/// agent runs on the wrong model with no error anywhere. Same for
/// `compress.yaml` (hook never engages) and `fleets/` (delegation cannot
/// resolve a fleet). Audit §7.1.
#[test]
fn the_runtimes_own_central_store_reads_are_granted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(mur_home.join("config.yaml"), "").unwrap();
    std::fs::write(mur_home.join("compress.yaml"), "").unwrap();
    std::fs::create_dir_all(mur_home.join("compress")).unwrap();
    std::fs::create_dir_all(mur_home.join("fleets")).unwrap();

    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);

    for name in ["config.yaml", "compress.yaml", "compress", "fleets"] {
        assert!(
            policy.fs_read.contains(&mur_home.join(name)),
            "{name} must be readable or the runtime silently degrades: {:?}",
            policy.fs_read
        );
    }
}

/// Git-push, Landlock read side: with `git_push.enabled` the agent gets exactly
/// the registry and its OWN status dir — not the broker dir, not broker-private
/// state, not a sibling's status. Off (the default) it gets nothing there.
#[test]
fn git_push_reads_are_exactly_registry_and_own_status_and_only_when_enabled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let reg = mur_common::git_push::registry_path(mur_home);
    let own = mur_common::git_push::status_dir(mur_home, "mur");
    let sibling = mur_common::git_push::status_dir(mur_home, "pm");
    let private = mur_common::git_push::private_dir(mur_home);
    for d in [&own, &sibling, &private] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(&reg, "repos: {}\n").unwrap();
    let broker = mur_common::git_push::broker_dir(mur_home);
    let reaches =
        |p: &SandboxPolicy, x: &std::path::Path| p.fs_read.iter().any(|r| x.starts_with(r));

    let off = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    assert!(
        !reaches(&off, &reg) && !reaches(&off, &own),
        "default off: {:?}",
        off.fs_read
    );

    std::fs::write(mur_home.join("config.yaml"), "git_push:\n  enabled: true\n").unwrap();
    let on = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    assert!(reaches(&on, &reg) && reaches(&on, &own), "{:?}", on.fs_read);
    assert!(!on.fs_read.contains(&broker), "never the whole broker dir");
    assert!(
        !reaches(&on, &private) && !reaches(&on, &sibling),
        "{:?}",
        on.fs_read
    );
}

/// ...and the grant is existence-checked like every other one (Issue 16):
/// a rule on a path that does not exist destabilizes the profile, so an
/// absent `compress.yaml` must not be emitted.
#[test]
fn absent_central_store_paths_are_not_granted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    // Nothing created: no config.yaml, no compress.yaml, no fleets/.

    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);

    for name in ["config.yaml", "compress.yaml", "compress", "fleets"] {
        assert!(
            !policy.fs_read.contains(&mur_home.join(name)),
            "{name} does not exist and must not be granted"
        );
    }
}

/// The agents tree is granted WHOLE now, not two files per agent — so a
/// peer created after the seal is readable and signature verification does
/// not silently degrade (#850 option (c) step 3).
#[test]
fn the_agents_tree_is_readable_as_one_grant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(mur_home.join("agents").join("pm")).unwrap();

    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);

    assert!(
        policy.fs_read.contains(&mur_home.join("agents")),
        "agents/ must be readable as one grant: {:?}",
        policy.fs_read
    );
    // ...and no private key is inside it to be exposed by that grant.
    assert!(
        !policy
            .fs_read
            .iter()
            .any(|p| p.starts_with(mur_home.join("keys"))),
        "keys/ must never appear in a read grant"
    );
}

/// The paths the builder itself adds must survive the read partition.
///
/// Partitioning the FINISHED `fs_read` instead of just the user-declared
/// part silently dropped `/private/tmp` whenever `mur_home` nested under
/// it — which is every temp-dir test, and would be a real agent under a
/// relocated MUR home. Only a human-declared grant may be dropped, because
/// only a human can fix it.
#[test]
fn the_builders_own_read_paths_survive_an_overbroad_user_grant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(mur_home.join("secrets")).unwrap();
    std::fs::write(mur_home.join("config.yaml"), "").unwrap();

    let mut ent = minimal_entitlements();
    // Overbroad: contains <mur_home>/secrets, so it is dropped.
    ent.filesystem.read = vec![mur_home.to_string_lossy().into_owned()];
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert!(policy.dropped_read_grants.contains(&mur_home.to_path_buf()));
    // ...but the builder's own additions are still there.
    assert!(
        policy.fs_read.contains(&mur_home.join("config.yaml")),
        "the runtime's own config read was collateral damage: {:?}",
        policy.fs_read
    );
    for sys in system_read_paths() {
        assert!(
            policy.fs_read.contains(&sys),
            "system read path {} was dropped: {:?}",
            sys.display(),
            policy.fs_read
        );
    }
}

/// A read grant that reaches the credential store is dropped whole and
/// RECORDED, so `mur agent doctor` can say so instead of the agent finding
/// out from an errno.
#[test]
fn an_overbroad_read_grant_is_recorded_as_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(mur_home.join("secrets")).unwrap();
    let skills = mur_home.join("skills");
    std::fs::create_dir_all(&skills).unwrap();

    let mut ent = minimal_entitlements();
    ent.filesystem.read = vec![
        mur_home.to_string_lossy().into_owned(),
        skills.to_string_lossy().into_owned(),
    ];
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert!(
        policy.dropped_read_grants.contains(&mur_home.to_path_buf()),
        "{:?}",
        policy.dropped_read_grants
    );
    // Negative control: the narrow grant is not dropped.
    assert!(!policy.dropped_read_grants.contains(&skills));
}

#[test]
fn channels_dir_always_in_write() {
    // agent_home = <mur_home>/agents/<name> → channels = <mur_home>/channels,
    // granted even though minimal_entitlements lists no write paths.
    let home = PathBuf::from("/tmp/mur/agents/rs");
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
    assert!(
        policy
            .fs_write
            .contains(&PathBuf::from("/tmp/mur/channels"))
    );
}

#[test]
fn open_items_log_is_writable_so_the_built_in_tool_works() {
    // The `open_item` tool has exactly one write target and no entitlement
    // of its own. Without this grant, "record a todo" failed for every
    // sandboxed agent while the tool's own docs said it needed nothing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    let log = mur_home.join(mur_open_items::LOG_FILE);
    assert!(policy.fs_write.contains(&log), "{:?}", policy.fs_write);
    // Created up front: a rule on a path that does not exist yet is
    // dropped at seal time, and the log is append-only from absent.
    assert!(log.exists());
}

#[test]
fn fleet_run_carveins_are_config_gated() {
    // Not allowlisted (no config.yaml) → no fleets/commander/conversations grants.
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    let dr_state = mur_common::paths::fleet_state_dir(mur_home, "deep-research");
    assert!(!policy.fs_write.contains(&dr_state));
    assert!(!policy.fs_write.contains(&mur_home.join("runs")));

    // Allowlisted in config.yaml → every run-state dir is carved in, and
    // `fleet-state/` only for the fleets the operator named.
    std::fs::write(
        mur_home.join("config.yaml"),
        "fleet_run:\n  agents: [mur]\n  fleets: [deep-research, ../escape]\n",
    )
    .unwrap();
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    for dir in mur_common::paths::RUN_STATE_DIRS {
        if dir == mur_common::paths::FLEET_STATE {
            continue;
        }
        assert!(
            policy.fs_write.contains(&mur_home.join(dir)),
            "{dir} should be carved in for an allowlisted agent"
        );
    }
    assert!(policy.fs_write.contains(&dr_state), "{:?}", policy.fs_write);
    // Not the whole tree: an agent must not queue work for a fleet it was
    // never allowed to run (a cron fleet would then run it unattended).
    assert!(
        !policy
            .fs_write
            .contains(&mur_home.join(mur_common::paths::FLEET_STATE))
    );
    // A traversal name in config.yaml never becomes a grant.
    assert!(
        policy
            .fs_write
            .iter()
            .all(|p| !p.to_string_lossy().contains("escape")),
        "{:?}",
        policy.fs_write
    );
    // The definitions are never writable from a run: members, limits,
    // HITL pre-approvals and the `.stopped` kill-switch all live there.
    assert!(
        policy
            .fs_write
            .iter()
            .all(|p| !p.starts_with(mur_home.join(mur_common::paths::FLEETS))),
        "{:?}",
        policy.fs_write
    );

    // A different (non-allowlisted) agent stays denied.
    let other_home = mur_home.join("agents").join("dr_worker_1");
    std::fs::create_dir_all(&other_home).unwrap();
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &other_home);
    assert!(!policy.fs_write.contains(&dr_state));
}

#[test]
fn tilde_expands_to_home_dir() {
    // Exercised via `deny` rather than `read`/`write`: deny entries are
    // exempt from the dead-grant existence filter by design (a stale
    // deny path is kept verbatim rather than dropped, since dropping it
    // would be fail-open). `~/Documents` may not exist on CI runners
    // (e.g. Ubuntu, no home Documents dir), so asserting through `read`
    // makes this test's outcome depend on runner environment. Routing
    // it through `deny` still exercises the same `expand` tilde
    // substitution logic while staying environment-independent.
    let mut ent = minimal_entitlements();
    ent.filesystem.deny.push("~/Documents".to_string());
    let agent_home = PathBuf::from("/tmp/agent_home_test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
    let expected = dirs::home_dir().unwrap().join("Documents");
    assert!(
        policy.fs_deny.contains(&expected),
        "~/Documents should expand to {expected:?}, got: {:?}",
        policy.fs_deny
    );
}

#[test]
fn deny_paths_propagated() {
    let agent_home = PathBuf::from("/tmp/agent_home_test");
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
    let expected = dirs::home_dir().unwrap().join(".ssh");
    assert!(
        policy.fs_deny.contains(&expected),
        "~/.ssh should expand to {expected:?}, got: {:?}",
        policy.fs_deny
    );
}

#[test]
fn allow_extra_write_paths_adds_and_dedups() {
    let ent = minimal_entitlements();
    let agent_home = PathBuf::from("/tmp/a");
    let mut policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
    let extra = PathBuf::from("/home/u/.mur/runtime/vlc-snapshots");
    policy.allow_extra_write_paths(std::slice::from_ref(&extra));
    assert!(policy.fs_write.contains(&extra));
    // Idempotent — re-adding doesn't duplicate.
    policy.allow_extra_write_paths(std::slice::from_ref(&extra));
    assert_eq!(policy.fs_write.iter().filter(|p| **p == extra).count(), 1);
}

/// The system prompt's output-locations rule (`output_locations_rule`)
/// tells every agent to put reports and scratch output in
/// `~/.mur/artifacts/<agent>/<run>/`. Nothing granted it, so an agent
/// following its own instructions was refused — observed 2026-09-13, after
/// which it reached for `/tmp` and tripped the tool-withdrawal path.
/// Two MUR-authored strings in direct contradiction, the same failure
/// shape `PATH_FORMS` exists to prevent.
#[test]
fn the_agents_own_artifacts_dir_is_granted_but_not_a_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("w1");
    std::fs::create_dir_all(&agent_home).unwrap();
    let ent = minimal_entitlements();
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    let mine = mur_home.join("artifacts").join("w1");
    assert!(
        policy.fs_write.contains(&mine),
        "the agent must be able to write where the system prompt sends it: {:?}",
        policy.fs_write
    );
    // The grant idiom creates the dir so Landlock rules stick.
    assert!(mine.is_dir());

    // Negative controls: scoped to this agent, not the shared tree.
    assert!(
        !policy.fs_write.contains(&mur_home.join("artifacts")),
        "the whole artifacts tree must NOT be granted — every other \
             agent's output lives there"
    );
    assert!(
        !policy
            .fs_write
            .contains(&mur_home.join("artifacts").join("w2")),
        "a sibling agent's artifacts dir must never be writable"
    );
}

#[test]
fn channel_index_subdir_granted_not_whole_index_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("w1");
    std::fs::create_dir_all(&agent_home).unwrap();
    let ent = minimal_entitlements();
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
    assert!(
        policy.fs_write.contains(&mur_home.join("channels")),
        "pre-existing channels carve-out must remain"
    );
    assert!(
        policy
            .fs_write
            .contains(&mur_home.join("index").join("channels")),
        "channels read-model subdir must be granted alongside channels"
    );
    assert!(
        !policy.fs_write.contains(&mur_home.join("index")),
        "the whole index dir must NOT be granted — capabilities.json and \
             the lance stores also live there and must stay unwritable"
    );
    // The grant idiom creates the dir so Landlock rules stick.
    assert!(mur_home.join("index").join("channels").is_dir());
}

/// #1/#2: the per-agent scratch dir `<mur_home>/tmp/<agent>` is granted at
/// the kernel layer, created `0700`, never dropped, and never widened to
/// the shared `tmp/` root or a sibling.
#[test]
fn the_agents_own_scratch_dir_is_granted_but_not_a_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("w1");
    std::fs::create_dir_all(&agent_home).unwrap();
    let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);

    let mine = mur_home.join("tmp").join("w1");
    assert!(policy.fs_write.contains(&mine), "{:?}", policy.fs_write);
    assert!(mine.is_dir());
    assert!(!policy.fs_write.contains(&mur_home.join("tmp")));
    assert!(!policy.fs_write.contains(&mur_home.join("tmp").join("w2")));
    let mine_s = mine.to_string_lossy();
    assert!(
        policy.dropped.iter().all(|d| d.path != mine_s),
        "scratch grant must not be dropped: {:?}",
        policy.dropped
    );
}

/// #10 grant half: a home with no `<mur_home>/agents` ancestor grants no
/// scratch path and logs exactly the error the operator needs.
#[test]
fn scratch_grant_is_skipped_and_logged_when_helper_errs() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || w.clone())
        .with_max_level(tracing::Level::ERROR)
        .with_ansi(false)
        .finish();
    let policy = tracing::subscriber::with_default(sub, || {
        SandboxPolicy::from_entitlements(&minimal_entitlements(), Path::new("/w1"))
    });
    assert!(
        policy.fs_write.iter().all(|p| !p.ends_with("tmp/w1")),
        "{:?}",
        policy.fs_write
    );
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("scratch dir not granted"), "log: {log}");
}
