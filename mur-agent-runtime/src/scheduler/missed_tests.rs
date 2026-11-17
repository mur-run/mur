use super::super::{ScheduleSink, run_entry, schedule_channel};
use super::*;
use crate::task_runner::TaskRunner;
use mur_common::identity::AgentIdentity;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The production case from #2436: `0 10 3 11 *`, once, due 3 Nov, first
/// seen by a runtime that started after it.
const CRON: &str = "0 10 3 11 *";
const PAST_BOUND: &str = "2020-11-03T10:00:00+08:00";

fn sink(home: &Path) -> ScheduleSink {
    ScheduleSink {
        mur_home: home.to_path_buf(),
        agent: "probe".into(),
        identity: Arc::new(AgentIdentity::generate()),
        key_version: 1,
        locale: "en".into(),
    }
}

fn once(not_after: &str) -> ScheduleEntry {
    ScheduleEntry {
        cron: CRON.into(),
        message: "run pkg audit".into(),
        sends_to: None,
        not_after: Some(not_after.into()),
    }
}

fn channel_events(home: &Path) -> Vec<ChannelEvent> {
    let svc = ChannelService::open(home).unwrap();
    let id = schedule_channel(&svc, home, "probe").unwrap();
    svc.load_events(&id).unwrap()
}

fn notices(home: &Path) -> Vec<ChannelEvent> {
    channel_events(home)
        .into_iter()
        .filter(|e| {
            e.idempotency_key
                .as_deref()
                .is_some_and(|k| k.starts_with("sched-missed:"))
        })
        .collect()
}

async fn start(home: &Path, entry: ScheduleEntry) {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let sink = Some(Arc::new(sink(home)));
    run_entry(entry, runner.clone(), CancellationToken::new(), sink).await;
    assert_eq!(
        runner.last_activity_at(),
        0,
        "a missed one-off is reported, never run late"
    );
}

#[tokio::test]
async fn a_one_off_missed_while_down_is_reported_on_start() {
    let tmp = tempfile::tempdir().unwrap();
    start(tmp.path(), once(PAST_BOUND)).await;

    let got = notices(tmp.path());
    assert_eq!(got.len(), 1, "exactly one notice, got {got:?}");
    let text = got[0].payload["text"].as_str().unwrap();
    assert!(
        text.contains("run pkg audit"),
        "the notice carries the reminder: {text}"
    );
    assert!(text.contains("2020-11-03"), "and when it was due: {text}");

    let inbox = tmp.path().join("agents/probe/companion/inbox");
    assert_eq!(
        std::fs::read_dir(&inbox).map(|d| d.count()).unwrap_or(0),
        1,
        "the person is told where they already look"
    );
}

#[tokio::test]
async fn a_restart_does_not_repeat_the_notice() {
    let tmp = tempfile::tempdir().unwrap();
    start(tmp.path(), once(PAST_BOUND)).await;
    start(tmp.path(), once(PAST_BOUND)).await;
    assert_eq!(notices(tmp.path()).len(), 1, "one miss, one notice");
    let inbox = tmp.path().join("agents/probe/companion/inbox");
    assert_eq!(std::fs::read_dir(&inbox).unwrap().count(), 1);
}

#[tokio::test]
async fn a_one_off_that_did_fire_is_not_reported_as_missed() {
    let tmp = tempfile::tempdir().unwrap();
    // The reply a real firing leaves, recorded after the bound (now > 2020).
    let s = sink(tmp.path());
    crate::protocol::methods::channel_delegate::append_self_reply(
        tmp.path(),
        &schedule_channel(
            &ChannelService::open(tmp.path()).unwrap(),
            tmp.path(),
            "probe",
        )
        .unwrap(),
        "probe",
        &s.identity,
        1,
        "done",
        "t1",
        Some(reply_key("t1", &once(PAST_BOUND))),
    )
    .unwrap();

    start(tmp.path(), once(PAST_BOUND)).await;
    assert!(
        notices(tmp.path()).is_empty(),
        "it fired; nothing was missed"
    );
}

#[test]
fn a_reply_from_an_earlier_year_does_not_account_for_this_one() {
    // Cron has no year: last year's firing of the same expression says
    // nothing about this year's.
    let bound = DateTime::parse_from_rfc3339(PAST_BOUND)
        .unwrap()
        .with_timezone(&Local);
    let ev: ChannelEvent = serde_json::from_value(serde_json::json!({
        "seq": 0,
        "ts": "2019-11-03T02:01:00Z",
        "actor": {"kind": "agent", "id": "probe"},
        "kind": "message",
        "payload": {},
        "idempotency_key": format!("sched:t0:{CRON}"),
    }))
    .unwrap();
    assert!(!already_accounted(&[ev], &once(PAST_BOUND), bound));
}

#[test]
fn a_reply_for_a_different_entry_does_not_count() {
    let bound = DateTime::parse_from_rfc3339(PAST_BOUND)
        .unwrap()
        .with_timezone(&Local);
    let ev: ChannelEvent = serde_json::from_value(serde_json::json!({
        "seq": 0,
        "ts": "2030-01-01T00:00:00Z",
        "actor": {"kind": "agent", "id": "probe"},
        "kind": "message",
        "payload": {},
        "idempotency_key": "sched:t0:0 9 * * 1",
    }))
    .unwrap();
    assert!(!already_accounted(&[ev], &once(PAST_BOUND), bound));
}

fn once_saying(message: &str) -> ScheduleEntry {
    ScheduleEntry {
        message: message.into(),
        ..once(PAST_BOUND)
    }
}

/// Two one-offs due at the same minute are two reminders: missing both
/// must raise two notices, each with its own text.
#[tokio::test]
async fn two_reminders_due_together_are_each_reported() {
    let tmp = tempfile::tempdir().unwrap();
    start(tmp.path(), once_saying("pay invoice")).await;
    start(tmp.path(), once_saying("call dentist")).await;

    let got = notices(tmp.path());
    assert_eq!(got.len(), 2, "one notice per reminder, got {got:?}");
    let texts: Vec<&str> = got
        .iter()
        .map(|e| e.payload["text"].as_str().unwrap())
        .collect();
    assert!(texts.iter().any(|t| t.contains("pay invoice")), "{texts:?}");
    assert!(
        texts.iter().any(|t| t.contains("call dentist")),
        "{texts:?}"
    );
}

/// A firing of one reminder says nothing about another sharing its minute.
#[tokio::test]
async fn a_reply_for_one_reminder_does_not_cover_its_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let s = sink(tmp.path());
    let fired = once_saying("pay invoice");
    crate::protocol::methods::channel_delegate::append_self_reply(
        tmp.path(),
        &schedule_channel(
            &ChannelService::open(tmp.path()).unwrap(),
            tmp.path(),
            "probe",
        )
        .unwrap(),
        "probe",
        &s.identity,
        1,
        "done",
        "t1",
        Some(reply_key("t1", &fired)),
    )
    .unwrap();

    start(tmp.path(), fired).await;
    start(tmp.path(), once_saying("call dentist")).await;

    let got = notices(tmp.path());
    assert_eq!(got.len(), 1, "only the unfired one, got {got:?}");
    let text = got[0].payload["text"].as_str().unwrap();
    assert!(text.contains("call dentist"), "{text}");
}

/// Replies recorded before entries carried an identity end in the bare
/// cron. They still count, so upgrading never raises a notice for a
/// reminder that did fire.
#[test]
fn a_reply_in_the_old_key_format_still_counts() {
    let bound = DateTime::parse_from_rfc3339(PAST_BOUND)
        .unwrap()
        .with_timezone(&Local);
    let ev: ChannelEvent = serde_json::from_value(serde_json::json!({
        "seq": 0,
        "ts": "2030-01-01T00:00:00Z",
        "actor": {"kind": "agent", "id": "probe"},
        "kind": "message",
        "payload": {},
        "idempotency_key": format!("sched:t0:{CRON}"),
    }))
    .unwrap();
    assert!(already_accounted(&[ev], &once(PAST_BOUND), bound));
}

/// The host-side reader `schedule list` uses (#2437): no channel means no
/// record, and reading must not create one.
#[test]
fn the_reader_reports_no_record_without_creating_a_channel() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("agents/probe")).unwrap();
    let bound = DateTime::parse_from_rfc3339(PAST_BOUND)
        .unwrap()
        .with_timezone(&Local);
    let got = read_firing_outcome(tmp.path(), "probe", &once(PAST_BOUND), bound).unwrap();
    assert_eq!(got, FiringOutcome::NoRecord);
    assert!(
        !tmp.path()
            .join("agents/probe")
            .join(SCHEDULE_CHANNEL_FILE)
            .exists(),
        "a read must not mint the schedule channel"
    );
}

#[tokio::test]
async fn the_reader_tells_a_missed_entry_from_its_fired_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let fired = once_saying("pay invoice");
    let missed = once_saying("call dentist");
    let s = sink(tmp.path());
    crate::protocol::methods::channel_delegate::append_self_reply(
        tmp.path(),
        &schedule_channel(
            &ChannelService::open(tmp.path()).unwrap(),
            tmp.path(),
            "probe",
        )
        .unwrap(),
        "probe",
        &s.identity,
        1,
        "paid",
        "t1",
        Some(reply_key("t1", &fired)),
    )
    .unwrap();
    start(tmp.path(), fired.clone()).await;
    start(tmp.path(), missed.clone()).await;

    let bound = DateTime::parse_from_rfc3339(PAST_BOUND)
        .unwrap()
        .with_timezone(&Local);
    let read = |e: &ScheduleEntry| read_firing_outcome(tmp.path(), "probe", e, bound).unwrap();
    assert_eq!(read(&fired), FiringOutcome::Fired);
    assert_eq!(read(&missed), FiringOutcome::Missed);
}
