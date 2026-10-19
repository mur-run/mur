use super::*;

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tools: PathBuf,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("serena");
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(&home).unwrap();
    Fx {
        _tmp: tmp,
        home,
        tools,
    }
}

/// A complete install with the marker, linked the way setup links it.
fn installed(f: &Fx) -> PathBuf {
    let dir = VUE.dir(&f.tools);
    for shim in [vue_shim(&dir), ts_shim(&dir)] {
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, b"").unwrap();
    }
    std::fs::write(dir.join(MARKER_FILE), marker()).unwrap();
    let link = resources_link(&f.home);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    dir
}

/// serena reinstalls unless the marker reads exactly
/// `<vue>_<typescript>_<typescript-language-server>`.
#[test]
fn marker_is_serenas_version_triple() {
    assert_eq!(marker(), "3.1.5_5.9.3_5.1.3");
}

/// The marker and the settings MUR writes are the same three pins, so
/// serena's expected marker is ours whatever its own defaults are.
#[test]
fn version_settings_spell_the_marker() {
    let v = version_settings();
    assert_eq!(
        v.map(|(s, k, _)| (s, k)),
        [
            ("vue", "vue_language_server_version"),
            ("typescript", "typescript_version"),
            ("typescript", "typescript_language_server_version"),
        ]
    );
    assert_eq!(v.map(|(_, _, p)| p).join("_"), marker());
}

#[test]
fn the_link_is_where_serena_looks_for_vue_lsp() {
    assert_eq!(
        resources_link(Path::new("/a/serena")),
        Path::new("/a/serena/language_servers/static/VueLanguageServer/vue-lsp")
    );
}

#[test]
fn absent_and_intact_links_pass() {
    let f = fx();
    assert_eq!(check_link(&f.home, &f.tools), Ok(LinkState::Absent));
    installed(&f);
    assert_eq!(check_link(&f.home, &f.tools), Ok(LinkState::Linked));
}

/// The agent can write `SERENA_HOME`; a link it re-points is refused.
#[test]
fn a_link_pointing_elsewhere_is_refused() {
    let f = fx();
    installed(&f);
    let elsewhere = f._tmp.path().join("evil");
    for shim in [vue_shim(&elsewhere), ts_shim(&elsewhere)] {
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, b"").unwrap();
    }
    std::fs::write(elsewhere.join(MARKER_FILE), marker()).unwrap();
    let link = resources_link(&f.home);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let e = check_link(&f.home, &f.tools).unwrap_err();
    assert!(e.contains("not "), "{e}");
}

/// serena's own download (a real dir) is what MUR exists to replace.
#[test]
fn a_real_directory_is_refused() {
    let f = fx();
    std::fs::create_dir_all(resources_link(&f.home).join("node_modules")).unwrap();
    let e = check_link(&f.home, &f.tools).unwrap_err();
    assert!(e.contains("not MUR's link"), "{e}");
}

#[test]
fn a_stale_or_missing_marker_is_refused() {
    let f = fx();
    let dir = installed(&f);
    std::fs::write(dir.join(MARKER_FILE), "0.0.0_5.9.3_5.1.3").unwrap();
    assert!(
        check_link(&f.home, &f.tools)
            .unwrap_err()
            .contains("expected")
    );
    std::fs::remove_file(dir.join(MARKER_FILE)).unwrap();
    assert!(check_link(&f.home, &f.tools).is_err());
}

#[test]
fn a_missing_shim_is_refused() {
    let f = fx();
    let dir = installed(&f);
    std::fs::remove_file(ts_shim(&dir)).unwrap();
    assert!(
        check_link(&f.home, &f.tools)
            .unwrap_err()
            .contains("missing")
    );
}

#[test]
fn a_dangling_link_is_refused() {
    let f = fx();
    let link = resources_link(&f.home);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(VUE.dir(&f.tools), &link).unwrap();
    assert!(check_link(&f.home, &f.tools).is_err());
}
