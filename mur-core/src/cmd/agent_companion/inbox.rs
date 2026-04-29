//! `mur agent companion inbox` + `mur agent companion ack` (Spec §3.6, M6.6).

use anyhow::{Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

// ── CLI types ──────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct InboxArgs {
    pub name: String,
    /// Only show messages whose response is still \<unset\>.
    #[arg(long)]
    pub unread_only: bool,
}

#[derive(Args, Debug)]
pub struct AckArgs {
    pub name: String,
    pub msg_id: String,
    #[arg(long, conflicts_with_all = ["bad", "dismiss"])]
    pub good: bool,
    #[arg(long, conflicts_with_all = ["good", "dismiss"])]
    pub bad: bool,
    #[arg(long, conflicts_with_all = ["good", "bad"])]
    pub dismiss: bool,
}

// ── Domain types ───────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct InboxEntry {
    pub id: String,
    pub situation: String,
    pub template_id: String,
    pub locale: String,
    pub generated_at: String,
    pub body: String,
    /// `"<unset>"` | `"good"` | `"bad"` | `"dismiss"` | other
    pub response: String,
    pub path: PathBuf,
}

// ── Entry point: inbox ─────────────────────────────────────────────────────

pub async fn run_inbox(args: InboxArgs) -> Result<()> {
    let agent_home = super::util::agent_home_for(&args.name)?;
    let dir = agent_home.join("companion/inbox");
    list_inbox_at(&dir, args.unread_only, &mut std::io::stdout())
}

fn list_inbox_at(dir: &Path, unread_only: bool, out: &mut impl std::io::Write) -> Result<()> {
    if !dir.exists() {
        writeln!(out, "(inbox is empty)")?;
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|r| r.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| parse_inbox_file(&e.path()).ok())
        .filter(|e| !unread_only || e.response == "<unset>")
        .collect();
    entries.sort_by(|a, b| a.generated_at.cmp(&b.generated_at));
    if entries.is_empty() {
        writeln!(out, "(no entries)")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<28} {:<22} {:<10} {:<10} body (head)",
        "id", "generated_at", "response", "situation"
    )?;
    for e in &entries {
        let head: String = e.body.chars().take(60).collect();
        writeln!(
            out,
            "{:<28} {:<22} {:<10} {:<10} {}",
            e.id, e.generated_at, e.response, e.situation, head
        )?;
    }
    Ok(())
}

// ── Entry point: ack ───────────────────────────────────────────────────────

pub async fn run_ack(args: AckArgs) -> Result<()> {
    let signal = if args.good {
        "good"
    } else if args.bad {
        "bad"
    } else if args.dismiss {
        "dismiss"
    } else {
        anyhow::bail!("one of --good, --bad, --dismiss is required");
    };

    let agent_home = super::util::agent_home_for(&args.name)?;
    ack_at(&agent_home, &args.msg_id, signal)
}

fn ack_at(agent_home: &Path, msg_id: &str, signal: &str) -> Result<()> {
    let dir = agent_home.join("companion/inbox");
    let path = dir.join(format!("{msg_id}.md"));
    if !path.exists() {
        anyhow::bail!("inbox file not found: {}", path.display());
    }

    // 1. Rewrite the response line.
    let body = std::fs::read_to_string(&path)?;
    let new_body = rewrite_response_line(&body, signal)?;
    super::util::atomic_write_bytes(&path, new_body.as_bytes())?;

    // 2. Append UserSignal event to today's ledger.
    let ledger_dir = agent_home.join("companion/outbox-ledger");
    std::fs::create_dir_all(&ledger_dir)?;
    let mut ledger =
        mur_agent_runtime::durable::ledger::Ledger::open(&ledger_dir).context("open ledger")?;
    let signal_enum = match signal {
        "good" => mur_common::companion::Signal::Positive,
        "bad" => mur_common::companion::Signal::Negative,
        "dismiss" => mur_common::companion::Signal::Dismiss,
        _ => unreachable!(),
    };
    let event = mur_agent_runtime::companion::telemetry::OutboxEvent::UserSignal {
        id: msg_id.to_string(),
        signal: signal_enum,
        at: chrono::Utc::now(),
    };
    ledger.append(&event).context("append ledger event")?;

    // TODO(M?.x): picker.record(signal) live integration — the picker lives in
    // the runtime process; the CLI cannot reach its in-memory state.  The
    // runtime's outbox tick will consume this UserSignal event from the ledger
    // on its next wake-up and apply it to the bandit state (Phase 1.2).

    println!("✓ ack {msg_id} = {signal}");
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn parse_inbox_file(path: &Path) -> Result<InboxEntry> {
    let content = std::fs::read_to_string(path)?;

    // Strip the opening `---\n` delimiter.
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("inbox file does not start with `---`"))?;

    // Find the closing `\n---\n` delimiter.
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("inbox file missing closing `---`"))?;

    let front_str = &rest[..end];
    let body_part = &rest[end + 5..]; // past "\n---\n"

    #[derive(serde::Deserialize)]
    struct Front {
        id: String,
        situation: String,
        template_id: String,
        locale: String,
        generated_at: String,
    }
    let front: Front = serde_yaml_ng::from_str(front_str).context("parse front-matter")?;

    // Find the response line (last `>>> response: ...` line in the file).
    let response_marker = ">>> response: ";
    let response = body_part
        .lines()
        .rev()
        .find(|l| l.starts_with(response_marker))
        .map(|l| l[response_marker.len()..].trim().to_string())
        .unwrap_or_else(|| "<unset>".to_string());

    // Body = everything before the last response line.
    let body = body_part
        .trim()
        .rsplit_once(response_marker)
        .map(|(b, _)| {
            // Strip the ">>> " prefix that precedes the response_marker when
            // rsplit_once cuts at the marker itself (the marker already starts
            // with ">>> "), but actually body_part.rsplit_once splits at the
            // marker so `b` is everything before `>>> response: `.
            b.trim_end().to_string()
        })
        .unwrap_or_else(|| body_part.trim().to_string());

    Ok(InboxEntry {
        id: front.id,
        situation: front.situation,
        template_id: front.template_id,
        locale: front.locale,
        generated_at: front.generated_at,
        body,
        response,
        path: path.to_path_buf(),
    })
}

fn rewrite_response_line(body: &str, new_value: &str) -> Result<String> {
    let marker = ">>> response: ";
    let mut found = false;
    let new_lines: Vec<String> = body
        .lines()
        .map(|l| {
            if l.starts_with(marker) && !found {
                found = true;
                format!("{marker}{new_value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        anyhow::bail!("inbox file has no `>>> response: ` line");
    }
    let mut out = new_lines.join("\n");
    if body.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_inbox_file(dir: &Path, id: &str, body: &str, response: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let content = format!(
            "---\nid: {id}\nsituation: morning_greeting\ntemplate_id: tmpl-a\nlocale: en-US\ngenerated_at: 2026-04-29T08:30:00+08:00\n---\n\n{body}\n\n>>> response: {response}\n"
        );
        std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
    }

    #[test]
    fn list_inbox_shows_entries() {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("companion/inbox");
        write_inbox_file(&inbox, "msg-001", "Good morning, friend!", "<unset>");
        write_inbox_file(&inbox, "msg-002", "Hope your day is going well.", "good");

        let mut buf: Vec<u8> = Vec::new();
        list_inbox_at(&inbox, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("msg-001"));
        assert!(out.contains("msg-002"));
        assert!(out.contains("good"));
        assert!(out.contains("<unset>"));
    }

    #[test]
    fn list_inbox_unread_only_filters() {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("companion/inbox");
        write_inbox_file(&inbox, "msg-001", "Hi", "<unset>");
        write_inbox_file(&inbox, "msg-002", "Hi 2", "good");

        let mut buf: Vec<u8> = Vec::new();
        list_inbox_at(&inbox, true, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("msg-001"));
        assert!(!out.contains("msg-002"));
    }

    #[test]
    fn ack_rewrites_response_line() {
        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("companion/inbox");
        write_inbox_file(&inbox, "msg-001", "Hi", "<unset>");

        ack_at(tmp.path(), "msg-001", "good").unwrap();
        let path = inbox.join("msg-001.md");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(">>> response: good"));
        assert!(!body.contains(">>> response: <unset>"));
    }

    #[test]
    fn ack_appends_user_signal_to_ledger() {
        use mur_agent_runtime::companion::telemetry::OutboxEvent;
        use mur_agent_runtime::durable::ledger::Ledger;

        let tmp = TempDir::new().unwrap();
        let inbox = tmp.path().join("companion/inbox");
        write_inbox_file(&inbox, "msg-001", "Hi", "<unset>");

        ack_at(tmp.path(), "msg-001", "dismiss").unwrap();

        let ledger_dir = tmp.path().join("companion/outbox-ledger");
        let events: Vec<OutboxEvent> = Ledger::scan_days::<OutboxEvent>(&ledger_dir, 1)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();
        let user_signal = events
            .iter()
            .find(|e| matches!(e, OutboxEvent::UserSignal { .. }));
        assert!(user_signal.is_some());
        match user_signal.unwrap() {
            OutboxEvent::UserSignal { id, signal, .. } => {
                assert_eq!(id, "msg-001");
                assert!(matches!(signal, mur_common::companion::Signal::Dismiss));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn ack_missing_file_errors() {
        let tmp = TempDir::new().unwrap();
        let result = ack_at(tmp.path(), "nope", "good");
        assert!(result.is_err());
    }
}
