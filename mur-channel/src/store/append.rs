use super::*;

/// What the sealing hook of [`ChannelStore::append_event_with`] is handed
/// (channel-log-integrity spec 7.3.1). It describes the line that was just
/// written, so the hook never has to re-read the log under the lock.
#[derive(Debug)]
pub struct SealInput<'a> {
    pub channel_id: &'a str,
    /// The event as written, with its assigned `seq` and `ts`.
    pub event: &'a ChannelEvent,
    /// The exact bytes appended to `events.jsonl`, terminator included.
    pub record: &'a [u8],
}

/// The post-write sealing hook type (see [`SealHooks::post_write`]).
pub type SealHook<'h> = dyn for<'a, 'b> Fn(&'b SealInput<'a>) -> Result<()> + 'h;

/// What the pre-write hook is handed: the event about to be written, with
/// its final `seq`. No `record` — nothing is on disk yet.
#[derive(Debug)]
pub struct PreSealInput<'a> {
    pub channel_id: &'a str,
    pub event: &'a ChannelEvent,
}

/// The pre-write hook type (see [`SealHooks::pre_write`]).
pub type PreSealHook<'h> = dyn for<'a, 'b> Fn(&'b PreSealInput<'a>) -> Result<()> + 'h;

/// The two points an append lends to its caller, both UNDER the lock.
///
/// - `pre_write` runs after `seq` is assigned and before the line is
///   written. An `Err` aborts the append with nothing written. It exists for
///   the chain-head dirty marker (#1770): evidence that the head is about to
///   lag must be on disk before the line is, or a crash between the write
///   and `post_write` leaves an unanchored line with no trace.
/// - `post_write` runs after the line is written (and again on a dedup hit,
///   with nothing written). An `Err` makes the append `Err` but the line
///   stays (decision B1).
#[derive(Clone, Copy, Default)]
pub struct SealHooks<'h> {
    pub pre_write: Option<&'h PreSealHook<'h>>,
    pub post_write: Option<&'h SealHook<'h>>,
}

impl<'h> SealHooks<'h> {
    /// Only a post-write hook — the shape every pre-#1770 caller used.
    pub fn post(post_write: Option<&'h SealHook<'h>>) -> Self {
        Self {
            pre_write: None,
            post_write,
        }
    }
}

impl ChannelStore {
    /// Append one event under an advisory lock so `seq` stays monotonic across
    /// processes (the Hub and the CLI may append concurrently). Returns the
    /// event with its assigned `seq` and timestamp.
    #[allow(clippy::too_many_arguments)]
    pub fn append_event(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: Option<String>,
        key_version: Option<u32>,
    ) -> Result<ChannelEvent> {
        self.append_event_with(
            id,
            actor,
            kind,
            payload,
            idempotency_key,
            sig,
            key_version,
            SealHooks::default(),
        )
    }

    /// [`append_event`](Self::append_event) plus optional sealing hooks
    /// (spec 7.3.1; see [`SealHooks`]). `post_write` runs after the line is
    /// written and before the lock is released. A dedup hit writes nothing but
    /// runs it again on the existing line, so a retry never reports `Ok` for
    /// an unsealed event. If it returns `Err` the append returns `Err`
    /// (decision B1) but the line stays on disk; the caller must treat the
    /// event as written and unsealed, not retry it blindly.
    #[allow(clippy::too_many_arguments)]
    pub fn append_event_with(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: Option<String>,
        key_version: Option<u32>,
        hooks: SealHooks<'_>,
    ) -> Result<ChannelEvent> {
        self.append_sealed(
            id,
            actor,
            kind,
            payload,
            idempotency_key,
            |_| chain::Seal {
                sig,
                key_version,
                prev_hash: None,
            },
            hooks,
        )
    }

    /// The append body. `signer` runs UNDER the lock with the chain hash of the
    /// current tail line ([`GENESIS_PREV_HASH`] for an empty log), so a v2
    /// signer commits to exactly the line this event lands after. `hooks` are
    /// the pre/post-write points of [`SealHooks`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_sealed(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        signer: impl FnOnce(chain::Unsealed<'_>) -> chain::Seal,
        hooks: SealHooks<'_>,
    ) -> Result<ChannelEvent> {
        self.with_append_lock(id, || {
            self.append_locked(id, actor, kind, payload, idempotency_key, signer, hooks)
        })
    }

    /// Run `f` holding the channel's append lock. Creates the channel
    /// directory and (write-if-absent) its manifest first.
    ///
    /// An append creates the directory, so it can resurrect a channel whose
    /// manifest is gone — archived away, or never written because the first
    /// thing that touched the id was an append. The events then accumulate
    /// in a channel every reader calls unreadable, because `load_manifest`
    /// is the existence check (the fleet rail's `⚠ channel unreadable` came
    /// from exactly this, permanently, on a live install). Write-if-absent:
    /// an existing manifest is authoritative and must never be clobbered by
    /// a write on the event path.
    ///
    /// The lock is a SIDECAR file — never events.jsonl itself. On Windows OS
    /// file locks are mandatory (not advisory like flock on macOS/Linux), so
    /// holding a lock on the data file blocks our own reads/writes of it (os
    /// error 33). Locking a separate file gives cross-process mutual exclusion
    /// while the data file stays freely readable/writable on every platform.
    ///
    /// Not re-entrant: `f` must use the `*_locked` bodies, never a locking
    /// append, or it deadlocks on its own lock.
    pub(crate) fn with_append_lock<T>(&self, id: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let dir = self.channel_dir(id)?;
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        self.ensure_manifest(id)?;
        let lock_path = dir.join(super::LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive().context("lock channel events")?;
        let out = f();
        FileExt::unlock(&lock).ok();
        out
    }

    /// The append body, for a caller that ALREADY holds the channel's append
    /// lock (`append_sealed`, and the quarantine-and-restart of #1965, which
    /// must move the old log aside and write the fresh chain's first line in
    /// one lock acquisition).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_locked(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        signer: impl FnOnce(chain::Unsealed<'_>) -> chain::Seal,
        hooks: SealHooks<'_>,
    ) -> Result<ChannelEvent> {
        let dir = self.channel_dir(id)?;
        let path = self.events_path(id)?;

        // One read of the log under the lock serves both the dedup lookup and
        // `next_seq`. Lines are split keeping their terminator so a dedup hit
        // can hand the seal hook the exact bytes that were appended.
        let content = Self::read_events_file(&path, &dir)?.unwrap_or_default();
        let mut last_seq = None;
        let mut hit: Option<(ChannelEvent, &str)> = None;
        let mut damaged = Vec::new();
        for (idx, segment) in content.split_inclusive('\n').enumerate() {
            let body = segment.trim_end_matches(['\n', '\r']);
            if body.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChannelEvent>(body) {
                Ok(e) => {
                    last_seq = Some(e.seq);
                    if hit.is_none()
                        && idempotency_key.is_some()
                        && e.idempotency_key == idempotency_key
                    {
                        hit = Some((e, segment));
                    }
                }
                Err(_) => damaged.push(idx + 1),
            }
        }
        if !damaged.is_empty() {
            tracing::warn!(
                channel_id = %id,
                path = %path.display(),
                dropped = damaged.len(),
                lines = ?damaged,
                "unparseable event line(s) skipped — the events they carried are missing from every fold of this channel"
            );
        }

        // Dedup: if this idempotency_key already exists in the log, return the
        // prior event unchanged (exactly-once for crash-reruns; v3c). Done under
        // the lock so a concurrent writer can't slip a duplicate in between.
        //
        // A hit also gives the seal hook another chance: the first attempt may
        // have written the line and then failed to seal it, and the retry must
        // not be answered `Ok` with the event still unsealed. The hook owns
        // idempotency (head already covers this seq => no-op). Nothing is
        // written here.
        if let Some((existing, segment)) = hit {
            let resealed = match hooks.post_write {
                Some(hook) => Self::record_of(segment).and_then(|record| {
                    hook(&SealInput {
                        channel_id: id,
                        event: &existing,
                        record,
                    })
                }),
                None => Ok(()),
            };
            resealed.context("seal failed after the event was written")?;
            return Ok(existing);
        }

        let next_seq = last_seq.map(|s| s + 1).unwrap_or(0);
        let chain::Seal {
            sig,
            key_version,
            prev_hash,
        } = signer(chain::Unsealed {
            channel_id: id,
            actor: &actor,
            kind,
            payload: &payload,
            idempotency_key: idempotency_key.as_deref(),
            tail_hash: &chain::tail_hash(&content),
        });

        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
            sig,
            key_version,
            prev_hash,
        };
        let line = serde_json::to_string(&ev).context("serialize event")?;
        if let Some(pre) = hooks.pre_write
            && let Err(e) = pre(&PreSealInput {
                channel_id: id,
                event: &ev,
            })
        {
            return Err(e.context("pre-write seal failed; nothing was written"));
        }
        let mut data = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        // ONE `write_all` of the record plus its terminator, not `writeln!`.
        // `File` is unbuffered, so `writeln!` is two `write(2)` calls: a process
        // that dies between them leaves a line with no newline, and the next
        // append (O_APPEND, starting at EOF) glues its JSON onto the orphan —
        // losing BOTH events, silently, in every reader. One `write_all`
        // removes that guaranteed two-syscall window. It is atomic in practice
        // for a small record on a local filesystem, not by guarantee (a short
        // write makes `write_all` loop). A torn tail is still reported as an
        // unparseable line, and tail truncation is the head anchor's job (#1770).
        let mut record = line.into_bytes();
        record.push(b'\n');
        data.write_all(&record)
            .with_context(|| format!("write {}", path.display()))?;
        // Seal under the lock, after the write: event first, head second.
        let sealed = match hooks.post_write {
            Some(hook) => hook(&SealInput {
                channel_id: id,
                event: &ev,
                record: &record,
            }),
            None => Ok(()),
        };
        sealed.context("seal failed after the event was written")?;
        Ok(ev)
    }

    /// The exact bytes originally appended for `segment` (one `split_inclusive`
    /// line of the log). Appends only ever write `<json>\n`, so a segment with
    /// a `\r\n` terminator or no terminator was edited by hand: the original
    /// bytes cannot be reproduced, and handing the hook a divergent record
    /// would let it hash something the writer never wrote. Refuse instead.
    /// (The formal `record` contract belongs to the head slice.)
    fn record_of(segment: &str) -> Result<&[u8]> {
        match segment.strip_suffix('\n') {
            Some(body) if !body.ends_with('\r') => Ok(segment.as_bytes()),
            _ => anyhow::bail!(
                "cannot re-seal: the event line does not end in a bare LF, so its original bytes are unknown"
            ),
        }
    }
}
