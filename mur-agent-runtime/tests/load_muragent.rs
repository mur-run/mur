//! `--load` materializes an agent home from a `.muragent`. Mirrors the
//! supervisor's load path (read archive → installer::install → agents/<slug>).

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::installer;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

#[test]
fn load_muragent_materializes_agent_home() {
    let tmp = tempfile::TempDir::new().unwrap();
    let pkg = tmp.path().join("coach.muragent");
    let mur_home = tmp.path().join("murhome");

    // Author a minimal signed .muragent.
    let mut profile = AgentProfile::default_for_tests();
    profile.name = "coach".into();
    profile.display_name = "Coach".into();
    let identity = AgentIdentity::generate();
    let manifest = build_manifest_from_profile(&profile, "1.0.0");
    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    MuragentWriter::new(manifest, profile_yaml, identity)
        .write(&pkg)
        .unwrap();

    // The load path: read + install into mur_home/agents/<slug>.
    // Direct TrustStore to the temp dir so it sees an empty trust store
    // (a new key without a rotation manifest is accepted on first install).
    // SAFETY: single-threaded test startup, no other code reads MUR_HOME
    // concurrently during this test.
    unsafe { std::env::set_var("MUR_HOME", &mur_home); }
    let archive = MuragentArchive::read(&pkg).unwrap();
    let outcome = installer::install(&archive, &mur_home, "cli").unwrap();
    assert_eq!(outcome.manifest.agent.slug, "coach");

    let home = mur_home.join("agents").join("coach");
    assert!(home.join("profile.yaml").exists(), "profile.yaml extracted");
}
