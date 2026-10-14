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

/// The sealing hook type taken by [`ChannelStore::append_event_with`].
pub type SealHook<'h> = dyn for<'a, 'b> Fn(&'b SealInput<'a>) -> Result<()> + 'h;

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
            None,
        )
    }

    /// [`append_event`](Self::append_event) plus an optional sealing hook
    /// (spec 7.3.1). The hook runs after the line is written and before the
    /// lock is released, on the write path only: the dedup return writes
    /// nothing and never runs it. If the hook returns `Err` the append returns
    /// `Err` (decision B1) but the line stays on disk; the caller must treat
    /// the event as written and unsealed, not retry it blindly.
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
        seal: Option<&SealHook<'_>>,
    ) -> Result<ChannelEvent> {
        let dir = self.channel_dir(id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        // An append creates the directory, so it can resurrect a channel whose
        // manifest is gone — archived away, or never written because the first
        // thing that touched the id was an append. The events then accumulate
        // in a channel every reader calls unreadable, because `load_manifest`
        // is the existence check (the fleet rail's `⚠ channel unreadable` came
        // from exactly this, permanently, on a live install). Write-if-absent:
        // an existing manifest is authoritative and must never be clobbered by
        // a write on the event path.
        self.ensure_manifest(id)?;
        let path = self.events_path(id);

        // Serialize concurrent appends (CLI + Hub may write the same channel) via
        // a SIDECAR lock file — never lock events.jsonl itself. On Windows OS file
        // locks are mandatory (not advisory like flock on macOS/Linux), so holding
        // a lock on the data file blocks our own reads/writes of it (os error 33).
        // Locking a separate file gives cross-process mutual exclusion while the
        // data file stays freely readable/writable on every platform.
        let lock_path = dir.join("events.lock");
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive().context("lock channel events")?;

        // Dedup: if this idempotency_key already exists in the log, return the
        // prior event unchanged (exactly-once for crash-reruns; v3c). Done under
        // the lock so a concurrent writer can't slip a duplicate in between.
        if let Some(key) = idempotency_key.as_deref()
            && let Some(existing) = self
                .load_events(id)?
                .into_iter()
                .find(|e| e.idempotency_key.as_deref() == Some(key))
        {
            FileExt::unlock(&lock).ok();
            return Ok(existing);
        }

        // Compute next seq from the existing (unlocked) log, held under the lock.
        let next_seq = self.load_events(id)?.last().map(|e| e.seq + 1).unwrap_or(0);

        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
            sig,
            key_version,
        };
        let line = serde_json::to_string(&ev).context("serialize event")?;
        let mut data = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        // ONE `write_all` of the record plus its terminator, not `writeln!`.
        // `File` is unbuffered, so `writeln!` is two `write(2)` calls: a process
        // that dies between them leaves a line with no newline, and the next
        // append (O_APPEND, starting at EOF) glues its JSON onto the orphan —
        // losing BOTH events, silently, in every reader. A single write cannot
        // be torn that way by a crash between syscalls.
        let mut record = line.into_bytes();
        record.push(b'\n');
        data.write_all(&record)
            .with_context(|| format!("write {}", path.display()))?;
        // Seal under the lock, after the write: event first, head second.
        let sealed = match seal {
            Some(hook) => hook(&SealInput {
                channel_id: id,
                event: &ev,
                record: &record,
            }),
            None => Ok(()),
        };
        FileExt::unlock(&lock).ok();
        sealed.context("seal failed after the event was written")?;
        Ok(ev)
    }
}
