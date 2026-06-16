//! Shared helpers for the MUR mobile pairing / LAN transport (P1).
//!
//! Used by both the daemon endpoint (`mur-daemon::mobile_server`) and the
//! `mur agent pair` CLI so the QR a user scans always matches the token, port,
//! and paths the daemon actually serves. The wire protocol itself lives in
//! `mur_common::mobile`. Design:
//! `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// Default LAN port for the mobile WebSocket endpoint (distinct from the
/// signal server's 9421). Override with `MUR_MOBILE_PORT`.
pub const DEFAULT_MOBILE_PORT: u16 = 9430;
/// Default bind address — `0.0.0.0` so a phone on the LAN can reach it.
/// Override with `MUR_MOBILE_BIND`.
pub const DEFAULT_MOBILE_BIND: &str = "0.0.0.0";
/// Agent the phone talks to when none is named (the concierge).
pub const DEFAULT_MOBILE_AGENT: &str = "mur";

const PAIR_TOKEN_FILE: &str = "mobile/pair-token";
const PAIRED_DEVICES_FILE: &str = "mobile/paired.json";

/// Effective mobile port, honouring the `MUR_MOBILE_PORT` override.
pub fn mobile_port() -> u16 {
    std::env::var("MUR_MOBILE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MOBILE_PORT)
}

/// Effective bind address, honouring the `MUR_MOBILE_BIND` override.
pub fn mobile_bind() -> String {
    std::env::var("MUR_MOBILE_BIND").unwrap_or_else(|_| DEFAULT_MOBILE_BIND.to_string())
}

/// Path to the one-time pairing token under `<home>/mobile/pair-token`.
pub fn pair_token_path(home: &Path) -> PathBuf {
    home.join(PAIR_TOKEN_FILE)
}

/// Path to the paired-device store under `<home>/mobile/paired.json`.
pub fn paired_devices_path(home: &Path) -> PathBuf {
    home.join(PAIRED_DEVICES_FILE)
}

/// Read the pairing token, generating and persisting one (mode 0600) on first
/// use. Both the daemon and the `pair` CLI call this so they agree.
pub fn ensure_pair_token(home: &Path) -> Result<String> {
    let path = pair_token_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let token =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        return Ok(token.trim().to_owned());
    }
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, &token).with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Best-effort primary LAN IP of this host. No traffic is sent — we open a UDP
/// socket "connected" to a public address and read which local interface the
/// OS would route through. Returns `None` if there is no usable route.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

/// Build the pairing URI encoded into the QR. The phone parses host/port/token
/// and calls `connect_lan`. Token (uuid) and agent slug are URL-safe.
pub fn pairing_uri(host: &str, port: u16, token: &str, agent: &str) -> String {
    format!("mur-pair://{host}:{port}/?token={token}&agent={agent}")
}

/// Max channels returned to the phone (v4 scale is small).
const MOBILE_CHANNEL_LIMIT: usize = 200;

/// Serve a channel pull for the phone. `op` ∈ "list" | "events".
/// "list" → array of `{id,title,state,goal,updated_at,agents,turns}` (newest
/// first, empties hidden). "events" → that channel's events at/after `since_seq`.
/// Ownership filter: single-user, so all local channels are the owner's.
pub fn channel_query(
    home: &std::path::Path,
    op: &str,
    channel_id: Option<String>,
    since_seq: Option<u64>,
) -> anyhow::Result<serde_json::Value> {
    let svc = ChannelService::open(home)?;
    match op {
        "list" => {
            let mut out = Vec::new();
            for row in svc.list(MOBILE_CHANNEL_LIMIT)? {
                let events = svc.load_events(&row.id).unwrap_or_default();
                if events.is_empty() {
                    continue;
                }
                let manifest = svc.store().load_manifest(&row.id).ok();
                let agents: Vec<String> = manifest
                    .as_ref()
                    .map(|m| {
                        m.participants
                            .iter()
                            .filter_map(|p| match &p.actor {
                                ChannelActor::Agent { id } => Some(id.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let goal = manifest
                    .as_ref()
                    .map(|m| m.goal.statement.clone())
                    .unwrap_or_default();
                out.push(serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "state": row.state,
                    "goal": goal,
                    "updated_at": row.updated_at,
                    "agents": agents,
                    "turns": events.len(),
                }));
            }
            Ok(serde_json::Value::Array(out))
        }
        "events" => {
            let id = channel_id.ok_or_else(|| anyhow::anyhow!("events query needs channel_id"))?;
            let evs: Vec<_> = svc
                .load_events(&id)?
                .into_iter()
                .filter(|e| since_seq.is_none_or(|s| e.seq >= s))
                .collect();
            Ok(serde_json::to_value(evs)?)
        }
        other => anyhow::bail!("unknown channel query op `{other}`"),
    }
}

/// Persist one mobile user→agent exchange into the agent's channel (resolved
/// once), so phone conversations are durable and shared with the Hub/CLI.
/// Best-effort: failures are logged, never surfaced to the phone. Mirrors the
/// Hub's `chat::persist_exchange`. The channel is created on the first exchange.
pub fn persist_mobile_exchange(
    home: &std::path::Path,
    agent: &str,
    user_text: &str,
    agent_text: &str,
) {
    persist_mobile_exchange_into(home, agent, None, user_text, agent_text)
}

/// Like [`persist_mobile_exchange`] but lands the turn in an EXPLICIT
/// `channel_id` when given (v4c: the phone "drops into" a specific channel — a
/// Hub/CLI-originated one, or a non-latest one), else the agent's latest/new
/// channel. Chat turns stay unsigned `append_message` (mobile chat is not a
/// gate-authority path; the signed path is HITL respond, see `respond_hitl`).
pub fn persist_mobile_exchange_into(
    home: &std::path::Path,
    agent: &str,
    channel_id: Option<&str>,
    user_text: &str,
    agent_text: &str,
) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = match channel_id {
            Some(id) => id.to_string(),
            None => match svc.latest_for_agent(agent)? {
                Some(id) => id,
                None => svc.create_for_agent(agent)?.id,
            },
        };
        svc.append_message(
            &id,
            ChannelActor::local_human(),
            EventKind::Message,
            user_text,
            None,
        )?;
        svc.append_message(
            &id,
            ChannelActor::Agent {
                id: agent.to_string(),
            },
            EventKind::Message,
            agent_text,
            None,
        )?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("mobile channel persist failed for {agent}: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_query_list_and_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(tmp.path(), "mur", "hi", "hello");
        // list → one summary with the agent + a turn count.
        let list = channel_query(tmp.path(), "list", None, None).unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["agents"][0], "mur");
        assert!(arr[0]["turns"].as_u64().unwrap() >= 2);
        let cid = arr[0]["id"].as_str().unwrap().to_string();
        // events → the two messages; since_seq filters.
        let evs = channel_query(tmp.path(), "events", Some(cid.clone()), None).unwrap();
        assert_eq!(evs.as_array().unwrap().len(), 2);
        let evs1 = channel_query(tmp.path(), "events", Some(cid), Some(1)).unwrap();
        assert_eq!(evs1.as_array().unwrap().len(), 1);
    }

    #[test]
    fn persist_mobile_exchange_writes_both_turns_to_one_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(
            tmp.path(),
            "mur",
            "what's my schedule?",
            "you have 2 meetings",
        );
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let id = svc
            .latest_for_agent("mur")
            .unwrap()
            .expect("channel created");
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "what's my schedule?");
        assert_eq!(evs[1].payload["text"], "you have 2 meetings");
        // Second exchange appends to the SAME channel (shared, like the Hub).
        persist_mobile_exchange(tmp.path(), "mur", "and tomorrow?", "3 meetings");
        assert_eq!(svc.list(10).unwrap().len(), 1);
        assert_eq!(svc.load_events(&id).unwrap().len(), 4);
    }

    #[test]
    fn persist_into_explicit_channel_targets_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let a = svc.create_for_agent("mur").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let _b = svc.create_for_agent("mur").unwrap(); // newer = latest_for_agent
        // An explicit channel_id lands the turn in `a`, NOT the newer `_b`.
        persist_mobile_exchange_into(tmp.path(), "mur", Some(&a.id), "q", "ans");
        assert_eq!(
            svc.load_events(&a.id).unwrap().len(),
            2,
            "explicit id targeted"
        );
        assert_eq!(
            svc.load_events(&_b.id).unwrap().len(),
            0,
            "newer channel untouched"
        );
        // None falls back to the latest (`_b`).
        persist_mobile_exchange_into(tmp.path(), "mur", None, "q2", "ans2");
        assert_eq!(svc.load_events(&_b.id).unwrap().len(), 2, "None → latest");
    }
}
