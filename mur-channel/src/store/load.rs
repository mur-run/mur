use super::*;

/// The damage [`ChannelStore::load_events_with_damage`] found in a log — the
/// additive P4 read API §8.2 depends on. Both fields are empty for a clean
/// log, which is the only case `load_events_with_damage` and `load_events`
/// agree is undamaged.
#[derive(Debug, Clone, Default)]
pub struct DamageReport {
    /// 1-based line numbers of lines that failed to parse as a
    /// [`ChannelEvent`] at all (unparseable or truncated), in file order.
    pub unparseable_lines: Vec<usize>,
    /// Events that parsed but whose signature failed verification (present
    /// and invalid, or missing while `require_sig` is set), in file order.
    /// Still included in the `events` this call returns — this is a report,
    /// not a filter (see the method doc for why).
    pub unverified: Vec<ChannelEvent>,
    /// 1-based source line number of each entry in the `events` vector
    /// `load_events_with_damage` returns, same length and order. Lets a
    /// caller (the review driver's replay fold) interleave "this event came
    /// from line N" with `unparseable_lines` to find exactly where in the
    /// file the first damage sits relative to the parsed events — needed to
    /// decide a round boundary (§8.2) without re-parsing the file itself.
    pub event_lines: Vec<usize>,
    /// `seq` of each event whose `prev_hash` does not match the raw line
    /// before it (#1770) — a deleted, reordered, or edited earlier line.
    /// Only events that carry a `prev_hash` (sig-input v2) can appear here.
    pub chain_breaks: Vec<u64>,
    /// The signed head in `<mur_home>/channel-heads/` (#1868) does not hold
    /// for this log (#1770): its signature fails, or the log's first
    /// `head.lines` lines do not chain to `head.chain_hash` — truncated below
    /// the head, or rewritten inside it. `chain_breaks` cannot see either;
    /// every remaining link still holds. A log *longer* than the head is fine
    /// (sandboxed agents append past it); one with no head at all is too.
    pub anchor_mismatch: Option<AnchorMismatch>,
    /// The channel's dirty marker is present: an anchored append began (the
    /// marker goes down under the lock before the line is written) and never
    /// advanced the anchor. The anchor is known stale until a later anchored
    /// append succeeds. A crash between the marker and the write leaves a
    /// marker with no new line — a deliberate false positive: over-reporting
    /// is recoverable, a silent stale anchor is not. Do not "optimise" it out.
    pub anchor_dirty: bool,
}

/// Where the signed head and the log disagree. See
/// [`DamageReport::anchor_mismatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorMismatch {
    /// The head as read from disk.
    pub head: crate::head::Head,
    pub kind: AnchorMismatchKind,
    /// `\n`-terminated lines the log actually has.
    pub log_lines: u64,
}

/// Why a head does not hold for its log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorMismatchKind {
    /// The signature does not verify under the writer's key: the head file
    /// itself was forged or edited.
    BadSignature,
    /// Validly signed, but for a different channel: copied in from another.
    WrongChannel,
    /// The log has fewer lines than the head covers.
    Truncated,
    /// The log has the lines, but they chain to a different hash.
    Rewritten,
}

impl DamageReport {
    /// No unparseable lines, no verification failures, no chain breaks, and
    /// the chain-head anchor (if any) is both trusted and matched.
    pub fn is_clean(&self) -> bool {
        self.unparseable_lines.is_empty()
            && self.unverified.is_empty()
            && self.chain_breaks.is_empty()
            && self.anchor_mismatch.is_none()
            && !self.anchor_dirty
    }
}

/// What a `NotFound` on a channel's event log actually means.
///
/// Normally: genuine absence — no such channel, or a channel with no events
/// yet. But `NotFound` also covers a BROKEN PATH on Windows, which maps "a
/// component of the path is not a directory" to `NotFound` where Unix reports
/// `ENOTDIR`. Left alone, a corrupted channel directory reads as an error on
/// Unix and as "no such run" on Windows — the same lying-absence defect the
/// run-status work exists to remove, reintroduced one platform at a time.
///
/// A free function, not an inline check, so the decision is testable on EVERY
/// platform: on Unix the corrupted case never reaches the `NotFound` arm, so
/// an end-to-end test there exercises the ENOTDIR path and proves nothing
/// about this one.
pub(super) fn check_absence_is_not_corruption(dir: &Path) -> Result<()> {
    if fs::metadata(dir).is_ok_and(|m| !m.is_dir()) {
        anyhow::bail!(
            "channel path {} exists but is not a directory — its event log cannot be \
             read, and this is NOT an absent channel",
            dir.display()
        );
    }
    Ok(())
}

impl ChannelStore {
    /// Read a channel's events. A line that fails to parse is skipped so one
    /// damaged record cannot make a whole channel unreadable — but it is
    /// COUNTED and WARNED with its line number, because a dropped line is a
    /// dropped event and every fold above this (run rebuild, pending HITL
    /// gates, the rail, `mur channel show`) would otherwise be unable to tell
    /// "that never happened" from "I could not read it". Losing a
    /// `HitlResponse` silently makes an approved gate read as still waiting.
    pub fn load_events(&self, id: &str) -> Result<Vec<ChannelEvent>> {
        let path = self.events_path(id);
        let content = match Self::read_events_file(&path, &self.channel_dir(id))? {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };
        let mut events = Vec::new();
        let mut damaged = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChannelEvent>(line) {
                Ok(ev) => events.push(ev),
                // 1-based: matches what an editor or `sed -n` shows.
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
        Ok(events)
    }

    /// Shared miss-path handling for the events file: `Ok(None)` means
    /// genuine absence (no such channel, or none with events yet — the
    /// caller returns an empty log); `Ok(Some(content))` is the file's raw
    /// text for the caller to parse its own way. Factored out so
    /// [`load_events`] and [`load_events_with_damage`] (§8.2 P4) agree on
    /// what "absent" means without duplicating the Windows `ENOTDIR`
    /// handling (see [`check_absence_is_not_corruption`]).
    pub(super) fn read_events_file(path: &Path, channel_dir: &Path) -> Result<Option<String>> {
        match fs::read_to_string(path) {
            Ok(c) => Ok(Some(c)),
            // `NotFound` normally means genuine absence: no such channel, or a
            // channel with no events yet. But it ALSO covers a broken path on
            // Windows, which maps "a component of the path is not a directory"
            // to `NotFound` where Unix reports `ENOTDIR`. Without this check a
            // corrupted channel directory reads as an error on Unix and as
            // "no such run" on Windows — the same defect this module exists to
            // remove, reintroduced per-platform. Only reached on the miss path,
            // so it costs one `stat` on a read that already failed.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                check_absence_is_not_corruption(channel_dir)?;
                Ok(None)
            }
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// [`load_events`] plus a damage report (§8.2, P4 decided): the 1-based
    /// line numbers of unparseable/truncated lines, and every PARSED event
    /// whose signature fails verification against `writer_pubkey` (via
    /// [`crate::sign::verify_one`], same `require_sig` semantics it
    /// documents).
    ///
    /// Additive only. `load_events` is NOT changed by this: same signature,
    /// same skip-and-`warn!` behaviour, same return value for every existing
    /// caller (AC11f). This is a new, separate path used only by the review
    /// driver, which — unlike every other reader — must not silently
    /// continue from a damaged or unverifiable log (§8.2: "never continue
    /// silently from a corrupted or partially rebuilt state").
    ///
    /// This is a REPORT, not a filter: a signature-failing event is still
    /// present in `events` (same contents `load_events` would return, minus
    /// the lines that fail to parse at all) — the caller decides what the
    /// damage means for its own fold, which is the whole point of keeping
    /// `verify_log`'s drop-on-fail policy out of the read path (§12
    /// non-goals).
    pub fn load_events_with_damage(
        &self,
        id: &str,
        writer_pubkey: &[u8; 32],
        require_sig: bool,
    ) -> Result<(Vec<ChannelEvent>, DamageReport)> {
        let path = self.events_path(id);
        let content = match Self::read_events_file(&path, &self.channel_dir(id))? {
            Some(c) => c,
            // No log but an anchor (or a dirty marker) is the most complete
            // truncation there is; fall through with empty content.
            None if crate::head_io::read_head(&crate::anchor::heads_dir(&self.mur_home), id)?
                .is_none()
                && !crate::anchor::is_dirty(&self.mur_home, id) =>
            {
                return Ok((Vec::new(), DamageReport::default()));
            }
            None => String::new(),
        };
        let mut events = Vec::new();
        let mut report = DamageReport::default();
        for (idx, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChannelEvent>(line) {
                Ok(ev) => {
                    if !crate::sign::verify_one(id, &ev, writer_pubkey, require_sig) {
                        report.unverified.push(ev.clone());
                    }
                    events.push(ev);
                    // 1-based, matching `unparseable_lines` below.
                    report.event_lines.push(idx + 1);
                }
                // 1-based: matches what an editor or `sed -n` shows, and
                // matches `load_events`'s own `lines` field.
                Err(_) => report.unparseable_lines.push(idx + 1),
            }
        }
        report.chain_breaks = super::chain::chain_breaks(&content);
        report.anchor_mismatch =
            super::chain::anchor_mismatch(&self.mur_home, id, &content, writer_pubkey)?;
        report.anchor_dirty = crate::anchor::is_dirty(&self.mur_home, id);
        Ok((events, report))
    }
}
