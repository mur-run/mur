/// Regression for #1509's Windows CI break: `symlink_skill_dir`'s
/// non-unix fallback called `std::fs::copy` on every directory entry
/// without checking whether it was a subdirectory, which `browser` (D3,
/// the first skill with a `references/` subdir) turned into "Access is
/// denied" (os error 5) on Windows. Runs on every platform — the bug
/// shipped unnoticed specifically because nothing exercised this
/// function outside the Windows-only leg it's gated for.
#[test]
fn copy_dir_recursive_copies_nested_subdirectories() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(src.join("references")).unwrap();
    std::fs::write(src.join("skill.yaml"), "top-level").unwrap();
    std::fs::write(src.join("references").join("auth.md"), "nested").unwrap();

    super::skill_install::copy_dir_recursive(&src, &dest).unwrap();

    assert_eq!(
        std::fs::read_to_string(dest.join("skill.yaml")).unwrap(),
        "top-level"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("references").join("auth.md")).unwrap(),
        "nested"
    );
}

#[test]
fn installs_project_search_skill() {
    let home = std::env::temp_dir().join(format!(
        "mur-skilltest-{}-{}",
        std::process::id(),
        std::fs::read_dir(std::env::temp_dir())
            .map(|d| d.count())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&home).unwrap();

    super::ensure_mur_skill(&home, &home.join(".mur")).unwrap();

    let skill_yaml = home
        .join(".mur")
        .join("skills")
        .join("mur-project-search")
        .join("skill.yaml");
    assert!(
        skill_yaml.exists(),
        "mur-project-search skill.yaml must be written"
    );
    let body = std::fs::read_to_string(&skill_yaml).unwrap();
    assert!(body.contains("name: mur-project-search"));

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn ensure_mur_skill_ships_mur_settlement_and_it_loads_at_session_start() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    super::ensure_mur_skill(&home, &root).unwrap();

    let path = root.join("skills/mur-settlement/skill.yaml");
    assert!(
        path.exists(),
        "mur-settlement must be written by ensure_mur_skill"
    );

    // A reporting rule has to be in context when the turn ends, so the
    // trigger is load-bearing: shipped with only `manual` it would never
    // fire and the skill would be dead weight nobody notices.
    //
    // Assert it through the parser, not on the raw text. A `contains`
    // check passes on a file the loader rejects — which is exactly what
    // happened while this skill shipped an unparseable `procedure: []`.
    let body = std::fs::read_to_string(&path).unwrap();
    let m = mur_common::skill::parse_canonical(&body)
        .unwrap_or_else(|e| panic!("mur-settlement must parse: {e}\n{body}"));
    assert!(
        m.triggers
            .iter()
            .any(|t| t.kind == mur_common::skill::types::TriggerKind::SessionStart),
        "mur-settlement must load at session start, got: {:?}",
        m.triggers
    );
}

#[test]
fn ensure_mur_skill_ships_mur_native_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    super::ensure_mur_skill(&home, &root).unwrap();

    let path = root.join("skills/mur-native-tools/skill.yaml");
    assert!(
        path.exists(),
        "mur-native-tools must be written to the global store by ensure_mur_skill"
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    let m = mur_common::skill::parse_canonical(&raw).unwrap();
    assert_eq!(m.name, "mur-native-tools");
}

#[test]
fn ensure_mur_skill_ships_browser_hub_and_its_reference_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    super::ensure_mur_skill(&home, &root).unwrap();

    // The concrete regression D2 exists to prevent: `mur-browser/SKILL.md`
    // is authored with Anthropic frontmatter, but `skill.yaml` is read
    // only through `parse_canonical` — writing the raw markdown there
    // would ship a manifest the loader cannot load.
    let skill_yaml = root.join("skills/browser/skill.yaml");
    assert!(skill_yaml.exists(), "browser skill.yaml must be written");
    let raw = std::fs::read_to_string(&skill_yaml).unwrap();
    let m = mur_common::skill::parse_canonical(&raw)
        .unwrap_or_else(|e| panic!("browser skill.yaml must parse canonically: {e}\n{raw}"));
    assert_eq!(m.name, "browser");

    // D3: reference bundle rides alongside the manifest, not just the
    // manifest itself.
    for file in ["auth.md", "testing.md", "automation.md"] {
        let p = root.join("skills/browser/references").join(file);
        assert!(p.exists(), "browser references/{file} must be written");
    }
}
