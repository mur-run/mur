//! `step_app_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;
use crate::cmd::agent::cli::step::StepState;

fn app() -> App {
    App::test_fixture()
}

#[test]
fn step_interleaves_between_text_segments() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.append_delta("reading file", false);
    a.push_step_started(
        "s1".into(),
        "read".into(),
        serde_json::json!({ "path": "a.rs" }),
    );
    // After push_step_started: prior segment frozen, step card pushed.
    // append_delta now creates a new streaming segment.
    a.append_delta("done, summary", false);

    // Expect 3 agent-role messages: frozen text, step card, new streaming text.
    let agent_msgs: Vec<_> = a
        .messages
        .iter()
        .filter(|m| m.role == Role::Agent)
        .collect();
    assert_eq!(
        agent_msgs.len(),
        3,
        "frozen segment + step card + new segment"
    );
    assert_eq!(agent_msgs[0].text, "reading file");
    assert!(!agent_msgs[0].streaming, "first segment must be frozen");
    assert!(
        agent_msgs[1].step.is_some(),
        "middle message must be a step card"
    );
    assert_eq!(agent_msgs[2].text, "done, summary");
    assert!(agent_msgs[2].streaming, "new segment must be streaming");
}

#[test]
fn step_before_text_drops_empty_placeholder() {
    let mut a = app();
    a.begin_user_turn("hi");
    // No delta yet — placeholder is empty.
    a.push_step_started("s1".into(), "bash".into(), serde_json::json!({}));
    // Empty placeholder dropped; only step card remains as agent message.
    let agent_msgs: Vec<_> = a
        .messages
        .iter()
        .filter(|m| m.role == Role::Agent)
        .collect();
    assert_eq!(
        agent_msgs.len(),
        1,
        "empty placeholder dropped, only step card"
    );
    assert!(agent_msgs[0].step.is_some(), "must be step card");
}

/// The screenshot bug: a `fleet_run` card kept spinning after the turn
/// carrying it had already died on the runtime's 600s idle timeout. No
/// `tool/result` ever arrives for such a call, so nothing else will ever
/// move the card off `Running`.
#[test]
fn a_failed_turn_stops_the_spinner_on_its_open_step_cards() {
    let mut a = app();
    a.begin_user_turn("run the fleet");
    a.push_step_started("s1".into(), "fleet_run".into(), serde_json::json!({}));
    let running = |a: &App| {
        a.messages
            .iter()
            .filter_map(|m| m.step.as_ref())
            .filter(|c| c.state == StepState::Running)
            .count()
    };
    assert_eq!(running(&a), 1, "card starts out running");

    a.fail_turn("agent 'mur' went idle for 600s without a response");

    assert_eq!(running(&a), 0, "no card may outlive the turn that owns it");
    let card = a
        .messages
        .iter()
        .find_map(|m| m.step.as_ref())
        .expect("step card");
    assert_eq!(card.state, StepState::Error);
    assert!(
        card.error.as_deref().is_some_and(|e| e.contains("idle")),
        "the card must say WHY it stopped, not just that it did: {:?}",
        card.error
    );
    assert!(
        card.duration_ms.is_none(),
        "a call nobody answered has no honest duration"
    );
}

/// An auto-armed rail is live state that never reaches scrollback, so the
/// run's outcome has to be committed to the transcript before the band
/// goes away — and the band has to go away, or it outlives its run.
#[test]
fn an_auto_armed_fleet_rail_lands_in_the_transcript_and_retires() {
    let mut a = app();
    a.arm_auto_fleet("s1", "dev", std::time::Instant::now());
    assert!(a.fleet.is_some(), "arming should raise the rail");

    a.finish_auto_fleet("s1", true, 4000);

    assert!(
        a.fleet.is_none(),
        "an auto-armed rail must not outlive its run"
    );
    let last = a.messages.last().expect("outcome message").text.clone();
    assert!(last.contains("⛴ fleet dev finished"), "got: {last}");
    // The rail's own view, not just the headline: this is the part that
    // used to exist only on screen.
    assert!(last.lines().count() > 1, "rail view not committed: {last}");
}

/// A run that hit a cap is not "finished". The headline takes the rail's
/// stop word so the transcript's first line already says why.
#[test]
fn the_headline_says_stopped_when_the_rail_reports_a_stop() {
    let mut a = app();
    a.arm_auto_fleet("s1", "dev", std::time::Instant::now());
    // Plant a stop event in the fleet's channel so the rail's final poll
    // folds it. The fixture home has no channel yet; create it.
    let svc = mur_channel::ChannelService::open(&a.home).unwrap();
    svc.create_for_fleet("dev", "mur", &["qa".to_string()])
        .unwrap();
    svc.append(
        "fleet-dev",
        mur_common::channel::ChannelActor::System,
        mur_common::channel::EventKind::StateChange,
        serde_json::json!({"from": "working", "to": "failed",
                           "stop_reason": "max-iterations",
                           "remedy": "raise it: mur fleet settings dev --max-iterations <N>"}),
        None,
    )
    .unwrap();

    a.finish_auto_fleet("s1", true, 4000);

    let last = a.messages.last().expect("outcome message").text.clone();
    assert!(
        last.starts_with("⛴ fleet dev stopped: max-iterations ("),
        "got: {last}"
    );
    assert!(
        !last.lines().next().unwrap().contains("finished"),
        "got: {last}"
    );
    assert!(
        last.contains("■ stopped: max-iterations — raise it"),
        "rail line missing: {last}"
    );
}

/// A `--fleet` rail is a band the user asked to keep; a delegated run
/// ending inside it must not take it down.
#[test]
fn a_user_armed_fleet_rail_survives_a_delegated_run() {
    let mut a = app();
    a.fleet = Some(super::super::super::fleet_rail::FleetRail::start("dev"));
    a.arm_auto_fleet("s1", "dev", std::time::Instant::now());

    a.finish_auto_fleet("s1", true, 4000);

    assert!(a.fleet.is_some(), "a user-armed rail must survive the run");
}

#[test]
fn update_step_completed_marks_card_done() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.push_step_started(
        "s1".into(),
        "bash".into(),
        serde_json::json!({ "cmd": "ls" }),
    );
    a.update_step_completed(
        "s1",
        true,
        "foo.rs\n".into(),
        false,
        7,
        None,
        42,
        false,
        false,
    );
    let card = a
        .messages
        .iter()
        .find_map(|m| m.step.as_ref())
        .expect("step card");
    assert_eq!(card.state, StepState::Done);
    assert_eq!(card.duration_ms, Some(42));
    assert_eq!(card.output, "foo.rs\n");
}

/// Test 10 (card half) — the card opens on the keypress, accumulates,
/// and stamps a non-zero exit; a clean exit stamps nothing.
#[test]
fn shell_card_opens_accumulates_and_stamps_exit() {
    use crate::cmd::agent::cli::shell::ShellEnd;
    let mut a = app();
    a.begin_shell("cargo test");
    let card = a.messages.last().expect("card");
    assert_eq!(card.text, "$ cargo test");
    assert!(card.streaming, "the card is live");

    a.append_shell_output("running 3 tests");
    a.append_shell_output("test result: FAILED");
    let body = a.finish_shell(&ShellEnd::Exited(1));
    let card = a.messages.last().expect("card");
    assert!(!card.streaming, "the card is finalised");
    assert_eq!(
        card.text,
        "$ cargo test\nrunning 3 tests\ntest result: FAILED\n[exit 1]"
    );
    assert_eq!(body, "running 3 tests\ntest result: FAILED\n[exit 1]");

    let mut b = app();
    b.begin_shell("true");
    b.append_shell_output("ok");
    b.finish_shell(&ShellEnd::Exited(0));
    assert_eq!(b.messages.last().unwrap().text, "$ true\nok");
}

/// Test 15 — D2: a silent command still has a live card, immediately.
#[test]
fn a_silent_command_still_shows_a_live_card() {
    let mut a = app();
    a.begin_shell("sleep 45");
    let card = a.messages.last().expect("card");
    assert_eq!(card.text, "$ sleep 45");
    assert!(card.streaming, "live before any output exists");
}

/// D4 render half: a cancelled card says so.
#[test]
fn cancelled_shell_card_is_marked() {
    use crate::cmd::agent::cli::shell::ShellEnd;
    let mut a = app();
    a.begin_shell("sleep 60");
    a.append_shell_output("partial");
    let body = a.finish_shell(&ShellEnd::Cancelled);
    assert_eq!(
        a.messages.last().unwrap().text,
        "$ sleep 60\npartial\n[cancelled]"
    );
    assert!(body.contains("[cancelled]"));
}

/// Test 7 — D6: the card cap keeps the tail and never eats `$ cmd`.
#[test]
fn shell_card_cap_keeps_the_tail_and_the_command_line() {
    use crate::cmd::agent::cli::shell::SHELL_CARD_MAX_BYTES;
    let mut a = app();
    a.begin_shell("noisy");
    a.append_shell_output(&"x".repeat(SHELL_CARD_MAX_BYTES + 1024));
    a.append_shell_output("LAST");
    let text = &a.messages.last().unwrap().text;
    assert!(text.starts_with("$ noisy\n"), "command line survived");
    assert!(text.ends_with("LAST"), "tail survived");
    assert!(text.contains("[output truncated]"));
    assert!(text.len() < SHELL_CARD_MAX_BYTES + 512);
}

/// An empty-output command still leaves exactly one card, and a teardown
/// that cleared the transcript leaves nothing for a late chunk to hit.
#[test]
fn empty_output_leaves_one_card_and_a_cleared_one_absorbs_late_chunks() {
    use crate::cmd::agent::cli::shell::ShellEnd;
    let mut a = app();
    a.begin_shell("true");
    a.finish_shell(&ShellEnd::Exited(0));
    assert_eq!(
        a.messages.iter().filter(|m| m.role == Role::Shell).count(),
        1
    );
    assert_eq!(a.messages.last().unwrap().text, "$ true");

    a.messages.clear(); // what /clear does
    a.append_shell_output("late");
    assert!(
        a.messages.is_empty(),
        "a late chunk found no card and was dropped"
    );
}

/// Test 18 — a yield is ⏳, and the end of the turn does not abandon it
/// the way it abandons a card the runtime never answered.
#[test]
fn a_running_step_renders_yielded_not_done() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.push_step_started(
        "s1".into(),
        "bash".into(),
        serde_json::json!({ "command": "cargo test" }),
    );
    a.update_step_completed(
        "s1",
        true,
        "[still running after 30s — job_id: j-1]".into(),
        false,
        40,
        None,
        30_000,
        false,
        true,
    );
    let state = |a: &App| {
        a.messages
            .iter()
            .rev()
            .find_map(|m| m.step.as_ref())
            .map(|c| (c.state, c.glyph()))
            .unwrap()
    };
    assert_eq!(state(&a), (StepState::Yielded, "⏳"));
    a.resolve_open_steps("turn ended");
    assert_eq!(state(&a).0, StepState::Yielded, "abandon must skip a yield");
}

#[test]
fn tool_turn_reply_is_pushed_not_dropped() {
    let mut a = app();
    a.begin_user_turn("read the file");
    a.push_step_started(
        "s1".into(),
        "read".into(),
        serde_json::json!({"path":"a.rs"}),
    );
    a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5, false, false);
    // No streaming segment now (tool turn, no text deltas).
    a.finish_agent_turn("here is the summary".into(), Some("t1".into()));
    let last = a.messages.last().unwrap();
    assert!(last.step.is_none());
    assert_eq!(last.role, Role::Agent);
    assert_eq!(last.text, "here is the summary");
    assert!(!last.streaming);
    assert!(last.rendered.is_some());
}

#[test]
fn multi_segment_finish_sets_trailing_keeps_frozen() {
    let mut a = app();
    a.begin_user_turn("hi");
    a.append_delta("looking at it", false);
    a.push_step_started("s1".into(), "read".into(), serde_json::json!({}));
    a.append_delta("here is the answer", false);
    // reply = final iteration text only
    a.finish_agent_turn("here is the answer".into(), Some("t1".into()));
    let segs: Vec<_> = a
        .messages
        .iter()
        .filter(|m| m.role == Role::Agent && m.step.is_none())
        .collect();
    assert_eq!(segs[0].text, "looking at it"); // frozen, untouched
    assert_eq!(segs[1].text, "here is the answer"); // trailing got reply
    assert!(!segs[1].streaming);
}
