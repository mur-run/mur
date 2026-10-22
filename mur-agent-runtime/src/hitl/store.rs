//! Gate B's memory: settled chat-gate decisions, one remembered channel per
//! agent. `HitlResponse` events only, signed by the agent's own identity.
//! Lookup is newest-wins inside `mur_common::hitl::APPROVAL_TTL_SECS`, and an
//! event the agent's pubkey cannot verify is skipped, never trusted. Age and
//! order come from the signed `issued_at`, never the store's unsigned `ts` or
//! line order (#1764 option C); a decision without one is skipped.
//! A signed revocation note cancels the decisions it covers that are not
//! newer than it (#1806); see [`super::remembered`] for the full fold.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::agent::HITL_CHANNEL_FILE;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::{HitlResponse, HitlRevocation, RevocationScope};
use mur_common::identity::AgentIdentity;

use super::integrity::{self, IntegrityError};
use super::remembered::{LiveDecision, live_decisions};

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

/// What `hitl/list` can report: the live decisions, or why there are none to
/// trust.
#[derive(Debug)]
pub enum Listing {
    Live(Vec<LiveDecision>),
    Unavailable { reason: String, channel_id: String },
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

    /// The live decisions — the same fold the gate and `hitl/list` read.
    fn live(&self) -> Result<std::collections::HashMap<String, LiveDecision>> {
        let svc = ChannelService::open(&self.mur_home)?;
        let channel_id = self.channel_id(&svc)?;
        let pubkey = self.identity.verifying_key_bytes();
        // Hold the append lock shared across check and read: a reader must not
        // see a line that is written but not sealed yet. A log whose integrity
        // cannot be established yields no decisions at all (#1859) — never the
        // older allow that survived.
        let _guard = integrity::read_lock(&self.mur_home, &channel_id);
        integrity::verify(&self.mur_home, &self.agent, &channel_id, &pubkey)?;
        let events = svc.load_events(&channel_id)?;
        drop(svc);
        Ok(live_decisions(
            &channel_id,
            &events,
            &pubkey,
            chrono::Utc::now(),
        ))
    }

    fn scan(&self, action_hash: &str) -> Result<Option<Settled>> {
        Ok(self.live()?.get(action_hash).map(LiveDecision::settled))
    }

    /// Live decisions, newest first.
    pub fn list(&self) -> Result<Vec<LiveDecision>> {
        let mut v: Vec<_> = self.live()?.into_values().collect();
        v.sort_by(|a, b| {
            b.issued_at
                .cmp(&a.issued_at)
                .then(a.action_hash.cmp(&b.action_hash))
        });
        Ok(v)
    }

    /// Like [`Self::list`], but an untrusted log is reported as
    /// [`Listing::Unavailable`] (reason + channel id) rather than an error, so
    /// `hitl/list` can say why the memory is empty and what to run. The gate
    /// still sees no decisions from such a log; only the reporting differs.
    pub fn list_report(&self) -> Result<Listing> {
        match self.list() {
            Ok(decisions) => Ok(Listing::Live(decisions)),
            Err(e) => match e.downcast_ref::<IntegrityError>() {
                // An unreadable log/head is an internal error, not a verdict.
                Some(ie) if !ie.is_io() => {
                    let svc = ChannelService::open(&self.mur_home)?;
                    Ok(Listing::Unavailable {
                        reason: ie.reason.clone(),
                        channel_id: self.channel_id(&svc)?,
                    })
                }
                _ => Err(e),
            },
        }
    }

    /// Revoke live decisions by appending ONE signed revocation; no existing
    /// line is touched (#1770). `target` = `None` revokes all, `Some(prefix)`
    /// the single live decision whose hash starts with it. Returns the
    /// hashes revoked. A prefix matching none, or more than one, is an error
    /// and writes nothing — a guess here would forget the wrong decision.
    pub fn forget(&self, target: Option<&str>) -> Result<Vec<String>> {
        let live = match self.list() {
            Ok(l) => l,
            // Nothing is remembered while the log is untrusted, so there is
            // nothing to revoke; say so rather than sealing over the damage.
            Err(e) if e.downcast_ref::<IntegrityError>().is_some() => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let (scope, hashes) = match target {
            None => (
                RevocationScope::All,
                live.into_iter().map(|d| d.action_hash).collect::<Vec<_>>(),
            ),
            Some(prefix) => {
                let prefix = prefix.trim();
                anyhow::ensure!(!prefix.is_empty(), "empty hash prefix");
                let hits: Vec<_> = live
                    .into_iter()
                    .filter(|d| d.action_hash.starts_with(prefix))
                    .map(|d| d.action_hash)
                    .collect();
                match hits.as_slice() {
                    [one] => (RevocationScope::One(one.clone()), hits),
                    [] => anyhow::bail!("no remembered decision matches '{prefix}'"),
                    _ => anyhow::bail!(
                        "'{prefix}' matches {} remembered decisions; use a longer prefix",
                        hits.len()
                    ),
                }
            }
        };
        let now = chrono::Utc::now();
        self.append_revocations(vec![(scope, now)], hashes)
    }

    /// Revoke only the live DENIES, leaving remembered allows replaying.
    /// One signed revocation per denied hash (`One` scopes), so the wire
    /// format and older readers are unchanged. Returns the hashes revoked.
    pub fn forget_denies(&self) -> Result<Vec<String>> {
        let live = match self.list() {
            Ok(l) => l,
            Err(e) if e.downcast_ref::<IntegrityError>().is_some() => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        self.revoke_denies_in(live)
    }

    /// Revoke the denies in a `list()` snapshot. Split out so a test can land
    /// a decision between the snapshot and the revocation.
    fn revoke_denies_in(&self, live: Vec<LiveDecision>) -> Result<Vec<String>> {
        // Each revocation is stamped with the issue time of the deny it saw,
        // not "now": a revocation covers every decision at or before its
        // stamp, so a stamp of "now" would also revoke an allow recorded for
        // the same hash after this snapshot was read.
        let denied: Vec<_> = live.into_iter().filter(|d| !d.allow).collect();
        let hashes: Vec<String> = denied.iter().map(|d| d.action_hash.clone()).collect();
        let scopes = denied
            .into_iter()
            .map(|d| (RevocationScope::One(d.action_hash), d.issued_at))
            .collect();
        self.append_revocations(scopes, hashes)
    }

    /// Append one signed revocation per scope; nothing is touched on an empty
    /// set. A failure part-way leaves the earlier ones in force — forgetting
    /// is idempotent, so the caller can simply run it again.
    fn append_revocations(
        &self,
        scopes: Vec<(RevocationScope, chrono::DateTime<chrono::Utc>)>,
        hashes: Vec<String>,
    ) -> Result<Vec<String>> {
        if hashes.is_empty() {
            return Ok(hashes);
        }
        let svc = self.sealing_service()?;
        let channel_id = self.writable_channel(&svc)?;
        for (scope, issued_at) in scopes {
            let rev = HitlRevocation { scope, issued_at };
            svc.append_signed(
                &channel_id,
                &self.identity,
                self.key_version,
                ChannelActor::Agent {
                    id: self.agent.clone(),
                },
                EventKind::Note,
                rev.to_payload(),
                Some(format!("chat-hitl-revoke:{}", uuid::Uuid::now_v7())),
            )?;
        }
        Ok(hashes)
    }

    /// A service whose every append seals a fresh head (#1859).
    fn sealing_service(&self) -> Result<ChannelService> {
        Ok(
            ChannelService::open(&self.mur_home)?.with_seal(integrity::seal_hook(
                self.mur_home.clone(),
                self.agent.clone(),
                self.identity.clone(),
                self.key_version,
            )),
        )
    }

    /// The channel to append to. If the current one fails its integrity
    /// check, start a fresh one: sealing over a log with a hole would make the
    /// surviving older allow live again. The damaged channel is left on disk
    /// as evidence and never read again; the cost is that the agent asks
    /// again for everything it had remembered.
    fn writable_channel(&self, svc: &ChannelService) -> Result<String> {
        let channel_id = self.channel_id(svc)?;
        let pubkey = self.identity.verifying_key_bytes();
        let verdict = {
            let _guard = integrity::read_lock(&self.mur_home, &channel_id);
            integrity::verify(&self.mur_home, &self.agent, &channel_id, &pubkey)
        };
        let Err(e) = verdict else {
            return Ok(channel_id);
        };
        tracing::warn!(
            error = %e,
            damaged_channel = %channel_id,
            "chat-gate decision log failed its integrity check; starting a new one"
        );
        let fresh = svc.create_for_agent(&self.agent)?;
        let marker = self
            .mur_home
            .join("agents")
            .join(&self.agent)
            .join(HITL_CHANNEL_FILE);
        std::fs::write(&marker, &fresh.id).context("record the replacement hitl channel id")?;
        Ok(fresh.id)
    }

    fn append(&self, resp: &HitlResponse) -> Result<()> {
        let svc = self.sealing_service()?;
        let channel_id = self.writable_channel(&svc)?;
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
            label: None,
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
        let path = svc.store().events_path(marker_id(home).trim()).unwrap();
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
        // Editing a line is a deletion as far as the head is concerned, so
        // re-seal after each edit: this test is about which clock the fold
        // reads, not about tamper detection (store_integrity_tests).
        let reseal = |s: &ChannelDecisionStore| {
            super::super::integrity::reseal_for_test(
                tmp.path(),
                "qa",
                marker_id(tmp.path()).trim(),
                &s.identity,
            )
        };
        rewrite_ts(tmp.path(), chrono::Utc::now());
        reseal(&s);
        assert_eq!(s.lookup("h-old").await, None, "fresh ts, stale decision");
        rewrite_ts(tmp.path(), chrono::Utc::now() - chrono::Duration::days(30));
        reseal(&s);
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

    fn line_count(home: &Path) -> Vec<String> {
        let svc = ChannelService::open(home).unwrap();
        let path = svc.store().events_path(marker_id(home).trim()).unwrap();
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn at(r: HitlResponse, secs_ago: i64) -> HitlResponse {
        HitlResponse {
            issued_at: Some(chrono::Utc::now() - chrono::Duration::seconds(secs_ago)),
            ..r
        }
    }

    #[tokio::test]
    async fn a_revoked_deny_is_no_longer_replayed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("h1", false, "a"), 5)).await;
        assert_eq!(s.forget(Some("h1")).unwrap(), vec!["h1".to_string()]);
        assert_eq!(s.lookup("h1").await, None, "the gate asks again");
    }

    #[tokio::test]
    async fn forgetting_one_leaves_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("aa1", false, "a"), 5)).await;
        s.record(at(resp("bb2", true, "b"), 5)).await;
        s.forget(Some("aa")).unwrap();
        assert_eq!(s.lookup("aa1").await, None);
        assert_eq!(s.lookup("bb2").await, Some(Settled::Allow));
    }

    #[tokio::test]
    async fn forget_all_then_a_newer_decision_is_honoured() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("h1", false, "a"), 5)).await;
        s.record(at(resp("h2", true, "b"), 5)).await;
        let mut gone = s.forget(None).unwrap();
        gone.sort();
        assert_eq!(gone, vec!["h1".to_string(), "h2".to_string()]);
        assert!(s.list().unwrap().is_empty());
        s.record(resp("h1", true, "c")).await;
        assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
    }

    #[tokio::test]
    async fn revocation_appends_and_rewrites_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("h1", false, "a"), 5)).await;
        s.record(at(resp("h2", false, "b"), 5)).await;
        let before = line_count(tmp.path());
        s.forget(None).unwrap();
        let after = line_count(tmp.path());
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(&after[..before.len()], &before[..], "old lines untouched");
    }

    #[tokio::test]
    async fn an_unverifiable_revocation_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("h1", false, "a"), 5)).await;
        // Same channel, a key that is not the agent's.
        let forger = ChannelDecisionStore::new(
            tmp.path().to_path_buf(),
            "qa".into(),
            Arc::new(AgentIdentity::generate()),
            0,
        );
        // The forger's view of the log is empty (nothing verifies), so build
        // the revocation by hand through its signing path.
        let svc = ChannelService::open(tmp.path()).unwrap();
        svc.append_signed(
            marker_id(tmp.path()).trim(),
            &forger.identity,
            0,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Note,
            HitlRevocation {
                scope: RevocationScope::All,
                issued_at: chrono::Utc::now(),
            }
            .to_payload(),
            None,
        )
        .unwrap();
        drop(svc);
        assert_eq!(s.lookup("h1").await, Some(Settled::Deny));
    }

    #[tokio::test]
    async fn ambiguous_or_unknown_prefix_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("ab1", false, "a"), 5)).await;
        s.record(at(resp("ab2", false, "b"), 5)).await;
        let before = line_count(tmp.path()).len();
        assert!(s.forget(Some("ab")).is_err());
        assert!(s.forget(Some("zz")).is_err());
        assert_eq!(line_count(tmp.path()).len(), before);
        assert_eq!(s.list().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_shows_only_live_decisions_with_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut old = resp("old", true, "a");
        old.issued_at = Some(chrono::Utc::now() - chrono::Duration::days(8));
        s.record(old).await;
        let mut labelled = at(resp("new", false, "b"), 5);
        labelled.label = Some("edit_file: Cargo.toml".into());
        s.record(labelled).await;
        s.record(at(resp("gone", true, "c"), 5)).await;
        s.forget(Some("gone")).unwrap();
        let live = s.list().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].action_hash, "new");
        assert_eq!(live[0].label.as_deref(), Some("edit_file: Cargo.toml"));
    }

    #[tokio::test]
    async fn forget_denies_keeps_allows_replaying() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("al1", true, "a"), 5)).await;
        s.record(at(resp("de1", false, "b"), 5)).await;
        s.record(at(resp("de2", false, "c"), 5)).await;
        let mut gone = s.forget_denies().unwrap();
        gone.sort();
        assert_eq!(gone, vec!["de1".to_string(), "de2".to_string()]);
        assert_eq!(s.lookup("al1").await, Some(Settled::Allow));
        assert_eq!(s.lookup("de1").await, None);
        assert_eq!(s.lookup("de2").await, None);
    }

    #[tokio::test]
    async fn forget_denies_spares_an_allow_recorded_after_the_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("h1", false, "a"), 10)).await;
        let snapshot = s.list().unwrap();
        // The operator allows the same action after the snapshot was read.
        s.record(at(resp("h1", true, "b"), 1)).await;
        assert_eq!(
            s.revoke_denies_in(snapshot).unwrap(),
            vec!["h1".to_string()]
        );
        assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
    }

    #[tokio::test]
    async fn forget_denies_with_none_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(at(resp("al1", true, "a"), 5)).await;
        let before = line_count(tmp.path()).len();
        assert!(s.forget_denies().unwrap().is_empty());
        assert_eq!(line_count(tmp.path()).len(), before);
        assert_eq!(s.lookup("al1").await, Some(Settled::Allow));
    }
}
