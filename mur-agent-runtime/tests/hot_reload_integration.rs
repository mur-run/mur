//! Hot reload, end to end through the public seams the supervisor wires up:
//! an on-disk `profile.yaml` / `config.yaml` → [`LiveSettings`] → a running
//! [`TaskRunner`] / [`CronScheduler`] → `reload_state.json` (what
//! `mur agent runtime-doctor` reads). No real model: a scripted LLM stands in,
//! so these run in CI and pin what the manual E2E for #1788 checked by hand.

use mur_agent_runtime::live_profile::LiveSettings;
use mur_agent_runtime::llm::stub::SequenceLlm;
use mur_agent_runtime::llm::{LlmResponse, RequestIntent, StopReason};
use mur_agent_runtime::scheduler::CronScheduler;
use mur_agent_runtime::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::{RELOAD_STATE_FILE, ReloadState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const FIXTURE: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");
/// The fixture's agent name; `agent_home` is named after it, as on disk.
const AGENT: &str = "bridge_test";

/// Atomic temp-file + rename — how every MUR writer (CLI, Hub, `remind`)
/// saves YAML, so the reload sees a new inode, not an in-place edit.
fn save(path: &Path, text: &str) {
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

struct Home {
    _root: tempfile::TempDir,
    mur_home: PathBuf,
    agent_home: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mur_home = root.path().join("mur");
        let agent_home = mur_home.join("agents").join(AGENT);
        std::fs::create_dir_all(&agent_home).unwrap();
        save(&agent_home.join("profile.yaml"), FIXTURE);
        save(&mur_home.join("config.yaml"), "{}\n");
        Self {
            _root: root,
            mur_home,
            agent_home,
        }
    }

    /// What the supervisor builds after the seal (`supervisor/mod.rs` step 4).
    fn live(&self) -> Arc<LiveSettings> {
        let boot = mur_agent_runtime::profile::Profile::load(&self.agent_home)
            .unwrap()
            .inner;
        LiveSettings::new(&self.agent_home, &self.mur_home, boot, Default::default())
    }

    fn edit_profile(&self, extra: &str) {
        save(
            &self.agent_home.join("profile.yaml"),
            &format!("{FIXTURE}{extra}"),
        );
    }

    fn doctor_view(&self) -> ReloadState {
        ReloadState::load(&self.agent_home.join(RELOAD_STATE_FILE))
            .expect("runtime must keep reload_state.json for doctor")
    }
}

fn end_turn() -> LlmResponse {
    LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: "DONE".into(),
        input_tokens: 5,
        output_tokens: 5,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
    }
}

/// An unattended turn — the kind a deadline applies to (attended turns have
/// none, spec §3.2), i.e. a scheduled or delegated one.
fn unattended(text: &str) -> TaskSpec {
    TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: text.into() }],
        },
        context_task_id: None,
        task_id: None,
        active_fleet: None,
        active_team: None,
        intent: RequestIntent::Interactive,
        output_artifact_path: None,
        cwd: None,
        attended: false,
        deadline_secs: None,
    }
}

/// `Some("deadline")` when the turn was cut by its deadline; `None` when it
/// ran to its natural end.
fn stopped_by(out: TaskOutcome) -> Option<String> {
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

/// One long-lived runner, as in a running agent — the same instance must see
/// the edit, not a freshly built one.
fn runner(live: Arc<LiveSettings>) -> Arc<TaskRunner> {
    let r = Arc::new(TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
        end_turn(),
    ]))));
    r.attach_live(live);
    r
}

#[tokio::test]
async fn a_profile_limits_edit_bounds_the_next_turn_of_the_same_runner() {
    let home = Home::new();
    let r = runner(home.live());
    assert_eq!(stopped_by(r.run_sync(unattended("one")).await), None);

    home.edit_profile("\nlimits: { deadline: 0s }\n");

    assert_eq!(
        stopped_by(r.run_sync(unattended("two")).await).as_deref(),
        Some("deadline"),
        "the edited deadline must bound the very next turn, no restart"
    );
    let seen = home.doctor_view();
    assert_eq!(seen.generation, 1, "{seen:?}");
    assert_eq!(seen.last_reload_error, None);
    assert!(seen.pending_cold_restart.is_empty(), "{seen:?}");
}

#[tokio::test]
async fn a_config_yaml_limits_edit_bounds_the_next_turn() {
    let home = Home::new();
    let r = runner(home.live());
    assert_eq!(stopped_by(r.run_sync(unattended("one")).await), None);

    save(
        &home.mur_home.join("config.yaml"),
        "limits: { deadline: 0s }\n",
    );

    assert_eq!(
        stopped_by(r.run_sync(unattended("two")).await).as_deref(),
        Some("deadline")
    );
    assert_eq!(home.doctor_view().config_limits_error, None);
}

/// The #1788 edge case: `deadline: soon` is rejected, the agent keeps the last
/// good bound, and doctor sees why — while generation does not move.
#[tokio::test]
async fn a_rejected_edit_keeps_the_previous_bound_and_tells_doctor_why() {
    let home = Home::new();
    let r = runner(home.live());
    home.edit_profile("\nlimits: { deadline: 0s }\n");
    assert_eq!(
        stopped_by(r.run_sync(unattended("good")).await).as_deref(),
        Some("deadline")
    );

    home.edit_profile("\nlimits: { deadline: soon }\n");

    assert_eq!(
        stopped_by(r.run_sync(unattended("bad")).await).as_deref(),
        Some("deadline"),
        "a rejected edit must leave the last good deadline in force"
    );
    let seen = home.doctor_view();
    assert_eq!(seen.generation, 1, "rejection is not a reload: {seen:?}");
    let why = seen
        .last_reload_error
        .expect("doctor must see the rejection");
    assert!(why.contains("soon"), "{why}");
}

/// A locked (cold) field edited alongside a hot one: the hot half applies,
/// the cold half is reported for restart and never reaches a reader.
#[tokio::test]
async fn a_cold_edit_is_reported_for_restart_not_applied() {
    let home = Home::new();
    let live = home.live();
    let edited = FIXTURE.replace(
        "processes: { spawn: { mode: allowlist, allowed: [] } }",
        "processes: { spawn: { mode: any, allowed: [] } }",
    );
    assert_ne!(edited, FIXTURE, "fixture changed; update the edit");
    save(
        &home.agent_home.join("profile.yaml"),
        &format!("{edited}\nlimits: {{ deadline: 0s }}\n"),
    );

    let now = live.profile.current();
    assert_eq!(now.limits.as_ref().unwrap().deadline.as_deref(), Some("0s"));
    let boot = mur_agent_runtime::profile::Profile::load(&home.agent_home)
        .unwrap()
        .inner;
    assert_ne!(
        now.entitlements.processes.spawn.mode, boot.entitlements.processes.spawn.mode,
        "a cold field must keep its boot value"
    );
    let seen = home.doctor_view();
    assert!(
        seen.pending_cold_restart
            .iter()
            .any(|f| f.starts_with("entitlements")),
        "{seen:?}"
    );
}

/// A reminder added to `profile.yaml` while the agent runs fires with no
/// restart and no turn — the scheduler is wired exactly as the supervisor
/// wires it (step 8c). Paused tokio time makes the minute wait virtual.
#[tokio::test(start_paused = true)]
async fn a_schedule_added_to_profile_yaml_fires_without_a_restart() {
    let home = Home::new();
    let live = home.live();
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let src = live.profile.clone();
    let tick = Duration::from_secs(mur_agent_runtime::scheduler::RECONCILE_SECS);
    let handle = CronScheduler::live(
        Arc::new(move || src.current().lifecycle.schedule.clone()),
        runner.clone(),
    )
    .with_reconcile_interval(tick)
    .spawn();
    tokio::time::sleep(tick * 3).await;
    assert_eq!(runner.last_activity_at(), 0, "nothing scheduled yet");

    let with_schedule = FIXTURE.replace(
        "mcp_required: true }",
        "mcp_required: true, schedule: [{ cron: \"* * * * *\", message: \"ping\" }] }",
    );
    assert_ne!(with_schedule, FIXTURE, "fixture changed; update the edit");
    save(&home.agent_home.join("profile.yaml"), &with_schedule);

    for _ in 0..10 {
        if runner.last_activity_at() != 0 {
            break;
        }
        tokio::time::sleep(tick).await;
    }
    handle.abort();
    assert_ne!(runner.last_activity_at(), 0, "the added entry never fired");
    assert_eq!(home.doctor_view().generation, 1);
}
