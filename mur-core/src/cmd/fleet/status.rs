//! `mur fleet status <name>` — the fleet's most recent run, rendered by the
//! same code path as `mur job status` (spec §4: a lookup plus the same
//! renderer, NOT a separate status computation).

use std::path::Path;

use anyhow::Result;

/// The fleet's most recent run: the newest run whose sidecar records the
/// fleet's channel, derived through `status_of` (the single derivation
/// point). `Ok(None)` when no run has ever recorded this channel.
pub fn find_latest_run_for_channel(
    mur_home: &Path,
    channel_id: &str,
) -> Result<Option<crate::run_status::RunStatus>> {
    use crate::run_status::store;

    let mut newest: Option<(chrono::DateTime<chrono::Utc>, crate::run_status::RunStatus)> = None;
    for run_id in store::list_ids(mur_home)? {
        // A sidecar read failure skips the candidate — a corrupt index on an
        // unrelated run must not hide this fleet's runs — but it is warned,
        // never silent: `load_sidecar` distinguishes "absent" from "unreadable"
        // and that distinction is the operator's only clue.
        let sidecar = match store::load_sidecar(mur_home, &run_id) {
            Ok(Some(sidecar)) => sidecar,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    run_id,
                    %error,
                    "unreadable sidecar — skipping this run while looking for the fleet's latest"
                );
                continue;
            }
        };
        if sidecar.channel_id != channel_id {
            continue;
        }
        // Same rule for the record itself: one unreadable `run.json` on the
        // fleet's channel must not make `mur fleet status` fail outright. It
        // is warned, because skipping it may mean an older run is shown.
        let status = match crate::run_status::status_of(mur_home, &run_id) {
            Ok(Some(status)) => status,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    run_id,
                    error = format!("{error:#}"),
                    "unreadable run record — skipping this run while looking for the fleet's latest"
                );
                continue;
            }
        };
        if newest
            .as_ref()
            .is_none_or(|(latest, _)| status.run.started_at > *latest)
        {
            newest = Some((status.run.started_at, status));
        }
    }
    Ok(newest.map(|(_, status)| status))
}

/// `mur fleet status <name>` — the fleet's most recent run via the shared
/// `job::print_status` renderer.
pub fn cmd_fleet_status(mur_home: &Path, name: &str, out: &mut dyn std::io::Write) -> Result<()> {
    let fleet = super::store::load_fleet(mur_home, name)?;
    let pause_target = super::control::paused_run_id(mur_home, name);
    let targeted = pause_target
        .as_deref()
        .and_then(|id| pause_targeted_run(mur_home, &fleet.channel_id, id));
    let latest = match targeted {
        Some(status) => Some(status),
        None => find_latest_run_for_channel(mur_home, &fleet.channel_id)?,
    };
    let Some(status) = latest else {
        anyhow::bail!(
            "no run recorded for fleet `{name}` (channel `{}`) — try `mur fleet run {name}`",
            fleet.channel_id
        );
    };
    crate::cmd::job::print_status(out, &status);
    crate::cmd::job::print_paused(out, mur_home, &status);
    if let Some(note) = pausing_note(
        pause_target.as_deref(),
        super::control::is_stopped(mur_home, name),
        &status,
    ) {
        let _ = writeln!(out, "{note}");
    }
    Ok(())
}

/// The run the pause sentinel names, when it is this fleet's and not over.
///
/// The sentinel names the OUTER loop run, and the outer record has no sidecar
/// (`loop_run/run_record.rs`), so [`find_latest_run_for_channel`] only ever
/// finds an iteration: its id never matches the sentinel and its record holds
/// none of the outer checkpoint's spend. Read the named record directly. A
/// stale sentinel (another channel, a finished run, an unreadable record)
/// yields `None`, and the sidecar lookup answers as before.
fn pause_targeted_run(
    mur_home: &Path,
    channel_id: &str,
    run_id: &str,
) -> Option<crate::run_status::RunStatus> {
    let status = match crate::run_status::status_of(mur_home, run_id) {
        Ok(Some(status)) => status,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                run_id,
                error = format!("{error:#}"),
                "unreadable record for the pause-targeted run — showing the fleet's latest instead"
            );
            return None;
        }
    };
    (status.run.channel_id.as_deref() == Some(channel_id) && !status.state.is_terminal())
        .then_some(status)
}

/// The "pausing…" line (pause/resume design §5.5): `.paused` names this run
/// but its record has not reached `State::Paused` yet, so the loop is still
/// on its way to a safe point. `mur fleet list` keeps showing `▶` for the same
/// window; this line is what explains it. Nothing when stop already won, when
/// the sentinel is for another run, or when the process is gone (`abandoned`
/// says the true thing there, and a dead run is not pausing).
fn pausing_note(
    pause_target: Option<&str>,
    stopped: bool,
    status: &crate::run_status::RunStatus,
) -> Option<&'static str> {
    let targets_this_run = pause_target.is_some_and(|id| id == status.run.run_id);
    (targets_this_run
        && !stopped
        && !status.abandoned
        && status.state == crate::run_status::State::Running)
        .then_some("pause     requested — pausing… (the run stops at its next safe point)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_status::{RUN_SCHEMA, RunKind, RunState, SIDECAR_SCHEMA, Sidecar, State, store};

    /// Record a finished run + its sidecar, with a controllable start time so
    /// "most recent" is testable.
    fn save_run(
        mur_home: &Path,
        run_id: &str,
        channel_id: &str,
        started_at: chrono::DateTime<chrono::Utc>,
    ) {
        store::save(
            mur_home,
            &RunState {
                schema: RUN_SCHEMA,
                run_id: run_id.to_string(),
                channel_id: Some(channel_id.to_string()),
                kind: RunKind::Fleet,
                label: "fleet run".to_string(),
                pid: std::process::id(),
                started_at,
                last_heartbeat_at: Some(chrono::Utc::now()),
                state: State::Done,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".to_string(),
                build_sha: "deadbee".to_string(),
            },
        )
        .unwrap();
        store::save_sidecar(
            mur_home,
            run_id,
            &Sidecar {
                schema: SIDECAR_SCHEMA,
                channel_id: channel_id.to_string(),
                kind: RunKind::Fleet,
                first_seq: 0,
            },
        )
        .unwrap();
    }

    fn sample_fleet(name: &str, channel_id: &str) -> mur_common::fleet::Fleet {
        mur_common::fleet::Fleet {
            name: name.to_string(),
            display_name: String::new(),
            goal: "g".to_string(),
            router: None,
            team_id: None,
            members: vec![],
            channel_id: channel_id.to_string(),
            procedure: vec![],
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
            limits: None,
            needs: vec![],
        }
    }

    /// A corrupt sidecar on an unrelated run must not hide this fleet's runs
    /// — but it must not vanish either. The skip is warned, so an operator
    /// who wonders why a run is missing has one line telling them.
    #[test]
    fn an_unreadable_sidecar_is_warned_not_silently_skipped() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let base = chrono::Utc::now();

        // The fleet's real run, plus an unrelated run whose sidecar is corrupt.
        save_run(mur_home, "run-good", "fleet-x", base);
        let dir = store::runs_dir(mur_home).join("run-bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sidecar.json"), b"{ not json").unwrap();

        let capture = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .finish();
        let found = tracing::subscriber::with_default(subscriber, || {
            find_latest_run_for_channel(mur_home, "fleet-x")
        })
        .unwrap();

        assert_eq!(
            found
                .expect("the corrupt neighbour must not hide the good run")
                .run
                .run_id,
            "run-good"
        );
        let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("sidecar"),
            "the unreadable sidecar must be warned, not silently skipped: {logged}"
        );
    }

    /// THE lookup rule: the newest run whose SIDECAR records the fleet's
    /// channel wins. A newer run on a different channel must not leak in,
    /// and older runs on the fleet's channel must not shadow the newest.
    #[test]
    fn find_latest_run_for_channel_picks_the_newest_matching_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let base = chrono::Utc::now();
        save_run(
            mur_home,
            "run-old",
            "fleet-dev",
            base - chrono::Duration::hours(2),
        );
        save_run(
            mur_home,
            "run-new",
            "fleet-dev",
            base - chrono::Duration::hours(1),
        );
        // Newest of all, but on ANOTHER channel — must be ignored.
        save_run(mur_home, "run-other", "fleet-qa", base);

        let found = find_latest_run_for_channel(mur_home, "fleet-dev")
            .unwrap()
            .expect("two runs recorded the fleet's channel");
        assert_eq!(
            found.run.run_id, "run-new",
            "the newest matching sidecar must win; the other channel's newer \
             run must not leak in"
        );
    }

    #[test]
    fn find_latest_run_for_channel_is_none_without_a_matching_run() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            find_latest_run_for_channel(tmp.path(), "fleet-dev")
                .unwrap()
                .is_none()
        );
    }

    /// `fleet status` must render through the SAME function as `mur job
    /// status` — the state/liveness lines appear verbatim. Two renderers is
    /// how two surfaces drift into disagreeing about one fact.
    #[test]
    fn fleet_status_renders_through_the_shared_status_renderer() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        save_run(mur_home, "run-new", "fleet-dev", chrono::Utc::now());

        let mut out = Vec::new();
        cmd_fleet_status(mur_home, "dev", &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("run       run-new\n"), "{text}");
        assert!(
            text.contains("state     done\n") && text.contains("liveness  -\n"),
            "fleet status must carry the same state/liveness lines as \
             `mur job status`: {text}"
        );
    }

    /// #1622 follow-up, end to end through `status_of`: the fleet's latest
    /// run is still `running` on disk, its process is gone, and its last
    /// heartbeat is long past the grace window. `mur fleet status` must say
    /// `abandoned`, not `running`.
    #[test]
    fn fleet_status_reports_a_long_dead_run_as_abandoned() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        save_run(mur_home, "run-dead", "fleet-dev", chrono::Utc::now());
        let stale = crate::run_status::stale_after(&mur_common::config::RunsConfig::default());
        store::update(mur_home, "run-dead", |r| {
            r.state = State::Running;
            r.pid = dead_pid();
            r.last_heartbeat_at = Some(
                chrono::Utc::now()
                    - crate::run_status::abandon::grace(stale)
                    - chrono::Duration::minutes(1),
            );
        })
        .unwrap();

        let mut out = Vec::new();
        cmd_fleet_status(mur_home, "dev", &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("state     abandoned\n"), "{text}");
        assert!(!text.contains("state     running"), "{text}");
    }

    /// Write the pause sentinel the way `mur fleet pause` does.
    fn request_pause(mur_home: &Path, fleet: &str, run_id: &str) {
        let path = super::super::control::paused_path(mur_home, fleet);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("run_id: {run_id}\n")).unwrap();
    }

    fn status_text(mur_home: &Path) -> String {
        let mut out = Vec::new();
        cmd_fleet_status(mur_home, "dev", &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// §5.5: sentinel written, run still `Running` → "pausing…". Once the
    /// record reads `Paused` the state line says so and the note goes away.
    #[test]
    fn fleet_status_says_pausing_until_the_record_is_paused() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        save_run(mur_home, "run-1", "fleet-dev", chrono::Utc::now());
        store::update(mur_home, "run-1", |r| r.state = State::Running).unwrap();

        assert!(
            !status_text(mur_home).contains("pausing"),
            "no sentinel yet"
        );

        request_pause(mur_home, "dev", "run-1");
        let text = status_text(mur_home);
        assert!(text.contains("state     running\n"), "{text}");
        assert!(text.contains("pause     requested — pausing…"), "{text}");

        store::update(mur_home, "run-1", |r| r.state = State::Paused).unwrap();
        let text = status_text(mur_home);
        assert!(text.contains("state     paused\n"), "{text}");
        assert!(!text.contains("pausing"), "{text}");
    }

    /// Production layout (§5.5): the sentinel names the OUTER run, which has
    /// no sidecar; the newest sidecar-indexed run is an iteration. Status must
    /// follow the sentinel to the outer record, so the pending pause reads
    /// "pausing…" and, once paused, the outer checkpoint's spend shows.
    #[test]
    fn fleet_status_follows_the_sentinel_to_the_sidecarless_outer_run() {
        use crate::cmd::fleet::loop_run::checkpoint;
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        let start = chrono::Utc::now() - chrono::Duration::minutes(10);
        // The outer loop record: as `run_record::open` writes it, no sidecar.
        store::save(
            mur_home,
            &RunState {
                schema: RUN_SCHEMA,
                run_id: "r".to_string(),
                channel_id: Some("fleet-dev".to_string()),
                kind: RunKind::Fleet,
                label: "fleet dev loop".to_string(),
                pid: std::process::id(),
                started_at: start,
                last_heartbeat_at: Some(chrono::Utc::now()),
                state: State::Running,
                steps: vec![],
                blocked_on: None,
                binary_version: "0.0.0-test".to_string(),
                build_sha: "deadbee".to_string(),
            },
        )
        .unwrap();
        // The current iteration, sidecar-indexed and newer.
        save_run(mur_home, "r-3", "fleet-dev", chrono::Utc::now());
        store::update(mur_home, "r-3", |r| r.state = State::Running).unwrap();

        request_pause(mur_home, "dev", "r");
        let text = status_text(mur_home);
        assert!(text.contains("pause     requested — pausing…"), "{text}");

        store::update(mur_home, "r", |r| r.state = State::Paused).unwrap();
        checkpoint::save(mur_home, &checkpoint::sample_for_test("r")).unwrap();
        let text = status_text(mur_home);
        assert!(text.contains("state     paused\n"), "{text}");
        assert!(text.contains("$1.84 of $5.00"), "{text}");
        assert!(text.contains("`mur fleet resume dev`"), "{text}");
    }

    /// A sentinel naming a finished run is stale: status falls back to the
    /// sidecar lookup instead of resurrecting the old record.
    #[test]
    fn a_stale_sentinel_for_a_finished_run_falls_back_to_the_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        let base = chrono::Utc::now();
        save_run(mur_home, "run-old", "fleet-dev", base - chrono::Duration::hours(1));
        save_run(mur_home, "run-new", "fleet-dev", base);
        request_pause(mur_home, "dev", "run-old");
        let text = status_text(mur_home);
        assert!(text.contains("run-new"), "{text}");
    }

    /// A stale sentinel for another run, or one that lost to `stop`, never
    /// claims the shown run is pausing.
    #[test]
    fn pausing_note_ignores_other_runs_and_a_stopped_fleet() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();
        save_run(mur_home, "run-1", "fleet-dev", chrono::Utc::now());
        store::update(mur_home, "run-1", |r| r.state = State::Running).unwrap();
        let status = crate::run_status::status_of(mur_home, "run-1")
            .unwrap()
            .unwrap();

        assert!(pausing_note(Some("run-old"), false, &status).is_none());
        assert!(pausing_note(Some("run-1"), true, &status).is_none());
        assert!(pausing_note(None, false, &status).is_none());
        assert!(pausing_note(Some("run-1"), false, &status).is_some());

        let mut dead = status.clone();
        dead.abandoned = true;
        assert!(pausing_note(Some("run-1"), false, &dead).is_none());
    }

    fn dead_pid() -> u32 {
        #[cfg(unix)]
        let mut child = std::process::Command::new("true").spawn().unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn fleet_status_without_any_run_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        super::super::store::save_fleet(mur_home, &sample_fleet("dev", "fleet-dev")).unwrap();

        let mut out = Vec::new();
        let err = cmd_fleet_status(mur_home, "dev", &mut out).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no run recorded for fleet `dev`"),
            "the operator must be told there is no run, not an empty screen: {msg}"
        );
    }

    /// #1674: an unreadable `run.json` on the fleet's own channel is skipped,
    /// not fatal — even when it is the newest run — so `mur fleet status`
    /// still shows the newest run it can read.
    #[test]
    fn an_unreadable_run_record_on_the_channel_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let base = chrono::Utc::now();
        save_run(
            mur_home,
            "run-good",
            "fleet-x",
            base - chrono::Duration::hours(1),
        );
        save_run(mur_home, "run-bad", "fleet-x", base);
        std::fs::write(store::run_path(mur_home, "run-bad"), b"{ not json").unwrap();

        let found = find_latest_run_for_channel(mur_home, "fleet-x")
            .expect("one bad record must not fail the lookup")
            .expect("the readable run is still there");
        assert_eq!(found.run.run_id, "run-good");
    }
}
