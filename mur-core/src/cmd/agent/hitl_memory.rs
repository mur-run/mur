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
    /// Set when the decision log could not be verified: why, and which channel
    /// to repair (#2033). `decisions` is empty then, and stays empty.
    #[serde(default)]
    unavailable: Option<Unavailable>,
}

#[derive(Debug, Deserialize)]
struct Unavailable {
    reason: String,
    channel_id: String,
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
    match &list.unavailable {
        Some(u) => print!("{}", render_unavailable(&canonicalize_for_msg(agent), u)),
        None => print!("{}", render(&list.decisions, chrono::Utc::now())),
    }
    Ok(())
}

pub fn cmd_hitl_forget(
    agent: &str,
    hash: Option<&str>,
    all: bool,
    denies_only: bool,
) -> Result<()> {
    let params = forget_params(hash, all, denies_only)?;
    let r: ForgetResult = serde_json::from_value(dial(agent, HITL_FORGET_METHOD, params)?)?;
    match r.revoked.len() {
        0 if denies_only => println!("nothing to forget — no remembered denials"),
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

/// Exactly one target. Denies-only is sent as its own key, never alongside
/// `all`, so a runtime that predates it errors instead of forgetting allows.
fn forget_params(hash: Option<&str>, all: bool, denies_only: bool) -> Result<serde_json::Value> {
    match (hash, all, denies_only) {
        (None, true, false) => Ok(serde_json::json!({ "all": true })),
        (None, false, true) => Ok(serde_json::json!({ "denies_only": true })),
        (Some(h), false, false) => Ok(serde_json::json!({ "hash_prefix": h })),
        (None, false, false) => anyhow::bail!("give a hash prefix, --all, or --denies-only"),
        _ => anyhow::bail!("use only one of a hash prefix, --all, or --denies-only"),
    }
}

fn canonicalize_for_msg(agent: &str) -> String {
    resolve_mur_home()
        .map(|home| canonicalize_agent_name(&home, agent))
        .unwrap_or_else(|_| agent.to_string())
}

/// Plain explanation for a log that cannot be trusted, with the recovery
/// command ready to copy. Nothing is auto-allowed meanwhile (#2033).
fn render_unavailable(agent: &str, u: &Unavailable) -> String {
    // Only this one reason is the benign upgrade state; any other (a record
    // missing, a head that does not match) must not be waved off as expected.
    let note = if u.reason.contains("no signed head") {
        " (expected after upgrading past #1890)"
    } else {
        ""
    };
    format!(
        "remembered approvals for '{agent}' are unavailable: {reason}{note}.\n\
         Gated calls will ask again; nothing is auto-allowed.\n\
         To start a fresh signed log now: stop the agent, then run \
         `mur channel repair {channel}` in a terminal.\n",
        reason = u.reason,
        channel = u.channel_id,
    )
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
    fn forget_params_sends_one_target_under_its_own_key() {
        assert_eq!(
            forget_params(None, false, true).unwrap(),
            serde_json::json!({ "denies_only": true })
        );
        assert_eq!(
            forget_params(None, true, false).unwrap(),
            serde_json::json!({ "all": true })
        );
        assert_eq!(
            forget_params(Some("ab"), false, false).unwrap(),
            serde_json::json!({ "hash_prefix": "ab" })
        );
        assert!(forget_params(None, false, false).is_err());
        assert!(forget_params(None, true, true).is_err());
        assert!(forget_params(Some("ab"), false, true).is_err());
    }

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
    fn render_unavailable_gives_reason_channel_and_repair_command() {
        let out = render_unavailable(
            "dr_worker_2",
            &Unavailable {
                reason: "log has records but no signed head".into(),
                channel_id: "chan-42".into(),
            },
        );
        assert!(out.contains("'dr_worker_2'"));
        assert!(out.contains("log has records but no signed head"));
        assert!(out.contains("`mur channel repair chan-42`"));
        assert!(out.contains("nothing is auto-allowed"));
        assert!(!out.contains("-32603") && !out.contains("internal:"));
    }

    #[test]
    fn list_result_parses_with_and_without_unavailable() {
        let plain: ListResult =
            serde_json::from_value(serde_json::json!({"decisions": []})).unwrap();
        assert!(plain.unavailable.is_none());
        let bad: ListResult = serde_json::from_value(serde_json::json!({
            "decisions": [],
            "unavailable": {"reason": "r", "channel_id": "c"}
        }))
        .unwrap();
        assert_eq!(bad.unavailable.unwrap().channel_id, "c");
    }

    #[test]
    fn render_empty() {
        assert!(render(&[], chrono::Utc::now()).contains("no remembered decisions"));
    }
}
