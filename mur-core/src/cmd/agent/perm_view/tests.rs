use super::*;
use mur_common::agent::{DroppedGrant, McpServerEntry, McpServerNetwork, SandboxRecord};

fn fs_profile(read: &[&str], write: &[&str]) -> mur_common::AgentProfile {
    let mut p = mur_common::AgentProfile::default_for_tests();
    p.entitlements.filesystem.read = read.iter().map(|s| s.to_string()).collect();
    p.entitlements.filesystem.write = write.iter().map(|s| s.to_string()).collect();
    p
}

fn lock_with(sandbox: Option<SandboxRecord>) -> LockFile {
    LockFile {
        schema: 1,
        uuid: "u".into(),
        name: "mur".into(),
        pid: 1,
        ppid: 0,
        started_at: "t".into(),
        binary_version: "v".into(),
        transports: mur_common::agent::LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: "d".into(),
        capabilities: vec![],
        build_sha: String::new(),
        proto_version: 0,
        sandbox,
        startup_override: None,
        override_rejected: None,
    }
}

fn sealed(profile: &mur_common::AgentProfile, dropped: Vec<DroppedGrant>) -> SandboxRecord {
    SandboxRecord {
        enforcing: true,
        mode: "macos-sbpl".into(),
        granted_digest: mur_common::agent::filesystem_grants_digest(
            &profile.entitlements.filesystem,
        ),
        granted_spawn_dirs_digest: Some(mur_common::agent::spawn_dirs_digest(
            &profile.entitlements.processes.spawn.allowed_dirs,
        )),
        dropped,
    }
}

/// The report is the audit surface, so what it omits is what people
/// conclude is absent. Listing a `✓` write grant on `~/.mur` with a DENY
/// section that never mentions `agents/` reads as "this agent can write
/// every sibling's profile.yaml" — the reading that made #1198 look like a
/// privilege-escalation path. The emitter closes that tree again after the
/// allows, and the report has to say so.
#[test]
fn the_deny_section_shows_the_launch_chain_and_its_one_exception() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let chain = mur_agent_runtime::sandbox::launch_chain::LaunchChain::for_test(
        &agent_home,
        &bin,
        tmp.path(),
    );

    let p = fs_profile(&[], &[]);
    let out = paths_picture("mur", &p, Some(&lock_with(None)), Some(&chain));

    assert!(
        out.contains("launch chain"),
        "the denies that do most of the work must be named: {out}"
    );
    assert!(
        out.contains(&mur_home.join("agents").display().to_string()),
        "the sibling tree is the whole point of the report: {out}"
    );
    assert!(
        out.contains("except") && out.contains(&agent_home.display().to_string()),
        "the re-allow is why the agent's own tree is writable: {out}"
    );
}

/// Without a running agent nothing is enforced, and the listing must not
/// imply otherwise by ticking every row.
#[test]
fn a_stopped_agent_is_listed_without_claiming_anything_took_effect() {
    let p = fs_profile(&[], &["/tmp/x"]);
    let out = paths_picture("mur", &p, None, None);
    assert!(out.contains("is not running"), "{out}");
    assert!(out.contains("· /tmp/x"), "{out}");
    assert!(
        !out.contains('✓'),
        "a stopped agent has nothing effective: {out}"
    );
}

#[test]
fn a_dropped_grant_is_marked_with_its_reason() {
    let p = fs_profile(&[], &["/tmp/gone", "/tmp/live"]);
    let rec = sealed(
        &p,
        vec![DroppedGrant {
            path: "/tmp/gone".into(),
            verb: "write".into(),
            reason: "path does not exist on disk".into(),
        }],
    );
    let out = paths_picture("mur", &p, Some(&lock_with(Some(rec))), None);
    assert!(out.contains("✗ /tmp/gone"), "{out}");
    assert!(out.contains("does not exist on disk"), "{out}");
    assert!(out.contains("✓ /tmp/live"), "{out}");
    assert!(!out.contains("restart"), "nothing drifted: {out}");
}

/// #2030: a build-lane grant dropped at the seal is otherwise invisible
/// until the build dies with a bare `Operation not permitted`.
#[test]
fn a_dropped_build_lane_grant_is_listed_as_inactive_with_its_reason() {
    let mut p = fs_profile(&[], &[]);
    p.entitlements.processes.spawn.allowed_dirs = vec!["/tmp/later".into(), "/tmp/live".into()];
    let rec = sealed(
        &p,
        vec![DroppedGrant {
            path: "/tmp/later".into(),
            verb: "spawn".into(),
            reason: "did not exist when the agent started; restart to apply".into(),
        }],
    );
    let out = paths_picture("mur", &p, Some(&lock_with(Some(rec))), None);
    assert!(out.contains("BUILD LANE"), "{out}");
    assert!(out.contains("✗ /tmp/later"), "{out}");
    assert!(out.contains("inactive — did not exist"), "{out}");
    assert!(out.contains("✓ /tmp/live"), "{out}");
}

/// The loudest case: no kernel sandbox at all means MORE access than the
/// grants below, so it cannot be a footnote.
#[test]
fn an_unenforced_sandbox_is_stated_before_anything_else() {
    let p = fs_profile(&[], &["/tmp/x"]);
    let mut rec = sealed(&p, vec![]);
    rec.enforcing = false;
    rec.mode = "advisory-only".into();
    let out = paths_picture("mur", &p, Some(&lock_with(Some(rec))), None);
    let first = out.lines().next().unwrap_or_default();
    assert!(first.contains("WITHOUT a kernel sandbox"), "{out}");
    assert!(first.contains("MORE"), "{out}");
}

/// Editing grants after the seal does not change what is enforced, and the
/// listing has to say so or its ticks are lies.
#[test]
fn grants_edited_after_the_seal_are_flagged_for_restart() {
    let sealed_profile = fs_profile(&[], &["/tmp/x"]);
    let rec = sealed(&sealed_profile, vec![]);
    let now = fs_profile(&[], &["/tmp/x", "/tmp/added-later"]);
    let out = paths_picture("mur", &now, Some(&lock_with(Some(rec))), None);
    assert!(out.contains("changed since this agent sealed"), "{out}");
    assert!(out.contains("mur agent restart mur"), "{out}");
}

/// An agent started by an older runtime has no record; saying nothing would
/// let its rows read as verified.
#[test]
fn a_lock_without_a_seal_record_says_the_effect_is_unknown() {
    let p = fs_profile(&[], &["/tmp/x"]);
    let out = paths_picture("mur", &p, Some(&lock_with(None)), None);
    assert!(out.contains("did not record its seal"), "{out}");
    assert!(!out.contains('✓'), "{out}");
}

fn profile_with(
    servers: Vec<(&str, Option<mur_common::agent::McpNetMode>)>,
) -> mur_common::AgentProfile {
    use mur_common::agent::{McpServerEntry, McpServerNetwork};
    let mut p = mur_common::AgentProfile::default_for_tests();
    p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
    for (name, mode) in servers {
        p.mcp_servers.push(McpServerEntry {
            name: name.into(),
            command: "npx".into(),
            network: mode.map(|m| McpServerNetwork {
                mode: m,
                allow_hosts: vec!["db.internal".into()],
                deny_hosts: vec![],
                authorization: None,
            }),
            ..Default::default()
        });
    }
    p
}

/// The whole point of the view: an `inherit` server must be shown as NOT
/// covered by the agent's allow_hosts. Believing otherwise is what sent
/// someone chasing a MySQL connection that the host list never governed.
#[test]
fn inherit_servers_are_shown_as_not_bounded_by_allow_hosts() {
    let p = profile_with(vec![("media", None)]);
    let out = outbound_picture(&p);
    assert!(out.contains("api.example.com"), "{out}");
    assert!(
        out.contains("NOT bounded by allow_hosts"),
        "an inherit server must be called out: {out}"
    );
    assert!(
        out.contains("ports, not hosts"),
        "must name what DOES bound it: {out}"
    );
    assert!(out.contains("set-network"), "must name the remedy: {out}");
}

/// A restricted server is genuinely host-bounded — it must NOT carry the
/// warning, or the warning becomes noise and stops being read.
#[test]
fn restricted_servers_show_their_own_hosts_without_the_warning() {
    let p = profile_with(vec![(
        "db",
        Some(mur_common::agent::McpNetMode::Restricted),
    )]);
    let out = outbound_picture(&p);
    assert!(out.contains("db.internal"), "{out}");
    assert!(!out.contains("NOT bounded by allow_hosts"), "{out}");
    assert!(!out.contains("set-network"), "no remedy needed: {out}");
}

/// With no MCP servers there is only one policy, and nothing to disambiguate.
#[test]
fn no_servers_means_no_server_section() {
    let p = profile_with(vec![]);
    let out = outbound_picture(&p);
    assert!(out.contains("runtime's own traffic"), "{out}");
    assert!(!out.contains("MCP servers"), "{out}");
}

fn profile() -> mur_common::AgentProfile {
    let mut p = mur_common::AgentProfile::default_for_tests();
    p.entitlements.filesystem.write = vec!["/tmp/x".into()];
    p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
    p
}

fn sealed_rec(p: &mur_common::AgentProfile, enforcing: bool) -> SandboxRecord {
    SandboxRecord {
        enforcing,
        mode: if enforcing {
            "macos-sbpl"
        } else {
            "advisory-only"
        }
        .into(),
        granted_digest: filesystem_grants_digest(&p.entitlements.filesystem),
        granted_spawn_dirs_digest: Some(mur_common::agent::spawn_dirs_digest(
            &p.entitlements.processes.spawn.allowed_dirs,
        )),
        dropped: vec![],
    }
}

#[test]
fn enforcement_covers_all_four_states() {
    let p = profile();
    assert_eq!(
        permissions_view(&p, None).enforcement,
        Enforcement::NotRunning
    );
    assert_eq!(
        permissions_view(&p, Some(&lock_with(None))).enforcement,
        Enforcement::SealUnknown
    );
    assert_eq!(
        permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, false))))).enforcement,
        Enforcement::Advisory
    );
    assert_eq!(
        permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, true))))).enforcement,
        Enforcement::Enforcing
    );
}

/// Not running means no grant is verified — the view must not tick rows.
#[test]
fn a_stopped_agent_has_only_unverified_grants() {
    let v = permissions_view(&profile(), None);
    assert_eq!(v.filesystem.write.len(), 1);
    assert_eq!(v.filesystem.write[0].raw, "/tmp/x");
    assert_eq!(v.filesystem.write[0].status, GrantStatus::Unverified);
    assert!(!v.grants_drifted);
}

#[test]
fn a_dropped_grant_carries_its_reason_and_drift_is_detected() {
    let mut p = profile();
    p.entitlements.filesystem.write.push("/tmp/gone".into());
    let mut rec = sealed_rec(&p, true);
    rec.dropped.push(DroppedGrant {
        path: "/tmp/gone".into(),
        verb: "write".into(),
        reason: "path does not exist on disk".into(),
    });
    let l = lock_with(Some(rec));
    let v = permissions_view(&p, Some(&l));
    assert_eq!(v.filesystem.write[0].status, GrantStatus::Effective);
    assert_eq!(
        v.filesystem.write[1].status,
        GrantStatus::Dropped {
            reason: "path does not exist on disk".into()
        }
    );
    assert!(!v.grants_drifted);

    p.entitlements
        .filesystem
        .write
        .push("/tmp/added-later".into());
    assert!(permissions_view(&p, Some(&l)).grants_drifted);
}

/// The load-bearing case from the spec: an `inherit` server is Unbounded
/// even though the agent's own allow_hosts is non-empty.
#[test]
fn inherit_server_is_unbounded_while_agent_allow_hosts_is_set() {
    let mut p = profile();
    p.mcp_servers.push(McpServerEntry {
        name: "media".into(),
        command: "npx".into(),
        network: None,
        ..Default::default()
    });
    p.mcp_servers.push(McpServerEntry {
        name: "db".into(),
        command: "npx".into(),
        network: Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts: vec!["db.internal".into()],
            deny_hosts: vec![],
            authorization: None,
        }),
        ..Default::default()
    });
    let v = permissions_view(&p, None);
    assert_eq!(v.runtime_outbound.allow_hosts, vec!["api.example.com"]);
    assert!(v.runtime_outbound.model_host_always_allowed);
    assert_eq!(v.mcp_servers[0].scope, McpScope::Unbounded);
    assert_eq!(v.mcp_servers[1].scope, McpScope::OwnHosts);
    assert_eq!(v.mcp_servers[1].allow_hosts, vec!["db.internal"]);
}

/// The Hub deserialises what mur-core serialises; a private field or a
/// non-string enum tag would break at runtime, not compile time.
#[test]
fn round_trips_through_json() {
    let p = profile();
    let v = permissions_view(&p, Some(&lock_with(Some(sealed_rec(&p, true)))));
    let s = serde_json::to_string(&v).unwrap();
    let back: PermissionsView = serde_json::from_str(&s).unwrap();
    assert_eq!(back, v);
    assert!(s.contains("\"enforcement\":\"enforcing\""), "{s}");
    assert!(s.contains("\"status\":\"effective\""), "{s}");
}

// ── build-lane grants vs the seal (#2030) ─────────────────────────────

fn spawn_dir_profile(dirs: &[&str]) -> mur_common::AgentProfile {
    let mut p = mur_common::AgentProfile::default_for_tests();
    p.entitlements.processes.spawn.allowed_dirs = dirs.iter().map(|s| s.to_string()).collect();
    p
}

/// A directory added to `allowed_dirs` after the agent started never reached
/// the sandbox. It must not tick, and the drift warning must fire — the
/// filesystem digest alone is blind to this edit.
#[test]
fn a_build_lane_added_after_the_seal_is_not_effective_and_drift_is_flagged() {
    let sealed_with = spawn_dir_profile(&["/work/a"]);
    let l = lock_with(Some(sealed(&sealed_with, vec![])));

    let now = spawn_dir_profile(&["/work/a", "/work/added-later"]);
    let v = permissions_view(&now, Some(&l));

    assert!(v.grants_drifted);
    assert_eq!(v.processes.dir_grants.len(), 2);
    for g in &v.processes.dir_grants {
        assert_eq!(g.status, GrantStatus::Unverified, "{}", g.raw);
    }
}

#[test]
fn an_unchanged_build_lane_is_effective_and_not_drifted() {
    let p = spawn_dir_profile(&["/work/a"]);
    let l = lock_with(Some(sealed(&p, vec![])));
    let v = permissions_view(&p, Some(&l));
    assert!(!v.grants_drifted);
    assert_eq!(v.processes.dir_grants[0].status, GrantStatus::Effective);
}

/// A lock sealed by a runtime that predates the digest cannot establish
/// anything about build-lane grants, so nothing may tick and no drift is
/// claimed (there is nothing to compare against).
#[test]
fn a_lock_without_the_spawn_digest_leaves_build_lanes_unverified() {
    let p = spawn_dir_profile(&["/work/a"]);
    let mut rec = sealed(&p, vec![]);
    rec.granted_spawn_dirs_digest = None;
    let v = permissions_view(&p, Some(&lock_with(Some(rec))));
    assert!(!v.grants_drifted);
    assert_eq!(v.processes.dir_grants[0].status, GrantStatus::Unverified);
}

/// The configured directory is valid but a linked-worktree candidate derived
/// from it was rejected. The runtime records the candidate by its own path;
/// the listing has to carry it and the reason.
#[test]
fn a_dropped_linked_worktree_candidate_is_listed_with_its_reason() {
    let p = spawn_dir_profile(&["/work/main/target"]);
    let rec = sealed(
        &p,
        vec![DroppedGrant {
            path: "/work/wt/target".into(),
            verb: "spawn".into(),
            reason: "not a directory".into(),
        }],
    );
    let v = permissions_view(&p, Some(&lock_with(Some(rec))));

    let rows = &v.processes.dir_grants;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].raw, "/work/main/target");
    assert_eq!(rows[0].status, GrantStatus::Effective);
    assert_eq!(rows[1].raw, "/work/wt/target");
    assert_eq!(
        rows[1].status,
        GrantStatus::Dropped {
            reason: "not a directory".into()
        }
    );
}

fn with_override(mut lock: LockFile, full_perms: bool) -> LockFile {
    lock.startup_override = Some(mur_common::agent::OverrideRecord {
        nonce: "ovr-0123456789abcdef0123456789abcdef".into(),
        flags: mur_common::agent::OverrideFlags {
            no_hitl: false,
            net_unrestricted: false,
            full_perms,
        },
        confirmed_by: mur_common::startup_override::ConfirmedBy::Tty,
    });
    lock
}

/// A seal built from the override-merged profile, as the runtime does.
fn sealed_merged(profile: &mur_common::AgentProfile) -> SandboxRecord {
    let mut merged = profile.clone();
    mur_common::startup_override::merge::apply(
        &mut merged.entitlements,
        mur_common::agent::OverrideFlags {
            full_perms: true,
            ..Default::default()
        },
    );
    sealed(&merged, vec![])
}

fn deny_profile() -> mur_common::AgentProfile {
    let mut p = fs_profile(&[], &["/tmp/x"]);
    p.entitlements.filesystem.deny = vec!["/tmp/x/keep".into()];
    p
}

/// `--full-perms` resets fs.deny, so the sealed digest differs from the file
/// by design. That is reported as the override, never as an edit.
#[test]
fn an_override_relaxation_is_not_reported_as_drift() {
    let p = deny_profile();
    let lock = with_override(lock_with(Some(sealed_merged(&p))), true);
    let v = permissions_view(&p, Some(&lock));
    assert!(!v.grants_drifted, "{v:?}");
    let r = v.relaxed_by_override.expect("override shown");
    assert_eq!(r.flags, vec!["--full-perms".to_string()]);
    let out = paths_picture("mur", &p, Some(&lock), None);
    assert!(out.contains("Relaxed by startup override (ovr-"), "{out}");
    assert!(!out.contains("changed since this agent sealed"), "{out}");
}

/// Negative control: the same seal without an override on the lock is drift.
#[test]
fn the_same_seal_without_an_override_is_drift() {
    let p = deny_profile();
    let v = permissions_view(&p, Some(&lock_with(Some(sealed_merged(&p)))));
    assert!(v.grants_drifted);
    assert!(v.relaxed_by_override.is_none());
}

/// An edit made after an override boot is still drift, and both are shown.
#[test]
fn an_edit_under_an_override_is_still_drift() {
    let p = deny_profile();
    let lock = with_override(lock_with(Some(sealed_merged(&p))), true);
    let mut edited = p.clone();
    edited
        .entitlements
        .filesystem
        .write
        .push("/tmp/added-later".into());
    let v = permissions_view(&edited, Some(&lock));
    assert!(v.grants_drifted);
    assert!(v.relaxed_by_override.is_some());
}
