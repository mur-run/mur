use super::*;

/// The published docs say `mur agent perm allow-read` / `allow-write`
/// refuse these. Before #850 they did not — `protects_read` matched only a
/// sibling `identity.key`, so a grant covering the credential store was
/// accepted, and two live agents held one. This pins the claim.
#[test]
fn the_grant_gates_refuse_the_credential_store() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);
    for p in [
        mur.join("secrets"),
        mur.join("secrets").join("anthropic.key"),
        mur.join("auth.json"),
        mur.join("identity.key"),
        mur.join("commander").join("signing.key"),
        mur.join("mobile").join("pair-token"),
        mur.join("actions-runner"),
        mur.join("actions-runner").join(".credentials"),
        mur.join("queue"),
        mur.join("queue").join("events.jsonl"),
        mur.join("session").join("recordings"),
        mur.join("conversations"),
        mur.join("telemetry"),
        mur.join("traces"),
        mur.join("commander").join(".env"),
        mur.join("runtime").join("vlc.json"),
    ] {
        assert!(
            chain.protects_read(&p).is_some(),
            "read grant not refused: {}",
            p.display()
        );
        assert!(
            chain.protects_write(&p).is_some(),
            "write grant not refused: {}",
            p.display()
        );
    }
}

/// ...and must not swallow the ordinary stores an agent legitimately uses.
#[test]
fn the_grant_gates_still_allow_the_ordinary_stores() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);
    // Includes the non-secret NEIGHBOURS of the new denies: `commander/`
    // holds the constitution and the audit log, `mobile/` holds the paired
    // device list. Denying a whole directory to reach one credential
    // inside it is the mistake this guards against.
    for p in [
        mur.join("skills"),
        mur.join("channels"),
        mur.join("workflows"),
        mur.join("commander"),
        mur.join("commander").join("constitution.toml"),
        mur.join("mobile").join("paired.json"),
    ] {
        assert!(
            chain.protects_read(&p).is_none(),
            "an ordinary store was refused: {}",
            p.display()
        );
    }
}

/// The whole point of the move: a key created AFTER the policy was sealed
/// is still protected, because `keys/` is a subtree rather than a list.
///
/// The enumeration this replaced could not do that — `sibling_signing_keys`
/// listed what was on disk at seal time, so a sibling created a minute
/// later stayed readable until the reader restarted. That gap is gone, and
/// this test is what proves it rather than the comment claiming it.
#[test]
fn a_key_created_after_the_seal_is_still_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);

    // Nothing exists yet — an enumeration built now would be empty.
    let latecomer = mur.join("keys").join("created-later").join("identity.key");

    assert!(
        chain.protects_read(&latecomer).is_some(),
        "a key under keys/ must be refused even though it did not exist \
         when the chain was built"
    );
    assert!(chain.protects_write(&latecomer).is_some());
}

/// ...and the agents tree is now ordinary. Peers read `identity.pub` and
/// `rotations.jsonl` there to verify signed events; refusing those
/// fail-closes every multi-agent channel (audit §2).
#[test]
fn the_agents_tree_holds_nothing_read_protected_any_more() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);
    let pm = mur.join("agents").join("pm");

    for p in [
        pm.join("identity.pub"),
        pm.join("rotations.jsonl"),
        pm.join("profile.yaml"),
        mur.join("agents"),
    ] {
        assert!(
            chain.protects_read(&p).is_none(),
            "{} must be readable — peers verify from it",
            p.display()
        );
    }
}

/// Every path the SBPL emitter denies must ALSO be refused by the
/// predicate, and vice versa.
///
/// `protects_credential`/`protects_capture_store` (predicates) and
/// `credential_paths()` (the concrete list the macOS emitter walks) are two
/// implementations of one rule. They are consulted by different callers —
/// the predicate by the file tools and `mur agent perm`, the list by the
/// kernel profile — so a divergence means a path is denied by one and
/// allowed by the other, with nothing saying so.
///
/// This is not hypothetical: adding `keys/` to the list alone (#850 option
/// (c) step 3) left `protects_read` still permitting it, and only a
/// behaviour test caught it. Neutering this test the same way reproduces
/// that failure exactly.
///
/// LIMIT, deliberate: this checks one direction only — list ⊆ predicate.
/// The reverse (a predicate branch with no emitted path) cannot be
/// enumerated, because the predicates match by prefix and family rather
/// than by a closed set. A gap that way is also less severe: the file
/// tools still refuse, so the kernel is merely less strict than the tools,
/// not the other way round.
#[test]
fn every_emitted_credential_path_is_also_refused_by_the_predicate() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);

    let mut gaps: Vec<String> = Vec::new();
    for p in chain.credential_paths() {
        if chain.protects_read(&p).is_none() {
            gaps.push(format!("{} (read)", p.display()));
        }
        if chain.protects_write(&p).is_none() {
            gaps.push(format!("{} (write)", p.display()));
        }
    }

    assert!(
        gaps.is_empty(),
        "the SBPL emitter denies these but the predicate permits them, so \
         the file tools and `mur agent perm` disagree with the kernel: {}",
        gaps.join(", ")
    );
}

/// #712: the pins decide which entitlements the supervisor trusts, so the
/// agent must not write them — refused by the tools, denied by the kernel,
/// and a grant covering them (e.g. `~/.mur`) dropped whole on Landlock.
#[test]
fn entitlement_pins_are_write_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);
    let pin = mur_common::entitlements_pin::pin_path(&mur, "alice");
    assert!(chain.protects_write(&pin).is_some());
    assert!(chain.deny_paths().iter().any(|d| pin.starts_with(d)));
    let (kept, dropped) = chain.partition_grants(std::slice::from_ref(&mur));
    assert!(kept.is_empty() && dropped == vec![mur]);
}

/// A read grant wide enough to contain the credential store is dropped
/// whole, exactly as the write side already drops it. Before #850 the two
/// diverged: `fs_write: [~/.mur]` was refused and `fs_read: [~/.mur]` was
/// installed, handing out every API key on a backend with no deny rule.
#[test]
fn a_read_grant_containing_the_credential_store_is_dropped_not_carved() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);
    let skills = mur.join("skills");

    let (kept, dropped) = chain.partition_read_grants(&[mur.clone(), skills.clone()]);

    // `<mur_home>` contains `<mur_home>/secrets`, and Landlock cannot carve it out.
    assert_eq!(dropped, vec![mur], "{kept:?}");
    // Negative control: a grant that contains nothing protected survives intact.
    assert_eq!(kept, vec![skills]);
}

/// The §2 constraint, as a test: denying a sibling's PRIVATE key must not
/// take its PUBLIC verification material with it. `identity.pub` and
/// `rotations.jsonl` are what every multi-agent channel reads to verify a
/// peer's signed events — dropping those grants fail-closes delegation.
#[test]
fn a_read_grant_on_peer_public_material_survives_the_signing_key_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let pm = mur.join("agents").join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    // sibling_signing_keys() enumerates what is on disk, so the key must exist.
    std::fs::write(pm.join("identity.key"), b"k").unwrap();
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &mur);

    // The private key now lives under `keys/pm/`, not beside the public
    // material — that separation is what lets the deny be a subtree.
    let pm_key = mur.join("keys").join("pm").join("identity.key");
    let (kept, dropped) = chain.partition_read_grants(&[
        pm_key.clone(),
        pm.join("identity.pub"),
        pm.join("rotations.jsonl"),
    ]);

    assert_eq!(dropped, vec![pm_key]);
    assert_eq!(
        kept,
        vec![pm.join("identity.pub"), pm.join("rotations.jsonl")],
        "public verification material must stay readable or every signed \
         channel fails closed"
    );
}

/// The agent's own home is exempt, as on the write side — it must read its
/// own profile and state.
#[test]
fn a_read_grant_on_the_agents_own_home_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().to_path_buf();
    let own = mur.join("agents").join("alice");
    let chain = LaunchChain::for_test(&own, &mur.join("bin"), &mur);

    let (kept, dropped) = chain.partition_read_grants(std::slice::from_ref(&own));

    assert_eq!(kept, vec![own]);
    assert!(dropped.is_empty());
}

/// `<tmp>/agents/mur` as agent_home, so mur_home is `<tmp>`.
fn chain(tmp: &Path) -> LaunchChain {
    LaunchChain::for_test(
        &tmp.join("agents").join("mur"),
        &tmp.join("bin"),
        &tmp.join("home"),
    )
}

#[test]
fn sibling_agent_files_are_write_protected_but_own_are_left_to_self_protect() {
    let tmp = tempfile::tempdir().unwrap();
    let c = chain(tmp.path());
    let agents = tmp.path().join("agents");

    assert!(c.protects_write(&agents.join("pm/profile.yaml")).is_some());
    assert!(c.protects_write(&agents.join("pm/identity.key")).is_some());
    assert!(c.protects_write(&agents.join("pm/anything/else")).is_some());

    // Negative control: the agent's own home stays writable. Without this,
    // a predicate that returned Some() for everything would still pass.
    assert!(c.protects_write(&agents.join("mur/running.lock")).is_none());
    assert!(
        c.protects_write(&agents.join("mur/skills/x.yaml"))
            .is_none()
    );
}

#[test]
fn protects_agents_created_after_the_policy_was_built() {
    // The regression a path list cannot catch: this directory does not
    // exist, and never existed when any list would have been built.
    let tmp = tempfile::tempdir().unwrap();
    let c = chain(tmp.path());
    let unborn = tmp.path().join("agents/not-created-yet/profile.yaml");
    assert!(!unborn.exists());
    assert!(c.protects_write(&unborn).is_some());
}

#[test]
fn only_murs_own_launch_artifacts_in_bin_dir_are_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let c = chain(tmp.path());
    let bin = tmp.path().join("bin");

    assert!(c.protects_write(&bin.join("mur-agent-runtime")).is_some());
    assert!(c.protects_write(&bin.join("mur_agent_pm")).is_some());

    // Negative control: an agent installing a tool for itself still works.
    assert!(c.protects_write(&bin.join("ripgrep")).is_none());
    assert!(c.protects_write(&bin.join("murmur-notes")).is_none());
}

#[test]
fn sibling_identity_key_is_read_protected_and_nothing_else_is() {
    let tmp = tempfile::tempdir().unwrap();
    let c = chain(tmp.path());
    let agents = tmp.path().join("agents");

    assert!(c.protects_read(&agents.join("pm/identity.key")).is_some());

    // #007 moved ownership of this rule here. It used to be enforced by
    // the `fs.deny` list (`for_file_tools` + the SBPL emission), which is
    // also what made the agent's own profile.yaml unreadable. Splitting
    // the two meant the key rule had to live somewhere no entitlement can
    // reach — which is this module. Same protection, correct layer.
    assert!(c.protects_read(&agents.join("mur/identity.key")).is_some());

    // Negative controls: reads are otherwise untouched by this module.
    assert!(c.protects_read(&agents.join("pm/profile.yaml")).is_none());
    assert!(c.protects_read(&agents.join("mur/profile.yaml")).is_none());
    assert!(c.protects_read(&tmp.path().join("skills/x.yaml")).is_none());
}

#[test]
fn overbroad_roots_are_rejected_and_normal_project_dirs_are_not() {
    let home = PathBuf::from("/Users/someone");
    assert!(is_overbroad_grant_root(Path::new("/"), &home));
    assert!(is_overbroad_grant_root(&home, &home));
    assert!(is_overbroad_grant_root(Path::new("/Users"), &home));
    assert!(is_overbroad_grant_root(Path::new("/usr"), &home));
    assert!(is_overbroad_grant_root(Path::new("/opt/homebrew"), &home));
    assert!(is_overbroad_grant_root(Path::new("/Volumes/Disk"), &home));

    // Negative controls: real grants people legitimately make.
    assert!(!is_overbroad_grant_root(&home.join("Projects/app"), &home));
    assert!(!is_overbroad_grant_root(
        Path::new("/Volumes/Disk/Projects/app"),
        &home
    ));
}

/// The user's own credential dirs are refused by the chain itself, so a
/// profile whose `filesystem.deny` is empty — every concierge seeded before
/// the template carried `DEFAULT_DENY_PATHS` — is still covered. No grant can
/// lift it: the chain sits before the allow/deny lists.
#[test]
fn user_credential_dirs_are_refused_even_with_an_empty_deny_list() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mur = home.join(".mur");
    let chain = LaunchChain::for_test(&mur.join("agents").join("mur"), &mur.join("bin"), &home);
    for d in mur_common::agent::DEFAULT_DENY_PATHS {
        let dir = home.join(d.trim_start_matches("~/"));
        for p in [dir.clone(), dir.join("id_ed25519")] {
            assert!(chain.protects_read(&p).is_some(), "read of {}", p.display());
            assert!(
                chain.protects_write(&p).is_some(),
                "write of {}",
                p.display()
            );
        }
        // The kernel side: macOS emits `credential_paths()` as deny clauses.
        assert!(chain.credential_paths().contains(&dir), "{}", dir.display());
    }
    // A broad read grant over the whole home would hand them out on Landlock,
    // which cannot carve — so it is dropped whole, like one over `~/.mur`.
    let (kept, dropped) = chain.partition_read_grants(std::slice::from_ref(&home));
    assert!(kept.is_empty() && dropped == vec![home.clone()]);
}

/// ...and only those: neighbours that merely share a prefix stay readable.
#[test]
fn user_credential_dirs_do_not_swallow_their_neighbours() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mur = home.join(".mur");
    let chain = LaunchChain::for_test(&mur.join("agents").join("mur"), &mur.join("bin"), &home);
    for p in [
        home.join(".ssh-notes"),
        home.join(".awsome/x"),
        home.join("Projects/app/.ssh-config.example"),
        home.join(".config/gh"),
    ] {
        assert!(chain.protects_read(&p).is_none(), "{}", p.display());
    }
}
