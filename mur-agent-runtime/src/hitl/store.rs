//! Gate B's memory: settled chat-gate decisions, one remembered channel per
//! agent. `HitlResponse` events only, signed by the agent's own identity.
//! Lookup is newest-wins inside `mur_common::hitl::APPROVAL_TTL_SECS`, and an
//! event the agent's pubkey cannot verify is skipped, never trusted. Age and
//! order come from the signed `issued_at`, never the store's unsigned `ts` or
//! line order (#1764 option C); a decision without one is skipped.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::agent::HITL_CHANNEL_FILE;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::{HitlResponse, check_fresh};
use mur_common::identity::AgentIdentity;

/// The `step_or_call_id` slot of every chat-gate hash. A per-call id would
/// make every hash unique and the memory useless; a constant makes "the same
/// tool with the same input" the unit the user actually decided about.
pub const CHAT_GATE_STEP: &str = "chat";

/// Canonical hash of a chat tool call. Same pin as gate A, with the channel
/// slot empty (chat turns are not bound to a channel) and the step slot fixed.
pub fn chat_action_hash(tool_name: &str, input: &serde_json::Value, agent: &str) -> String {
    mur_common::hitl::pin::action_hash(tool_name, input, "", CHAT_GATE_STEP, agent)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    Allow,
    Deny,
}

#[async_trait::async_trait]
pub trait DecisionStore: Send + Sync {
    /// Newest verified `HitlResponse` for `action_hash` inside the TTL, if any.
    async fn lookup(&self, action_hash: &str) -> Option<Settled>;
    /// Append a signed `HitlResponse`. Best-effort: an error is logged, never returned.
    async fn record(&self, resp: HitlResponse);
}

pub struct ChannelDecisionStore {
    mur_home: PathBuf,
    agent: String,
    identity: Arc<AgentIdentity>,
    key_version: u32,
}

impl ChannelDecisionStore {
    pub fn new(
        mur_home: PathBuf,
        agent: String,
        identity: Arc<AgentIdentity>,
        key_version: u32,
    ) -> Self {
        Self {
            mur_home,
            agent,
            identity,
            key_version,
        }
    }

    /// The channel decisions live in — created once, then remembered in the
    /// agent's home. Mirrors `scheduler::schedule_channel`: a marker naming a
    /// channel that no longer loads is replaced, not treated as an error.
    fn channel_id(&self, svc: &ChannelService) -> Result<String> {
        decision_channel(svc, &self.mur_home, &self.agent)
    }

    fn scan(&self, action_hash: &str) -> Result<Option<Settled>> {
        let svc = ChannelService::open(&self.mur_home)?;
        let channel_id = self.channel_id(&svc)?;
        let events = svc.load_events(&channel_id)?;
        drop(svc);
        let pubkey = self.identity.verifying_key_bytes();
        let now = chrono::Utc::now();
        // (signed decision time, decision); newest signed time wins, a deny
        // wins a tie.
        let mut settled: Option<(chrono::DateTime<chrono::Utc>, Settled)> = None;
        for e in &events {
            if e.kind != EventKind::HitlResponse {
                continue;
            }
            let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
                continue;
            };
            if r.action_hash != action_hash {
                continue;
            }
            // require_sig = true: this store only ever writes signed events,
            // so an unsigned one is not ours.
            if !mur_channel::sign::verify_one(&channel_id, e, &pubkey, true) {
                continue;
            }
            let Some(at) = r.issued_at else { continue };
            if check_fresh(at, now).is_err() {
                continue;
            }
            let this = if r.allow {
                Settled::Allow
            } else {
                Settled::Deny
            };
            let newer = settled.as_ref().is_none_or(|(t, d)| {
                at > *t || (at == *t && *d == Settled::Allow && this == Settled::Deny)
            });
            if newer {
                settled = Some((at, this));
            }
        }
        Ok(settled.map(|(_, d)| d))
    }

    fn append(&self, resp: &HitlResponse) -> Result<()> {
        let svc = ChannelService::open(&self.mur_home)?;
        let channel_id = self.channel_id(&svc)?;
        svc.append_signed(
            &channel_id,
            &self.identity,
            self.key_version,
            ChannelActor::Agent {
                id: self.agent.clone(),
            },
            EventKind::HitlResponse,
            serde_json::to_value(resp)?,
            Some(format!("chat-hitl:{}", resp.hitl_id)),
        )?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl DecisionStore for ChannelDecisionStore {
    async fn lookup(&self, action_hash: &str) -> Option<Settled> {
        match self.scan(action_hash) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "chat-gate decision lookup failed; asking");
                None
            }
        }
    }

    async fn record(&self, resp: HitlResponse) {
        if let Err(e) = self.append(&resp) {
            tracing::warn!(error = %e, hitl_id = %resp.hitl_id, "chat-gate decision not recorded");
        }
    }
}

fn decision_channel(svc: &ChannelService, mur_home: &Path, agent: &str) -> Result<String> {
    let marker = mur_home.join("agents").join(agent).join(HITL_CHANNEL_FILE);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        let id = raw.trim();
        if !id.is_empty() && svc.exists(id) {
            return Ok(id.to_string());
        }
    }
    let ch = svc.create_for_agent(agent)?;
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&marker, &ch.id).context("record the hitl channel id")?;
    Ok(ch.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(home: &Path) -> ChannelDecisionStore {
        ChannelDecisionStore::new(
            home.to_path_buf(),
            "qa".into(),
            Arc::new(AgentIdentity::generate()),
            0,
        )
    }

    fn resp(hash: &str, allow: bool, id: &str) -> HitlResponse {
        HitlResponse {
            hitl_id: id.into(),
            action_hash: hash.into(),
            allow,
            reason: String::new(),
            surface: "hub".into(),
            issued_at: Some(chrono::Utc::now()),
        }
    }

    fn marker_id(home: &Path) -> String {
        std::fs::read_to_string(home.join("agents/qa").join(HITL_CHANNEL_FILE)).unwrap()
    }

    #[tokio::test]
    async fn unknown_hash_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(store(tmp.path()).lookup("nope").await, None);
    }

    #[tokio::test]
    async fn recorded_allow_is_found_and_newest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(resp("h1", true, "a")).await;
        assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
        s.record(resp("h1", false, "b")).await;
        assert_eq!(
            s.lookup("h1").await,
            Some(Settled::Deny),
            "newest decision counts"
        );
    }

    #[tokio::test]
    async fn marker_survives_and_channel_is_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(resp("h1", true, "a")).await;
        let id = marker_id(tmp.path());
        s.record(resp("h2", true, "b")).await;
        assert_eq!(marker_id(tmp.path()), id);
    }

    #[tokio::test]
    async fn expired_decision_is_not_settled() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut old = resp("h1", true, "a");
        old.issued_at = Some(chrono::Utc::now() - chrono::Duration::days(8));
        s.record(old).await;
        assert_eq!(s.lookup("h1").await, None);
    }

    /// Set every event's unsigned `ts` on disk to `to`; signatures still
    /// verify afterwards.
    fn rewrite_ts(home: &Path, to: chrono::DateTime<chrono::Utc>) {
        let svc = ChannelService::open(home).unwrap();
        let path = svc.store().events_path(marker_id(home).trim());
        let raw = std::fs::read_to_string(&path).unwrap();
        let rewritten: Vec<String> = raw
            .lines()
            .map(|l| {
                let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["ts"] = serde_json::Value::String(to.to_rfc3339());
                v.to_string()
            })
            .collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();
    }

    /// `ts` is outside the signature, so it decides nothing: an old decision
    /// with a fresh `ts` stays expired, a fresh one with an old `ts` stands.
    #[tokio::test]
    async fn rewriting_ts_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut old = resp("h-old", true, "a");
        old.issued_at = Some(chrono::Utc::now() - chrono::Duration::days(8));
        s.record(old).await;
        s.record(resp("h-new", true, "b")).await;
        rewrite_ts(tmp.path(), chrono::Utc::now());
        assert_eq!(s.lookup("h-old").await, None, "fresh ts, stale decision");
        rewrite_ts(tmp.path(), chrono::Utc::now() - chrono::Duration::days(30));
        assert_eq!(
            s.lookup("h-new").await,
            Some(Settled::Allow),
            "old ts, fresh decision"
        );
    }

    /// Newest by signed time, not by line: an allow appended after a newer
    /// deny does not outrank it.
    #[tokio::test]
    async fn newest_signed_time_wins_over_line_order() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(resp("h1", false, "deny")).await;
        let mut older = resp("h1", true, "allow");
        older.issued_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        s.record(older).await;
        assert_eq!(s.lookup("h1").await, Some(Settled::Deny));
    }

    /// Two decisions for `h1`, recorded in the given line order; returns
    /// what the store settles on.
    async fn settle(lines: [(bool, chrono::DateTime<chrono::Utc>); 2]) -> Option<Settled> {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        for (i, (allow, at)) in lines.into_iter().enumerate() {
            let mut r = resp("h1", allow, &format!("r{i}"));
            r.issued_at = Some(at);
            s.record(r).await;
        }
        s.lookup("h1").await
    }

    /// Newest signed decision wins whichever line it is on, for an allow as
    /// well as a deny — not "first seen", not "last seen", not "deny always".
    #[tokio::test]
    async fn newest_signed_decision_wins_in_either_line_order() {
        let older = chrono::Utc::now() - chrono::Duration::hours(2);
        let newer = chrono::Utc::now() - chrono::Duration::hours(1);
        assert_eq!(
            settle([(false, newer), (true, older)]).await,
            Some(Settled::Deny)
        );
        assert_eq!(
            settle([(true, older), (false, newer)]).await,
            Some(Settled::Deny)
        );
        assert_eq!(
            settle([(true, newer), (false, older)]).await,
            Some(Settled::Allow)
        );
        assert_eq!(
            settle([(false, older), (true, newer)]).await,
            Some(Settled::Allow)
        );
    }

    /// Equal signed `issued_at` — one value written into both, not two clock
    /// reads — and a deny wins, whichever line it is on.
    #[tokio::test]
    async fn a_deny_wins_a_tie_on_signed_time() {
        let same = chrono::Utc::now() - chrono::Duration::hours(1);
        assert_eq!(
            settle([(true, same), (false, same)]).await,
            Some(Settled::Deny)
        );
        assert_eq!(
            settle([(false, same), (true, same)]).await,
            Some(Settled::Deny)
        );
    }

    #[tokio::test]
    async fn a_decision_without_issued_at_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut legacy = resp("h1", true, "a");
        legacy.issued_at = None;
        s.record(legacy).await;
        assert_eq!(s.lookup("h1").await, None);
    }

    #[tokio::test]
    async fn unsigned_event_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let svc = ChannelService::open(tmp.path()).unwrap();
        let id = decision_channel(&svc, tmp.path(), "qa").unwrap();
        svc.append(
            &id,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::HitlResponse,
            serde_json::to_value(resp("h1", true, "forged")).unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(s.lookup("h1").await, None);
    }

    #[test]
    fn chat_hash_ignores_call_id_and_key_order() {
        let a = chat_action_hash("bash", &serde_json::json!({"b": 1, "a": 2}), "qa");
        let b = chat_action_hash("bash", &serde_json::json!({"a": 2, "b": 1}), "qa");
        assert_eq!(a, b);
        assert_ne!(
            a,
            chat_action_hash("bash", &serde_json::json!({"a": 3}), "qa")
        );
        assert_ne!(
            a,
            chat_action_hash("bash", &serde_json::json!({"b": 1, "a": 2}), "pm")
        );
    }
}
