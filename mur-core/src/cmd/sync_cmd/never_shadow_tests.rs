/// A user-authored skill occupying a dev-discipline name must survive
/// `ensure_mur_skill` untouched (spec 2026-07-23 §6 never-shadow).
#[test]
fn user_skill_with_dev_name_is_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let mur_root = home.path().join(".mur");
    let dir = mur_root.join("skills").join("mur-tdd");
    std::fs::create_dir_all(&dir).unwrap();
    let user_yaml = "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\n\
                         description: my own tdd notes\ncategory: workflow\n\
                         content:\n  abstract: mine\n  context: keep me\n";
    std::fs::write(dir.join("skill.yaml"), user_yaml).unwrap();

    super::ensure_mur_skill(home.path(), &mur_root).unwrap();

    let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
    assert_eq!(
        after, user_yaml,
        "user-authored skill must not be clobbered"
    );
}

/// Unparseable existing YAML is treated as user-authored (fail-safe skip).
#[test]
fn unparseable_existing_dev_skill_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let mur_root = home.path().join(".mur");
    let dir = mur_root.join("skills").join("mur-tdd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("skill.yaml"), ": not yaml {{{{").unwrap();

    super::ensure_mur_skill(home.path(), &mur_root).unwrap();

    let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
    assert_eq!(after, ": not yaml {{{{");
}

#[test]
fn shadow_predicate_publisher_rules() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("skill.yaml");
    // Foreign publisher → shadowed (skip).
    std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
    assert!(super::skill_install::dev_skill_shadowed_by_user(
        dir.path(),
        "mur-tdd"
    ));
    // MUR publisher → not shadowed (update as usual).
    std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:mur-official\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
    assert!(!super::skill_install::dev_skill_shadowed_by_user(
        dir.path(),
        "mur-tdd"
    ));
    // Non-dev names never shadow (existing builtin semantics unchanged).
    assert!(!super::skill_install::dev_skill_shadowed_by_user(
        dir.path(),
        "mur-run"
    ));
    // No file on disk → nothing to shadow.
    std::fs::remove_file(&f).unwrap();
    assert!(!super::skill_install::dev_skill_shadowed_by_user(
        dir.path(),
        "mur-tdd"
    ));
}

/// Pins the predicate the pull-path guard uses, on the file that actually
/// appeared in a user's `~/.mur/workflows/`: 32 bytes, valid YAML, missing
/// the fields `KnowledgeBase` requires. It was rewritten on every sync
/// while the store warned about it on every read (#803).
///
/// Scope: this asserts what the guard tests, not that `device_sync` calls
/// it — that path is an async network function with no local seam. Read
/// the name literally.
#[test]
fn the_real_broken_workflow_fails_the_guards_parse_check() {
    let bad = "id: test-wf\nname: Test\nsteps: []";
    let parsed = serde_yaml_ng::from_str::<mur_common::workflow::Workflow>(bad);
    assert!(
        parsed.is_err(),
        "fixture must be unparseable, or this test proves nothing"
    );
    // Valid YAML: the old filename/path guards would all have passed it.
    assert!(serde_yaml_ng::from_str::<serde_yaml_ng::Value>(bad).is_ok());
}

/// The guard must not reject workflows that do load, or a sync would
/// silently stop delivering them — the same absence, a new cause.
///
/// The fixture is the head of a real file from `~/.mur/workflows/`, not an
/// invented one: writing this test by guessing at the shape produced a
/// "well-formed" sample that did not parse, which would have made the
/// assertion pass for the wrong reason had it been the other way round.
#[test]
fn a_well_formed_server_workflow_still_parses() {
    let good = concat!(
        "schema: 2\n",
        "name: find-prices-shopping-websites\n",
        "description: use agent-browser to find prices.\n",
        "content: ''\n",
        "tier: session\n",
        "importance: 0.5\n",
        "confidence: 0.5\n",
    );
    let parsed = serde_yaml_ng::from_str::<mur_common::workflow::Workflow>(good);
    assert!(
        parsed.is_ok(),
        "well-formed workflow must survive: {parsed:?}"
    );
}
