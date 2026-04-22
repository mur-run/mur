use mur_agent_runtime::export::pkg::export_to_pkg;
use mur_agent_runtime::import::{ImportOptions, import_pkg};
use mur_common::AgentProfile;
use tempfile::TempDir;

fn write_minimal_agent(agent_home: &std::path::Path) {
    std::fs::create_dir_all(agent_home).unwrap();
    std::fs::write(
        agent_home.join("profile.yaml"),
        include_str!("fixtures/profile_minimal.yaml"),
    )
    .unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "prompt body").unwrap();
    std::fs::create_dir_all(agent_home.join("skills")).unwrap();
    std::fs::write(agent_home.join("skills/research.md"), "skill body").unwrap();
}

#[test]
fn import_roundtrip_new_uuid_same_name() {
    let src = TempDir::new().unwrap();
    let src_agent = src.path().join("agent_a");
    write_minimal_agent(&src_agent);
    let pkg = src.path().join("agent_a.murpkg");
    export_to_pkg(&src_agent, &pkg).expect("export");

    let original: AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(src_agent.join("profile.yaml")).unwrap())
            .unwrap();

    let dest_home = TempDir::new().unwrap();
    let report = import_pkg(&pkg, dest_home.path(), ImportOptions::default()).expect("import");
    assert_eq!(report.installed_name, "agent_a");

    let imported_dir = dest_home.path().join("agents/agent_a");
    assert!(imported_dir.exists(), "agent dir should exist");
    assert!(imported_dir.join("sys_prompt.md").exists());
    assert!(imported_dir.join("skills/research.md").exists());

    let imported: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(imported_dir.join("profile.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(imported.name, "agent_a");
    assert_ne!(
        imported.id, original.id,
        "imported agent must get a fresh UUID"
    );
    let u = uuid::Uuid::parse_str(&imported.id).unwrap();
    assert_eq!(u.get_version_num(), 7, "new id must be UUIDv7");
}

#[test]
fn import_with_as_renames_and_detects_missing_mcp() {
    let src = TempDir::new().unwrap();
    let src_agent = src.path().join("agent_a");
    write_minimal_agent(&src_agent);
    // Add a fake MCP entry to profile before export so the prereq warning fires.
    let mut yaml: AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(src_agent.join("profile.yaml")).unwrap())
            .unwrap();
    yaml.mcp_servers.push(mur_common::agent::McpServerEntry {
        name: "ghost".into(),
        command: "/absolutely/not/a/real/binary-xyz123".into(),
        args: vec![],
    });
    std::fs::write(
        src_agent.join("profile.yaml"),
        serde_yaml_ng::to_string(&yaml).unwrap(),
    )
    .unwrap();
    let pkg = src.path().join("agent_a.murpkg");
    export_to_pkg(&src_agent, &pkg).unwrap();

    let dest_home = TempDir::new().unwrap();
    let report = import_pkg(
        &pkg,
        dest_home.path(),
        ImportOptions {
            rename: Some("agent_renamed".into()),
        },
    )
    .expect("import");
    assert_eq!(report.installed_name, "agent_renamed");
    assert!(
        report
            .missing_prerequisites
            .iter()
            .any(|b| b.contains("binary-xyz123")),
        "missing prereq not reported: {:?}",
        report.missing_prerequisites
    );
    let imported_dir = dest_home.path().join("agents/agent_renamed");
    assert!(imported_dir.exists());
    let imported: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(imported_dir.join("profile.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(imported.name, "agent_renamed");
}
