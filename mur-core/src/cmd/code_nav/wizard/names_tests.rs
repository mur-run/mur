use super::*;

fn ex(servers: &[&str], known: &[(&str, &str)]) -> Existing {
    Existing {
        servers: servers.iter().map(|s| (*s).to_owned()).collect(),
        known: known
            .iter()
            .map(|(n, p)| ((*n).to_owned(), PathBuf::from(p)))
            .collect(),
        unknown: Vec::new(),
    }
}

#[test]
fn sanitizes_to_what_spawn_accepts() {
    assert_eq!(sanitize("mur.run-website"), "mur-run-website");
    assert_eq!(sanitize("My Repo__2"), "my-repo_2");
    assert_eq!(sanitize("a..b--c"), "a-b-c");
    assert_eq!(sanitize("trail_"), "trail");
    assert_eq!(sanitize("專案"), FALLBACK_NAME);
    assert_eq!(sanitize(""), FALLBACK_NAME);
    let long = sanitize(&"x".repeat(200));
    assert_eq!(long.len(), MAX_ENTRY_NAME_LEN);
    for raw in ["mur.run-website", "My Repo__2", "-x-", "_a_"] {
        assert!(valid(&sanitize(raw)), "{raw}");
    }
}

#[test]
fn same_path_reuses_the_existing_name() {
    let e = ex(&["code"], &[("code", "/src/app")]);
    assert_eq!(
        name_for(&e, Path::new("/src/app"), "app", &[]),
        Naming::Reuse {
            entry: "code".into(),
            duplicates: vec![]
        }
    );
}

#[test]
fn several_entries_on_one_path_reuse_the_first_and_list_the_rest() {
    let e = ex(&["a", "b"], &[("a", "/p"), ("b", "/p")]);
    assert_eq!(
        name_for(&e, Path::new("/p"), "p", &[]),
        Naming::Reuse {
            entry: "a".into(),
            duplicates: vec!["b".into()]
        }
    );
}

#[test]
fn same_name_on_another_path_is_suffixed_never_repointed() {
    let e = ex(&["app", "app-2"], &[("app", "/old"), ("app-2", "/older")]);
    assert_eq!(
        name_for(&e, Path::new("/new"), "app", &[]),
        Naming::New {
            entry: "app-3".into(),
            wanted: Some("app".into())
        }
    );
}

#[test]
fn a_user_server_or_prefix_clash_takes_a_suffix() {
    let e = ex(&["app"], &[]);
    assert_eq!(name_for(&e, Path::new("/x"), "app", &[]).entry(), "app-2");
    // `my_app` and `my.app` share the tool prefix `mcp__my_app__`.
    let e = ex(&["my.app"], &[]);
    assert_eq!(
        name_for(&e, Path::new("/x"), "my_app", &[]).entry(),
        "my_app-2"
    );
}

#[test]
fn names_assigned_earlier_in_the_run_count_as_taken() {
    let e = ex(&[], &[]);
    let also = vec!["web".to_owned()];
    assert_eq!(name_for(&e, Path::new("/b"), "web", &also).entry(), "web-2");
}

#[test]
fn a_suffix_on_a_full_length_name_stays_valid() {
    let base = "y".repeat(MAX_ENTRY_NAME_LEN);
    let e = ex(&[base.as_str()], &[]);
    let n = name_for(&e, Path::new("/z"), &base, &[]);
    assert!(valid(n.entry()), "{}", n.entry());
    assert!(n.entry().ends_with("-2"));
}

#[test]
fn usable_rejects_invalid_and_taken_names() {
    let e = ex(&["taken"], &[]);
    assert!(usable(&e, "fresh", &[]));
    assert!(!usable(&e, "taken", &[]));
    assert!(!usable(&e, "Bad Name", &[]));
}
