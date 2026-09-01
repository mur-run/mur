//! `mur agent companion why-did-you-message <name> [<msg-id>]`
//!
//! No id: list last 7 days' MessageSent events.
//! With id: replay events for that id and print the full event chain.
//!
//! Read-only — never writes to ledger, inbox, or any state file.

use anyhow::Result;
use clap::Args;
use mur_agent_runtime::companion::telemetry::OutboxEvent;
use mur_agent_runtime::durable::ledger::Ledger;
use std::io::Write;
use std::path::Path;

// ── CLI types ──────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct WhyArgs {
    pub name: String,
    /// Specific message id; without it, lists the last 7 days of MessageSent events.
    pub msg_id: Option<String>,
}

// ── Entry point ────────────────────────────────────────────────────────────

pub async fn run(args: WhyArgs) -> Result<()> {
    let agent_home = super::util::agent_home_for(&args.name)?;
    let ledger_dir = agent_home.join("companion/outbox-ledger");
    let events: Vec<OutboxEvent> = if ledger_dir.exists() {
        Ledger::scan_days::<OutboxEvent>(&ledger_dir, 7)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    } else {
        Vec::new()
    };

    match args.msg_id {
        None => print_recent_sent(&events, &mut std::io::stdout()),
        Some(id) => print_event_chain(&events, &id, &agent_home, &mut std::io::stdout()),
    }
}

// ── Replay state ───────────────────────────────────────────────────────────

#[derive(Default)]
struct ReplayState {
    msg_id: Option<String>,
    situation: Option<String>,
    template_id: Option<String>,
    scheduled_at: Option<String>,
    generated_at: Option<String>,
    locale_used: Option<String>,
    fallback_from: Option<String>,
    linter_violations: Option<u32>,
    regen_count: Option<u32>,
    paused_at: Option<String>,
    pause_reason: Option<String>,
    resume_at: Option<String>,
    resumed_at: Option<String>,
    sent_at: Option<String>,
    channel: Option<String>,
    user_signal: Option<String>,
    user_signal_at: Option<String>,
    passive_dismiss_at: Option<String>,
    body_sha256: Option<String>,
    dropped_reason: Option<String>,
}

// ── Core helpers (pub(crate) for tests) ────────────────────────────────────

/// Print a compact table of MessageSent events from the last 7 days.
pub(crate) fn print_recent_sent(events: &[OutboxEvent], out: &mut impl Write) -> Result<()> {
    // Gather per-msg-id data by walking events in order.
    struct Row {
        msg_id: String,
        situation: String,
        sent_at: String,
        body_sha256: String,
    }

    let mut rows: Vec<Row> = Vec::new();

    for ev in events {
        if let OutboxEvent::MessageSent { id, sent_at, .. } = ev {
            // Cross-reference other events for this id.
            let situation = find_situation(events, id);
            let body_sha256 = find_body_sha256(events, id);
            rows.push(Row {
                msg_id: id.clone(),
                situation,
                sent_at: sent_at.to_rfc3339(),
                body_sha256,
            });
        }
    }

    // Sort descending by sent_at.
    rows.sort_by(|a, b| b.sent_at.cmp(&a.sent_at));

    if rows.is_empty() {
        writeln!(out, "(no MessageSent events in the last 7 days)")?;
        return Ok(());
    }

    // Header.
    writeln!(
        out,
        "{:<32} {:<24} {:<35} body_sha256",
        "msg_id", "situation", "sent_at"
    )?;
    writeln!(out, "{}", "-".repeat(100))?;

    for r in &rows {
        let sha_prefix = if r.body_sha256.len() >= 8 {
            r.body_sha256[..8].to_string()
        } else {
            r.body_sha256.clone()
        };
        writeln!(
            out,
            "{:<32} {:<24} {:<35} {}...",
            r.msg_id, r.situation, r.sent_at, sha_prefix
        )?;
    }

    Ok(())
}

/// Replay all events for a given msg_id and print the full event chain.
pub(crate) fn print_event_chain(
    events: &[OutboxEvent],
    id: &str,
    _agent_home: &Path,
    out: &mut impl Write,
) -> Result<()> {
    let mut state = ReplayState::default();
    let mut matched = false;

    for ev in events {
        match ev {
            OutboxEvent::MessageScheduled {
                id: eid,
                situation,
                template_id,
                scheduled_for,
            } if eid == id => {
                matched = true;
                state.msg_id = Some(eid.clone());
                state.situation = Some(situation_to_slug(situation).to_string());
                state.template_id = Some(template_id.clone());
                state.scheduled_at = Some(scheduled_for.to_rfc3339());
            }
            OutboxEvent::MessageGenerated {
                id: eid,
                locale_used,
                body_sha256,
                linter_violations,
                regen_count,
            } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                state.locale_used = Some(locale_used.clone());
                state.body_sha256 = Some(body_sha256.clone());
                state.linter_violations = Some(*linter_violations);
                state.regen_count = Some(*regen_count);
            }
            OutboxEvent::MessagePaused {
                id: eid,
                resume_at,
                reason,
            } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                // paused_at is not stored in the variant; use a placeholder.
                state.paused_at = Some("(not recorded)".to_string());
                state.pause_reason = Some(reason.clone());
                state.resume_at = Some(resume_at.to_rfc3339());
            }
            OutboxEvent::MessageSent {
                id: eid,
                channel,
                sent_at,
            } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                state.sent_at = Some(sent_at.to_rfc3339());
                state.channel = Some(channel.clone());
            }
            OutboxEvent::MessageDropped { id: eid, reason } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                state.dropped_reason = Some(reason.clone());
            }
            OutboxEvent::UserSignal {
                id: eid,
                signal,
                at,
            } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                state.user_signal = Some(format!("{signal:?}").to_lowercase());
                state.user_signal_at = Some(at.to_rfc3339());
            }
            OutboxEvent::PassiveDismissInferred { id: eid, at } if eid == id => {
                matched = true;
                state.msg_id.get_or_insert_with(|| eid.clone());
                state.passive_dismiss_at = Some(at.to_rfc3339());
            }
            _ => {}
        }
    }

    if !matched {
        anyhow::bail!("no events found for msg_id `{id}`; check `inbox` first");
    }

    // Render.
    let na = "n/a".to_string();
    let not_rec = "(not recorded)".to_string();

    writeln!(out, "# why-did-you-message {id}")?;
    writeln!(out)?;
    writeln!(out, "msg_id: {}", state.msg_id.as_deref().unwrap_or(id))?;

    let situation = state.situation.as_deref().unwrap_or(&na);
    let template_id = state.template_id.as_deref().unwrap_or(&na);
    writeln!(out, "situation: {situation:<24} template_id: {template_id}")?;

    let scheduled_at = state.scheduled_at.as_deref().unwrap_or(&not_rec);
    writeln!(
        out,
        "scheduled_at: {scheduled_at:<32} decided_by: weighted_random"
    )?;

    let generated_at = state.generated_at.as_deref().unwrap_or(&not_rec);
    let locale_used = state.locale_used.as_deref().unwrap_or(&na);
    let fallback_str = match &state.fallback_from {
        Some(fb) => format!("fallback from {fb}"),
        None => "no fallback".to_string(),
    };
    writeln!(
        out,
        "generated_at: {generated_at:<32} locale_used: {locale_used} ({fallback_str})"
    )?;

    let linter_violations = state.linter_violations.unwrap_or(0);
    let regen_count = state.regen_count.unwrap_or(0);
    let linter_passed = linter_violations == 0;
    writeln!(
        out,
        "linter_passed: {linter_passed:<18} regen_count: {regen_count}"
    )?;

    match &state.paused_at {
        Some(paused_at) => {
            let pause_reason = state.pause_reason.as_deref().unwrap_or(&na);
            let resume_at = state.resume_at.as_deref().unwrap_or(&na);
            writeln!(
                out,
                "paused_at: {paused_at:<28} reason: {pause_reason} (resume_at={resume_at})"
            )?;
            if let Some(resumed_at) = &state.resumed_at {
                writeln!(out, "resumed_at: {resumed_at}")?;
            }
        }
        None => {
            writeln!(out, "paused: none")?;
        }
    }

    let sent_at = state.sent_at.as_deref().unwrap_or(&na);
    let channel = state.channel.as_deref().unwrap_or(&na);
    writeln!(out, "sent_at: {sent_at:<36} channel: {channel}")?;

    match (&state.user_signal, &state.user_signal_at) {
        (Some(sig), Some(at)) => writeln!(out, "user_signal: {sig} at {at}")?,
        _ => writeln!(out, "user_signal: none")?,
    }

    let passive_dismiss_at = state.passive_dismiss_at.as_deref().unwrap_or(&na);
    writeln!(out, "passive_dismiss_at: {passive_dismiss_at}")?;

    let sha_prefix = match &state.body_sha256 {
        Some(s) if s.len() >= 8 => format!("{}...", &s[..8]),
        Some(s) => s.clone(),
        None => na.clone(),
    };
    writeln!(out, "body_sha256: {sha_prefix}")?;

    match &state.dropped_reason {
        Some(r) => writeln!(out, "dropped: {r}")?,
        None => writeln!(out, "dropped: n/a")?,
    }

    Ok(())
}

// ── Private helpers ────────────────────────────────────────────────────────

fn find_situation(events: &[OutboxEvent], msg_id: &str) -> String {
    for ev in events {
        if let OutboxEvent::MessageScheduled { id, situation, .. } = ev
            && id == msg_id
        {
            return situation_to_slug(situation).to_string();
        }
    }
    "n/a".to_string()
}

fn find_body_sha256(events: &[OutboxEvent], msg_id: &str) -> String {
    for ev in events {
        if let OutboxEvent::MessageGenerated {
            id, body_sha256, ..
        } = ev
            && id == msg_id
        {
            return body_sha256.clone();
        }
    }
    "n/a".to_string()
}

fn situation_to_slug(s: &mur_common::companion::Situation) -> &'static str {
    use mur_common::companion::Situation;
    match s {
        Situation::MorningGreeting => "morning_greeting",
        Situation::GentleCheckIn => "gentle_check_in",
        Situation::ShareQuote => "share_quote",
        Situation::ShareLink => "share_link",
        Situation::WorkflowNudge => "workflow_nudge",
        // Unreachable from here in practice: a scheduled reminder skips the
        // outbox, so it never writes the ledger events these functions read.
        // The arm exists because the enum is exhaustive, not because the
        // outbox handles schedules.
        Situation::Scheduled => "scheduled",
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_agent_runtime::companion::telemetry::OutboxEvent;
    use mur_agent_runtime::durable::ledger::Ledger;
    use mur_common::companion::{Signal, Situation};
    use tempfile::TempDir;

    fn make_ledger(tmp: &TempDir) -> Ledger {
        let dir = tmp.path().join("companion/outbox-ledger");
        std::fs::create_dir_all(&dir).unwrap();
        Ledger::open(&dir).unwrap()
    }

    fn ts(h: u32, m: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 4, 29, h, m, 0).unwrap()
    }

    #[test]
    fn no_id_lists_recent_sent() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = make_ledger(&tmp);
        ledger
            .append(&OutboxEvent::MessageScheduled {
                id: "msg-001".into(),
                situation: Situation::MorningGreeting,
                template_id: "tmpl-a".into(),
                scheduled_for: ts(8, 0),
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::MessageGenerated {
                id: "msg-001".into(),
                locale_used: "en-US".into(),
                body_sha256: "abcdef1234567890".into(),
                linter_violations: 0,
                regen_count: 0,
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::MessageSent {
                id: "msg-001".into(),
                channel: "stdout".into(),
                sent_at: ts(8, 5),
            })
            .unwrap();

        let mut buf: Vec<u8> = Vec::new();
        let events: Vec<OutboxEvent> =
            Ledger::scan_days::<OutboxEvent>(&tmp.path().join("companion/outbox-ledger"), 7)
                .into_iter()
                .filter_map(|r| r.ok())
                .collect();
        print_recent_sent(&events, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("msg-001"));
        assert!(out.contains("morning_greeting"));
        assert!(out.contains("abcdef12"));
    }

    #[test]
    fn with_id_renders_full_event_chain() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = make_ledger(&tmp);
        ledger
            .append(&OutboxEvent::MessageScheduled {
                id: "msg-001".into(),
                situation: Situation::GentleCheckIn,
                template_id: "tmpl-b".into(),
                scheduled_for: ts(8, 0),
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::MessageGenerated {
                id: "msg-001".into(),
                locale_used: "zh-TW".into(),
                body_sha256: "deadbeefcafebabe".into(),
                linter_violations: 0,
                regen_count: 0,
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::MessagePaused {
                id: "msg-001".into(),
                resume_at: ts(8, 2),
                reason: "rate_limit_429".into(),
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::MessageSent {
                id: "msg-001".into(),
                channel: "stdout".into(),
                sent_at: ts(8, 5),
            })
            .unwrap();
        ledger
            .append(&OutboxEvent::UserSignal {
                id: "msg-001".into(),
                signal: Signal::Positive,
                at: ts(9, 0),
            })
            .unwrap();

        let mut buf: Vec<u8> = Vec::new();
        let events: Vec<OutboxEvent> =
            Ledger::scan_days::<OutboxEvent>(&tmp.path().join("companion/outbox-ledger"), 7)
                .into_iter()
                .filter_map(|r| r.ok())
                .collect();
        print_event_chain(&events, "msg-001", tmp.path(), &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("msg-001"));
        assert!(out.contains("gentle_check_in"));
        assert!(out.contains("tmpl-b"));
        assert!(out.contains("zh-TW"));
        assert!(out.contains("rate_limit_429"));
        assert!(out.contains("stdout"));
        assert!(out.contains("Positive") || out.contains("positive"));
        assert!(out.contains("deadbeef"));
    }

    #[test]
    fn missing_id_errors() {
        let tmp = TempDir::new().unwrap();
        make_ledger(&tmp);
        let events: Vec<OutboxEvent> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let r = print_event_chain(&events, "no-such", tmp.path(), &mut buf);
        assert!(r.is_err());
    }
}
