use std::collections::BTreeSet;

use super::*;
use crate::cmd::code_nav::wizard::names::Existing;
use crate::cmd::code_nav::wizard::prompt::{Answer, Script};
use crate::cmd::code_nav::wizard::{AgentCtx, Project};

fn agent(name: &str, known: &[(&str, &Path)]) -> AgentCtx {
    AgentCtx {
        name: name.into(),
        home: PathBuf::new(),
        existing: Existing {
            servers: known.iter().map(|(n, _)| (*n).to_owned()).collect(),
            known: known
                .iter()
                .map(|(n, p)| ((*n).to_owned(), p.to_path_buf()))
                .collect(),
            unknown: Vec::new(),
        },
        manifest_path: PathBuf::new(),
        prior: Default::default(),
    }
}

#[test]
fn refuses_root_home_and_mur_home() {
    let t = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(t.path()).unwrap();
    let mur = home.join(".mur");
    std::fs::create_dir_all(mur.join("x")).unwrap();
    std::fs::create_dir_all(home.join("repo")).unwrap();
    let chk = |p: &Path| checked_path(p, Some(&home), &mur);
    assert!(
        chk(Path::new("/"))
            .unwrap_err()
            .to_string()
            .contains("root")
    );
    assert!(
        chk(&home)
            .unwrap_err()
            .to_string()
            .contains("home directory")
    );
    assert!(
        chk(&mur.join("x"))
            .unwrap_err()
            .to_string()
            .contains("MUR's own home")
    );
    assert!(chk(&home.join("nope")).is_err(), "must exist");
    assert_eq!(chk(&home.join("repo/.")).unwrap(), home.join("repo"));
}

#[test]
fn listed_skips_empty_and_missing_and_dedupes() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    let mut out = Vec::new();
    let rows = vec![
        ("a".into(), a.display().to_string()),
        ("a-again".into(), format!("{}/.", a.display())),
        ("blank".into(), String::new()),
        ("gone".into(), t.path().join("gone").display().to_string()),
    ];
    let v = listed(rows, &mut out).unwrap();
    assert_eq!(v.len(), 1);
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("skip blank: the index records no project path"),
        "{out}"
    );
    assert!(out.contains("skip gone:"), "{out}");
}

#[test]
fn preselects_only_what_every_agent_serves_and_labels_the_rest() {
    let t = tempfile::tempdir().unwrap();
    let (both, one) = (t.path().join("both"), t.path().join("one"));
    let projects = vec![
        Project::new("both".into(), both.clone(), true),
        Project::new("one".into(), one.clone(), true),
    ];
    let agents = vec![
        agent("pm", &[("both", &both), ("one", &one)]),
        agent("qa", &[("both", &both)]),
    ];
    assert_eq!(served_by_all(&projects, &agents), [0].into());
    assert_eq!(partly_served(&projects[0], &agents), None);
    assert_eq!(
        partly_served(&projects[1], &agents).as_deref(),
        Some("served by pm (qa not selected)")
    );
}

#[test]
fn add_path_dedupes_by_canonical_path() {
    let t = tempfile::tempdir().unwrap();
    let p = std::fs::canonicalize(t.path()).unwrap();
    let mut v = vec![Project::new("x".into(), p.clone(), true)];
    assert_eq!(add_project(&mut v, p, false), 0);
    assert_eq!(v.len(), 1);
    assert!(v[0].indexed, "the listed row wins");
}

#[test]
fn shared_picker_adds_a_path_and_keeps_ticks() {
    let t = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(t.path()).unwrap();
    let (a, b) = (root.join("a"), root.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let mut projects = vec![Project::new("a".into(), a, true)];
    let agents = vec![agent("pm", &[])];
    let mut s = Script::new([
        Answer::Ticks(vec![0, 1]),
        Answer::Text(b.display().to_string()),
        Answer::Defaults,
    ]);
    let mut out = Vec::new();
    let mur = root.join("mur-home");
    let got = shared(
        &mut projects,
        &agents,
        None,
        &mur,
        &BTreeSet::new(),
        &mut s,
        &mut out,
    )
    .unwrap();
    assert_eq!(got, [0, 1].into());
    assert_eq!(projects[1].path, b);
    assert!(!projects[1].indexed);
}

#[test]
fn languages_start_with_default_tier_only_and_ask_once() {
    let t = tempfile::tempdir().unwrap();
    for f in ["pyproject.toml", "Cargo.toml", "x.lua"] {
        std::fs::write(t.path().join(f), b"").unwrap();
    }
    let p = Project::new("p".into(), t.path().to_path_buf(), true);
    let mut s = Script::default();
    // `--yes`: no question; opt-in languages stay off.
    assert_eq!(languages(&p, false, &mut s).unwrap(), [Lang::Python].into());
    assert!(s.asked.is_empty());
    // Interactive: one question, and the user opts in to Rust.
    let mut s = Script::new([Answer::Ticks(vec![0, 2])]);
    let got = languages(&p, true, &mut s).unwrap();
    assert_eq!(s.asked.len(), 1);
    assert_eq!(got, [Lang::Python, Lang::Rust].into());
}
