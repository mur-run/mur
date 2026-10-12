//! `mur agent hitl list|forget` — the approval decisions gate B replays
//! without asking (#1806). Both go through the running runtime: it holds the
//! agent key that signs a revocation, and it is the one reading the log.

use anyhow::Result;
use mur_common::hitl::{HITL_FORGET_METHOD, HITL_LIST_METHOD};
use serde::Deserialize;

use crate::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};

use super::resolve_mur_home;

/// Leading hash characters shown in the table: enough to be a unique prefix
/// for `forget` on any realistic number of decisions.
const HASH_SHOWN: usize = 12;
/// Shown in place of a label for decisions recorded before #1806.
const NO_LABEL: &str = "(recorded before labels)";

#[derive(Debug, Deserialize)]
struct Row {
    action_hash: String,
    allow: bool,
    issued_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListResult {
    decisions: Vec<Row>,
}

#[derive(Debug, Deserialize)]
struct ForgetResult {
    revoked: Vec<String>,
}

fn dial(agent: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let home = resolve_mur_home()?;
    let name = canonicalize_agent_name(&home, agent);
    dial_method(&home, &name, method, params, DialMode::RequireRunning).map_err(|e| {
        if e.to_string().contains("is not running") {
            anyhow::anyhow!(
                "agent '{name}' is not running; its remembered decisions are read and \
                 revoked by the runtime. Start it with 'mur agent start {name}' and retry."
            )
        } else {
            e
        }
    })
}

pub fn cmd_hitl_list(agent: &str, json: bool) -> Result<()> {
    let v = dial(agent, HITL_LIST_METHOD, serde_json::json!({}))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let list: ListResult = serde_json::from_value(v)?;
    print!("{}", render(&list.decisions, chrono::Utc::now()));
    Ok(())
}

pub fn cmd_hitl_forget(agent: &str, hash: Option<&str>, all: bool) -> Result<()> {
    let params = match (hash, all) {
        (_, true) => serde_json::json!({ "all": true }),
        (Some(h), false) => serde_json::json!({ "hash_prefix": h }),
        (None, false) => anyhow::bail!("give a hash prefix, or --all"),
    };
    let r: ForgetResult = serde_json::from_value(dial(agent, HITL_FORGET_METHOD, params)?)?;
    match r.revoked.len() {
        0 => println!("nothing to forget — no remembered decisions"),
        n => {
            for h in &r.revoked {
                println!("forgot {}", short(h));
            }
            println!("{n} decision(s) forgotten; the agent will ask again next time");
        }
    }
    Ok(())
}

fn short(h: &str) -> &str {
    h.get(..HASH_SHOWN).unwrap_or(h)
}

fn render(rows: &[Row], now: chrono::DateTime<chrono::Utc>) -> String {
    if rows.is_empty() {
        return "no remembered decisions — every gated call will ask\n".into();
    }
    let mut out = format!(
        "{:<12}  {:<5}  {:<9}  {:<10}  WHAT\n",
        "HASH", "", "DECIDED", "EXPIRES"
    );
    for r in rows {
        out += &format!(
            "{:<12}  {:<5}  {:<9}  {:<10}  {}\n",
            short(&r.action_hash),
            if r.allow { "allow" } else { "deny" },
            format!("{} ago", human(now - r.issued_at)),
            format!("in {}", human(r.expires_at - now)),
            r.label.as_deref().unwrap_or(NO_LABEL),
        );
    }
    out
}

fn human(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_shows_label_or_marks_old_rows() {
        let now = chrono::Utc::now();
        let row = |h: &str, label: Option<&str>| Row {
            action_hash: h.into(),
            allow: false,
            issued_at: now - chrono::Duration::hours(2),
            expires_at: now + chrono::Duration::days(6),
            label: label.map(str::to_string),
        };
        let out = render(
            &[
                row("abcdef0123456789", Some("edit_file: Cargo.toml")),
                row("ff", None),
            ],
            now,
        );
        assert!(out.contains("abcdef012345  deny"));
        assert!(!out.contains("abcdef0123456789"));
        assert!(out.contains("2h ago"));
        assert!(out.contains("in 6d"));
        assert!(out.contains("edit_file: Cargo.toml"));
        assert!(out.contains(NO_LABEL));
    }

    #[test]
    fn render_empty() {
        assert!(render(&[], chrono::Utc::now()).contains("no remembered decisions"));
    }
}
