use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::cmd::code_nav::plan::{Detected, Flags, UV, plan};

fn tools() -> Detected {
    let mut on_path: BTreeSet<String> = ["node", "npm", "go", "gopls"]
        .into_iter()
        .map(String::from)
        .collect();
    on_path.insert(UV.into());
    Detected { on_path }
}

fn p(with_serena: bool, lsp: &[&str]) -> Plan {
    let flags = Flags {
        with_serena,
        no_ast_grep: false,
        lsp: lsp.iter().map(|s| s.to_string()).collect(),
    };
    plan(Path::new("/mh"), &flags, &tools()).unwrap()
}

/// What a successful apply of `plan` would record.
fn applied(plan: &Plan, project: &Path) -> Manifest {
    Manifest {
        installed: plan
            .install
            .iter()
            .filter(|r| r.missing.is_none())
            .map(|r| install_key(r.name, r.version))
            .collect(),
        granted: plan.permissions.iter().map(grant_key).collect(),
        languages: enabled_languages(plan).into_iter().collect(),
        project: Some(project.into()),
        ..Default::default()
    }
}

#[test]
fn first_run_needs_consent_for_everything() {
    let plan = p(true, &[]);
    let d = diff(&plan, Some(Path::new("/r")), &Manifest::default());
    assert!(!d.is_empty());
    // ast-grep, serena, and pyright (Python is on by default).
    assert_eq!(d.installs.len(), 3);
    assert_eq!(d.languages, ["python", "php", "lua", "cpp"]);
    assert_eq!(d.project.as_deref(), Some(Path::new("/r")));
}

#[test]
fn identical_rerun_asks_nothing() {
    let plan = p(true, &[]);
    let m = applied(&plan, Path::new("/r"));
    assert!(diff(&plan, Some(Path::new("/r")), &m).is_empty());
}

#[test]
fn adding_a_high_language_asks_only_for_it() {
    let before = p(true, &["python"]);
    let m = applied(&before, Path::new("/r"));
    let after = p(true, &["python", "go"]);
    let d = diff(&after, Some(Path::new("/r")), &m);
    assert!(d.installs.is_empty());
    assert_eq!(d.languages, ["go"]);
    let grants: BTreeSet<_> = d.grants.iter().map(String::as_str).collect();
    assert_eq!(grants, BTreeSet::from(["spawn go", "spawn gopls"]));
    assert!(d.project.is_none());
}

#[test]
fn changing_project_needs_consent() {
    let plan = p(true, &[]);
    let m = applied(&plan, Path::new("/r"));
    let d = diff(&plan, Some(Path::new("/other")), &m);
    assert_eq!(d.project.as_deref(), Some(Path::new("/other")));
    assert!(!d.is_empty());
}

#[test]
fn dropped_language_is_kept_and_reported_not_asked() {
    let m = applied(&p(true, &["python", "go"]), Path::new("/r"));
    let d = diff(&p(true, &["python"]), Some(Path::new("/r")), &m);
    assert!(
        d.is_empty(),
        "removing scope is not something to consent to"
    );
    assert_eq!(d.kept_languages, ["go"]);
    let mut out = Vec::new();
    print_diff(&mut out, &d, false).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("never revokes): go")
    );
}

#[test]
fn manifest_round_trips_and_missing_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = manifest_path(tmp.path(), "a");
    assert_eq!(path, tmp.path().join("setup/code-nav/a.json"));
    assert_eq!(load_manifest(&path).unwrap(), Manifest::default());
    let m = applied(&p(true, &[]), Path::new("/r"));
    save_manifest(&path, &m).unwrap();
    assert_eq!(load_manifest(&path).unwrap(), m);
}

#[test]
fn corrupt_manifest_is_an_error_not_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("m.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert!(load_manifest(&path).is_err());
}

#[test]
fn tables_show_skipped_flag_high_note_and_disabled_lines() {
    let mut detected = tools();
    detected.on_path.remove("node");
    let flags = Flags {
        with_serena: true,
        no_ast_grep: false,
        lsp: vec!["python".into(), "rust".into()],
    };
    let plan = plan(Path::new("/mh"), &flags, &detected).unwrap();
    let mut out = Vec::new();
    print_tables(&mut out, &plan).unwrap();
    let text = String::from_utf8(out).unwrap();
    for want in [
        "Install:",
        "Language servers (serena):",
        "skipped (--lsp go)",
        "python disabled: node not found on PATH",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // rust-analyzer is not on PATH either, so the Rust row is disabled and
    // its note (shown only for enabled rows) is not printed.
    assert!(text.contains("rust disabled: rust-analyzer not found on PATH"));
}

#[test]
fn enabled_high_row_prints_its_mandated_note() {
    let mut detected = tools();
    detected.on_path.insert("rust-analyzer".into());
    let flags = Flags {
        with_serena: true,
        no_ast_grep: false,
        lsp: vec!["rust".into()],
    };
    let plan = plan(Path::new("/mh"), &flags, &detected).unwrap();
    let mut out = Vec::new();
    print_tables(&mut out, &plan).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("HIGH"), "{text}");
    assert!(text.contains("runs its build scripts"), "{text}");
}
