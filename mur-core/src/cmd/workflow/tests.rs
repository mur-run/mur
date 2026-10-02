use super::*;

/// The smallest flat workflow YAML that loads. Everything else in
/// `KnowledgeBase` carries a serde default — including `schema`, which
/// fills in as 3. Pinned as a test because hand-writing this file is
/// otherwise a guessing game against a `#[serde(flatten)]` struct.
#[test]
fn minimal_flat_workflow_yaml_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("probe.yaml"),
        "name: probe\n\
             description: two shell steps\n\
             content: what this workflow is for\n\
             steps:\n\
             - order: 1\n  description: first\n  command: echo one\n\
             - order: 2\n  description: second\n  command: echo two\n",
    )
    .unwrap();

    let store = WorkflowYamlStore::new(dir).unwrap();
    let w = store.get("probe").unwrap();
    assert_eq!(w.steps.len(), 2);
    assert_eq!(w.base.schema, 3, "schema defaults, it is not required");
    assert_eq!(w.steps[0].command.as_deref(), Some("echo one"));
}

/// Negative control: what the user actually sees when a required field is
/// missing. `#[serde(flatten)]` is known to degrade serde's missing-field
/// messages, so assert the message still names the field.
#[test]
fn missing_required_field_names_the_field() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("probe.yaml"),
        "name: probe\ndescription: d\nsteps:\n- order: 1\n  description: s\n",
    )
    .unwrap();

    let err = format!(
        "{:#}",
        WorkflowYamlStore::new(dir)
            .unwrap()
            .get("probe")
            .unwrap_err()
    );
    assert!(
        err.contains("content"),
        "error should name the missing field, got: {err}"
    );
}

/// Write a skill.yaml under `<home>/skills/<name>/`.
fn write_skill(home: &std::path::Path, name: &str, category: &str, procedure: bool) {
    let dir = home.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let proc_block = if procedure {
        "\n  procedure:\n    steps:\n    - description: echo hi\n      id: s1\n      command: echo hi\n"
    } else {
        "\n"
    };
    std::fs::write(
        dir.join("skill.yaml"),
        format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
                 description: test skill\ncategory: {category}\nprovenance: human\n\
                 content:\n  abstract: t{proc_block}"
        ),
    )
    .unwrap();
}

#[test]
fn find_workflow_skill_matches_exact_runnable_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_skill(home, "nightly-scan", "workflow", true);
    write_skill(home, "no-procedure", "workflow", false);
    write_skill(home, "not-a-workflow", "context", true);

    // A runnable workflow skill resolves.
    assert!(find_workflow_skill(home, "nightly-scan").is_some());
    // category:Workflow without a procedure has nothing to execute.
    assert!(find_workflow_skill(home, "no-procedure").is_none());
    // Other categories are not workflows.
    assert!(find_workflow_skill(home, "not-a-workflow").is_none());
    // Exact only — a schedule fires without a TTY, and a fuzzy match
    // would be refused at run time, making the schedule a silent no-op.
    assert!(find_workflow_skill(home, "nightly").is_none());
    assert!(find_workflow_skill(home, "nightly-scan-extra").is_none());
    // Missing skills dir is not an error.
    assert!(find_workflow_skill(tmp.path().join("nope").as_path(), "x").is_none());
}

#[test]
fn create_draft_workflow_persists_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let store =
        crate::store::workflow_yaml::WorkflowYamlStore::new(tmp.path().join("workflows")).unwrap();
    create_draft_workflow_in(
        &store,
        "test-then-commit",
        "Run tests then commit",
        "after editing code",
        &["s1".into(), "s2".into()],
    )
    .unwrap();
    assert!(store.exists("test-then-commit"));
    let wf = store.get("test-then-commit").unwrap();
    assert_eq!(wf.base.maturity, mur_common::knowledge::Maturity::Draft);
    assert_eq!(wf.trigger, "after editing code");
}
