//! companion_why surfaces the ledger event chain for one msg_id.
//!
//! Production layout: `outbox-ledger/` is a directory of per-day
//! `<YYYY-MM-DD>.jsonl` files written by `mur_agent_runtime::durable::
//! ledger::Ledger`. `Ledger::scan_days` uses `chrono::Local::now()` as
//! the wall-clock anchor, so for determinism we write the fixture under
//! today's date dynamically rather than hard-coding it.

use chrono::Local;
use mur_agent_gui_lib::companion_bridge::commands::companion_why_inner;
use tempfile::TempDir;

#[test]
fn why_returns_events_in_order_for_one_msg() {
    let dir = TempDir::new().unwrap();
    let ledger_dir = dir.path().join("agents/alex/companion/outbox-ledger");
    std::fs::create_dir_all(&ledger_dir).unwrap();

    // Write today's file so it falls within `scan_days(.., 7)`'s window.
    let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
    let ledger_path = ledger_dir.join(format!("{today}.jsonl"));
    std::fs::write(
        &ledger_path,
        "\
{\"event\":\"MessageScheduled\",\"id\":\"01HWHY_001\",\"situation\":\"morning_greeting\",\"template_id\":\"t\",\"scheduled_for\":\"2026-05-02T07:00:00Z\"}
{\"event\":\"MessageScheduled\",\"id\":\"OTHER\",\"situation\":\"x\",\"template_id\":\"y\",\"scheduled_for\":\"2026-05-02T08:00:00Z\"}
{\"event\":\"MessageGenerated\",\"id\":\"01HWHY_001\",\"locale_used\":\"en\",\"body_sha256\":\"abc\",\"linter_violations\":0,\"regen_count\":0}
{\"event\":\"MessageSent\",\"id\":\"01HWHY_001\",\"channel\":\"stdout\",\"sent_at\":\"2026-05-02T07:00:01Z\"}
",
    )
    .unwrap();

    let events = companion_why_inner(dir.path(), "alex", "01HWHY_001").unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["event"], "MessageScheduled");
    assert_eq!(events[1]["event"], "MessageGenerated");
    assert_eq!(events[2]["event"], "MessageSent");
}

#[test]
fn why_returns_empty_when_ledger_missing() {
    let dir = TempDir::new().unwrap();
    let events = companion_why_inner(dir.path(), "alex", "any").unwrap();
    assert!(events.is_empty());
}
