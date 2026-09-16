use super::*;
use crate::run_status::{RunKind, RunState, State, store as run_store};
use chrono::{TimeZone, Utc};
use mur_common::hitl::RiskTier;
use mur_monitor::action::action_key;
use mur_monitor::store::{ListFilter, MonitorStore};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
}

fn home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    run_store::save(
        d.path(),
        &RunState {
            schema: 1,
            run_id: "run-1".into(),
            channel_id: None,
            kind: RunKind::Fleet,
            label: "x".into(),
            pid: std::process::id(),
            started_at: t0(),
            last_heartbeat_at: Some(t0()),
            state: State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: String::new(),
            build_sha: String::new(),
        },
    )
    .unwrap();
    d
}

fn spec_file(d: &Path, source: &str, reference: &str) -> PathBuf {
    let p = d.join("spec.yaml");
    std::fs::write(
            &p,
            format!(
                "schema_version: 1\nname: t\nsource: {{ type: {source}, reference: {reference} }}\nidempotency_key: k\ncreated_by: {{ actor: user:test }}\n"
            ),
        )
        .unwrap();
    p
}

fn go(d: &Path, a: MonitorAction) -> Result<String> {
    let mut out = Vec::new();
    run_to(d, a, &mut out, t0())?;
    Ok(String::from_utf8(out).unwrap())
}

/// One registered monitor, nothing else — the common starting point for
/// `show` tests that don't care about actions/notifications.
fn home_with_monitor() -> (tempfile::TempDir, String) {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();
    (d, id)
}

/// One registered monitor with one `write`-tier action of the given verb
/// claimed and then parked `Blocked` on the given approval id — mirrors
/// `mur-monitor::store::action`'s own `an_action_can_be_blocked_on_approval`
/// fixture so `show`'s rendering is exercised against the same shape the
/// store itself is tested with.
fn home_with_blocked_action(verb: &str, hitl_id: &str) -> (tempfile::TempDir, String) {
    let (d, id) = home_with_monitor();
    let s = MonitorStore::open(d.path()).unwrap();
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;
    let k = action_key(&id, &cyc, 1, verb, 0);
    s.claim_action(&k, &id, &cyc, RiskTier::Write, t0())
        .unwrap();
    s.block_action(&k, hitl_id).unwrap();
    (d, id)
}

/// Two actions on one monitor, neither `blocked` — nothing else exercises
/// this: `home_with_blocked_action` only ever produces a `blocked` row, so
/// `risk_str`'s output, the attempt-count text, and the non-blocked branch
/// of `show`'s action line were all unprotected. `notify` goes through
/// `block_action` twice before finishing `done` (attempt 2, exercising the
/// plural "attempts" text and proving `unblock` gates on STATE, not on
/// `approval_id`'s presence — `finish_action` never clears it); `rerun`
/// goes through it once before finishing `failed` (attempt 1, singular).
fn home_with_settled_actions() -> (tempfile::TempDir, String) {
    let (d, id) = home_with_monitor();
    let s = MonitorStore::open(d.path()).unwrap();
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;

    let done_key = action_key(&id, &cyc, 1, "notify", 0);
    s.claim_action(&done_key, &id, &cyc, RiskTier::Read, t0())
        .unwrap();
    s.block_action(&done_key, "hitl-irrelevant-1").unwrap();
    s.block_action(&done_key, "hitl-irrelevant-2").unwrap();
    s.finish_action(&done_key, ActionState::Done, "sent")
        .unwrap();

    let failed_key = action_key(&id, &cyc, 1, "rerun", 1);
    s.claim_action(&failed_key, &id, &cyc, RiskTier::Write, t0())
        .unwrap();
    s.block_action(&failed_key, "hitl-irrelevant-3").unwrap();
    s.finish_action(&failed_key, ActionState::Failed, "no executor for `rerun`")
        .unwrap();
    (d, id)
}

#[test]
fn add_creates_once_and_reports_the_existing_one() {
    let d = home();
    let f = spec_file(d.path(), "mur_run", "run-1");
    let first = go(
        d.path(),
        MonitorAction::Add {
            file: f.clone(),
            started_at: None,
        },
    )
    .unwrap();
    assert!(first.starts_with("monitor "), "{first}");
    assert!(first.contains("first check"), "{first}");
    let second = go(
        d.path(),
        MonitorAction::Add {
            file: f,
            started_at: None,
        },
    )
    .unwrap();
    assert!(second.contains("already exists"), "{second}");
    assert_eq!(
        MonitorStore::open(d.path())
            .unwrap()
            .list(&ListFilter::default())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn add_refuses_what_can_never_be_queried() {
    let d = home();
    let e = go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "custom", "x"),
            started_at: None,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("no adapter"), "{e:#}");
    let e = go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "'bad id!'"),
            started_at: None,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("reference"), "{e:#}");
}

/// The rule this protects: a probe that reports the credential itself is
/// the blocker must refuse `add` immediately, never silently create a
/// monitor that can only ever answer `unknown`. `env:` with a variable
/// guaranteed unset resolves to `None` deterministically (Task 10), so
/// this drives the real `credential_failure` path with no network.
#[test]
fn add_refuses_when_the_probe_reports_a_credential_failure() {
    const MISSING_VAR: &str = "MUR_TEST_MONITOR_ADD_CRED_DEFINITELY_NOT_SET_Q7Z";
    assert!(std::env::var_os(MISSING_VAR).is_none());
    let d = home();
    let p = d.path().join("gha.yaml");
    std::fs::write(
            &p,
            format!(
                "schema_version: 1\nname: t\nsource: {{ type: github_actions, reference: o/r/1, credential_ref: env:{MISSING_VAR} }}\nidempotency_key: k\ncreated_by: {{ actor: user:test }}\n"
            ),
        )
        .unwrap();
    let e = go(
        d.path(),
        MonitorAction::Add {
            file: p,
            started_at: None,
        },
    )
    .unwrap_err();
    assert!(
        e.to_string().contains("fix the credential reference"),
        "{e:#}"
    );
    assert_eq!(
        MonitorStore::open(d.path())
            .unwrap()
            .list(&ListFilter::default())
            .unwrap()
            .len(),
        0,
        "a refused probe must not create a monitor"
    );
}

/// The converse rule: an `unknown` probe for a reason that has nothing to
/// do with credentials (here, a `mur_run` reference with no run record
/// yet) must NOT refuse — a monitor for work that hasn't appeared yet is
/// legitimate, and only `credential_failure` should ever block `add`.
#[test]
fn add_succeeds_when_the_probe_is_unknown_for_a_non_credential_reason() {
    let d = home();
    let out = go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-does-not-exist-yet"),
            started_at: None,
        },
    )
    .unwrap();
    assert!(out.contains("probe: unknown"), "{out}");
    let list = go(
        d.path(),
        MonitorAction::List {
            state: None,
            all: false,
        },
    )
    .unwrap();
    assert!(list.contains("active"), "{list}");
}

/// The bug: `mur monitor add` (CLI/murmur) resolves its home via
/// `crate::paths::mur_root`, which honors `MUR_HOME`, but the daemon —
/// what actually polls the monitor going forward — always resolves its
/// home via `crate::store::yaml::default_mur_dir()`, which ignores
/// `MUR_HOME` entirely. A monitor created while `MUR_HOME` points anywhere
/// other than the daemon's default is silently written where the daemon
/// never looks, and just sits `sleeping` forever with no error anywhere.
/// `add` must say so up front.
#[test]
fn add_warns_when_mur_home_diverges_from_the_daemon_default() {
    let _g = crate::conversations::ENV_LOCK.lock().unwrap();
    let d = home();
    let f = spec_file(d.path(), "mur_run", "run-1");
    let prev = std::env::var("MUR_HOME").ok();
    unsafe { std::env::set_var("MUR_HOME", d.path()) };
    let out = go(
        d.path(),
        MonitorAction::Add {
            file: f,
            started_at: None,
        },
    );
    match prev {
        Some(p) => unsafe { std::env::set_var("MUR_HOME", p) },
        None => unsafe { std::env::remove_var("MUR_HOME") },
    }
    let out = out.unwrap();
    assert!(out.contains("warning: MUR_HOME"), "{out}");
    assert!(
        out.contains(&d.path().display().to_string()),
        "must name the CLI-side path: {out}"
    );
}

#[test]
fn list_show_cancel_retry() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();

    let list = go(
        d.path(),
        MonitorAction::List {
            state: None,
            all: false,
        },
    )
    .unwrap();
    assert!(list.contains(&id[..8]) && list.contains("active"), "{list}");
    let show = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: true,
        },
    )
    .unwrap();
    assert!(
        show.contains("mur_run run-1") && show.contains("created"),
        "{show}"
    );

    let e = go(
        d.path(),
        MonitorAction::Retry {
            id: id.clone(),
            reset_remediation_budget: false,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("exhausted"), "{e:#}");

    let c = go(d.path(), MonitorAction::Cancel { id: id.clone() }).unwrap();
    assert!(c.contains("NOT cancelled"), "{c}");
    assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Completed);
    assert!(
        go(
            d.path(),
            MonitorAction::List {
                state: None,
                all: false
            }
        )
        .unwrap()
        .contains("no monitors")
    );

    // NOTE: the brief's placeholder `conn_for_test_set_state` does not
    // exist. This is the real public API: a direct state write so the
    // test can reach `exhausted` without waiting on the scheduler.
    s.set_state(&id, MonitorState::Exhausted, t0()).unwrap();
    let r = go(
        d.path(),
        MonitorAction::Retry {
            id: id.clone(),
            reset_remediation_budget: true,
        },
    )
    .unwrap();
    assert!(r.contains("reactivated"), "{r}");
    assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Active);
}

// The "ordinary" exhaustion path: plan-2's remediation-attempt ceiling will
// also land a monitor in `Exhausted` without ever setting `hard_reached`.
// `retry` must keep reactivating that case cleanly — only the hard-deadline
// case (below) is special-cased.
#[test]
fn retry_reactivates_cleanly_when_exhausted_without_a_hard_deadline() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();

    s.set_state(&id, MonitorState::Exhausted, t0()).unwrap();
    assert!(!s.get(&id).unwrap().unwrap().hard_reached);

    let r = go(
        d.path(),
        MonitorAction::Retry {
            id: id.clone(),
            reset_remediation_budget: true,
        },
    )
    .unwrap();
    assert!(r.contains("reactivated"), "{r}");
    assert_eq!(s.get(&id).unwrap().unwrap().state, MonitorState::Active);
}

// The hard-deadline path: in this slice, `hard_reached` is the ONLY way a
// monitor reaches `Exhausted`, and `reactivate` deliberately never clears
// it. So retrying it would be a guaranteed no-op — the very next tick would
// re-exhaust the monitor while this command had already reported success.
// `retry` must refuse instead, naming the deadline, without mutating any
// state (state stays `exhausted`, `hard_reached` stays set).
#[test]
fn retry_refuses_when_the_hard_deadline_has_already_passed() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();

    s.set_state(&id, MonitorState::Exhausted, t0()).unwrap();
    {
        // No public API sets `hard_reached` directly (by design — it is an
        // internal fact the scheduler alone should set); reach it via a raw
        // connection to the same on-disk db, the same way `set_state` above
        // is itself a test-only backdoor around the scheduler.
        let db = mur_monitor::store::db_dir(d.path()).join(mur_monitor::store::DB_FILE);
        let conn = rusqlite::Connection::open(db).unwrap();
        let n = conn
            .execute(
                "UPDATE monitors SET hard_reached = 1 WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
        assert_eq!(n, 1);
    }
    assert!(s.get(&id).unwrap().unwrap().hard_reached);

    let e = go(
        d.path(),
        MonitorAction::Retry {
            id: id.clone(),
            reset_remediation_budget: true,
        },
    )
    .unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("hard deadline"), "{msg}");
    assert!(msg.contains("already passed"), "{msg}");
    assert!(msg.contains("exhausted"), "{msg}");
    assert!(msg.contains("new monitor"), "{msg}");
    assert!(msg.contains("hard_deadline"), "{msg}");

    // Refused, not silently reactivated-then-re-exhausted: state and
    // `hard_reached` are both untouched.
    let row = s.get(&id).unwrap().unwrap();
    assert_eq!(row.state, MonitorState::Exhausted);
    assert!(row.hard_reached);
}

#[test]
fn bad_state_filter_lists_the_valid_ones() {
    let d = home();
    let e = go(
        d.path(),
        MonitorAction::List {
            state: Some("bogus".into()),
            all: false,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("awaiting_approval"), "{e:#}");
}

// `list` truncates ids to `ID_SHORT` for the table, so that truncated
// string is the only identifier a user ever sees on screen — `show` /
// `cancel` / `retry` must accept it, not just the full 36-character id.
#[test]
fn show_accepts_the_prefix_list_prints_and_the_full_id_and_rejects_unknown() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();

    let by_prefix = go(
        d.path(),
        MonitorAction::Show {
            id: id[..8].to_string(),
            history: false,
        },
    )
    .unwrap();
    assert!(by_prefix.contains("mur_run run-1"), "{by_prefix}");

    let by_full_id = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: false,
        },
    )
    .unwrap();
    assert_eq!(
        by_prefix, by_full_id,
        "a prefix and the full id must resolve to the same monitor"
    );

    let e = go(
        d.path(),
        MonitorAction::Show {
            id: "no-such-prefix".into(),
            history: false,
        },
    )
    .unwrap_err();
    assert!(e.to_string().contains("no monitor"), "{e:#}");
}

// `footer::has_condition` (the murmur `monitor(n)` badge) counts
// `stalled_since`/`unknown_streak`; `show` used to print neither
// `stalled_since`, `soft_notified`, nor `hard_reached`, so a user staring at
// a monitor `show` called "fine" had no way to see why the footer badge lit
// up elsewhere.
#[test]
fn show_prints_the_stall_and_deadline_condition_fields() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();
    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(out.contains("condition:"), "{out}");
    assert!(out.contains("stalled since"), "{out}");
    assert!(out.contains("soft notified"), "{out}");
    assert!(out.contains("hard reached"), "{out}");
}

// Two monitors minted moments apart by the real store: UUIDv7 ids are
// time-ordered, so within one fast test run they reliably share a
// multi-character timestamp prefix (see `ID_SHORT`'s doc comment) —
// deriving the shared prefix from the real ids instead of hardcoding a
// length keeps the test honest about what actually collided.
#[test]
fn show_reports_every_candidate_on_an_ambiguous_prefix() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let f2 = d.path().join("spec2.yaml");
    std::fs::write(
            &f2,
            "schema_version: 1\nname: t2\nsource: { type: mur_run, reference: run-1 }\nidempotency_key: k2\ncreated_by: { actor: user:test }\n",
        )
        .unwrap();
    go(
        d.path(),
        MonitorAction::Add {
            file: f2,
            started_at: None,
        },
    )
    .unwrap();

    let s = MonitorStore::open(d.path()).unwrap();
    let ids: Vec<String> = s
        .list(&ListFilter::default())
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids.len(), 2);
    let common: String = ids[0]
        .chars()
        .zip(ids[1].chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect();
    assert!(
        !common.is_empty(),
        "test assumes two UUIDv7 ids minted in the same test share a \
             timestamp prefix; got {ids:?} — if this ever flakes, the store \
             is generating ids without the expected time-ordering"
    );

    let e = go(
        d.path(),
        MonitorAction::Show {
            id: common,
            history: false,
        },
    )
    .unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("ambiguous"), "{msg}");
    assert!(msg.contains("(t)"), "{msg}");
    assert!(msg.contains("(t2)"), "{msg}");
    assert!(msg.contains("more characters"), "{msg}");
}

#[test]
fn show_renders_notification_delivery_state() {
    // spec §錯誤處理: a delivery failure must be visible in the CLI.
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;
    // Prime "log" before the stall happens. A channel's high-water mark is
    // stamped on its first sight of the store, so a channel that has never
    // run inherits no history — in production both channels are registered
    // at daemon startup, long before any monitor stalls. Without this the
    // stall lands at or below the mark and `pending_notifications` correctly
    // returns nothing, leaving `ev[0]` to panic.
    s.pending_notifications("log", t0(), 1).unwrap();
    s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0())
        .unwrap();
    let ev = s.pending_notifications("log", t0(), 1).unwrap();
    s.mark_delivery_failed(ev[0].event_id, "log", t0()).unwrap();

    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(out.contains("  notifications:"), "{out}");
    // Pinned to a single line naming the event, the "log" channel, and its
    // "pending" state together — a bare `contains("log")` would also pass
    // if "log" ever showed up elsewhere in `show`'s output for unrelated
    // reasons, so this requires all three tokens on the same line.
    assert!(
        out.lines()
            .any(|l| l.contains("event") && l.contains("log") && l.contains("pending")),
        "{out}"
    );
}

// A monitor with no notification rows must still render `show` cleanly —
// the `notifications:` header is conditional on `deliveries` being
// non-empty, so this pins the empty-vec path staying silent rather than
// printing an empty header.
#[test]
fn show_omits_notifications_section_when_there_are_none() {
    let d = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();
    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(!out.contains("notifications:"), "{out}");
    assert!(out.contains("recent observations:"), "{out}");
}

#[test]
fn show_renders_actions_with_the_command_that_unblocks_them() {
    // spec: a blocked action whose command the user cannot find is the
    // silent-stop failure this whole slice exists to remove.
    let (d, id) = home_with_blocked_action("rerun", "hitl-abc");
    let out = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: false,
        },
    )
    .unwrap();
    assert!(out.contains("  actions:"), "{out}");
    assert!(
        out.lines().any(|l| l.contains("rerun")
            && l.contains("blocked")
            && l.contains("mur channel approve")
            && l.contains("hitl-abc")),
        "a blocked action must print the command that releases it: {out}"
    );
}

// Fix round 2: the shipped suite had a blocked-action test and an
// omitted-section test, but nothing for `done`/`failed`/`claimed` —
// `risk_str`'s output, the attempt-count text, and the non-blocked branch
// of the action line were all unprotected. The shipped `notifications:`
// test made exactly this mistake once already: three separate `contains`
// checks that could each be satisfied by unrelated lines, so this pins
// verb+tier+state+attempt-count together on ONE line per action instead.
#[test]
fn show_renders_a_non_blocked_actions_verb_tier_state_and_attempts_on_one_line() {
    let (d, id) = home_with_settled_actions();
    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(out.contains("  actions:"), "{out}");
    assert!(
        out.lines().any(|l| l.contains("notify")
            && l.contains("read")
            && l.contains("done")
            && l.contains("(2 attempts)")),
        "a done action's verb, tier, state and attempt count must share one line: {out}"
    );
    assert!(
        out.lines().any(|l| l.contains("rerun")
            && l.contains("write")
            && l.contains("failed")
            && l.contains("(1 attempt)")),
        "a failed action's verb, tier, state and attempt count must share one line: {out}"
    );
    // Neither action is blocked, so there is nothing to approve — and this
    // also proves `show`'s unblock text is gated on STATE, not merely on
    // `approval_id` being set: both fixture rows went through
    // `block_action` (which stamps `approval_id`) before finishing, so a
    // gate that checked presence instead of state would leak this text.
    assert!(
        !out.contains("mur channel approve"),
        "a non-blocked action must not print an approve command: {out}"
    );
}

// Would this pass if `show` were broken and printed nothing at all? No: it
// also asserts `recent observations:` is present, which only appears once
// `show` has run its full, unconditional body — a blank/aborted output
// fails that half regardless of the `actions:` absence check.
#[test]
fn show_omits_the_actions_section_when_there_are_none() {
    let (d, id) = home_with_monitor();
    let out = go(d.path(), MonitorAction::Show { id, history: false }).unwrap();
    assert!(!out.contains("actions:"), "{out}");
    assert!(out.contains("recent observations:"), "{out}");
}
