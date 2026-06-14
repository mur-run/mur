// Windows: gated — depends on `mur agent create` (unix symlink).
#![cfg(unix)]

use mur_common::AgentProfile;
use std::process::Command;
use tempfile::TempDir;

fn mur_create(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read_profile(mur_home: &std::path::Path, name: &str) -> AgentProfile {
    let yaml =
        std::fs::read_to_string(mur_home.join("agents").join(name).join("profile.yaml")).unwrap();
    serde_yaml_ng::from_str(&yaml).unwrap()
}

fn run(mur_home: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mur = env!("CARGO_BIN_EXE_mur");
    Command::new(mur)
        .env("MUR_HOME", mur_home)
        .args(args)
        .output()
        .expect("spawn mur")
}

/// Canonical-YAML skill source body (valid manifest, name = "research").
fn valid_yaml_skill() -> &'static str {
    "name: research\n\
     version: 1.0.0\n\
     publisher: human:t\n\
     description: research helper\n\
     category: context\n\
     content:\n  \
       abstract: research things\n  \
       context: how to research things\n"
}

/// Markdown-frontmatter skill source body (valid manifest, name = "research").
fn valid_md_skill() -> &'static str {
    "---\n\
     name: research\n\
     version: 1.0.0\n\
     publisher: human:t\n\
     description: research helper\n\
     category: context\n\
     ---\n\
     How to research things thoroughly.\n"
}

#[test]
fn skill_add_installs_subdir_and_appends_id() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.yaml");
    std::fs::write(&skill_src, valid_yaml_skill()).unwrap();

    let out = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // New, correct behavior: a per-skill subdir holding canonical skill.yaml,
    // named after the manifest (not the file basename).
    let dest = mur_home
        .path()
        .join("agents/agent_x/skills/research/skill.yaml");
    assert!(
        dest.exists(),
        "skill should be installed as a per-skill subdir with skill.yaml"
    );
    // And NO flat dead file at skills/<basename>.
    let flat = mur_home.path().join("agents/agent_x/skills/research.yaml");
    assert!(!flat.exists(), "no flat file should be created");

    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(p.skills, vec!["skills/research".to_string()]);
}

/// The added skill must actually be loadable by the runtime loader (the bug:
/// flat files were registered but never enumerated). Covers BOTH a `.yaml`
/// input and a `.md` input — each must land at `skills/<name>/skill.yaml` and
/// be enumerated + parsed by the loader.
#[test]
fn added_skill_is_loadable() {
    for (filename, body) in [
        ("research.yaml", valid_yaml_skill()),
        ("research.md", valid_md_skill()),
    ] {
        let mur_home = TempDir::new().unwrap();
        let bin_dir = TempDir::new().unwrap();
        mur_create(mur_home.path(), bin_dir.path(), "agent_x");
        let src = TempDir::new().unwrap();
        let skill_src = src.path().join(filename);
        std::fs::write(&skill_src, body).unwrap();

        let out = run(
            mur_home.path(),
            &[
                "agent",
                "skill",
                "add",
                "agent_x",
                skill_src.to_str().unwrap(),
            ],
        );
        assert!(
            out.status.success(),
            "add {filename} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The loader enumerates subdirs of agents/<agent>/skills.
        let names = mur_common::skill::local::list_installed_agent(mur_home.path(), "agent_x")
            .expect("list_installed_agent");
        assert!(
            names.contains(&"research".to_string()),
            "loader should enumerate the added skill for {filename}, got {names:?}"
        );

        // And it must parse via the loader's read path.
        let m =
            mur_common::skill::local::load_installed_agent(mur_home.path(), "agent_x", "research")
                .expect("load_installed_agent should parse the installed skill");
        assert_eq!(m.name, "research", "loaded manifest name for {filename}");
    }
}

/// A plain markdown file that is not a valid skill manifest must be rejected
/// with an error, and nothing should be written.
#[test]
fn add_rejects_non_manifest_md() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("notes.md");
    std::fs::write(&skill_src, "# Just some notes\nNo frontmatter here.").unwrap();

    let out = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );
    assert!(
        !out.status.success(),
        "adding a non-manifest .md should fail, stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Nothing registered, nothing written.
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.skills.is_empty(), "profile should have no skills");
    let skills_dir = mur_home.path().join("agents/agent_x/skills");
    if skills_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&skills_dir).unwrap().collect();
        assert!(
            entries.is_empty(),
            "no skill artifacts should be written, found {entries:?}"
        );
    }
}

#[test]
fn skill_list_show_remove_roundtrip() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.yaml");
    std::fs::write(&skill_src, valid_yaml_skill()).unwrap();
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );

    let list_out = run(mur_home.path(), &["agent", "skill", "list", "agent_x"]);
    assert!(list_out.status.success());
    let body = String::from_utf8(list_out.stdout).unwrap();
    assert!(body.contains("skills/research"));

    let show_out = run(
        mur_home.path(),
        &["agent", "skill", "show", "agent_x", "skills/research"],
    );
    assert!(
        show_out.status.success(),
        "{}",
        String::from_utf8_lossy(&show_out.stderr)
    );
    let shown = String::from_utf8(show_out.stdout).unwrap();
    assert!(shown.contains("research"));

    let rm_out = run(
        mur_home.path(),
        &["agent", "skill", "remove", "agent_x", "skills/research"],
    );
    assert!(
        rm_out.status.success(),
        "{}",
        String::from_utf8_lossy(&rm_out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.skills.is_empty());
    let dest = mur_home
        .path()
        .join("agents/agent_x/skills/research/skill.yaml");
    assert!(!dest.exists(), "orphaned skill should be deleted");
}

/// E3 regression: `skill show` and `skill remove` accept the basename
/// (`research`) and the stem in addition to the full stored id
/// (`skills/research`).
#[test]
fn skill_show_and_remove_accept_basename_and_stem() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.yaml");
    std::fs::write(&skill_src, valid_yaml_skill()).unwrap();
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );

    // show by basename (last path component of the stored id `skills/research`)
    let out = run(
        mur_home.path(),
        &["agent", "skill", "show", "agent_x", "research"],
    );
    assert!(
        out.status.success(),
        "show by basename failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("research"));

    // remove by basename
    let out = run(
        mur_home.path(),
        &["agent", "skill", "remove", "agent_x", "research"],
    );
    assert!(
        out.status.success(),
        "remove by basename failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.skills.is_empty(), "skill should be removed");
}
