use mur_common::agent::AgentProfile;

#[test]
fn legacy_p0a_profile_loads_with_empty_identity() {
    // Reach across crates to the mur-common test fixture. If the relative
    // path doesn't work under cargo's test invocation, inline a minimal YAML
    // directly.
    let yaml = std::fs::read_to_string(
        "../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
    ).unwrap_or_else(|_| {
        // Fallback: read via CARGO_MANIFEST_DIR if relative failed
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
        std::fs::read_to_string(p).expect("fixture must exist")
    });
    let p: AgentProfile = serde_yaml::from_str(&yaml).unwrap();
    assert!(p.identity.pubkey.is_empty(), "legacy P0a profile must default to empty pubkey");
    assert!(p.identity.owner.is_none(), "legacy P0a profile must default to no owner");
}
