use flate2::read::GzDecoder;
use mur_agent_runtime::export::pkg::export_to_pkg;
use std::collections::HashSet;
use std::io::Read;
use tar::Archive;
use tempfile::TempDir;

fn write_minimal_agent(agent_home: &std::path::Path) {
    std::fs::create_dir_all(agent_home).unwrap();
    std::fs::write(
        agent_home.join("profile.yaml"),
        include_str!("fixtures/profile_minimal.yaml"),
    )
    .unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "test prompt").unwrap();
    std::fs::create_dir_all(agent_home.join("skills")).unwrap();
    std::fs::write(agent_home.join("skills/research.md"), "skill body").unwrap();
}

#[test]
fn export_pkg_writes_tar_gz_with_expected_members() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agent_a");
    write_minimal_agent(&agent_home);
    let out_path = tmp.path().join("agent_a.murpkg");

    export_to_pkg(&agent_home, &out_path).expect("export");
    assert!(out_path.exists(), "package file should exist");

    let bytes = std::fs::read(&out_path).unwrap();
    let mut archive = Archive::new(GzDecoder::new(bytes.as_slice()));
    let mut members: HashSet<String> = HashSet::new();
    let mut manifest = String::new();
    for entry in archive.entries().unwrap() {
        let mut e = entry.unwrap();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        if path == "manifest.yaml" {
            e.read_to_string(&mut manifest).unwrap();
        }
        members.insert(path);
    }
    assert!(members.contains("manifest.yaml"), "members={members:?}");
    assert!(members.contains("profile.yaml"));
    assert!(members.contains("sys_prompt.md"));
    assert!(members.contains("skills/research.md"));
    assert!(members.contains("README.md"));

    let mv: serde_yaml_ng::Value = serde_yaml_ng::from_str(&manifest).unwrap();
    assert_eq!(mv["schema"].as_str(), Some("mur-agent-package/1"));
    assert!(mv["original_uuid"].as_str().is_some());
    assert!(mv["exported_at"].as_str().is_some());
    assert!(mv["sanitized"]["removed_fields"].is_sequence());
}
