//! Hot reload reaching a running runner: the same `Arc<TaskRunner>` picks up
//! an edited `profile.yaml` on its next turn, and keeps the last good one when
//! the edit is broken.

use super::*;
use crate::live_profile::{LiveProfile, LiveSettings, live_global_limits};

const FIXTURE: &str = include_str!("../../../../mur-common/tests/fixtures/minimal_profile.yaml");

fn write(path: &std::path::Path, text: &str) {
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn live_for(dir: &std::path::Path, profile_text: &str) -> Arc<LiveSettings> {
    write(&dir.join("profile.yaml"), profile_text);
    let boot = crate::profile::Profile::load(dir).unwrap().inner;
    Arc::new(LiveSettings {
        profile: LiveProfile::new(dir, boot),
        global_limits: live_global_limits(dir, Default::default()),
    })
}

fn unattended(spec: &mut TaskSpec) {
    spec.attended = false;
}

fn ran_until(out: &TaskOutcome) -> Option<String> {
    match out {
        TaskOutcome::Completed(task) => task
            .usage
            .as_ref()
            .and_then(|u| u.get("stop_reason"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        other => panic!("expected Completed, got {other:?}"),
    }
}

fn runner() -> Arc<TaskRunner> {
    use crate::llm::stub::SequenceLlm;
    let responses: Vec<crate::llm::LlmResponse> = (0..6)
        .map(|i| tool_call_response(&format!("id-{i}"), &format!("echo {i}")))
        .chain(std::iter::once(end_turn_response("DONE")))
        .collect();
    Arc::new(
        TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1)
            .with_iteration_ceiling(50),
    )
}

#[tokio::test]
async fn an_edited_limits_block_bounds_the_next_turn_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_for(dir.path(), FIXTURE);

    // Turn 1: no limits anywhere → runs to its natural end.
    let r = runner();
    r.attach_live(live.clone());
    let mut spec = loop_spec("one");
    unattended(&mut spec);
    assert_eq!(ran_until(&r.run_sync(spec).await), None);

    // Edit the file under the running runner: a zero deadline.
    write(
        &dir.path().join("profile.yaml"),
        &format!("{FIXTURE}\nlimits: {{ deadline: 0s }}\n"),
    );
    let r = runner();
    r.attach_live(live.clone());
    let mut spec = loop_spec("two");
    unattended(&mut spec);
    assert_eq!(
        ran_until(&r.run_sync(spec).await).as_deref(),
        Some("deadline")
    );
}

#[tokio::test]
async fn a_broken_limits_edit_does_not_fail_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let live = live_for(
        dir.path(),
        &format!("{FIXTURE}\nlimits: {{ deadline: 0s }}\n"),
    );
    write(
        &dir.path().join("profile.yaml"),
        &format!("{FIXTURE}\nlimits: {{ deadline: 5x }}\n"),
    );
    let r = runner();
    r.attach_live(live.clone());
    let mut spec = loop_spec("bad");
    unattended(&mut spec);
    let out = r.run_sync(spec).await;
    // The last good profile (deadline 0s) still serves; the turn is not a
    // `limits` failure.
    assert_eq!(ran_until(&out).as_deref(), Some("deadline"));
    assert!(live.profile.state().last_reload_error.is_some());
}
