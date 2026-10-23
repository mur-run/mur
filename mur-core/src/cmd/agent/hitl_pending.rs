//! `mur agent hitl <agent> pending|approve|deny` — answer an approval a
//! headless `mur agent send --wait-approval` turn is waiting on (#2085).
//!
//! Approving goes through the same `tool/hitl_respond` murmur uses, with the
//! home's approval token attached, so the runtime's approval authority is the
//! only thing deciding whether an allow is a human's. The answer is recorded
//! in the decision log exactly as an in-chat answer would be.

use anyhow::{Result, bail};
use mur_common::hitl::HITL_PENDING_METHOD;
use serde::Deserialize;

use crate::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};
use crate::cmd::agent::cli::stream::hitl_respond_params;

use super::resolve_mur_home;

/// Recorded as the answering surface in the signed decision log.
const SURFACE: &str = "cli";
/// Minimum leading characters of a `hitl_id` shown in the table; any unique
/// prefix is accepted back by `approve` / `deny`. The table widens past this
/// when needed: UUIDv7's first 12 hex digits are the millisecond, so calls
/// from one batch routinely share them.
const SHORT_ID: usize = 13;

#[derive(Debug, Deserialize)]
struct PendingList {
    pending: Vec<Pending>,
}

#[derive(Debug, Clone, Deserialize)]
struct Pending {
    hitl_id: String,
    label: String,
    parked_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

fn dial(agent: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let home = resolve_mur_home()?;
    let name = canonicalize_agent_name(&home, agent);
    dial_method(&home, &name, method, params, DialMode::RequireRunning).map_err(|e| {
        if e.to_string().contains("is not running") {
            anyhow::anyhow!(
                "agent '{name}' is not running, so nothing is waiting on it — a waiting \
                 approval lives only as long as the runtime and the turn that asked."
            )
        } else {
            e
        }
    })
}

fn fetch(agent: &str) -> Result<Vec<Pending>> {
    let v = dial(agent, HITL_PENDING_METHOD, serde_json::json!({}))?;
    Ok(serde_json::from_value::<PendingList>(v)?.pending)
}

pub fn cmd_hitl_pending(agent: &str, json: bool) -> Result<()> {
    let v = dial(agent, HITL_PENDING_METHOD, serde_json::json!({}))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let list: PendingList = serde_json::from_value(v)?;
    print!("{}", render(agent, &list.pending, chrono::Utc::now()));
    Ok(())
}

pub fn cmd_hitl_answer(agent: &str, id: &str, allow: bool) -> Result<()> {
    let p = resolve(&fetch(agent)?, id)?;
    let home = resolve_mur_home()?;
    let name = canonicalize_agent_name(&home, agent);
    dial_method(
        &home,
        &name,
        "tool/hitl_respond",
        hitl_respond_params(&home, &p.hitl_id, allow, SURFACE),
        DialMode::RequireRunning,
    )?;
    println!(
        "{} {} — {}",
        if allow { "approved" } else { "denied" },
        p.hitl_id,
        p.label
    );
    Ok(())
}

/// Exactly one waiting call whose id starts with `id`.
fn resolve(pending: &[Pending], id: &str) -> Result<Pending> {
    let id = id.trim();
    if id.is_empty() {
        bail!("give the id shown by `mur agent hitl <agent> pending`");
    }
    let hits: Vec<_> = pending
        .iter()
        .filter(|p| p.hitl_id.starts_with(id))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "nothing waiting matches '{id}' — it may have been answered, timed out, \
             or its turn ended; `mur agent hitl <agent> pending` lists what is waiting"
        ),
        _ => bail!(
            "'{id}' matches {} waiting calls; give more of the id",
            hits.len()
        ),
    }
}

/// Shortest prefix length (at least `SHORT_ID`) that tells every id apart.
fn unique_len(pending: &[Pending]) -> usize {
    let longest = pending.iter().map(|p| p.hitl_id.len()).max().unwrap_or(0);
    (SHORT_ID..=longest.max(SHORT_ID))
        .find(|&n| {
            let mut seen = std::collections::HashSet::new();
            pending
                .iter()
                .all(|p| seen.insert(&p.hitl_id[..p.hitl_id.len().min(n)]))
        })
        .unwrap_or(longest)
}

fn prefix(id: &str, n: usize) -> &str {
    &id[..id.len().min(n)]
}

fn render(agent: &str, pending: &[Pending], now: chrono::DateTime<chrono::Utc>) -> String {
    if pending.is_empty() {
        return "nothing is waiting for approval\n".to_string();
    }
    let width = unique_len(pending);
    let mut out = format!(
        "{:<width$}  {:<8}  {:<8}  {}\n",
        "ID", "WAITING", "LEFT", "CALL"
    );
    for p in pending {
        out += &format!(
            "{:<width$}  {:<8}  {:<8}  {}\n",
            prefix(&p.hitl_id, width),
            human(now - p.parked_at),
            human(p.expires_at - now),
            p.label,
        );
    }
    let first = prefix(&pending[0].hitl_id, width);
    out += &format!("\nanswer with: mur agent hitl {agent} approve {first}   (or: deny {first})\n");
    out
}

fn human(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// The line `mur agent send --wait-approval` prints to stderr for each call
/// it is now waiting on. One place, so it cannot drift from the subcommand.
/// Full ids: these are meant to be pasted, and a batch's calls share their
/// leading (timestamp) digits, so any fixed prefix can be ambiguous.
pub(crate) fn approve_hint(agent: &str, hitl: &serde_json::Value) -> Vec<String> {
    let calls = hitl
        .get("calls")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_else(|| vec![hitl.clone()]);
    calls
        .iter()
        .filter_map(|c| {
            let id = c.get("hitl_id")?.as_str()?;
            let tool = c.get("tool_name").and_then(|t| t.as_str()).unwrap_or("tool");
            let input = c.get("tool_input").cloned().unwrap_or_default();
            let label = mur_common::hitl::label::decision_label(tool, &input);
            Some(format!(
                "waiting for approval: {label}\n  approve: mur agent hitl {agent} approve {}\n  deny:    mur agent hitl {agent} deny {}",
                id, id
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> Pending {
        let now = chrono::Utc::now();
        Pending {
            hitl_id: id.into(),
            label: "bash: ls".into(),
            parked_at: now,
            expires_at: now + chrono::Duration::minutes(5),
        }
    }

    #[test]
    fn a_unique_prefix_resolves() {
        let list = [p("0199aaaa-1"), p("0199bbbb-2")];
        assert_eq!(resolve(&list, "0199b").unwrap().hitl_id, "0199bbbb-2");
    }

    #[test]
    fn an_ambiguous_prefix_is_refused() {
        let list = [p("0199aaaa-1"), p("0199aaab-2")];
        let e = resolve(&list, "0199aa").unwrap_err().to_string();
        assert!(e.contains("matches 2"), "{e}");
    }

    #[test]
    fn nothing_matching_says_why() {
        let e = resolve(&[p("abc")], "zzz").unwrap_err().to_string();
        assert!(e.contains("pending"), "{e}");
    }

    #[test]
    fn render_names_the_command_to_answer() {
        let out = render("mur", &[p("0199aaaa-bbbb-cccc")], chrono::Utc::now());
        assert!(
            out.contains("mur agent hitl mur approve 0199aaaa-bbbb"),
            "{out}"
        );
        assert!(out.contains("bash: ls"), "{out}");
    }

    // Two UUIDv7s minted in the same millisecond share their first 13 chars.
    const SAME_MS_A: &str = "0199aaaa-bbbb-7001-8000-000000000001";
    const SAME_MS_B: &str = "0199aaaa-bbbb-7002-8000-000000000002";

    #[test]
    fn same_millisecond_ids_are_shown_distinctly_and_resolve() {
        let list = [p(SAME_MS_A), p(SAME_MS_B)];
        let out = render("mur", &list, chrono::Utc::now());
        let ids: Vec<&str> = out
            .lines()
            .skip(1)
            .take(2)
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_ne!(ids[0], ids[1], "{out}");
        for (shown, full) in ids.iter().zip([SAME_MS_A, SAME_MS_B]) {
            assert_eq!(resolve(&list, shown).unwrap().hitl_id, full, "{out}");
        }
    }

    #[test]
    fn hint_commands_name_the_full_id() {
        let n = serde_json::json!({"calls": [
            {"hitl_id": SAME_MS_A, "tool_name": "bash", "tool_input": {"command": "ls"}},
            {"hitl_id": SAME_MS_B, "tool_name": "bash", "tool_input": {"command": "pwd"}},
        ]});
        let lines = approve_hint("mur", &n);
        assert!(
            lines[0].contains(&format!("approve {SAME_MS_A}")),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains(&format!("deny {SAME_MS_B}")),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn hint_covers_every_call_of_a_batch() {
        let n = serde_json::json!({
            "hitl_id": "h-1",
            "calls": [
                {"hitl_id": "h-1", "tool_name": "bash", "tool_input": {"command": "ls"}},
                {"hitl_id": "h-2", "tool_name": "bash", "tool_input": {"command": "pwd"}},
            ]
        });
        let lines = approve_hint("mur", &n);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("approve h-2"), "{}", lines[1]);
    }
}
