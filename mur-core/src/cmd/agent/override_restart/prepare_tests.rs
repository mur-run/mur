use super::super::Mode;
use super::super::outcome::Step;
use super::*;
use mur_common::agent::OverrideFlags;

const NAME: &str = "ovr-prepare-test";

fn home() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("agents").join(NAME)).unwrap();
    t
}

fn req(home: &Path, mode: Mode) -> OverrideRequest<'_> {
    OverrideRequest {
        home,
        name: NAME,
        mode,
        flags: OverrideFlags {
            no_hitl: true,
            ..OverrideFlags::default()
        },
    }
}

fn write_live_lock(home: &Path) {
    let lock: LockFile = serde_json::from_value(serde_json::json!({
        "schema": 1, "uuid": "u", "name": NAME, "pid": std::process::id(), "ppid": 1,
        "started_at": "2026-10-28T00:00:00Z", "binary_version": "0.0.0",
        "transports": {"stdio": true, "unix_socket": null, "tcp": null},
        "card_digest": "d", "capabilities": [], "build_sha": "s", "proto_version": 1,
    }))
    .unwrap();
    std::fs::write(
        home.join("agents").join(NAME).join("running.lock"),
        serde_json::to_vec(&lock).unwrap(),
    )
    .unwrap();
}

fn reason(r: Result<Prepared, Step>) -> String {
    match r {
        Err(Step::Refusal { reason }) => reason,
        other => panic!("{other:?}"),
    }
}

#[test]
fn restart_needs_a_running_agent() {
    let h = home();
    let why = reason(prepare(&req(h.path(), Mode::Restart)));
    assert!(
        why.contains("is not running") && why.contains("mur agent start"),
        "{why}"
    );
    assert!(why.contains(NOTHING_CHANGED));
}

#[test]
fn restart_on_a_running_agent_holds_the_lock() {
    let h = home();
    write_live_lock(h.path());
    let p = prepare(&req(h.path(), Mode::Restart)).unwrap();
    assert_eq!(p.old_pid, Some(std::process::id()));
    let why = reason(prepare(&req(h.path(), Mode::Restart)));
    assert!(why.contains("another override is pending"), "{why}");
    drop(p);
    prepare(&req(h.path(), Mode::Restart)).unwrap();
}

#[test]
fn start_refuses_a_running_agent_and_points_at_restart() {
    let h = home();
    write_live_lock(h.path());
    let why = reason(prepare(&req(h.path(), Mode::Start)));
    assert!(
        why.contains("already running") && why.contains("mur agent restart"),
        "{why}"
    );
}

#[test]
fn start_refuses_while_a_boot_holds_the_sentinel() {
    use fs2::FileExt;
    let h = home();
    let s = h.path().join("agents").join(NAME).join("running.sentinel");
    std::fs::write(&s, b"").unwrap();
    let held = std::fs::File::open(&s).unwrap();
    held.try_lock_exclusive().unwrap();
    let why = reason(prepare(&req(h.path(), Mode::Start)));
    assert!(why.contains("already running"), "{why}");
    drop(held);
    let p = prepare(&req(h.path(), Mode::Start)).unwrap();
    assert_eq!(p.old_pid, None);
}

#[test]
fn a_missing_agent_is_refused() {
    let h = tempfile::tempdir().unwrap();
    let why = reason(prepare(&req(h.path(), Mode::Start)));
    assert!(why.contains("not found"), "{why}");
}

#[test]
fn check_home_compares_the_units_baked_home() {
    let h = home();
    let other = tempfile::tempdir().unwrap();
    let plist = |home: &Path| {
        format!(
            "<plist><dict><key>EnvironmentVariables</key><dict><key>MUR_HOME</key>\
             <string>{}</string></dict></dict></plist>",
            home.display()
        )
    };
    let unit = h.path().join("run.mur.agent.x.plist");
    std::fs::write(&unit, plist(h.path())).unwrap();
    check_home(NAME, h.path(), &unit).unwrap();
    std::fs::write(&unit, plist(other.path())).unwrap();
    let why = match check_home(NAME, h.path(), &unit) {
        Err(Step::Refusal { reason }) => reason,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("would never reach it"), "{why}");
}
