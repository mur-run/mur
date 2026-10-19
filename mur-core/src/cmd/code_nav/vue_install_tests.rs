use super::*;
use mur_common::serena_vue::LS_RESOURCES_REL;

fn fake_install(tools: &Path) {
    let dir = VUE.dir(tools);
    for shim in [vue_shim(&dir), ts_shim(&dir)] {
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, b"").unwrap();
    }
}

/// Only the server's own `bin/` is an exec lane, not the whole install.
#[test]
fn exec_dir_is_the_servers_bin_dir() {
    assert_eq!(
        exec_dir(Path::new("/h/.mur/tools")),
        Path::new("/h/.mur/tools/vue-language-server/3.1.5/node_modules/@vue/language-server/bin")
    );
}

#[test]
fn marker_refuses_an_incomplete_install() {
    let tmp = tempfile::tempdir().unwrap();
    let e = write_marker(tmp.path()).unwrap_err();
    assert!(format!("{e:#}").contains("re-run setup"), "{e:#}");
}

#[test]
fn link_is_idempotent_and_replaces_serenas_own_download() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    fake_install(&tools);
    write_marker(&tools).unwrap();

    // serena's own earlier download sits where the link goes.
    let link_path = resources_link(&home);
    std::fs::create_dir_all(link_path.join("node_modules")).unwrap();
    assert_eq!(link(&home, &tools).unwrap(), LinkOutcome::Replaced);
    assert_eq!(link(&home, &tools).unwrap(), LinkOutcome::Unchanged);
    assert_eq!(std::fs::read_link(&link_path).unwrap(), VUE.dir(&tools));
    // serena's checks, through the link.
    assert!(vue_shim(&link_path).exists());
    assert_eq!(
        std::fs::read_to_string(link_path.join(MARKER_FILE)).unwrap(),
        marker()
    );
}

#[test]
fn a_link_to_another_version_is_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    let link_path = resources_link(&home);
    std::fs::create_dir_all(link_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(tools.join("vue-language-server/0.0.1"), &link_path).unwrap();
    assert_eq!(link(&home, &tools).unwrap(), LinkOutcome::Replaced);
    assert_eq!(std::fs::read_link(&link_path).unwrap(), VUE.dir(&tools));
}

#[test]
fn vue_files_are_detected_outside_skipped_dirs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
    std::fs::write(root.join("node_modules/x/A.vue"), b"").unwrap();
    std::fs::create_dir_all(root.join(".nuxt")).unwrap();
    std::fs::write(root.join(".nuxt/B.vue"), b"").unwrap();
    std::fs::write(root.join("main.ts"), b"").unwrap();
    assert!(!project_has_vue(root));
    std::fs::create_dir_all(root.join("src/components")).unwrap();
    std::fs::write(root.join("src/components/App.vue"), b"").unwrap();
    assert!(project_has_vue(root));
}

/// serena's download may hold symlinks out of `SERENA_HOME`; replacing it
/// must not follow them.
#[test]
fn replacing_a_real_dir_never_follows_links_inside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    fake_install(&tools);
    let precious = tmp.path().join("precious");
    std::fs::create_dir_all(&precious).unwrap();
    std::fs::write(precious.join("keep"), b"x").unwrap();
    let link_path = resources_link(&home);
    std::fs::create_dir_all(&link_path).unwrap();
    std::os::unix::fs::symlink(&precious, link_path.join("node_modules")).unwrap();
    assert_eq!(link(&home, &tools).unwrap(), LinkOutcome::Replaced);
    assert!(precious.join("keep").exists());
    assert_eq!(std::fs::read_link(&link_path).unwrap(), VUE.dir(&tools));
}

/// The agent can write `SERENA_HOME`: a `VueLanguageServer/` it turned
/// into a symlink out of it is refused, and nothing out there is touched.
#[test]
fn a_parent_symlinked_out_of_serena_home_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    fake_install(&tools);
    let victim = tmp.path().join("victim");
    std::fs::create_dir_all(victim.join("vue-lsp")).unwrap();
    std::fs::write(victim.join("vue-lsp/keep"), b"x").unwrap();
    let parent = resources_link(&home).parent().unwrap().to_path_buf();
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &parent).unwrap();
    let e = link(&home, &tools).unwrap_err();
    assert!(format!("{e:#}").contains("refusing"), "{e:#}");
    assert!(victim.join("vue-lsp/keep").exists());
}

/// No stage or aside entry is left next to the link.
#[test]
fn link_leaves_no_staging_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    fake_install(&tools);
    std::fs::create_dir_all(resources_link(&home).join("x")).unwrap();
    link(&home, &tools).unwrap();
    let parent = resources_link(&home).parent().unwrap().to_path_buf();
    let names: Vec<_> = std::fs::read_dir(&parent)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["vue-lsp"]);
}

/// #1998 review: an agent-planted `language_servers` symlink out of
/// `SERENA_HOME` is refused before anything is created at its target.
#[test]
fn a_planted_ancestor_symlink_gets_no_dirs_created_through_it() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let home = tmp.path().join("serena");
    fake_install(&tools);
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&outside, home.join(LS_RESOURCES_REL[0])).unwrap();
    let e = link(&home, &tools).unwrap_err();
    assert!(format!("{e:#}").contains("refusing"), "{e:#}");
    let left: Vec<_> = std::fs::read_dir(&outside).unwrap().collect();
    assert!(left.is_empty(), "setup wrote outside SERENA_HOME: {left:?}");
}
