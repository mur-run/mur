//! The merged plan: shared installs deduplicated, one equivalent command per
//! target, held targets shown but not applied, and the project × agent
//! matrix.

use super::names::Naming;
use super::run_tests::decided;
use super::table::{equivalent, print, shared_installs};
use super::*;

fn printed(d: &Decided) -> String {
    let mut out = Vec::new();
    print(d, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn shared_installs_list_each_tool_once() {
    let d = decided(&["pm", "qa"]);
    let rows = shared_installs(&d);
    let names: Vec<_> = rows.iter().map(|r| r.name).collect();
    let unique: BTreeSet<_> = names.iter().collect();
    assert_eq!(names.len(), unique.len(), "{names:?}");
    assert!(names.contains(&plan::SERENA), "{names:?}");
}

#[test]
fn held_targets_add_no_install_rows() {
    let mut d = decided(&["pm"]);
    d.specs[0].hold = Some(Hold::Skipped("user skipped".into()));
    assert!(shared_installs(&d).is_empty());
}

#[test]
fn equivalent_command_names_agent_project_entry_and_languages() {
    let d = decided(&["pm"]);
    assert_eq!(
        equivalent(&d, &d.specs[0]),
        "mur code-nav setup --agent pm --with-serena --project /src/p0 --entry p0 --lsp python"
    );
}

#[test]
fn plan_shows_each_target_its_command_and_the_matrix() {
    let mut d = decided(&["pm", "qa"]);
    d.specs[1].hold = Some(Hold::Blocked("unknown project for `serena`".into()));
    let s = printed(&d);
    assert!(s.contains("  + pm → /src/p0: new entry p0"), "{s}");
    assert!(s.contains("--agent pm --with-serena"), "{s}");
    assert!(s.contains("  ✗ qa → /src/p1"), "{s}");
    assert!(s.contains("cannot apply: unknown project"), "{s}");
    assert!(
        !s.contains("--agent qa"),
        "a held target has no command: {s}"
    );
    assert!(s.contains("Project × agent:"), "{s}");
    assert!(s.contains("+p0"), "{s}");
    assert!(s.contains("skip"), "{s}");
}

#[test]
fn reuse_and_suffix_are_spelled_out() {
    let mut d = decided(&["pm", "qa"]);
    d.specs[0].naming = Some(Naming::Reuse {
        entry: "code-nav".into(),
        duplicates: Vec::new(),
    });
    d.specs[1].naming = Some(Naming::New {
        entry: "p1-2".into(),
        wanted: Some("p1".into()),
    });
    let s = printed(&d);
    assert!(s.contains("reuse code-nav (already serves /src/p0)"), "{s}");
    assert!(s.contains("new entry p1-2 (`p1` is taken)"), "{s}");
}

#[test]
fn unindexed_project_gets_the_index_hint() {
    let mut d = decided(&["pm"]);
    d.projects[0].indexed = false;
    let s = printed(&d);
    assert!(s.contains("not indexed"), "{s}");
    assert!(s.contains("mur project index /src/p0"), "{s}");
}

#[test]
fn single_agent_prints_no_matrix() {
    assert!(!printed(&decided(&["pm"])).contains("Project × agent"));
}

#[test]
fn opt_in_only_languages_are_named_in_the_skip_reason() {
    use super::build::no_language_reason;
    let mut d = decided(&["pm"]);
    d.projects[0].detected = [Lang::Rust, Lang::Lua].into();
    let why = no_language_reason(&d.projects[0]);
    assert!(why.contains("rust") && why.contains("lua"), "{why}");
    assert!(why.contains("opt-in"), "{why}");
    d.projects[0].detected.clear();
    assert_eq!(
        no_language_reason(&d.projects[0]),
        "no language server selected for this project"
    );
}
