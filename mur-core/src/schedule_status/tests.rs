/// `(has a next fire, note explaining why not)` for any item.
fn next_state(it: &ScheduleItem) -> (bool, Option<&str>) {
    match it {
        ScheduleItem::AgentCron {
            next_fires,
            next_note,
            ..
        }
        | ScheduleItem::Workflow {
            next_fires,
            next_note,
            ..
        }
        | ScheduleItem::Fleet {
            next_fires,
            next_note,
            ..
        } => (!next_fires.is_empty(), next_note.as_deref()),
        ScheduleItem::AgentIdle { next_note, .. } => (false, next_note.as_deref()),
    }
}

/// The invariant the whole change exists for: a blank "Next" is never an
/// acceptable resting state. Either there is a fire time, or there is a
/// sentence saying why there is not — never neither.
#[test]
fn every_item_either_has_a_next_fire_or_says_why_not() {
    let home = tempfile::tempdir().unwrap();
    let fleets = home.path().join("fleets");
    for (name, trigger) in [
        ("timed", "cron:*/15 * * * *"),
        ("paced", "interval:30m"),
        ("handstart", "manual"),
        ("garbled", "whenever-i-feel-like-it"),
    ] {
        let d = fleets.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("fleet.yaml"),
            format!("name: {name}\nchannel_id: fleet-{name}\nloop:\n  trigger: \"{trigger}\"\n"),
        )
        .unwrap();
    }
    let st = schedule_status(home.path(), None);
    assert_eq!(st.schedules.len(), 4, "{:?}", st.schedules);
    for it in &st.schedules {
        let (has_fire, note) = next_state(it);
        assert!(
            has_fire != note.is_some(),
            "exactly one of a fire time and a note, got ({has_fire}, {note:?}) for {it:?}"
        );
    }
}

/// The stamp the daemon writes is enough to answer the question the old
/// note declined: a fleet on a 30-minute interval that ran ten minutes ago
/// fires twenty minutes from now.
#[test]
fn an_interval_fleet_with_a_last_run_gets_real_fire_times() {
    let home = tempfile::tempdir().unwrap();
    let d = home.path().join("fleets/paced");
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("fleet.yaml"),
        "name: paced\nchannel_id: fleet-paced\nloop:\n  trigger: \"interval:30m\"\n",
    )
    .unwrap();
    let ten_min_ago = chrono::Local::now().timestamp() - 600;
    fs::write(d.join(".last_run"), ten_min_ago.to_string()).unwrap();

    let st = schedule_status(home.path(), None);
    let ScheduleItem::Fleet {
        next_fires,
        next_note,
        ..
    } = &st.schedules[0]
    else {
        panic!("{:?}", st.schedules)
    };
    assert!(next_note.is_none(), "{next_note:?}");
    assert_eq!(next_fires.len(), 3, "{next_fires:?}");
    let first = chrono::DateTime::parse_from_rfc3339(&next_fires[0]).unwrap();
    let in_secs = first.timestamp() - chrono::Local::now().timestamp();
    assert!(
        (1150..=1250).contains(&in_secs),
        "expected ~20 minutes out, got {in_secs}s"
    );
}

/// A boundary that passed without the fleet running means nothing is
/// running it. Naming the boundary after that one would assert a wait the
/// fleet is not doing.
#[test]
fn an_overdue_interval_fleet_says_it_is_overdue() {
    let home = tempfile::tempdir().unwrap();
    let d = home.path().join("fleets/stalled");
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("fleet.yaml"),
        "name: stalled\nchannel_id: fleet-stalled\nloop:\n  trigger: \"interval:5m\"\n",
    )
    .unwrap();
    fs::write(
        d.join(".last_run"),
        (chrono::Local::now().timestamp() - 3600).to_string(),
    )
    .unwrap();

    let st = schedule_status(home.path(), None);
    let ScheduleItem::Fleet {
        next_fires,
        next_note,
        ..
    } = &st.schedules[0]
    else {
        panic!("{:?}", st.schedules)
    };
    assert!(next_fires.is_empty());
    assert!(
        next_note.as_deref().unwrap().contains("due since"),
        "{next_note:?}"
    );
}

/// The row from the report: a fleet running every half hour rendered as
/// "—", which reads as "will not run again".
#[test]
fn an_interval_fleet_says_why_it_has_no_next_time() {
    let home = tempfile::tempdir().unwrap();
    let d = home.path().join("fleets/smoke");
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("fleet.yaml"),
        "name: smoke\nchannel_id: fleet-smoke\nloop:\n  trigger: \"interval:30m\"\n",
    )
    .unwrap();
    let st = schedule_status(home.path(), None);
    let ScheduleItem::Fleet {
        description,
        next_note,
        ..
    } = &st.schedules[0]
    else {
        panic!("{:?}", st.schedules)
    };
    assert_eq!(description, "every 30m");
    assert!(
        next_note.as_deref().unwrap().contains("last run"),
        "{next_note:?}"
    );
}

#[test]
fn cron_is_phrased_for_the_shapes_that_occur() {
    for (expr, want) in [
        ("*/15 * * * *", "every 15 minutes"),
        ("* * * * *", "every minute"),
        ("0 * * * *", "hourly"),
        ("30 9 * * *", "daily at 09:30"),
        ("0 3 * * *", "daily at 03:00"),
        ("30 9 * * 1-5", "weekdays at 09:30"),
    ] {
        assert_eq!(describe_cron(expr), want, "for {expr}");
    }
}

/// A cron string a reader can look up beats a sentence that is wrong, so
/// anything unrecognised must come back verbatim rather than guessed at.
#[test]
fn a_bounded_entry_is_phrased_as_a_one_off_and_an_unreadable_bound_is_not() {
    assert!(
        describe_bounded("0 10 1 9 *", Some("2026-09-01T10:00:00+08:00")).starts_with("once,"),
        "a bound is the difference between one morning and every September"
    );
    // Both sides read the bound through `scheduler::parse_bound`, so this
    // cannot drift into showing a spent one-off while the entry keeps firing.
    assert_eq!(
        describe_bounded("0 9 * * *", Some("next tuesday")),
        describe_cron("0 9 * * *"),
        "an unreadable bound is no bound, on the surface as in the scheduler"
    );
}

/// A one-off whose date has passed must not read like one still to come:
/// that is the whole of #2437.
#[test]
fn a_past_one_off_is_marked_expired_and_a_future_one_is_not() {
    let bound = "2026-11-03T10:00:00+08:00";
    let at = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Local)
    };
    let before = at("2026-11-03T09:59:00+08:00");
    let after = at("2026-11-03T10:01:00+08:00");

    assert!(!describe_bounded_at("0 10 3 11 *", Some(bound), before).ends_with("(expired)"));
    assert!(describe_bounded_at("0 10 3 11 *", Some(bound), after).ends_with("(expired)"));
    assert!(!is_expired(Some(bound), before));
    assert!(is_expired(Some(bound), after));
    assert!(!is_expired(None, after), "an unbounded entry never expires");
    assert!(
        !is_expired(Some("next tuesday"), after),
        "nor does an unreadable bound"
    );
}

#[test]
fn an_unrecognised_cron_shape_is_returned_verbatim() {
    for expr in ["0 9 1 * *", "15 2 * 6 3", "not a cron", "0 9 * *"] {
        assert_eq!(describe_cron(expr), expr, "must not invent a phrasing");
    }
}

#[test]
fn trigger_phrasing_covers_all_three_forms() {
    assert_eq!(describe_trigger("cron:0 * * * *"), "hourly");
    assert_eq!(describe_trigger("interval:2h"), "every 2h");
    assert_eq!(
        describe_trigger("manual"),
        "manual — runs only when started"
    );
}

use super::*;
use std::fs;

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A minimally-valid `AgentProfile` YAML fixture (all required top-level
/// fields populated), parameterized by `name` and a `lifecycle:` block
/// body so tests can plug in cron/idle-trigger schedules.
fn minimal_profile_yaml(name: &str, lifecycle_body: &str) -> String {
    format!(
        r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: {name}
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: {{ tone: neutral, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "llama3.2:3b", params: {{ temperature: 0.2, max_tokens: 4096 }} }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: {{ mode: system }} }}
  filesystem: {{ read: [], write: [], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {lifecycle_body}
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#
    )
}

#[test]
fn aggregates_all_three_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write(
        &home.join("agents/alice/profile.yaml"),
        &minimal_profile_yaml(
            "alice",
            r#"{ restart: on_failure, schedule: [{ cron: "30 9 * * 1-5", message: hi }], idle_triggers: [{ after_secs: 3600, message: yo }] }"#,
        ),
    );
    write(
        &home.join("fleets/dev/fleet.yaml"),
        "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n  budget_usd: 1.0\n",
    );

    let st = schedule_status(home, None);
    assert!(st.warnings.is_empty(), "{:?}", st.warnings);

    let kinds: Vec<&str> = st
        .schedules
        .iter()
        .map(|s| match s {
            ScheduleItem::AgentCron { .. } => "agent_cron",
            ScheduleItem::AgentIdle { .. } => "agent_idle",
            ScheduleItem::Workflow { .. } => "workflow",
            ScheduleItem::Fleet { .. } => "fleet",
        })
        .collect();
    assert!(kinds.contains(&"agent_cron"));
    assert!(kinds.contains(&"agent_idle"));
    assert!(kinds.contains(&"fleet"));

    // cron entries got next-fire previews
    let cron = st
        .schedules
        .iter()
        .find_map(|s| match s {
            ScheduleItem::AgentCron { next_fires, .. } => Some(next_fires),
            _ => None,
        })
        .unwrap();
    assert_eq!(cron.len(), 3);
}

#[test]
fn stopped_fleet_reports_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write(
        &home.join("fleets/dev/fleet.yaml"),
        "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"interval:1h\"\n",
    );
    write(&home.join("fleets/dev/.stopped"), "");

    let st = schedule_status(home, None);
    let ScheduleItem::Fleet { status, .. } = &st.schedules[0] else {
        panic!("expected a fleet schedule item");
    };
    assert_eq!(status, "stopped");
}

#[test]
fn agent_filter_keeps_globals() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write(
        &home.join("agents/alice/profile.yaml"),
        &minimal_profile_yaml(
            "alice",
            r#"{ restart: on_failure, schedule: [{ cron: "0 9 * * *", message: hi }] }"#,
        ),
    );
    write(
        &home.join("agents/bob/profile.yaml"),
        &minimal_profile_yaml(
            "bob",
            r#"{ restart: on_failure, schedule: [{ cron: "0 8 * * *", message: yo }] }"#,
        ),
    );
    write(
        &home.join("fleets/dev/fleet.yaml"),
        "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"cron:0 3 * * *\"\n",
    );

    let st = schedule_status(home, Some("alice"));

    assert!(st.schedules.iter().all(|s| match s {
        ScheduleItem::AgentCron { owner, .. } => owner == "alice",
        _ => true,
    }));
    assert!(
        st.schedules
            .iter()
            .any(|s| matches!(s, ScheduleItem::Fleet { .. }))
    );
}

#[test]
fn broken_profile_is_a_warning_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write(&home.join("agents/broken/profile.yaml"), "{ not: yaml: [");
    write(
        &home.join("agents/ok/profile.yaml"),
        &minimal_profile_yaml(
            "ok",
            r#"{ restart: on_failure, schedule: [{ cron: "0 9 * * *", message: hi }] }"#,
        ),
    );

    let st = schedule_status(home, None);

    assert_eq!(st.warnings.len(), 1);
    assert!(
        st.schedules
            .iter()
            .any(|s| matches!(s, ScheduleItem::AgentCron { owner, .. } if owner == "ok"))
    );
}

/// A spent one-off with no channel yet says "no firing recorded", never
/// "missed": the runtime may not have started since the date (#2437).
/// Live and recurring entries get no note at all.
#[test]
fn firing_note_only_speaks_for_spent_one_offs_and_never_guesses_missed() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/probe")).unwrap();
    let at = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&Local)
    };
    let entry = |not_after: Option<&str>| ScheduleEntry {
        cron: "0 10 3 11 *".into(),
        message: "pay invoice".into(),
        sends_to: None,
        not_after: not_after.map(str::to_string),
    };
    let bound = "2026-11-03T10:00:00+08:00";
    let note = |e: &ScheduleEntry, now: &str| firing_note(home.path(), "probe", e, at(now));

    assert_eq!(note(&entry(Some(bound)), "2026-11-03T09:59:00+08:00"), None);
    assert_eq!(note(&entry(None), "2030-01-01T00:00:00+08:00"), None);
    assert_eq!(
        note(&entry(Some(bound)), "2026-11-03T10:01:00+08:00").as_deref(),
        Some("no firing recorded")
    );
}

#[test]
fn each_outcome_reads_differently() {
    let texts = [
        outcome_text(FiringOutcome::Fired),
        outcome_text(FiringOutcome::Missed),
        outcome_text(FiringOutcome::NoRecord),
    ];
    assert!(texts[0].starts_with("fired"));
    assert!(texts[1].starts_with("missed"));
    assert!(!texts[2].contains("missed"));
}
