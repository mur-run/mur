//! Resume (pause/resume design §5.4, §6, §7, §8.2a, §10).

use super::*;
use crate::cmd::fleet::loop_run::checkpoint::{CHECKPOINT_SCHEMA, Checkpoint};
use mur_common::fleet::{Fleet, JobStatus};
use mur_common::limits::Stuck;

const RUN: &str = "paused-run";

fn fleet() -> Fleet {
    Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "g".into(),
        router: None,
        members: vec!["pm".into()],
        channel_id: "fleet-dev".into(),
        procedure: vec![],
        team_id: None,
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

fn cp(job_id: Option<&str>) -> Checkpoint {
    Checkpoint {
        schema: CHECKPOINT_SCHEMA,
        run_id: RUN.into(),
        iteration_run_id: format!("{RUN}-2"),
        fleet: "dev".into(),
        iteration: 2,
        spent_usd: 1.5,
        active_elapsed_ms: 60_000,
        stuck_elapsed_ms: 5_000,
        start_seq: 7,
        job_id: job_id.map(str::to_string),
        goal_override: None,
        deadline_ms: 600_000,
        stuck_ms: Some(300_000),
        cost_usd: Some(5.0),
        hitl_unanswered: "defer".into(),
        target_cwd: None,
        procedure: vec![ProcedureStep {
            id: Some("s0".into()),
            description: "pm: work".into(),
            delegate_to: Some("pm".into()),
            ..Default::default()
        }],
        paused_at: "2026-10-22T00:00:00Z".into(),
        binary_version: "test".into(),
    }
}

fn record(home: &Path, state: crate::run_status::State) {
    let now = chrono::Utc::now();
    crate::run_status::store::save(
        home,
        &crate::run_status::RunState {
            schema: crate::run_status::RUN_SCHEMA,
            run_id: RUN.into(),
            channel_id: Some("fleet-dev".into()),
            kind: crate::run_status::RunKind::Fleet,
            label: "fleet dev loop".into(),
            pid: 1,
            started_at: now,
            last_heartbeat_at: Some(now),
            state,
            steps: vec![],
            blocked_on: None,
            binary_version: "test".into(),
            build_sha: String::new(),
        },
    )
    .unwrap();
}

/// A home with the fleet paused: sentinel + checkpoint + `Paused` record.
fn paused_home(job_id: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    store::save_fleet(tmp.path(), &fleet()).unwrap();
    std::fs::write(
        control::paused_path(tmp.path(), "dev"),
        format!("run_id: {RUN}\nrequested_at: t\n"),
    )
    .unwrap();
    checkpoint::save(tmp.path(), &cp(job_id)).unwrap();
    record(tmp.path(), crate::run_status::State::Paused);
    tmp
}

fn bounds(deadline_s: u64, stuck: Stuck, cost: Option<f64>) -> FleetBounds {
    let f = fleet();
    let mut b = super::super::fleet_bounds(tempfile::tempdir().unwrap().path(), &f, None, None)
        .expect("built-in bounds resolve");
    b.deadline = Duration::from_secs(deadline_s);
    b.stuck = stuck;
    b.cost_usd = cost;
    b
}

#[test]
fn bounds_are_the_tighter_of_checkpoint_and_fleet_yaml() {
    let r = resume_from(&cp(None), None, false).unwrap();
    // fleet.yaml tightened during the pause: it bites.
    let t = tighten(
        bounds(60, Stuck::After(Duration::from_secs(10)), Some(1.0)),
        &r,
    );
    assert_eq!(t.deadline, Duration::from_secs(60));
    assert_eq!(t.stuck, Stuck::After(Duration::from_secs(10)));
    assert_eq!(t.cost_usd, Some(1.0));
    // fleet.yaml loosened (or the checkpoint edited down): the frozen bound holds.
    let t = tighten(bounds(86_400, Stuck::Off, None), &r);
    assert_eq!(t.deadline, Duration::from_millis(600_000));
    assert_eq!(t.stuck, Stuck::After(Duration::from_millis(300_000)));
    assert_eq!(t.cost_usd, Some(5.0));
}

#[test]
fn counters_never_move_down_and_the_signed_note_wins_over_an_edited_checkpoint() {
    let mut edited = cp(None);
    edited.spent_usd = 0.01; // a run lowering its own spend
    edited.active_elapsed_ms = 1;
    let r = resume_from(&edited, Some((1.5, 60_000, 2)), false).unwrap();
    assert_eq!(r.spent_usd, 1.5);
    assert_eq!(r.active_elapsed, Duration::from_millis(60_000));
    assert_eq!(r.iteration, 2);
    assert!(
        !r.procedure.is_empty(),
        "same iteration: the stored plan is used"
    );
    // Negative control: without a signed note, the checkpoint is all there is.
    let r = resume_from(&edited, None, false).unwrap();
    assert_eq!(r.spent_usd, 0.01);
}

#[test]
fn a_signed_iteration_ahead_of_the_checkpoint_replans_instead_of_replaying() {
    let r = resume_from(&cp(None), Some((1.5, 60_000, 3)), false).unwrap();
    assert_eq!(r.iteration, 3);
    assert!(r.procedure.is_empty());
}

#[test]
fn hitl_is_never_loosened_from_defer() {
    let mut c = cp(None);
    // Unattended resume of a run frozen at `wait`: tightened to `defer`.
    c.hitl_unanswered = "wait".into();
    assert_eq!(
        resume_from(&c, None, true).unwrap().hitl_unanswered,
        Unanswered::Defer
    );
    // Resume from a terminal of a run frozen at `defer`: stays `defer`.
    c.hitl_unanswered = "defer".into();
    assert_eq!(
        resume_from(&c, None, false).unwrap().hitl_unanswered,
        Unanswered::Defer
    );
    // `deny` is stricter than `defer` and survives.
    c.hitl_unanswered = "deny".into();
    assert_eq!(
        resume_from(&c, None, true).unwrap().hitl_unanswered,
        Unanswered::Deny
    );
    c.hitl_unanswered = "maybe".into();
    assert!(resume_from(&c, None, true).is_err());
}

#[test]
fn admit_accepts_a_paused_run() {
    let home = paused_home(None);
    let cp = admit(home.path(), "dev").expect("paused run is resumable");
    assert_eq!(cp.run_id, RUN);
}

#[test]
fn admit_refuses_with_no_paused_run() {
    let home = tempfile::tempdir().unwrap();
    store::save_fleet(home.path(), &fleet()).unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert_eq!(
        format!("{err}"),
        "fleet 'dev' has no paused run; use `mur fleet run`"
    );
}

#[test]
fn admit_refuses_a_stopped_fleet() {
    let home = paused_home(None);
    std::fs::write(control::stopped_path(home.path(), "dev"), "stopped\n").unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(
        format!("{err}").contains("does not revive a paused run"),
        "{err}"
    );
}

#[test]
fn admit_refuses_a_live_run() {
    let home = paused_home(None);
    let _held = run_lock::acquire(home.path(), "dev").unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(format!("{err}").contains("is live"), "{err}");
}

#[test]
fn admit_refuses_a_foreign_schema_and_suggests_stop() {
    let home = paused_home(None);
    let mut c = cp(None);
    c.schema = CHECKPOINT_SCHEMA + 1;
    checkpoint::save(home.path(), &c).unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(format!("{err}").contains("mur fleet stop dev"), "{err}");
}

#[test]
fn admit_refuses_a_record_that_is_not_paused_or_is_missing() {
    let home = paused_home(None);
    record(home.path(), crate::run_status::State::Running);
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(format!("{err}").contains("not paused"), "{err}");

    std::fs::remove_file(crate::run_status::store::run_path(home.path(), RUN)).unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(format!("{err}").contains("run record missing"), "{err}");
}

#[test]
fn a_fresh_run_over_a_paused_one_is_refused() {
    let home = paused_home(None);
    let err = refuse_fresh_run_over_pause(home.path(), "dev").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("mur fleet resume dev"), "{msg}");
    assert!(msg.contains("mur fleet stop dev"), "{msg}");
    assert!(control::paused_path(home.path(), "dev").exists());
}

#[test]
fn a_sentinel_left_by_a_crash_does_not_block_a_fresh_run_and_is_removed() {
    let home = paused_home(None);
    record(home.path(), crate::run_status::State::Failed);
    refuse_fresh_run_over_pause(home.path(), "dev").expect("stale sentinel is ignored");
    assert!(!control::paused_path(home.path(), "dev").exists());
}

#[tokio::test]
async fn run_guarded_refuses_a_fresh_run_over_a_paused_one() {
    let home = paused_home(None);
    let err = super::super::run_guarded(home.path(), "dev", None, None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("has a paused run"), "{err}");
    // The paused run is untouched.
    assert!(checkpoint::load(home.path(), RUN).unwrap().is_some());
}

#[test]
fn stop_while_paused_ends_the_run_cancels_the_job_and_drops_the_checkpoint() {
    let home = paused_home(None);
    let mut job = crate::cmd::fleet::jobs::enqueue_job(home.path(), "dev", "t", "cli").unwrap();
    job.status = JobStatus::Running;
    crate::cmd::fleet::jobs::save_job(home.path(), "dev", &job).unwrap();
    checkpoint::save(home.path(), &cp(Some(&job.id))).unwrap();

    control::cmd_fleet_stop(home.path(), "dev").unwrap();

    let rec = crate::run_status::store::load(home.path(), RUN)
        .unwrap()
        .unwrap();
    assert_eq!(rec.state, crate::run_status::State::Stopped);
    let jobs = crate::cmd::fleet::jobs::list_jobs_raw(home.path(), "dev").unwrap();
    let j = jobs.iter().find(|j| j.id == job.id).unwrap();
    assert_eq!(j.status, JobStatus::Canceled);
    assert_eq!(
        j.error.as_deref(),
        Some("stopped by `mur fleet stop` while paused")
    );
    assert!(checkpoint::load(home.path(), RUN).unwrap().is_none());
    assert!(!control::paused_path(home.path(), "dev").exists());
    // And it cannot be resumed afterwards — not even once `start` clears the switch.
    control::cmd_fleet_start(home.path(), "dev").unwrap();
    let err = admit(home.path(), "dev").unwrap_err();
    assert!(format!("{err}").contains("no paused run"), "{err}");
}

#[test]
fn stop_with_a_live_run_leaves_its_pause_state_to_the_run() {
    let home = paused_home(None);
    let _held = run_lock::acquire(home.path(), "dev").unwrap();
    assert!(!end_paused_run(home.path(), "dev"));
    assert!(checkpoint::load(home.path(), RUN).unwrap().is_some());
}

#[test]
fn a_reentered_iteration_reuses_its_running_job_and_never_claims_another() {
    let home = paused_home(None);
    let mut held = crate::cmd::fleet::jobs::enqueue_job(home.path(), "dev", "held", "cli").unwrap();
    held.status = JobStatus::Running;
    crate::cmd::fleet::jobs::save_job(home.path(), "dev", &held).unwrap();
    let queued = crate::cmd::fleet::jobs::enqueue_job(home.path(), "dev", "next", "cli").unwrap();

    let r = resume_from(&cp(Some(&held.id)), None, false).unwrap();
    let (goal, job) = reentered_goal(home.path(), "dev", "standing", &r).unwrap();
    assert_eq!(goal, "held");
    assert_eq!(job.unwrap().id, held.id);
    let jobs = crate::cmd::fleet::jobs::list_jobs_raw(home.path(), "dev").unwrap();
    let q = jobs.iter().find(|j| j.id == queued.id).unwrap();
    assert_eq!(q.status, JobStatus::Queued, "the queue was not drained");
}

#[tokio::test]
async fn a_resumed_run_keeps_its_original_started_at() {
    let home = paused_home(None);
    let before = crate::run_status::store::load(home.path(), RUN)
        .unwrap()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let beat = super::super::run_record::open(home.path(), &fleet(), "dev", RUN);
    let after = crate::run_status::store::load(home.path(), RUN)
        .unwrap()
        .unwrap();
    drop(beat);

    assert_eq!(
        after.started_at, before.started_at,
        "resume must not restart the run's clock"
    );
    assert_eq!(after.state, crate::run_status::State::Running);
    assert_eq!(after.pid, std::process::id());
}

/// Review of #2067: two `resume`s can both pass `admit` before either holds
/// the lock. The later one must not reopen the run from its stale snapshot
/// once the earlier one resumed and paused again (new `paused_at`) or ended.
#[test]
fn a_stale_admission_cannot_reopen_a_superseded_pause() {
    let home = paused_home(None);
    let a = resume_from(&admit(home.path(), "dev").unwrap(), None, false).unwrap();
    let b = resume_from(&admit(home.path(), "dev").unwrap(), None, false).unwrap();

    // `a` wins the lock, runs further and pauses again: a newer checkpoint.
    revalidate_under_lock(home.path(), "dev", &a).expect("first resume is current");
    let mut newer = cp(None);
    newer.spent_usd = 3.0;
    newer.active_elapsed_ms = 120_000;
    newer.paused_at = "2026-10-22T01:00:00Z".into();
    checkpoint::save(home.path(), &newer).unwrap();

    let err = revalidate_under_lock(home.path(), "dev", &b).unwrap_err();
    assert!(format!("{err}").contains("no longer current"), "{err}");

    // `a` finished instead: the pause is gone, and `b` is refused too.
    let _ = std::fs::remove_file(control::paused_path(home.path(), "dev"));
    record(home.path(), crate::run_status::State::Done);
    assert!(revalidate_under_lock(home.path(), "dev", &b).is_err());
}

/// Review of #2067: start unattended (frozen `defer`) → resume from a terminal
/// → pause → resume from a terminal. The policy every pause persists is the
/// one enforced, so no cycle turns `defer` into `wait`; a stricter mode set
/// in `fleet.yaml` since still tightens it.
#[test]
fn repeated_pause_cycles_never_loosen_the_frozen_hitl_policy() {
    use mur_common::hitl::Unanswered::{Defer, Deny, Wait};
    // Start unattended, no `hitl.mode`: the process default is `defer`.
    let mut policy = effective_hitl(None, None, Defer);
    for _ in 0..3 {
        // A terminal resume reads back what the last pause persisted …
        let mut c = cp(None);
        c.hitl_unanswered = serde_json::to_value(policy)
            .unwrap()
            .as_str()
            .unwrap()
            .into();
        let r = resume_from(&c, None, false).unwrap();
        // … and the resumed run (terminal default `wait`) enforces/persists:
        policy = effective_hitl(None, Some(r.hitl_unanswered), Wait);
        assert_eq!(policy, Defer, "a terminal resume must keep `defer`");
    }
    assert_eq!(effective_hitl(Some(Deny), Some(Defer), Wait), Deny);
    assert_eq!(effective_hitl(Some(Wait), Some(Defer), Wait), Defer);
    assert_eq!(effective_hitl(Some(Wait), None, Defer), Wait);
}

/// `stop` on a paused run must leave the channel saying so: before this the
/// channel's last word was `run_paused`, and a status rebuilt from it read
/// `running` forever (the `resume-smoke` field report).
#[test]
fn stop_while_paused_ends_both_run_ids_on_the_channel() {
    let home = paused_home(None);
    let svc = mur_channel::ChannelService::open(home.path()).unwrap();
    svc.create_for_fleet("dev", "mur", &["pm".to_string()])
        .unwrap();
    let iter_run = format!("{RUN}-2");
    let sc = crate::run_status::Sidecar {
        schema: crate::run_status::SIDECAR_SCHEMA,
        channel_id: "fleet-dev".into(),
        kind: crate::run_status::RunKind::Fleet,
        first_seq: 0,
    };

    let mut iter_rec = crate::run_status::store::load(home.path(), RUN)
        .unwrap()
        .unwrap();
    iter_rec.run_id = iter_run.clone();
    crate::run_status::store::save(home.path(), &iter_rec).unwrap();

    control::cmd_fleet_stop(home.path(), "dev").unwrap();

    let rec = crate::run_status::store::load(home.path(), &iter_run)
        .unwrap()
        .unwrap();
    assert_eq!(
        rec.state,
        crate::run_status::State::Stopped,
        "the iteration's record too"
    );
    for id in [RUN, iter_run.as_str()] {
        assert_eq!(
            crate::run_status::rebuild::run_tail_state(home.path(), &sc, id).unwrap(),
            Some(crate::run_status::State::Stopped),
            "{id} must end on the channel"
        );
    }
}
