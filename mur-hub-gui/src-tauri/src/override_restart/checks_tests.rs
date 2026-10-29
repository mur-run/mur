use super::*;

fn home_with(names: &[&str]) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    for n in names {
        let d = t.path().join("agents").join(n);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("profile.yaml"), "name: x\n").unwrap();
    }
    t
}

#[test]
fn only_the_dashboard_may_ask() {
    for l in [
        "detail-agent-x",
        "chat-x",
        "pet-x",
        "popover",
        "murmur-panel",
        "",
        "Dashboard",
    ] {
        assert!(check_window(l).is_err(), "{l:?} must be refused");
    }
    assert!(check_window(DASHBOARD_LABEL).is_ok());
}

#[test]
fn no_flags_is_refused() {
    let e = check_flags(OverrideFlags::default()).unwrap_err();
    assert!(e.ends_with(NOTHING_CHANGED), "{e}");
    assert!(
        check_flags(OverrideFlags {
            full_perms: true,
            ..Default::default()
        })
        .is_ok()
    );
}

#[test]
fn bad_names_are_refused_before_any_path_join() {
    let t = home_with(&[]);
    // `agents/../x/profile.yaml` exists, so canonicalization alone would pass it.
    std::fs::create_dir_all(t.path().join("agents")).unwrap();
    std::fs::create_dir_all(t.path().join("x")).unwrap();
    std::fs::write(t.path().join("x/profile.yaml"), "").unwrap();
    let long = "a".repeat(65);
    for n in ["../x", "a/b", "-x", "", long.as_str()] {
        let e = check_name(t.path(), n).unwrap_err();
        assert!(e.ends_with(NOTHING_CHANGED), "{n:?}: {e}");
    }
}

#[test]
fn unknown_name_is_refused() {
    let t = home_with(&["foo"]);
    assert!(check_name(t.path(), "bar").is_err());
}

#[test]
fn a_dir_without_a_profile_is_refused() {
    let t = home_with(&[]);
    std::fs::create_dir_all(t.path().join("agents/foo")).unwrap();
    assert!(check_name(t.path(), "foo").is_err());
}

#[test]
fn a_case_variant_is_refused() {
    let t = home_with(&["foo"]);
    assert!(check_name(t.path(), "Foo").is_err());
}

#[test]
fn known_name_passes() {
    let t = home_with(&["foo"]);
    assert!(check_name(t.path(), "foo").is_ok());
}
