use super::*;
use tempfile::TempDir;

fn make_template(dir: &Path) {
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(dir.join("profile.yaml"), "name: Mur\n").unwrap();
    std::fs::write(dir.join("sys_prompt.md"), "# Mur\n").unwrap();
    std::fs::write(dir.join("skills/concierge.yaml"), "name: concierge\n").unwrap();
}

#[test]
fn seeds_when_missing() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    assert!(home.path().join("agents/mur/profile.yaml").exists());
    assert!(
        home.path()
            .join("agents/mur/skills/concierge.yaml")
            .exists()
    );
}

#[test]
fn seeds_authoring_dirs_so_the_template_grants_are_not_dead() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    for d in AUTHORING_DIRS {
        assert!(
            home.path().join(d).is_dir(),
            "<mur_home>/{d} must exist at seed time or the sandbox drops the \
             template's write grant for it and the concierge boots unable to author"
        );
    }
}

/// The concierge is the one agent every install has, and it holds bash.
/// Its template must not ship with an empty deny list; this pins it to the
/// same `DEFAULT_DENY_PATHS` the agent wizard uses.
#[test]
fn template_denies_the_default_credential_dirs() {
    let tpl = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources/mur-agent-template/profile.yaml");
    let yaml = std::fs::read_to_string(&tpl).unwrap();
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let deny: Vec<String> = v["entitlements"]["filesystem"]["deny"]
        .as_sequence()
        .expect("template must declare filesystem.deny")
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    let want: Vec<String> = mur_common::agent::DEFAULT_DENY_PATHS
        .iter()
        .map(|d| (*d).to_string())
        .collect();
    assert_eq!(deny, want);
}

/// The template is YAML and `AUTHORING_DIRS` is Rust; nothing but this test
/// stops them drifting into "granted but never created" (the original bug).
#[test]
fn template_write_grants_match_authoring_dirs() {
    let tpl = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources/mur-agent-template/profile.yaml");
    let yaml = std::fs::read_to_string(&tpl).unwrap();
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let writes: Vec<String> = v["entitlements"]["filesystem"]["write"]
        .as_sequence()
        .expect("template must declare filesystem.write")
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    let reads: Vec<String> = v["entitlements"]["filesystem"]["read"]
        .as_sequence()
        .expect("template must declare filesystem.read")
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();

    let expected: Vec<String> = AUTHORING_DIRS
        .iter()
        .map(|d| format!("~/.mur/{d}"))
        .collect();
    assert_eq!(writes, expected, "template write grants drifted");
    assert_eq!(
        reads, expected,
        "template read grants drifted — every granted path must be one the \
         seed creates, or the sandbox drops it at seal time"
    );

    // A blanket `~/.mur` read hands over every other agent's identity.key
    // (Ed25519 signing key); `~/.mur/agents` write lets this agent author a
    // sibling profile with unrestricted entitlements and start it.
    for bad in ["~/.mur", "~/.mur/agents", "~/.mur/"] {
        assert!(
            !reads.contains(&bad.to_string()),
            "read grant {bad} escapes"
        );
        assert!(
            !writes.contains(&bad.to_string()),
            "write grant {bad} escapes"
        );
    }
    // `fleets/` write lets this agent rewrite the members, limits and HITL
    // pre-approvals of fleets it can trigger, or delete their `.stopped`.
    assert!(
        !writes.contains(&"~/.mur/fleets".to_string()),
        "write grant ~/.mur/fleets lets the concierge govern its own fleets"
    );
}

#[test]
fn seeds_fleets_dir_for_the_runtimes_own_read() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    assert!(
        home.path().join(mur_common::paths::FLEETS).is_dir(),
        "the runtime's fleets/ read is existence-checked at seal time"
    );
}

fn make_dir_skill_template(dir: &Path) {
    std::fs::create_dir_all(dir.join("skills/concierge")).unwrap();
    std::fs::create_dir_all(dir.join("skills/brainstorming")).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: Mur\nskills:\n  - skills/concierge\n  - skills/brainstorming\n",
    )
    .unwrap();
    std::fs::write(dir.join("sys_prompt.md"), "# Mur\n").unwrap();
    std::fs::write(dir.join("skills/concierge/skill.yaml"), "name: concierge\n").unwrap();
    std::fs::write(
        dir.join("skills/brainstorming/skill.yaml"),
        "name: brainstorming\n",
    )
    .unwrap();
}

#[test]
fn seeds_missing_skill_into_existing_agent() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    // Agent already seeded with only the concierge skill (pre-upgrade state).
    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(agent_dir.join("skills/concierge")).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: Mur\nskills:\n  - skills/concierge\n",
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("skills/concierge/skill.yaml"),
        "name: concierge\n",
    )
    .unwrap();

    let seeded = seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    assert_eq!(seeded, vec!["brainstorming".to_string()]);
    assert!(agent_dir.join("skills/brainstorming/skill.yaml").exists());
    let profile = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(profile.contains("- skills/brainstorming"));
}

#[test]
fn never_overwrites_existing_skill() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(agent_dir.join("skills/brainstorming")).unwrap();
    std::fs::create_dir_all(agent_dir.join("skills/concierge")).unwrap();
    std::fs::write(
        agent_dir.join("skills/concierge/skill.yaml"),
        "name: concierge\n",
    )
    .unwrap();
    let sentinel = "name: brainstorming\n# USER EDIT\n";
    std::fs::write(agent_dir.join("skills/brainstorming/skill.yaml"), sentinel).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: Mur\nskills:\n  - skills/concierge\n  - skills/brainstorming\n",
    )
    .unwrap();

    let seeded = seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    assert_eq!(seeded, Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(agent_dir.join("skills/brainstorming/skill.yaml")).unwrap(),
        sentinel
    );
}

#[test]
fn idempotent_profile_reference() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(agent_dir.join("skills/concierge")).unwrap();
    std::fs::write(
        agent_dir.join("skills/concierge/skill.yaml"),
        "name: concierge\n",
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: Mur\nskills:\n  - skills/concierge\n",
    )
    .unwrap();

    seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    let profile = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert_eq!(profile.matches("- skills/brainstorming").count(), 1);
}

#[test]
fn appends_into_inline_empty_skills_list() {
    // Older seeders wrote `skills: []` (inline flow list). Must not panic.
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: Mur\nskills: []\ntransport:\n  stdio: true\n",
    )
    .unwrap();

    let seeded = seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    assert_eq!(seeded.len(), 2);
    let profile = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(!profile.contains("skills: []"), "profile: {profile}");
    assert!(
        profile.contains("skills:\n  - skills/"),
        "profile: {profile}"
    );
    assert!(profile.contains("- skills/concierge"), "profile: {profile}");
    assert!(
        profile.contains("- skills/brainstorming"),
        "profile: {profile}"
    );
    assert!(
        profile.contains("transport:\n  stdio: true"),
        "profile: {profile}"
    );
}

#[test]
fn appends_into_inline_nonempty_skills_list() {
    // Hand-edited flow list: keep existing entries, no duplicates, no panic.
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(agent_dir.join("skills/concierge")).unwrap();
    std::fs::write(
        agent_dir.join("skills/concierge/skill.yaml"),
        "name: concierge\n",
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: Mur\nskills: [skills/concierge]\n",
    )
    .unwrap();

    seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    let profile = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert_eq!(
        profile.matches("- skills/concierge").count(),
        1,
        "profile: {profile}"
    );
    assert_eq!(
        profile.matches("- skills/brainstorming").count(),
        1,
        "profile: {profile}"
    );
    assert!(!profile.contains('['), "profile: {profile}");
}

#[test]
fn appends_skills_section_when_key_missing() {
    // A profile with no skills key at all must not panic the Hub.
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap();
    make_dir_skill_template(tpl.path());

    let agent_dir = home.path().join("agents/mur");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("profile.yaml"), "name: Mur\n").unwrap();

    seed_missing_bundled_skills(tpl.path(), home.path()).unwrap();
    let profile = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(
        profile.contains("skills:\n  - skills/"),
        "profile: {profile}"
    );
}

#[test]
fn seeds_even_when_other_agents_exist() {
    // Existing users with other agents must still get the built-in MUR.
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join("agents/other")).unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    assert!(home.path().join("agents/mur/profile.yaml").exists());
}

#[test]
fn skips_when_mur_already_seeded() {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
    std::fs::write(home.path().join("agents/mur/profile.yaml"), "name: Mur\n").unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(!seed_mur_if_missing(tpl.path(), home.path()).unwrap());
}

#[test]
fn heals_broken_empty_mur_dir() {
    // A previous run left an empty agents/mur (no profile.yaml) — re-seed it.
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
    let tpl = TempDir::new().unwrap();
    make_template(tpl.path());
    assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    assert!(home.path().join("agents/mur/profile.yaml").exists());
}

#[test]
fn bundled_template_concierge_skill_is_loadable() {
    // Regression guard: the REAL bundled template must seed a concierge skill
    // in the loader's directory layout (skills/<name>/skill.yaml) with a
    // valid, current-schema manifest — not the old flat skills/concierge.yaml
    // (which the loader silently skipped, so the seeded concierge had no
    // loadable skills).
    use mur_common::skill::{read_from_dir, validate};
    let tpl = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/mur-agent-template");
    assert!(
        !tpl.join("skills/concierge.yaml").exists(),
        "legacy flat concierge.yaml must be gone"
    );
    let profile = std::fs::read_to_string(tpl.join("profile.yaml")).unwrap();
    assert!(
        profile.contains("- skills/concierge"),
        "profile must reference the dir-layout skill"
    );
    let skill = read_from_dir(&tpl.join("skills/concierge")).expect("concierge skill loads");
    validate(&skill).expect("concierge skill validates");
    assert_eq!(skill.name, "concierge");
}

#[test]
fn bundled_template_brainstorming_skill_is_loadable_and_stamped() {
    // Regression guard: the bundled brainstorming skill loads, validates,
    // and carries the origin stamp the upgrade pipeline needs.
    use mur_common::skill::{content_hash_for_origin, read_from_dir, validate};
    let tpl = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/mur-agent-template");
    let profile = std::fs::read_to_string(tpl.join("profile.yaml")).unwrap();
    assert!(
        profile.contains("- skills/brainstorming"),
        "profile must reference the brainstorming skill"
    );
    let skill =
        read_from_dir(&tpl.join("skills/brainstorming")).expect("brainstorming skill loads");
    validate(&skill).expect("brainstorming skill validates");
    assert_eq!(skill.name, "brainstorming");
    assert_eq!(
        skill.origin.as_deref(),
        Some("registry:mur-official/brainstorming")
    );
    assert_eq!(
        skill.origin_version.as_deref(),
        Some(skill.version.as_str())
    );
    assert_eq!(
        skill.origin_hash.as_deref(),
        Some(content_hash_for_origin(&skill).unwrap().as_str())
    );
}

#[test]
fn missing_template_errors_without_creating_dst() {
    let home = TempDir::new().unwrap();
    let tpl = TempDir::new().unwrap(); // empty: no profile.yaml
    assert!(seed_mur_if_missing(tpl.path(), home.path()).is_err());
    assert!(!home.path().join("agents/mur").exists());
}

#[test]
fn upsert_model_ref_inserts_before_model_block() {
    let yaml = "name: mur\nmodel:\n  provider: local\n  name: X\ntransport:\n  stdio: true\n";
    let out = upsert_model_ref(yaml, "claude_sonnet");
    assert!(
        out.contains("model_ref: claude_sonnet\nmodel:"),
        "got:\n{out}"
    );
    // Idempotent: applying again replaces, does not duplicate.
    let again = upsert_model_ref(&out, "claude_opus");
    assert_eq!(again.matches("model_ref:").count(), 1, "got:\n{again}");
    assert!(again.contains("model_ref: claude_opus"));
}

#[test]
fn upsert_model_ref_replaces_existing() {
    let yaml = "name: mur\nmodel_ref: old_alias\nmodel:\n  provider: local\n";
    let out = upsert_model_ref(yaml, "new_alias");
    assert_eq!(out.matches("model_ref:").count(), 1);
    assert!(out.contains("model_ref: new_alias"));
}

#[test]
fn bundled_profile_deserializes() {
    // The real template lives next to the crate; ensure it parses as a profile.
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/mur-agent-template/profile.yaml"
    );
    let body = std::fs::read_to_string(p).unwrap();
    let _profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&body).expect("seed profile must deserialize");
}
