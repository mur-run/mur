use super::*;

/// Conversation keys come off the wire as `context.task_id`, so they reach the
/// filename path. Only these characters are allowed through; anything else
/// keeps the conversation in memory rather than naming a file.
pub(super) fn conversation_file_stem(key: &str) -> Option<&str> {
    let ok = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(key)
}

/// Estimated token cost of a stored history. Text and the rendered turn
/// ledger are counted; images are never stored (see
/// [`TaskRunner::remember_turn`]) and raw tool scaffolding is dropped before a
/// turn is remembered — the ledger is its compact form.
pub(super) fn estimated_tokens(history: &[crate::llm::RichMessage]) -> u64 {
    use crate::llm::RichMessage as M;
    let chars: usize = history
        .iter()
        .map(|m| match m {
            M::Text { content, .. } => content.len(),
            M::ImageText { text, .. } => text.len(),
            M::ToolUse { text, .. } => text.as_ref().map_or(0, String::len),
            M::ToolResults { results } => results.iter().map(|r| r.content.len()).sum(),
            M::TurnLedger { turn, memory } => {
                crate::turn_ledger::render_memory(*turn, memory).len()
            }
        })
        .sum();
    (chars / CHARS_PER_TOKEN_ESTIMATE) as u64
}

/// Fraction of the model's context window used by the most recent LLM call —
/// the input to the adaptive injection cutoff. Before the first call
/// `last_call_input_tokens` is 0, so the first turn is never cut.
pub(super) fn context_fill_ratio(
    last_call_input_tokens: u64,
    model_max_context_tokens: u64,
) -> f64 {
    if model_max_context_tokens == 0 {
        return 0.0;
    }
    (last_call_input_tokens as f64 / model_max_context_tokens as f64).clamp(0.0, 1.0)
}

/// Does this message open a turn — a user-authored `Text`/`ImageText`?
///
/// The pinned `<project_instructions>` message is a user `Text` too, so this
/// would count it as a turn. It is safe today because the block is never
/// stored (`remember` / `drop_oldest_turn` never see it) and send-time trim
/// ([`trim_for_send`]) runs on `prior` before the block is added. A future
/// trimmer of the *sent* list (e.g. near `sanitize_dangling_tool_uses`) must
/// treat index 1 as fixed when a block is pinned (spec §4.4).
pub(super) fn opens_turn(m: &crate::llm::RichMessage) -> bool {
    use crate::llm::RichMessage as M;
    matches!(m, M::Text { role, .. } | M::ImageText { role, .. } if role == "user")
}

pub(super) fn turn_count(history: &[crate::llm::RichMessage]) -> usize {
    history.iter().filter(|m| opens_turn(m)).count()
}

/// Remove the oldest turn: index 0 up to (not including) the next message
/// that opens a turn. On a history that does not start with a user message
/// (nothing today writes one) this still removes up to the next user turn.
pub(super) fn drop_oldest_turn(history: &mut Vec<crate::llm::RichMessage>) {
    let end = history
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, m)| opens_turn(m))
        .map_or(history.len(), |(i, _)| i);
    history.drain(0..end);
}

/// Byte cap for the pinned project-instructions block: half the history
/// budget, never above the module's hard ceiling. The block comes out of the
/// history budget, so history + block stays within the quarter share (§5.3).
pub(super) fn pinned_cap_bytes(budget_tokens: u64) -> usize {
    let half = usize::try_from(budget_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN_ESTIMATE)
        / 2;
    half.min(crate::project_instructions::MAX_PROJECT_INSTRUCTIONS_BYTES)
}

/// Tokens left for prior turns once a pinned block of `pinned_len` bytes is
/// sent. Same divisor as [`estimated_tokens`] so the two can never disagree.
pub(super) fn trim_room(budget_tokens: u64, pinned_len: usize) -> u64 {
    budget_tokens.saturating_sub((pinned_len / CHARS_PER_TOKEN_ESTIMATE) as u64)
}

/// Send-time trim (§5.4): drop the oldest turns of a *copy* of the stored
/// history until it fits beside the pinned block. The newest turn is always
/// kept, the same guard `remember` uses. The block is never a candidate.
pub(super) fn trim_for_send(
    mut prior: Vec<crate::llm::RichMessage>,
    budget_tokens: u64,
    pinned_len: usize,
) -> Vec<crate::llm::RichMessage> {
    let room = trim_room(budget_tokens, pinned_len);
    while turn_count(&prior) > 1 && estimated_tokens(&prior) > room {
        drop_oldest_turn(&mut prior);
    }
    prior
}

/// Build one turn's LLM message list: `[system?, pinned?, prior…, current]`.
/// `pinned` is a separate argument, never part of `prior`, so nothing that
/// trims or stores history can touch it (spec §4.2). `prior` arrives already
/// fetched and trimmed by the caller. With no system prompt, no
/// block and no prior this is just `[user]`.
pub(super) fn seed_history(
    system: String,
    pinned: Option<String>,
    prior: Vec<crate::llm::RichMessage>,
    input: &Message,
) -> Vec<crate::llm::RichMessage> {
    use crate::llm::RichMessage as M;
    let mut h = Vec::with_capacity(prior.len() + 3);
    if !system.is_empty() {
        h.push(M::Text {
            role: "system".into(),
            content: system,
        });
    }
    if let Some(block) = pinned {
        h.push(M::Text {
            role: "user".into(),
            content: block,
        });
    }
    h.extend(prior);
    h.push(user_message(input));
    h
}

/// Multi-turn chat memory. The CLI and Hub thread `context.task_id` = the prior
/// reply's id on every send, so we key stored history by the id of the turn that
/// produced it; the next turn's `context.task_id` then recalls its predecessor.
/// Stores text only — a pasted image was seen the turn it arrived and is not
/// re-sent on later turns.
///
/// Backed by disk when `dir` is set (issue #1199): a restart used to drop every
/// conversation on the floor mid-session, which is not a rare event — editing an
/// entitlement forces one, so the ordinary "hit a denial, grant the path,
/// restart, carry on" loop destroyed the conversation that motivated the grant.
/// The memory map stays the fast path; disk is read only when a key misses,
/// which after a restart is once per conversation.
pub(super) struct ConversationStore {
    pub(super) map: HashMap<String, Vec<crate::llm::RichMessage>>,
    /// Insertion order for LRU eviction past `MAX_CONVERSATIONS`.
    pub(super) order: VecDeque<String>,
    /// Directory holding one JSON file per conversation key. `None` keeps the
    /// store purely in memory (stub runners, most tests).
    pub(super) dir: Option<std::path::PathBuf>,
    /// Estimated-token ceiling for one conversation's stored history.
    pub(super) budget_tokens: u64,
    /// Files left by earlier processes are swept once, on first write.
    pub(super) swept: bool,
}

impl Default for ConversationStore {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            dir: None,
            budget_tokens: DEFAULT_CONV_BUDGET_TOKENS,
            swept: false,
        }
    }
}

impl ConversationStore {
    /// Prior conversation for `key` (the caller's `context.task_id`), or empty.
    ///
    /// A miss falls through to disk: after a restart the caller still threads the
    /// id of a reply this process never produced, and that is precisely the case
    /// worth recovering.
    pub(super) fn prior(&self, key: Option<&str>) -> Vec<crate::llm::RichMessage> {
        let Some(k) = key else {
            return Vec::new();
        };
        if let Some(h) = self.map.get(k) {
            return h.clone();
        }
        self.load(k)
    }

    /// Path holding `key`'s history, or `None` when persistence is off or the
    /// key is not a name we are willing to put in a path.
    pub(super) fn path_for(&self, key: &str) -> Option<std::path::PathBuf> {
        let dir = self.dir.as_ref()?;
        let stem = conversation_file_stem(key)?;
        Some(dir.join(format!("{stem}.json")))
    }

    pub(super) fn load(&self, key: &str) -> Vec<crate::llm::RichMessage> {
        let Some(path) = self.path_for(key) else {
            return Vec::new();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Vec::new();
        };
        match serde_json::from_slice::<Vec<crate::llm::RichMessage>>(&bytes) {
            Ok(h) => {
                tracing::debug!(key, messages = h.len(), "conversation recovered from disk");
                h
            }
            // A truncated or stale-format file is not worth failing a turn over;
            // the conversation simply starts fresh, as it did before #1199.
            Err(e) => {
                tracing::warn!(key, error = %e, "unreadable conversation file; ignoring");
                Vec::new()
            }
        }
    }

    /// Write `history` for `key`, atomically (temp + rename, as the YAML stores
    /// do). Best-effort throughout: persistence must never fail a turn.
    pub(super) fn persist(&self, key: &str, history: &[crate::llm::RichMessage]) {
        let Some(path) = self.path_for(key) else {
            return;
        };
        let Some(dir) = path.parent() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, "cannot create conversation dir; memory only");
            return;
        }
        let Ok(json) = serde_json::to_vec(history) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub(super) fn forget_file(&self, key: &str) {
        if let Some(path) = self.path_for(key) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Drop conversation files left by earlier processes once this one starts
    /// writing. Without it the directory grows by one file per turn forever,
    /// since the in-memory LRU that bounds `map` starts empty on every boot.
    pub(super) fn sweep_stale_files(&mut self) {
        if self.swept {
            return;
        }
        self.swept = true;
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| {
                let m = e.metadata().ok()?.modified().ok()?;
                Some((m, e.path()))
            })
            .collect();
        if files.len() <= MAX_CONVERSATIONS {
            return;
        }
        files.sort_by_key(|(m, _)| *m);
        let excess = files.len() - MAX_CONVERSATIONS;
        for (_, path) in files.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Store `history` under `key`, trimming the oldest turns to the token
    /// budget and evicting the oldest conversation if over the cap.
    ///
    /// A turn is `[user, agent, ledger?]`; trimming drops whole turns so a
    /// ledger never outlives the text it describes and the history keeps
    /// starting on a `user` message (Anthropic requires that). The newest
    /// turn is always kept.
    pub(super) fn remember(&mut self, key: String, mut history: Vec<crate::llm::RichMessage>) {
        while turn_count(&history) > 1 && estimated_tokens(&history) > self.budget_tokens {
            drop_oldest_turn(&mut history);
        }
        while history.len() > MAX_CONV_MESSAGES && turn_count(&history) > 1 {
            drop_oldest_turn(&mut history);
        }
        self.sweep_stale_files();
        self.persist(&key, &history);
        if self.map.insert(key.clone(), history).is_none() {
            self.order.push_back(key);
            while self.order.len() > MAX_CONVERSATIONS {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                    self.forget_file(&old);
                }
            }
        }
    }
}

impl TaskRunner {
    /// Stored conversation threaded via `ctx` (the caller's `context.task_id`),
    /// or empty. A copy: trimming it for one send never touches the store.
    pub(super) fn stored_prior(&self, ctx: Option<&str>) -> Vec<crate::llm::RichMessage> {
        self.conversations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prior(ctx)
    }

    /// This turn's pinned `<project_instructions>` block and the stored prior,
    /// trimmed so both fit the history budget (spec §5.3–5.4). Rendered once
    /// per turn by the caller, before any tool-loop step, so a file edited
    /// mid-turn does not change what later steps see (§3.1). The block is
    /// returned separately and is never stored.
    pub(super) fn pinned_and_prior(
        &self,
        turn: &str,
        ctx: Option<&str>,
    ) -> (Option<String>, Vec<crate::llm::RichMessage>) {
        let (budget_tokens, prior) = {
            let store = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
            (store.budget_tokens, store.prior(ctx))
        };
        let pinned = match (&self.project_instructions, self.working_dir(Some(turn))) {
            (Some(p), Some(dir)) => p
                .render(&dir, pinned_cap_bytes(budget_tokens))
                .map(|r| r.text),
            _ => None,
        };
        let pinned_len = pinned.as_ref().map_or(0, String::len);
        (pinned, trim_for_send(prior, budget_tokens, pinned_len))
    }

    /// Persist this turn into multi-turn memory keyed by `key` (this turn's id),
    /// so the next send — whose `context.task_id` equals `key` — recalls it.
    ///
    /// Stores the user text, the reply text, and a `TurnLedger` projected from
    /// the reply's ledger Data part (spec 2026-09-19-turn-ledger-memory). A
    /// pasted image is not stored, but its presence is (`attachments`). A reply
    /// with no readable ledger — single-call paths, stub backends — is
    /// remembered as `narrative_only`, because "ran nothing" is the fact the
    /// next turn most needs. Roles `user`/`agent` map to Anthropic
    /// `user`/`assistant`.
    pub(super) fn remember_turn(
        &self,
        key: &str,
        ctx: Option<&str>,
        input: &Message,
        reply: &Message,
    ) {
        let attachments = image_count(input);
        let memory = match ledger_of(reply) {
            Some(l) => crate::turn_ledger::TurnMemory::project(&l, attachments),
            None => crate::turn_ledger::TurnMemory::empty(attachments),
        };
        let turn = u32::try_from(self.turn_counter.load(Ordering::Relaxed)).unwrap_or(u32::MAX);
        let mut store = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = store.prior(ctx);
        h.push(crate::llm::RichMessage::Text {
            role: "user".into(),
            content: text_of(input),
        });
        h.push(crate::llm::RichMessage::Text {
            role: "agent".into(),
            content: text_of(reply),
        });
        h.push(crate::llm::RichMessage::TurnLedger { turn, memory });
        store.remember(key.to_string(), h);
    }

    /// Open this turn's cwd slot: the caller's `cwd` when one was supplied and
    /// it is entitled, else the directory of the turn it continues
    /// (`context_task_id`), else the agent home. Absent means "a client with
    /// no notion of cwd" (Hub, `mur agent send`) — it keeps its conversation's
    /// directory and never inherits another conversation's.
    pub(super) fn adopt_cwd(
        &self,
        turn: &str,
        parent: Option<&str>,
        requested: Option<&std::path::Path>,
    ) {
        let Some((session, roots)) = &self.session_cwd else {
            return;
        };
        let entitled = requested.and_then(|req| match std::fs::canonicalize(req) {
            Ok(c) if crate::tools::fs_policy::under_any_or_worktree(roots, &c) => Some(c),
            Ok(_) => {
                tracing::warn!(cwd = %req.display(), "turn cwd outside entitlements; keeping conversation cwd");
                None
            }
            Err(_) => {
                tracing::warn!(cwd = %req.display(), "turn cwd does not exist; keeping conversation cwd");
                None
            }
        });
        session.begin_turn(turn, parent, entitled);
    }

    /// Turn `turn`'s working directory (`None` outside a turn: the home).
    pub(super) fn working_dir(&self, turn: Option<&str>) -> Option<std::path::PathBuf> {
        let (cwd, _) = self.session_cwd.as_ref()?;
        Some(match turn {
            Some(t) => cwd.for_turn(t),
            None => cwd.current(),
        })
    }
}
