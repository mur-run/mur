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
        let Some(status) = crate::run_status::status_of(mur_home, &run_id)? else {
            continue;
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
    let Some(status) = find_latest_run_for_channel(mur_home, &fleet.channel_id)? else {
        anyhow::bail!(
            "no run recorded for fleet `{name}` (channel `{}`) — try `mur fleet run {name}`",
            fleet.channel_id
        );
    };
    crate::cmd::job::print_status(out, &status);
    Ok(())
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
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
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
}
