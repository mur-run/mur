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

/// `minimal_profile.yaml` with one MCP server `probe` at `command`, pinned.
/// Spawn is `any` so this exercises the pin; the seal's own refusal is
/// covered in `live_profile_mcp_tests`.
fn with_probe(command: &Path, pin: &str) -> String {
    let base: String = FIXTURE
        .lines()
        .filter(|l| !l.starts_with("mcp_servers"))
        .map(|l| {
            l.replace(
                "spawn: { mode: allowlist, allowed: [] }",
                "spawn: { mode: any }",
            )
        })
        .map(|l| format!("{l}\n"))
        .collect();
    format!(
        "{base}mcp_servers:\n  - name: probe\n    command: {}\n    binary_sha256: {pin}\n",
        command.display()
    )
}

fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn a_verified_mcp_command_change_is_served_and_a_tampered_one_is_refused() {
    let bins = tempfile::tempdir().unwrap();
    let (a, b) = (bins.path().join("a"), bins.path().join("b"));
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let (d, live) = home_with(&with_probe(&a, &sha(b"a")));
    let profile = d.path().join("profile.yaml");

    write(&profile, &with_probe(&b, &sha(b"b")));
    assert_eq!(
        live.mcp_entry("probe").unwrap().command,
        b.display().to_string()
    );
    assert!(live.state().mcp_refusals.is_empty());

    // Same pin, different bytes: refused, and B — not boot's A — keeps serving.
    std::fs::write(&a, b"tampered").unwrap();
    write(&profile, &with_probe(&a, &sha(b"b")));
    assert_eq!(
        live.mcp_entry("probe").unwrap().command,
        b.display().to_string()
    );
    let refusals = live.state().mcp_refusals;
    assert!(
        refusals["probe"].contains("changed since install"),
        "{refusals:?}"
    );
    // The reload itself succeeded; only the one entry was held back.
    assert!(live.state().last_reload_error.is_none());
}
