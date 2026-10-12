use super::*;

const FIXTURE: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");

/// Atomic write, the way every MUR writer saves YAML.
fn write(path: &Path, text: &str) {
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn home_with(text: &str) -> (tempfile::TempDir, Arc<LiveProfile>) {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("profile.yaml"), text);
    let boot = crate::profile::Profile::load(dir.path()).unwrap().inner;
    let live = LiveProfile::new(dir.path(), boot);
    (dir, live)
}

fn with_line(extra: &str) -> String {
    format!("{FIXTURE}\n{extra}\n")
}

#[test]
fn unchanged_file_is_not_reloaded() {
    let (_d, live) = home_with(FIXTURE);
    let _ = live.current();
    let _ = live.current();
    assert_eq!(live.generation(), 0);
}

#[test]
fn a_hot_edit_is_served_from_the_next_read() {
    let (d, live) = home_with(FIXTURE);
    assert!(live.current().limits.is_none());
    write(
        &d.path().join("profile.yaml"),
        &with_line("limits: { deadline: 5m }"),
    );
    let p = live.current();
    assert_eq!(p.limits.as_ref().unwrap().deadline.as_deref(), Some("5m"));
    assert_eq!(live.generation(), 1);
    assert!(live.state().pending_cold_restart.is_empty());
}

#[test]
fn broken_yaml_keeps_the_last_good_profile_and_reports_once() {
    let (d, live) = home_with(&with_line("limits: { deadline: 5m }"));
    write(&d.path().join("profile.yaml"), "schema: [unclosed");
    let p = live.current();
    assert_eq!(p.limits.as_ref().unwrap().deadline.as_deref(), Some("5m"));
    let st = live.state();
    assert!(st.last_reload_error.is_some(), "{st:?}");
    assert_eq!(st.generation, 0);
    // Same broken stamp: settled, not re-read.
    assert!(live.file.is_settled(live.file.stamp()));
}

#[test]
fn unparsable_limits_is_rejected_not_applied() {
    let (d, live) = home_with(&with_line("limits: { deadline: 5m }"));
    write(
        &d.path().join("profile.yaml"),
        &with_line("limits: { deadline: 5x }"),
    );
    let p = live.current();
    assert_eq!(p.limits.as_ref().unwrap().deadline.as_deref(), Some("5m"));
    let err = live.state().last_reload_error.unwrap();
    assert!(err.contains("limits.deadline"), "{err}");
}

#[test]
fn fixing_the_file_clears_the_error() {
    let (d, live) = home_with(FIXTURE);
    write(&d.path().join("profile.yaml"), "schema: [unclosed");
    let _ = live.current();
    write(
        &d.path().join("profile.yaml"),
        &with_line("limits: { stuck: \"off\" }"),
    );
    let _ = live.current();
    let st = live.state();
    assert_eq!(st.last_reload_error, None);
    assert_eq!(st.generation, 1);
}

#[test]
fn a_different_agent_name_is_refused() {
    let (d, live) = home_with(FIXTURE);
    write(
        &d.path().join("profile.yaml"),
        &FIXTURE.replace("name: bridge_test", "name: someone_else"),
    );
    assert_eq!(live.current().name, "bridge_test");
    assert!(
        live.state()
            .last_reload_error
            .unwrap()
            .contains("someone_else")
    );
}

#[test]
fn a_locked_edit_is_flagged_and_never_applied() {
    let (d, live) = home_with(FIXTURE);
    let edited = FIXTURE.replace("allow_hosts: []", "allow_hosts: [\"evil.example\"]");
    assert_ne!(edited, FIXTURE, "fixture changed shape");
    write(
        &d.path().join("profile.yaml"),
        &format!("{edited}\nlimits: {{ deadline: 1h }}\n"),
    );
    let p = live.current();
    // The hot half of the same edit applied...
    assert_eq!(p.limits.as_ref().unwrap().deadline.as_deref(), Some("1h"));
    // ...the locked half did not.
    assert!(p.entitlements.network.outbound.allow_hosts.is_empty());
    let pending = live.state().pending_cold_restart;
    assert!(
        pending.iter().any(|f| f.starts_with("entitlements")),
        "{pending:?}"
    );
}

#[test]
fn reload_state_is_written_for_doctor() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join(RELOAD_STATE_FILE);
    // A previous run's leftover must not survive boot.
    std::fs::write(&state, r#"{"generation":9,"last_reload_error":"old"}"#).unwrap();
    write(&d.path().join("profile.yaml"), FIXTURE);
    let cfg = d.path().join("config.yaml");
    write(&cfg, "limits:\n  deadline: 10m\n");
    let boot = crate::profile::Profile::load(d.path()).unwrap().inner;
    let live = LiveSettings::new(d.path(), d.path(), boot, Default::default());
    let st = ReloadState::load(&state).unwrap();
    assert_eq!((st.generation, st.last_reload_error), (0, None));

    write(
        &d.path().join("profile.yaml"),
        &with_line("limits: { deadline: 5m }"),
    );
    let _ = live.profile.current();
    assert_eq!(ReloadState::load(&state).unwrap().generation, 1);

    write(&cfg, "limits:\n  deadline: soon\n");
    let _ = live.global_limits.current();
    let st = ReloadState::load(&state).unwrap();
    assert!(st.config_limits_error.is_some(), "{st:?}");
    assert_eq!(st.generation, 1);
}

#[test]
fn global_limits_keep_the_last_good_value() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.yaml");
    write(&cfg, "limits:\n  deadline: 10m\n");
    let boot = mur_common::config::Config::try_load(&cfg).unwrap().limits;
    let live = live_global_limits(d.path(), boot);
    write(&cfg, "limits:\n  deadline: 20m\n");
    assert_eq!(live.current().deadline.as_deref(), Some("20m"));
    write(&cfg, "limits:\n  deadline: soon\n");
    assert_eq!(live.current().deadline.as_deref(), Some("20m"));
    assert!(live.last_error().is_some());
}

/// #1794: the global scope reloads from `limits/global.yaml`.
#[test]
fn global_limits_reload_from_the_new_file() {
    let d = tempfile::tempdir().unwrap();
    let dir = mur_common::limits::global_limits_dir(d.path());
    std::fs::create_dir_all(&dir).unwrap();
    let path = mur_common::limits::global_limits_path(d.path());
    write(&path, "deadline: 10m\n");
    let boot = mur_common::limits::load_global_strict(d.path())
        .unwrap()
        .limits;
    let live = live_global_limits(d.path(), boot);
    assert_eq!(live.current().deadline.as_deref(), Some("10m"));
    assert_eq!(live.generation(), 0, "boot stamp is the loaded version");
    write(&path, "deadline: 20m\n");
    assert_eq!(live.current().deadline.as_deref(), Some("20m"));
    write(&path, "deadline: soon\n");
    assert_eq!(live.current().deadline.as_deref(), Some("20m"));
    assert!(live.last_error().is_some());
}

/// #1810/#1812: two same-length versions inside one mtime tick, with no
/// inode change to tell them apart (Windows has none; an in-place write keeps
/// it on unix). The stamp must still see the new version.
#[test]
fn same_length_rewrite_in_one_mtime_tick_is_reloaded() {
    let d = tempfile::tempdir().unwrap();
    let dir = mur_common::limits::global_limits_dir(d.path());
    std::fs::create_dir_all(&dir).unwrap();
    let path = mur_common::limits::global_limits_path(d.path());
    let tick = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let write_in_place = |text: &str| {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut &f, text.as_bytes()).unwrap();
        f.set_modified(tick).unwrap();
    };
    write_in_place("deadline: 10m\n");
    let boot = mur_common::limits::load_global_strict(d.path())
        .unwrap()
        .limits;
    let live = live_global_limits(d.path(), boot);
    assert_eq!(live.current().deadline.as_deref(), Some("10m"));
    write_in_place("deadline: 20m\n");
    assert_eq!(live.current().deadline.as_deref(), Some("20m"));
}

/// The first `mur limits --global` write creates the new file under a running
/// agent: it takes over at once, and from then on legacy edits are ignored.
#[test]
fn creating_the_new_file_takes_over_from_the_legacy_block() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config.yaml");
    write(&cfg, "limits:\n  deadline: 10m\n");
    let boot = mur_common::limits::load_global_strict(d.path())
        .unwrap()
        .limits;
    let live = live_global_limits(d.path(), boot);
    std::fs::create_dir_all(mur_common::limits::global_limits_dir(d.path())).unwrap();
    write(
        &mur_common::limits::global_limits_path(d.path()),
        "deadline: 30m\n",
    );
    assert_eq!(live.current().deadline.as_deref(), Some("30m"));
    let generation = live.generation();
    write(&cfg, "limits:\n  deadline: 99m\n");
    assert_eq!(live.current().deadline.as_deref(), Some("30m"));
    assert_eq!(live.generation(), generation, "legacy edit is not a reload");
}
