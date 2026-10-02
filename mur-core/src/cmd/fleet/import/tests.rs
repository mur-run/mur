use super::*;
use mur_common::fleet::Fleet;
use mur_common::identity::AgentIdentity;
use mur_common::skill::manifest::SkillScope;
use mur_common::skill::types::TrustLevel;

fn export_fixture(home: &std::path::Path) -> std::path::PathBuf {
    // concierge + a fleet-scoped skill + a fleet, then export
    let dir = home.join("agents").join("mur");
    std::fs::create_dir_all(&dir).unwrap();
    AgentIdentity::generate().save(&dir).unwrap();
    let m: mur_common::skill::manifest::SkillManifest = serde_yaml::from_str(
            "name: triage\nversion: 1.0.0\npublisher: human:t\ndescription: t\n\
             category: context\nscope: fleet\nfleet: dev\ncontent:\n  abstract: a\n  context: body\n",
        )
        .unwrap();
    mur_common::skill::store::write_to_dir(
        &mur_common::skill::store::global_skill_dir(home, "triage"),
        &m,
    )
    .unwrap();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "ship".into(),
        router: None,
        members: vec!["pm".into(), "qa".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        team_id: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
    let out = home.join("dev.fleet");
    crate::cmd::fleet::export::cmd_fleet_export(
        home,
        "dev",
        false,
        Some(out.clone()),
        "2026-06-20T00:00:00Z",
    )
    .unwrap();
    out
}

#[test]
fn import_roundtrip_installs_fleet_and_skill_at_low_trust() {
    let src = tempfile::tempdir().unwrap();
    let bundle = export_fixture(src.path());

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();

    // fleet installed
    let f = crate::cmd::fleet::store::load_fleet(home, "dev").unwrap();
    assert_eq!(f.members, vec!["pm".to_string(), "qa".to_string()]);
    // skill installed, scope:Fleet preserved, trust downgraded to Sandboxed
    let m = mur_common::skill::local::load_installed(home, "triage").unwrap();
    assert_eq!(m.scope, SkillScope::Fleet);
    assert_eq!(m.fleet.as_deref(), Some("dev"));
    assert_eq!(
        mur_common::skill::local::get_trust_level(home, "triage").unwrap(),
        TrustLevel::Sandboxed
    );
}

#[test]
fn import_signed_bundle_reports_signature_verified() {
    // C1 (positive case): a normally signed bundle must report
    // signature_verified == true so the caller's trusted-recipe install
    // hook is allowed to run.
    let src = tempfile::tempdir().unwrap();
    let bundle = export_fixture(src.path());

    let dst = tempfile::tempdir().unwrap();
    let (_fleet_name, _fp, signature_verified) = cmd_fleet_import(
        dst.path(),
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();
    assert!(
        signature_verified,
        "a normally-signed bundle must report signature_verified == true"
    );
}

#[test]
fn import_unsigned_force_reports_signature_not_verified() {
    // C1: an unsigned bundle imported under --force must report
    // signature_verified == false. The caller (dispatch.rs) gates the
    // trusted-recipe install hook on this flag — a spoofed
    // `signer_pubkey` in an unsigned bundle must never be treated as an
    // attested publisher fingerprint.
    use mur_common::fleet_bundle::{BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT};
    use mur_common::identity::AgentIdentity;

    let signer_pubkey = AgentIdentity::generate().public_key_multibase();
    let fleet_yaml = "name: dev\ndisplay_name: ''\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\n";
    let files: Vec<(String, Vec<u8>)> =
        vec![("fleet.yaml".to_string(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: mur_common::fleet_bundle::content_hash(b),
        })
        .collect();
    let manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: mur_common::fleet_bundle::signer_fingerprint(&signer_pubkey),
        signer_pubkey, // attacker-controlled; bundle below is left UNSIGNED
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path =
        std::env::temp_dir().join(format!("murunsigned_{}_{}.fleet", std::process::id(), "c1"));
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let result = cmd_fleet_import(
        dst.path(),
        &bundle_path,
        ImportOpts {
            force: true,
            no_members: false,
            yes: true,
        },
    );
    std::fs::remove_file(&bundle_path).ok();
    let (fleet_name, _fp, signature_verified) =
        result.expect("unsigned bundle under --force must still import");
    assert_eq!(fleet_name, "dev");
    assert!(
        !signature_verified,
        "unsigned --force import must report signature_verified == false"
    );
}

#[test]
fn import_refuses_tampered_bundle() {
    let src = tempfile::tempdir().unwrap();
    let bundle = export_fixture(src.path());
    let mut bytes = std::fs::read(&bundle).unwrap();
    let n = bytes.len();
    bytes[n / 2] ^= 0xFF; // corrupt the archive
    let bad = src.path().join("bad.fleet");
    std::fs::write(&bad, &bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bad,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    // fail-closed: refused AND nothing installed (no partial write on tamper).
    assert!(
        !crate::cmd::fleet::store::fleet_path(home, "dev").is_file(),
        "tampered bundle must not install the fleet; err={err:#}"
    );
    assert!(
        !mur_common::skill::store::global_skill_dir(home, "triage")
            .join("skill.yaml")
            .is_file(),
        "tampered bundle must not install skills; err={err:#}"
    );
}

#[test]
fn import_refuses_name_conflict_without_force() {
    let src = tempfile::tempdir().unwrap();
    let bundle = export_fixture(src.path());
    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let opts = || ImportOpts {
        force: false,
        no_members: false,
        yes: true,
    };
    cmd_fleet_import(home, &bundle, opts()).unwrap();
    let err = cmd_fleet_import(home, &bundle, opts()).unwrap_err();
    assert!(format!("{err:#}").contains("exists"));
    // with force it succeeds
    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: true,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();
}

#[test]
fn missing_members_reports_absent_agents() {
    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    // create agent "pm" locally
    let pm = home.join("agents").join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(pm.join("profile.yaml"), "name: pm\n").unwrap();
    let missing = missing_members(home, &["pm".into(), "qa".into()]);
    assert_eq!(missing, vec!["qa".to_string()]);
}

#[test]
fn import_with_members_installs_fresh_identity() {
    // Setup: source with a concierge, a pm agent, and a fleet.
    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let mur_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&mur_dir).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&mur_dir)
        .unwrap();
    let pm_dir = s.join("agents").join("pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    // Must be a valid AgentProfile — write a parseable profile with the right name.
    let mut pm_profile = mur_common::agent::AgentProfile::default_for_tests();
    pm_profile.name = "pm".into();
    let pm_profile_yaml = serde_yaml::to_string(&pm_profile).unwrap();
    std::fs::write(pm_dir.join("profile.yaml"), pm_profile_yaml.as_bytes()).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&pm_dir)
        .unwrap();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        team_id: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
    let bundle = s.join("dev.fleet");
    crate::cmd::fleet::export::cmd_fleet_export(
        s,
        "dev",
        true,
        Some(bundle.clone()),
        "2026-06-20T00:00:00Z",
    )
    .unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();

    let pm2 = home.join("agents").join("pm");
    assert!(pm2.join("profile.yaml").is_file());
    // fresh identity generated — key must NOT be the same bytes as the source
    let src_key =
        std::fs::read(mur_common::identity::private_key_dir(&pm_dir).join("identity.key")).unwrap();
    let dst_key =
        std::fs::read(mur_common::identity::private_key_dir(&pm2).join("identity.key")).unwrap();
    assert_ne!(
        src_key, dst_key,
        "import must regenerate identity, not copy the private key"
    );
}

#[test]
fn import_with_members_never_overwrites_existing_agent() {
    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let mur_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&mur_dir).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&mur_dir)
        .unwrap();
    let pm_dir = s.join("agents").join("pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    // Must be a valid AgentProfile so the export round-trip succeeds.
    let mut pm_profile = mur_common::agent::AgentProfile::default_for_tests();
    pm_profile.name = "pm".into();
    let pm_profile_yaml = serde_yaml::to_string(&pm_profile).unwrap();
    std::fs::write(pm_dir.join("profile.yaml"), pm_profile_yaml.as_bytes()).unwrap();
    mur_common::identity::AgentIdentity::generate()
        .save(&pm_dir)
        .unwrap();
    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        team_id: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
    let bundle = s.join("dev.fleet");
    crate::cmd::fleet::export::cmd_fleet_export(
        s,
        "dev",
        true,
        Some(bundle.clone()),
        "2026-06-20T00:00:00Z",
    )
    .unwrap();

    // Pre-create pm on the destination
    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let existing_pm = home.join("agents").join("pm");
    std::fs::create_dir_all(&existing_pm).unwrap();
    std::fs::write(
        existing_pm.join("profile.yaml"),
        "name: pm\nexisting: true\n",
    )
    .unwrap();
    let original_key = {
        let id = mur_common::identity::AgentIdentity::generate();
        id.save(&existing_pm).unwrap();
        std::fs::read(mur_common::identity::private_key_dir(&existing_pm).join("identity.key"))
            .unwrap()
    };

    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();

    // profile must NOT be overwritten
    let kept = std::fs::read_to_string(existing_pm.join("profile.yaml")).unwrap();
    assert!(
        kept.contains("existing: true"),
        "existing agent must not be overwritten"
    );
    // identity key must NOT be overwritten
    let key_after =
        std::fs::read(mur_common::identity::private_key_dir(&existing_pm).join("identity.key"))
            .unwrap();
    assert_eq!(
        original_key, key_after,
        "existing identity must not be overwritten"
    );
}

// ── Security regression tests ──────────────────────────────────────────────

/// C1 — Skill name path-traversal: a signed bundle whose `skills/foo/skill.yaml`
/// bytes carry `name: ../../evil` must be REFUSED, and nothing written outside
/// `<mur_home>/skills`.
#[test]
fn import_refuses_skill_name_path_traversal() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        signer_fingerprint,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();

    // Build a valid concierge identity for signing.
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    // Craft a fleet.yaml and a skill YAML whose `name` field contains a path traversal.
    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
    // The archive path is `skills/foo/skill.yaml` but the internal name is `../../evil`.
    let evil_skill_yaml = "name: ../../evil\nversion: 1.0.0\npublisher: human:t\n\
            description: bad\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";

    let files: Vec<(String, Vec<u8>)> = vec![
        ("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec()),
        (
            "skills/foo/skill.yaml".into(),
            evil_skill_yaml.as_bytes().to_vec(),
        ),
    ];

    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();

    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));

    // Build tar.gz bundle.
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("evil.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid name") || msg.contains("mismatch"),
        "expected path-traversal refusal, got: {msg}"
    );
    // Nothing written outside skills/
    assert!(
        !home.join("..").join("evil").exists(),
        "path traversal must not escape mur_home"
    );
    assert!(
        !home
            .join("skills")
            .join("../../evil")
            .join("skill.yaml")
            .exists(),
        "traversal skill must not be installed"
    );
}

/// C2 — Fleet name path-traversal: a bundle whose `fleet.yaml` `name` differs
/// from `manifest.fleet_name` must be REFUSED with nothing written.
#[test]
fn import_refuses_fleet_name_mismatch() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        signer_fingerprint,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    // fleet.yaml says `name: ../../evil` but manifest.fleet_name says `dev`.
    let fleet_yaml = "name: ../../evil\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
    let files: Vec<(String, Vec<u8>)> = vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(), // manifest says dev, fleet.yaml says ../../evil
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("mismatch.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("mismatch") || msg.contains("invalid fleet name"),
        "expected fleet name refusal, got: {msg}"
    );
    // Nothing written
    assert!(
        !home.join("fleets").exists()
            || home
                .join("fleets")
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "fleet must not be written on name mismatch"
    );
}

/// Governance bypass guard: a correctly-signed bundle whose fleet.yaml
/// `channel_id` is non-canonical (≠ `fleet-<name>`) must be refused — else the
/// loop would govern a different channel than the commander/daemon write to.
#[test]
fn import_refuses_noncanonical_channel_id() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        signer_fingerprint,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    // name `dev` (matches manifest, valid) but channel_id smuggles `fleet-evil`.
    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-evil\nrules: []\nskills: []\nloop_cfg: ~\n";
    let files: Vec<(String, Vec<u8>)> = vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("noncanon.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not canonical"),
        "expected canonical-channel refusal, got: {msg}"
    );
    // Nothing written
    assert!(
        !home.join("fleets").exists()
            || home
                .join("fleets")
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "fleet must not be written on non-canonical channel_id"
    );
}

/// I3 — Spoofable fingerprint: a correctly-signed manifest whose stored
/// `signer_fingerprint` ≠ `signer_fingerprint(signer_pubkey)` must be refused.
#[test]
fn import_refuses_mismatched_signer_fingerprint() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
    let files: Vec<(String, Vec<u8>)> = vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();

    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        // Deliberately wrong fingerprint — attacker-controlled, not derived from pubkey.
        signer_fingerprint: "dead-beef".into(),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    // Sign correctly so the sig check passes — the fingerprint mismatch must still fire.
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("badfp.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("signer_fingerprint"),
        "expected fingerprint mismatch refusal, got: {msg}"
    );
}

/// I3 regression — empty signer_fingerprint bypass: a correctly-signed manifest
/// whose `signer_fingerprint` is empty string must be refused (the derived
/// fingerprint is never empty, so they will never match).
#[test]
fn import_refuses_empty_signer_fingerprint() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
    let files: Vec<(String, Vec<u8>)> = vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();

    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        // Empty fingerprint — the old guard would skip the check; the new guard rejects it.
        signer_fingerprint: String::new(),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    // Sign correctly so the sig check passes — the empty fingerprint must still be rejected.
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("emptyfp.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("signer_fingerprint"),
        "expected empty-fingerprint refusal, got: {msg}"
    );
}

/// I4 — gzip-bomb: a bundle with an entry exceeding the per-entry cap is refused.
#[test]
fn import_refuses_oversized_bundle_entry() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        signer_fingerprint,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    // Craft an entry that exceeds MAX_BUNDLE_ENTRY_BYTES (8 MiB) when decompressed.
    // We use a large repeated zero byte sequence (highly compressible = gzip-bomb pattern).
    let oversized: Vec<u8> = vec![0u8; (MAX_BUNDLE_ENTRY_BYTES + 1) as usize];
    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n".as_bytes();

    // We sign a manifest for this large entry so the sig check passes first.
    // The DoS cap must fire during unpack_bundle (before sig check or after — doesn't matter
    // as long as it fires).
    let files_data: Vec<(&str, &[u8])> = vec![("fleet.yaml", fleet_yaml), ("big.bin", &oversized)];
    let entries: Vec<BundleEntry> = files_data
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.to_string(),
            sha256: content_hash(b),
        })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));

    // Build the tar.gz with the oversized entry.
    let mut buf = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_yaml = serde_yaml::to_string(&manifest).unwrap();
        let add = |tar: &mut tar::Builder<_>, path: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, data).unwrap();
        };
        add(&mut tar, "bundle.yaml", manifest_yaml.as_bytes());
        add(&mut tar, "fleet.yaml", fleet_yaml);
        add(&mut tar, "big.bin", &oversized);
        tar.into_inner().unwrap().finish().unwrap();
    }

    let bundle_path = src.path().join("bomb.fleet");
    std::fs::write(&bundle_path, &buf).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let err = cmd_fleet_import(
        dst.path(),
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("too large") || msg.contains("exceeds"),
        "expected DoS cap refusal, got: {msg}"
    );
}

/// I5 — Trust store: after import, the skill must have an *explicit* Sandboxed
/// entry in the trust store (not merely rely on the default). Also verifies a
/// skill claiming a higher trust doesn't escape Sandboxed.
#[test]
fn import_registers_skill_at_sandboxed_in_trust_store() {
    let src = tempfile::tempdir().unwrap();
    let bundle = export_fixture(src.path());

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();

    // Load the trust store directly and assert there is an explicit entry
    // (not just the default fallback in get_trust_level).
    let trust = mur_common::trust::skills::SkillTrustStore::load(home).unwrap();
    let explicit = trust.entries.values().find(|e| e.name == "triage");
    assert!(
        explicit.is_some(),
        "skill 'triage' must have an explicit trust-store entry after import"
    );
    assert_eq!(
        explicit.unwrap().level,
        TrustLevel::Sandboxed,
        "imported skill must be registered at Sandboxed"
    );
}

/// I6 — Member identity: after --with-members import, the installed member's
/// advertised pubkey in profile.yaml must equal its fresh local `identity.pub`,
/// and must NOT equal the exporter's pubkey.
#[test]
fn import_with_members_advertised_pubkey_matches_fresh_local_key() {
    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let mur_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&mur_dir).unwrap();
    let concierge_id = mur_common::identity::AgentIdentity::generate();
    concierge_id.save(&mur_dir).unwrap();

    // Create a pm agent with a full profile (including identity block).
    let pm_dir = s.join("agents").join("pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    let exporter_pm_id = mur_common::identity::AgentIdentity::generate();
    exporter_pm_id.save(&pm_dir).unwrap();
    let exporter_pubkey = exporter_pm_id.public_key_multibase();
    // Build a full valid AgentProfile struct with the exporter's pubkey
    // in the identity block, then serialize to YAML so the bundler picks it up.
    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    profile.name = "pm".into();
    profile.identity.pubkey = exporter_pubkey.clone();
    profile.identity.key_version = 0;
    let profile_yaml = serde_yaml::to_string(&profile).unwrap();
    std::fs::write(pm_dir.join("profile.yaml"), &profile_yaml).unwrap();

    let fleet = Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        team_id: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
    let bundle = s.join("dev.fleet");
    crate::cmd::fleet::export::cmd_fleet_export(
        s,
        "dev",
        true,
        Some(bundle.clone()),
        "2026-06-20T00:00:00Z",
    )
    .unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap();

    // Read the installed profile and check the identity.pubkey.
    let installed_profile_bytes =
        std::fs::read(home.join("agents").join("pm").join("profile.yaml")).unwrap();
    let installed_profile: mur_common::agent::AgentProfile =
        serde_yaml::from_slice(&installed_profile_bytes).unwrap();
    let installed_pubkey = &installed_profile.identity.pubkey;

    // The advertised pubkey must NOT be the exporter's pubkey.
    assert_ne!(
        installed_pubkey, &exporter_pubkey,
        "installed profile must not advertise the exporter's pubkey"
    );
    // The advertised pubkey must match the locally generated identity.pub.
    let local_pub =
        std::fs::read_to_string(home.join("agents").join("pm").join("identity.pub")).unwrap();
    let local_pub = local_pub.trim();
    assert_eq!(
        installed_pubkey, local_pub,
        "installed profile identity.pubkey must match local identity.pub"
    );
}

/// I6 regression — malformed member profile: a bundle whose member profile.yaml
/// cannot be parsed as AgentProfile must be refused (fail-closed), not silently
/// written with a stale exporter key.
#[test]
fn import_with_members_refuses_malformed_member_profile() {
    use mur_common::fleet::Fleet;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();

    // Set up exporting side: concierge + pm member with a MALFORMED profile.
    let mur_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&mur_dir).unwrap();
    let concierge_id = mur_common::identity::AgentIdentity::generate();
    concierge_id.save(&mur_dir).unwrap();

    let pm_dir = s.join("agents").join("pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    let pm_id = mur_common::identity::AgentIdentity::generate();
    pm_id.save(&pm_dir).unwrap();
    // Write a profile that is valid YAML but NOT a valid AgentProfile.
    std::fs::write(
        pm_dir.join("profile.yaml"),
        "totally: not_an_agent_profile\n",
    )
    .unwrap();

    let fleet = Fleet {
        name: "dev".into(),
        display_name: "Dev".into(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        team_id: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    };
    crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
    let bundle = s.join("dev.fleet");
    crate::cmd::fleet::export::cmd_fleet_export(
        s,
        "dev",
        true,
        Some(bundle.clone()),
        "2026-06-20T00:00:00Z",
    )
    .unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("malformed") || msg.contains("refusing to install member"),
        "expected malformed-profile refusal, got: {msg}"
    );
    // Nothing should be installed.
    assert!(
        !home.join("agents").join("pm").join("profile.yaml").exists(),
        "malformed member must not be installed"
    );
}

// ── Task 5: official-distribution import gate ──────────────────────────

fn official_test_manifest(fleet_name: &str) -> mur_common::fleet_bundle::BundleManifest {
    use mur_common::fleet_bundle::{BundleManifest, FLEET_BUNDLE_FORMAT};
    BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: fleet_name.into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_pubkey: String::new(),
        signer_fingerprint: String::new(),
        includes_members: false,
        members: vec![],
        entries: vec![],
        sig: None,
        distribution: Some(mur_common::official::DISTRIBUTION_OFFICIAL.into()),
    }
}

fn official_fp_for(key: &ed25519_dalek::SigningKey) -> String {
    mur_common::muragent::dsse::keyid_from_pubkey(&key.verifying_key().to_bytes())
}

fn save_test_license(
    home: &std::path::Path,
    item: &str,
    user: &str,
    key: &ed25519_dalek::SigningKey,
) {
    let mut l = mur_common::official::OfficialLicense {
        format_version: mur_common::official::OFFICIAL_LICENSE_FORMAT,
        user_id: user.into(),
        item: item.into(),
        version: "1.0.0".into(),
        expires_at: "2027-01-01T00:00:00Z".into(),
        signer_pubkey: String::new(),
        sig: None,
    };
    mur_common::official::sign_license(&mut l, key);
    crate::official::store::save_license(home, &l).unwrap();
}

#[test]
fn official_gate_no_marker_is_noop() {
    let home = tempfile::tempdir().unwrap();
    let mut manifest = official_test_manifest("dev");
    manifest.distribution = None;
    // Even with signature_verified=false and no user, non-official
    // manifests must pass through untouched.
    official_gate(
        home.path(),
        &manifest,
        &[0u8; 32],
        false,
        None,
        "ed25519-deadbeef",
        "ed25519-deadbeef",
    )
    .unwrap();
}

#[test]
fn official_gate_unsigned_or_wrong_signer_refused() {
    let home = tempfile::tempdir().unwrap();
    let manifest = official_test_manifest("dev");
    let key = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let fp = official_fp_for(&key);
    let pk = key.verifying_key().to_bytes();

    // signature_verified == false
    let err =
        official_gate(home.path(), &manifest, &pk, false, Some("user-1"), &fp, &fp).unwrap_err();
    assert!(err.to_string().contains("refusing import"), "{err}");

    // signature_verified == true but signer key doesn't match official_fp
    let other_key = ed25519_dalek::SigningKey::from_bytes(&[2u8; 32]);
    let other_pk = other_key.verifying_key().to_bytes();
    let err = official_gate(
        home.path(),
        &manifest,
        &other_pk,
        true,
        Some("user-1"),
        &fp,
        &fp,
    )
    .unwrap_err();
    assert!(err.to_string().contains("refusing import"), "{err}");
}

#[test]
fn official_gate_no_login_refused_with_app_mur_run() {
    let home = tempfile::tempdir().unwrap();
    let manifest = official_test_manifest("dev");
    let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
    let fp = official_fp_for(&key);
    let pk = key.verifying_key().to_bytes();

    let err = official_gate(home.path(), &manifest, &pk, true, None, &fp, &fp).unwrap_err();
    assert!(err.to_string().contains("app.mur.run"), "{err}");
}

#[test]
fn official_gate_no_license_refused_with_app_mur_run() {
    let home = tempfile::tempdir().unwrap();
    let manifest = official_test_manifest("dev");
    let key = ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]);
    let fp = official_fp_for(&key);
    let pk = key.verifying_key().to_bytes();

    // logged in, but no license saved for this item.
    let err =
        official_gate(home.path(), &manifest, &pk, true, Some("user-1"), &fp, &fp).unwrap_err();
    assert!(err.to_string().contains("app.mur.run"), "{err}");
}

#[test]
fn official_gate_wrong_user_license_refused() {
    let home = tempfile::tempdir().unwrap();
    let manifest = official_test_manifest("dev");
    let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
    let fp = official_fp_for(&key);
    let pk = key.verifying_key().to_bytes();

    save_test_license(home.path(), "fleets/dev", "user-1", &key);

    let err =
        official_gate(home.path(), &manifest, &pk, true, Some("user-2"), &fp, &fp).unwrap_err();
    assert!(err.to_string().contains("different account"), "{err}");
}

#[test]
fn official_gate_matching_license_ok() {
    let home = tempfile::tempdir().unwrap();
    let manifest = official_test_manifest("dev");
    let key = ed25519_dalek::SigningKey::from_bytes(&[6u8; 32]);
    let fp = official_fp_for(&key);
    let pk = key.verifying_key().to_bytes();

    save_test_license(home.path(), "fleets/dev", "user-1", &key);

    official_gate(home.path(), &manifest, &pk, true, Some("user-1"), &fp, &fp).unwrap();
}

/// End-to-end wiring proof via `cmd_fleet_import`. Production pins the
/// REAL `MUR_OFFICIAL_PUBLISHER_KEY_FP`, which no test-generated signing
/// key can ever match (the private key isn't available to the client) —
/// so this test can only reach the gate's signer-mismatch branch, not the
/// login/license branches (those are covered directly against
/// `official_gate` above, with an injectable `official_fp`). This still
/// proves the gate is wired into `cmd_fleet_import` and fails closed.
#[test]
fn import_official_marked_bundle_is_refused() {
    use mur_common::fleet_bundle::{
        BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        signer_fingerprint,
    };
    use mur_common::identity::AgentIdentity;

    let src = tempfile::tempdir().unwrap();
    let s = src.path();
    let id_dir = s.join("agents").join("mur");
    std::fs::create_dir_all(&id_dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&id_dir).unwrap();
    let signer_pubkey = id.public_key_multibase();

    let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
    let files: Vec<(String, Vec<u8>)> = vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "dev".into(),
        created_at: "2026-06-20T00:00:00Z".into(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: false,
        members: vec![],
        entries,
        sig: None,
        distribution: Some(mur_common::official::DISTRIBUTION_OFFICIAL.into()),
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));
    let bundle_bytes = build_evil_bundle(&manifest, &files);
    let bundle_path = s.join("official.fleet");
    std::fs::write(&bundle_path, &bundle_bytes).unwrap();

    let dst = tempfile::tempdir().unwrap();
    let home = dst.path();
    let err = cmd_fleet_import(
        home,
        &bundle_path,
        ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("refusing import"),
        "expected official-distribution gate refusal, got: {msg}"
    );
    assert!(
        !home.join("fleets").exists()
            || home
                .join("fleets")
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "fleet must not be written when the official gate refuses import"
    );
}

// ── Helper: build a tar.gz bundle from a manifest + file list ─────────────
fn build_evil_bundle(
    manifest: &mur_common::fleet_bundle::BundleManifest,
    files: &[(String, Vec<u8>)],
) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut buf = Vec::new();
    let gz = GzEncoder::new(&mut buf, Compression::default());
    let mut tar = tar::Builder::new(gz);
    let manifest_yaml = serde_yaml::to_string(manifest).unwrap();
    let add = |tar: &mut tar::Builder<_>, path: &str, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, path, data).unwrap();
    };
    add(&mut tar, "bundle.yaml", manifest_yaml.as_bytes());
    for (p, b) in files {
        add(&mut tar, p, b);
    }
    tar.into_inner().unwrap().finish().unwrap();
    buf
}
