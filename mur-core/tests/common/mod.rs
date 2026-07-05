//! Test-only: filesystem git repo that behaves like the real mur skill registry.
//! Tests clone it via `git clone file://...` exactly as production does.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

pub struct TestRegistry {
    dir: TempDir,
}

impl TestRegistry {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "-b", "main"]);
        run(dir.path(), &["config", "user.email", "test@mur.local"]);
        run(dir.path(), &["config", "user.name", "test"]);
        let index_yaml = "schema_version: 1\nupdated_at: 2026-01-01T00:00:00Z\nskills: {}\n";
        std::fs::write(dir.path().join("index.yaml"), index_yaml).unwrap();
        Self { dir }
    }

    /// Publish a skill version. Each call adds a new `versions/<v>.yaml` and
    /// updates `index.yaml`'s `latest` to this version.
    pub fn publish(&self, name: &str, version: &str, requires: &[(&str, &str)]) {
        let vdir = self.dir.path().join("skills").join(name).join("versions");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join(format!("{version}.yaml")),
            build_skill_yaml(name, version, requires),
        )
        .unwrap();
        bump_index_latest(self.dir.path(), name, version);
    }

    /// `git add . && git commit` so the repo has a real HEAD that `git clone` can fetch.
    pub fn commit(&self) {
        run(self.dir.path(), &["add", "."]);
        run(self.dir.path(), &["commit", "-q", "-m", "test fixture"]);
    }

    /// `file://` URL accepted by `git clone` on macOS/Linux/Windows alike.
    pub fn url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }
}

fn run(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git available");
    assert!(st.success(), "git {:?} failed in {}", args, cwd.display());
}

fn build_skill_yaml(name: &str, version: &str, requires: &[(&str, &str)]) -> String {
    let mut s = format!(
        "name: {name}\nversion: {version}\npublisher: human:test\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  context: b\n"
    );
    if !requires.is_empty() {
        s.push_str("requires:\n");
        for (n, v) in requires {
            s.push_str(&format!("  - name: {n}\n    version: \"{v}\"\n"));
        }
    }
    s
}

fn bump_index_latest(reg_root: &Path, name: &str, version: &str) {
    use mur_common::skill::registry::{RegistryIndex, RegistrySkillEntry};
    let p = reg_root.join("index.yaml");
    let mut idx: RegistryIndex =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let entry = idx.skills.entry(name.into()).or_insert(RegistrySkillEntry {
        latest: version.into(),
        description: "test".into(),
        publisher: "human:test".into(),
        category: "context".into(),
        tags: vec![],
        content_sha256: String::new(),
        install_count: 0,
        recommended_roles: vec![],
    });
    entry.latest = version.into();
    std::fs::write(&p, idx.to_yaml().unwrap()).unwrap();
}
