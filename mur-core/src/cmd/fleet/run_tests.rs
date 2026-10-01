//! Tests for `run.rs`. Split out as pure code movement to stay
//! under CLAUDE.md's 800-line-per-file rule; nothing changed in the move.

use super::*;

#[test]
fn exec_flag_gates_parallel_execution() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var(EXEC_FLAG_ENV);
    assert!(!parallel_exec_enabled(false));
    envg.set_var(EXEC_FLAG_ENV, "1");
    assert!(parallel_exec_enabled(false));
    envg.unset_var(EXEC_FLAG_ENV);
}

#[test]
fn force_worktree_bypasses_env_var() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var(EXEC_FLAG_ENV);
    assert!(
        parallel_exec_enabled(true),
        "an explicit force=true must enable isolation even with the env var unset"
    );
    assert!(!parallel_exec_enabled(false));
}

#[test]
fn delegate_fanout_is_bounded_even_without_the_parallel_flag() {
    // The regression: the cap used to apply ONLY under the experimental
    // worktree flag, so the ordinary path fanned out unbounded and six
    // members dialed one gateway at once.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var(FANOUT_ENV);
    assert_eq!(delegate_fanout(6), DEFAULT_DELEGATE_FANOUT);
    assert_eq!(delegate_fanout(1), 1, "never exceeds the step count");
    assert!(delegate_fanout(0) >= 1, "never zero");
}

#[test]
fn delegate_fanout_env_override_never_unbounds() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(FANOUT_ENV, "8");
    assert_eq!(delegate_fanout(20), 8);
    // Zero and garbage fall back to the default, not to "unbounded" —
    // unbounded is the bug this cap exists to prevent.
    envg.set_var(FANOUT_ENV, "0");
    assert_eq!(delegate_fanout(20), DEFAULT_DELEGATE_FANOUT);
    envg.set_var(FANOUT_ENV, "lots");
    assert_eq!(delegate_fanout(20), DEFAULT_DELEGATE_FANOUT);
    envg.unset_var(FANOUT_ENV);
}

#[test]
fn fanout_cap_is_bounded_and_nonzero() {
    assert!(fanout_cap(0) >= 1, "never zero");
    assert_eq!(fanout_cap(1), 1, "never exceeds the step count");
    assert!(fanout_cap(64) >= 1);
}

#[test]
fn worktree_routing_injected_per_track_by_id() {
    use crate::parallel::track::{Track, TrackSet};
    use mur_common::parallel::TrackConfig;
    let mk = |id: &str, intent: &str| ProcedureStep {
        id: Some(id.into()),
        intent: Some(intent.into()),
        ..Default::default()
    };
    let mut proc = Procedure {
        variables: vec![],
        steps: vec![
            mk("track-a", "do A"),
            mk("track-b", "do B"),
            mk("other", "do C"),
        ],
    };
    let track = |name: &str, path: &str| Track {
        config: TrackConfig {
            name: name.into(),
            approach: String::new(),
            model: None,
        },
        worktree_path: path.into(),
    };
    let ts = TrackSet {
        tracks: vec![track("track-a", "/wt/a"), track("track-b", "/wt/b")],
    };

    inject_worktree_routing(&mut proc, &ts);

    let a = proc.steps[0].intent.as_ref().unwrap();
    assert!(a.starts_with("do A"), "original intent preserved");
    assert!(
        a.contains("/wt/a") && a.contains("cwd"),
        "routed to its worktree via cwd"
    );
    assert!(proc.steps[1].intent.as_ref().unwrap().contains("/wt/b"));
    // A step with no matching track is left untouched.
    assert_eq!(proc.steps[2].intent.as_deref(), Some("do C"));
}

#[test]
fn build_fleet_procedure_one_delegate_step_per_member() {
    let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], None).unwrap();
    assert_eq!(p.steps.len(), 2);
    assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
    assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
    assert_eq!(p.steps[0].intent.as_deref(), Some("ship it"));
    assert!(p.steps[0].depends_on.is_empty()); // parallel rank 0
}

#[test]
fn parallel_goal_injects_approach() {
    let cfg = ParallelConfig {
        mode: mur_common::parallel::ParallelMode::Speculative,
        tracks: vec![
            mur_common::parallel::TrackConfig {
                name: "track1".to_string(),
                approach: "functional style".to_string(),
                model: None,
            },
            mur_common::parallel::TrackConfig {
                name: "track2".to_string(),
                approach: "imperative style".to_string(),
                model: None,
            },
        ],
        judge: mur_common::parallel::JudgeConfig {
            model: "test-model".to_string(),
            rubric: mur_common::parallel::Rubric::default(),
        },
        pre_filter: vec![],
        partition: None,
    };
    let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()], Some(&cfg))
        .unwrap();
    assert_eq!(p.steps.len(), 2);
    // First track should have functional style approach injected
    assert_eq!(p.steps[0].id.as_deref(), Some("track1"));
    let intent0 = p.steps[0].intent.as_deref().unwrap();
    assert!(intent0.contains("ship it"));
    assert!(intent0.contains("functional style"));
    assert!(intent0.contains("Approach:"));
    // Second track should have imperative style approach injected
    assert_eq!(p.steps[1].id.as_deref(), Some("track2"));
    let intent1 = p.steps[1].intent.as_deref().unwrap();
    assert!(intent1.contains("ship it"));
    assert!(intent1.contains("imperative style"));
    assert!(intent1.contains("Approach:"));
}

#[test]
fn resolve_goal_prefers_arg_then_queued_then_standing() {
    // pure resolution helper — no execution
    use mur_common::fleet::JobStatus;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    super::super::create::cmd_fleet_create(
        home,
        "dev",
        vec!["pm".into()],
        None,
        Some("standing".into()),
        None,
    )
    .unwrap();

    // no arg, empty queue → standing goal
    let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
    assert_eq!(goal, "standing");
    assert!(job.is_none());

    // queued job → job's text, marked running
    super::super::jobs::enqueue_job(home, "dev", "queued-work", "cli").unwrap();
    let (goal, job) = resolve_run_goal(home, "dev", None, "standing").unwrap();
    assert_eq!(goal, "queued-work");
    assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);

    // explicit arg jumps ahead of queue and is persisted
    let (goal, job) = resolve_run_goal(home, "dev", Some("urgent".into()), "standing").unwrap();
    assert_eq!(goal, "urgent");
    assert_eq!(job.as_ref().unwrap().status, JobStatus::Running);
    // arg job must be persisted (source=="cli")
    let all_jobs = super::super::jobs::list_jobs(home, "dev").unwrap();
    assert!(
        all_jobs.iter().any(|j| j.text == "urgent"),
        "arg job should be persisted"
    );
}

#[test]
fn partition_procedure_constrains_each_member_to_its_region() {
    use mur_common::parallel::TrackConfig;
    let source = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 0 }\n";
    let tracks = vec![
        TrackConfig {
            name: "track-0".into(),
            approach: String::new(),
            model: None,
        },
        TrackConfig {
            name: "track-1".into(),
            approach: String::new(),
            model: None,
        },
    ];
    let members = vec!["pm".to_string(), "qa".to_string()];
    let p = build_partition_procedure("build widget", "src/widget.rs", source, &members, &tracks)
        .unwrap();
    assert_eq!(p.steps.len(), 2);
    // Each step delegates to a member and mentions the file
    let intents: Vec<&str> = p.steps.iter().filter_map(|s| s.intent.as_deref()).collect();
    assert_eq!(intents.len(), 2);
    assert!(intents.iter().all(|i| i.contains("src/widget.rs")));
    // Steps reference different units
    assert_ne!(intents[0], intents[1]);
}
