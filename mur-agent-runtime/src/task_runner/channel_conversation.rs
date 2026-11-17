//! Channel-keyed conversation memory (at-mention design §3.4).
//!
//! A second store beside [`super::conversation::ConversationStore`]: a turn
//! that names `context.channel_id` keys its history by the channel, and the
//! channel log — not this cache — is the source of truth. The cache keeps
//! what the log cannot: the turn ledger (never persisted to the channel) and
//! the reply this runtime produced before murmur persisted it (the
//! *provisional tail*). Every entry carries the `seq` of the event it came
//! from; `seq == None` is provisional.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mur_channel::conversation::{Assessment, CacheState, Role};
use serde::{Deserialize, Serialize};

use super::conversation::{conversation_file_stem, drop_oldest_turn, estimated_tokens, turn_count};
use super::{DEFAULT_CONV_BUDGET_TOKENS, MAX_CHANNEL_CONVERSATIONS, MAX_CONV_MESSAGES};
use crate::llm::RichMessage;

/// Subdirectory of the runtime's conversation dir holding channel histories.
const CHANNELS_SUBDIR: &str = "channels";

/// Tracing target of the Phase 1 conversation audit (§6). The runtime never
/// writes to the channel, so the audit is the runtime log; a channel-signed
/// audit is Phase 2.
pub(crate) const AUDIT: &str = "mur_agent_runtime::audit";

/// One stored message of a channel conversation. `seq == None` is a
/// provisional entry (the reply the runtime produced and murmur has not yet
/// persisted), together with its ledger.
///
/// Not `PartialEq`: [`RichMessage`] does not implement it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub seq: Option<u64>,
    /// `Text{role,content}` or `TurnLedger`.
    pub message: RichMessage,
    /// Reply text BEFORE the settlement card was appended (revision 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_settlement: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ChannelHistory {
    /// Highest `seq` this cache folded; `None` = nothing folded yet.
    pub watermark: Option<u64>,
    pub entries: Vec<Entry>,
}

fn text(role: &str, content: &str) -> RichMessage {
    RichMessage::Text {
        role: role.into(),
        content: content.into(),
    }
}

fn is_agent_text(m: &RichMessage) -> bool {
    matches!(m, RichMessage::Text { role, .. } if role == "agent")
}

/// Same shape as `ConversationStore`, keyed by channel id, under
/// `<conversation_dir>/channels`, with its own cap and sweep.
pub(crate) struct ChannelConversationStore {
    pub(crate) map: HashMap<String, ChannelHistory>,
    /// Insertion order for LRU eviction past [`MAX_CHANNEL_CONVERSATIONS`].
    order: VecDeque<String>,
    /// `None` keeps the store in memory only.
    dir: Option<PathBuf>,
    /// Estimated-token ceiling for one conversation's stored history.
    budget_tokens: u64,
    /// Files left by earlier processes are swept once, on first write.
    swept: bool,
    /// Retract targets the last `reconcile` refused (§4.3), for the caller
    /// to record in the log; see [`Self::take_refused`].
    refused: Vec<u64>,
}

impl Default for ChannelConversationStore {
    fn default() -> Self {
        Self::new(None, DEFAULT_CONV_BUDGET_TOKENS)
    }
}

impl ChannelConversationStore {
    /// `conversation_dir` is the runtime's conversation dir (the one the
    /// task-id store uses); channel files go in its `channels` subdirectory.
    pub(crate) fn new(conversation_dir: Option<&Path>, budget_tokens: u64) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            dir: conversation_dir.map(|d| d.join(CHANNELS_SUBDIR)),
            budget_tokens: budget_tokens.max(1),
            swept: false,
            refused: Vec::new(),
        }
    }

    /// Resize the per-conversation token ceiling (the runner's context window
    /// became known after the store was built).
    pub(crate) fn set_budget_tokens(&mut self, budget_tokens: u64) {
        self.budget_tokens = budget_tokens.max(1);
    }

    /// Retract targets the last [`Self::reconcile`] refused because their
    /// turn ran tools. The caller records each in the log so history readers
    /// keep showing what the model still sees.
    pub(crate) fn take_refused(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.refused)
    }

    pub(crate) fn cache_state(&self, channel_id: &str) -> CacheState {
        let watermark = match self.map.get(channel_id) {
            Some(h) => h.watermark,
            None => self.load(channel_id).and_then(|h| h.watermark),
        };
        CacheState { watermark }
    }

    /// Apply an assessment: discard on rebuild, else fold new events and
    /// reconcile the provisional tail. Returns the history to send (text
    /// messages + confirmed ledgers, only `seq < channel_seq`).
    pub(crate) fn reconcile(
        &mut self,
        channel_id: &str,
        a: &Assessment,
        channel_seq: u64,
    ) -> Vec<RichMessage> {
        let mut h = self.take(channel_id);
        if let Some(old_watermark) = a.rebuilt_after_repair {
            tracing::info!(
                target: AUDIT,
                channel_id,
                old_watermark,
                "rebuilt after repair (old watermark {old_watermark})"
            );
            h = ChannelHistory::default();
        }
        for seq in &a.ignored_retracts {
            tracing::info!(target: AUDIT, channel_id, seq, "retract ignored: withdraws nothing");
        }
        self.refused = apply_retracts(channel_id, &mut h.entries, &a.retracted);

        let (mut entries, provisional): (Vec<Entry>, Vec<Entry>) =
            h.entries.into_iter().partition(|e| e.seq.is_some());
        let mut provisional = VecDeque::from(provisional);
        let known: HashSet<u64> = entries.iter().filter_map(|e| e.seq).collect();

        for p in a
            .projected
            .iter()
            .filter(|p| h.watermark.is_none_or(|w| p.seq > w) && !known.contains(&p.seq))
        {
            match p.role {
                Role::User => entries.push(Entry {
                    seq: Some(p.seq),
                    message: text("user", &p.text),
                    pre_settlement: None,
                }),
                Role::Agent => {
                    // One persisted agent reply per turn: it confirms or
                    // replaces the oldest provisional reply.
                    let reply = pop_provisional_reply(&mut provisional);
                    match reply {
                        Some((mut reply, ledger))
                            if reply.pre_settlement.as_deref() == Some(p.text.as_str()) =>
                        {
                            reply.seq = Some(p.seq);
                            entries.push(reply);
                            if let Some(mut l) = ledger {
                                l.seq = Some(p.seq);
                                entries.push(l);
                            }
                        }
                        other => {
                            if other.is_some() {
                                tracing::info!(
                                    target: AUDIT,
                                    channel_id,
                                    seq = p.seq,
                                    "provisional reply dropped: differs from the log"
                                );
                            }
                            entries.push(Entry {
                                seq: Some(p.seq),
                                message: text("agent", &p.text),
                                pre_settlement: None,
                            });
                        }
                    }
                }
            }
        }
        if !provisional.is_empty() {
            tracing::info!(
                target: AUDIT,
                channel_id,
                "provisional reply dropped: not confirmed by the log"
            );
        }
        // Stable: a ledger keeps its place right after the reply sharing its seq.
        entries.sort_by_key(|e| e.seq);

        // Everything below `channel_seq` is now folded (the projection holds
        // all of it). Events in `channel_seq..=log_last_seq` are not in the
        // projection, so the watermark must not claim them: they are folded
        // next turn.
        let folded = channel_seq.checked_sub(1);
        let watermark = h.watermark.max(folded).map(|w| w.min(a.log_last_seq));
        let out = entries
            .iter()
            .filter(|e| e.seq.is_some_and(|s| s < channel_seq))
            .map(|e| e.message.clone())
            .collect();
        self.put(channel_id, ChannelHistory { watermark, entries });
        out
    }

    /// After the turn: push user (with seq = channel_seq) + provisional reply
    /// + ledger, trim to budget, persist.
    pub(crate) fn remember_turn(
        &mut self,
        channel_id: &str,
        channel_seq: u64,
        user: &str,
        reply_pre_settlement: &str,
        ledger: RichMessage,
    ) {
        let mut h = self.take(channel_id);
        // A retry of the same seq replaces that turn instead of repeating it.
        h.entries.retain(|e| e.seq != Some(channel_seq));
        h.entries.push(Entry {
            seq: Some(channel_seq),
            message: text("user", user),
            pre_settlement: None,
        });
        h.entries.push(Entry {
            seq: None,
            message: text("agent", reply_pre_settlement),
            pre_settlement: Some(reply_pre_settlement.to_string()),
        });
        h.entries.push(Entry {
            seq: None,
            message: ledger,
            pre_settlement: None,
        });
        h.watermark = h.watermark.max(Some(channel_seq));
        self.trim(&mut h.entries);
        self.put(channel_id, h);
    }

    /// Same rules as `ConversationStore::remember`: drop whole turns from the
    /// front until the token budget and message ceiling hold; the newest turn
    /// is always kept.
    fn trim(&self, entries: &mut Vec<Entry>) {
        let mut msgs: Vec<RichMessage> = entries.iter().map(|e| e.message.clone()).collect();
        while turn_count(&msgs) > 1
            && (estimated_tokens(&msgs) > self.budget_tokens || msgs.len() > MAX_CONV_MESSAGES)
        {
            let before = msgs.len();
            drop_oldest_turn(&mut msgs);
            entries.drain(0..before - msgs.len());
        }
    }

    /// Remove and return `channel_id`'s history (memory, else disk, else empty).
    fn take(&mut self, channel_id: &str) -> ChannelHistory {
        match self.map.remove(channel_id) {
            Some(h) => h,
            None => self.load(channel_id).unwrap_or_default(),
        }
    }

    /// Store `h` in memory and on disk, evicting the oldest past the cap.
    fn put(&mut self, channel_id: &str, h: ChannelHistory) {
        self.sweep_stale_files();
        self.persist(channel_id, &h);
        self.map.insert(channel_id.to_string(), h);
        if !self.order.iter().any(|k| k == channel_id) {
            self.order.push_back(channel_id.to_string());
        }
        while self.order.len() > MAX_CHANNEL_CONVERSATIONS {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
                if let Some(path) = self.path_for(&old) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    fn path_for(&self, channel_id: &str) -> Option<PathBuf> {
        let stem = conversation_file_stem(channel_id)?;
        Some(self.dir.as_ref()?.join(format!("{stem}.json")))
    }

    fn load(&self, channel_id: &str) -> Option<ChannelHistory> {
        let bytes = std::fs::read(self.path_for(channel_id)?).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(h) => Some(h),
            // The log is the source of truth: an unreadable cache is a miss.
            Err(e) => {
                tracing::warn!(channel_id, error = %e, "unreadable channel conversation file; ignoring");
                None
            }
        }
    }

    /// Atomic (temp + rename) and best-effort: persistence never fails a turn.
    fn persist(&self, channel_id: &str, h: &ChannelHistory) {
        let Some(path) = self.path_for(channel_id) else {
            return;
        };
        let Some(dir) = path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, "cannot create channel conversation dir; memory only");
            return;
        }
        let Ok(json) = serde_json::to_vec(h) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Keep the newest [`MAX_CHANNEL_CONVERSATIONS`] files left by earlier
    /// processes; the in-memory LRU starts empty on every boot.
    fn sweep_stale_files(&mut self) {
        if std::mem::replace(&mut self.swept, true) {
            return;
        }
        let Some(Ok(read)) = self.dir.as_ref().map(std::fs::read_dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = read
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() <= MAX_CHANNEL_CONVERSATIONS {
            return;
        }
        files.sort_by_key(|(m, _)| *m);
        let excess = files.len() - MAX_CHANNEL_CONVERSATIONS;
        for (_, path) in files.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Retract spec §4.3. For each withdrawn seq the cache still holds, the turn
/// is the user entry plus the tail `remember_turn` pushed after it (the
/// provisional reply and ledger, or a ledger an earlier refusal pinned to the
/// same seq). A turn whose ledger records no tool call is dropped whole. One
/// that ran tools is refused: the user entry stays and its ledger is pinned
/// to the user's seq, so the model keeps knowing what ran; the unconfirmed
/// `turn ended:` reply is dropped as usual. Returns the refused seqs.
fn apply_retracts(channel_id: &str, entries: &mut Vec<Entry>, retracted: &[u64]) -> Vec<u64> {
    let mut refused = Vec::new();
    for &t in retracted {
        let Some(i) = entries
            .iter()
            .position(|e| e.seq == Some(t) && is_user_text(&e.message))
        else {
            continue;
        };
        let end = entries[i + 1..]
            .iter()
            .position(|e| e.seq.is_some_and(|s| s != t))
            .map_or(entries.len(), |k| i + 1 + k);
        let ran_tools = entries[i + 1..end].iter().any(|e| {
            matches!(&e.message, RichMessage::TurnLedger { memory, .. } if !memory.tools.is_empty())
        });
        if !ran_tools {
            entries.drain(i..end);
            tracing::info!(target: AUDIT, channel_id, seq = t, "retract honoured");
            continue;
        }
        let mut tail: Vec<Entry> = entries.drain(i + 1..end).collect();
        if tail.iter().any(|e| e.seq.is_none()) {
            tracing::info!(target: AUDIT, channel_id, seq = t, "retract refused: turn {t} ran tools");
        }
        refused.push(t);
        tail.retain(|e| matches!(e.message, RichMessage::TurnLedger { .. }));
        for e in &mut tail {
            e.seq = Some(t);
        }
        entries.splice(i + 1..i + 1, tail);
    }
    refused
}

fn is_user_text(m: &RichMessage) -> bool {
    matches!(m, RichMessage::Text { role, .. } if role == "user")
}

/// Pop the oldest provisional agent reply and, when it directly follows, its
/// ledger. Provisional entries before it (none are written today) are dropped.
fn pop_provisional_reply(p: &mut VecDeque<Entry>) -> Option<(Entry, Option<Entry>)> {
    while let Some(e) = p.pop_front() {
        if is_agent_text(&e.message) {
            let ledger = p
                .front()
                .is_some_and(|l| matches!(l.message, RichMessage::TurnLedger { .. }))
                .then(|| p.pop_front())
                .flatten();
            return Some((e, ledger));
        }
    }
    None
}

/// A turn was refused because too many turns already wait on its
/// conversation (`CONVERSATION_BUSY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Busy;

struct Slot {
    sem: Arc<tokio::sync::Semaphore>,
    waiting: Arc<AtomicUsize>,
}

/// Per-conversation serialization (§3.4): `acquire` waits; over
/// `max_waiting` it returns `Err(Busy)` immediately.
pub(crate) struct ConversationLocks {
    slots: std::sync::Mutex<HashMap<String, Slot>>,
    max_waiting: usize,
}

/// Decrements the waiting count when the waiter stops waiting — acquired or
/// cancelled (the caller's future dropped).
struct WaitGuard(Arc<AtomicUsize>);

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ConversationLocks {
    pub(crate) fn new(max_waiting: usize) -> Self {
        Self {
            slots: std::sync::Mutex::new(HashMap::new()),
            max_waiting,
        }
    }

    pub(crate) async fn acquire(
        &self,
        channel_id: &str,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, Busy> {
        let (sem, waiting) = {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if !slots.contains_key(channel_id) && slots.len() >= MAX_CHANNEL_CONVERSATIONS {
                // Idle slots: nobody holds or waits on the semaphore.
                slots.retain(|_, s| Arc::strong_count(&s.sem) > 1);
            }
            let s = slots.entry(channel_id.to_string()).or_insert_with(|| Slot {
                sem: Arc::new(tokio::sync::Semaphore::new(1)),
                waiting: Arc::new(AtomicUsize::new(0)),
            });
            (Arc::clone(&s.sem), Arc::clone(&s.waiting))
        };
        if let Ok(p) = Arc::clone(&sem).try_acquire_owned() {
            return Ok(p);
        }
        if waiting.fetch_add(1, Ordering::AcqRel) >= self.max_waiting {
            waiting.fetch_sub(1, Ordering::AcqRel);
            return Err(Busy);
        }
        let _guard = WaitGuard(waiting);
        // The semaphore is never closed; a closed one would be a bug, and
        // refusing the turn is the safe answer.
        sem.acquire_owned().await.map_err(|_| Busy)
    }
}

/// What a runner needs to serve channel conversations: where channels live
/// (`mur_home`), the channel-keyed cache, and the per-conversation locks.
/// `None` on the runner = channel turns are not served (stub runners).
pub(crate) struct ChannelConversations {
    pub(crate) mur_home: PathBuf,
    pub(crate) store: std::sync::Mutex<ChannelConversationStore>,
    pub(crate) locks: ConversationLocks,
    /// Signs the refusal note (§4.3) as this agent; `None` = refusals are
    /// audit-only (stub runners, tests that do not exercise it).
    pub(crate) signer: Option<RefusalSigner>,
}

impl ChannelConversations {
    pub(crate) fn new(
        mur_home: PathBuf,
        conversation_dir: Option<&Path>,
        budget_tokens: u64,
        max_waiting: usize,
    ) -> Self {
        Self {
            mur_home,
            store: std::sync::Mutex::new(ChannelConversationStore::new(
                conversation_dir,
                budget_tokens,
            )),
            locks: ConversationLocks::new(max_waiting),
            signer: None,
        }
    }

    /// `agent`'s current channel writer key, from its home under `mur_home`
    /// (rotation chain head, else `identity.pub`).
    pub(crate) fn writer_pubkey(&self, agent: &str) -> Option<[u8; 32]> {
        mur_channel::sign::resolve_writer_pubkey(&self.mur_home.join("agents").join(agent), None)
    }
}

/// Who signs the refusal note: this agent's identity and key version.
pub(crate) type RefusalSigner = (Arc<mur_common::identity::AgentIdentity>, u32);

/// Record each refused retract in the log as a signed `Agent{agent}` `Note`
/// (retract spec §4.3), so replay and Hub keep the message the model still
/// receives. Keyed per target: a refusal seen again on a later turn appends
/// nothing. A failed append is audited, never fatal — the turn has its
/// history either way.
pub(crate) fn record_refusals(
    mur_home: &Path,
    signer: &RefusalSigner,
    agent: &str,
    channel_id: &str,
    refused: &[u64],
) {
    use mur_common::channel::{
        ChannelActor, EventKind, RetractRefusal, retract_refused_idempotency_key,
    };
    let svc = match mur_channel::ChannelService::open(mur_home) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: AUDIT, channel_id, "retract refusal not recorded: {e:#}");
            return;
        }
    };
    let (identity, kv) = signer;
    for &t in refused {
        if let Err(e) = svc.append_signed(
            channel_id,
            identity,
            *kv,
            ChannelActor::Agent { id: agent.into() },
            EventKind::Note,
            RetractRefusal { target_seq: t }.to_payload(),
            Some(retract_refused_idempotency_key(t)),
        ) {
            tracing::warn!(target: AUDIT, channel_id, seq = t, "retract refusal not recorded: {e:#}");
        }
    }
}
