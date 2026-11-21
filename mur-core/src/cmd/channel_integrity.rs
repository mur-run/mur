//! `mur channel verify <channel_id>` — a read-only integrity check of one
//! channel's log against its signed head.
//!
//! It is a thin report over what the reader already computes
//! (`ChannelStore::load_events_with_damage`): unparseable lines, hash-chain
//! breaks, and whether the signed head in `channel-heads/` still holds for the
//! log (truncated below it, rewritten inside it, forged, or copied from
//! another channel). It takes no lock and writes nothing.
//!
//! Event signatures are checked per actor (`channel_verify::verify_event`),
//! NOT against the single key `load_events_with_damage` is handed: an agent's
//! events are signed by that agent, so a one-key check would flag every one of
//! them.
//!
//! One line has no actor of its own key: murmur's `!cmd` card is a `System`
//! note the session agent signs (see [`is_session_card`]).

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_channel::store::AnchorMismatchKind;
use mur_channel::store::ChannelStore;
use mur_common::channel::{ChannelActor, ChannelPurpose, EventKind};

/// Exit code for a channel that verified clean.
pub const EXIT_INTACT: i32 = 0;
/// Exit code for a channel with damage. `1` stays reserved for "the check
/// itself could not run", so a script can tell the two apart.
pub const EXIT_DAMAGED: i32 = 2;

/// What `mur channel verify` found. Plain data, so the verdict and the
/// rendering are both testable without a terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    pub channel_id: String,
    /// Events that parsed.
    pub events: usize,
    /// `\n`-terminated lines the log has.
    pub log_lines: u64,
    /// Lines the signed head covers; `None` when the channel has no head.
    pub anchored_lines: Option<u64>,
    /// 1-based numbers of lines that did not parse as an event (corrupt).
    pub unparseable_lines: Vec<usize>,
    /// 1-based numbers of lines written by a newer MUR with an event kind
    /// this binary does not know (#2287). The kind alone is not damage; their
    /// signatures and chain links are checked like every other line's.
    pub newer_kind_lines: Vec<usize>,
    /// `seq` of events whose signature failed under their actor's key.
    pub unverified_seqs: Vec<u64>,
    /// `seq` of events whose `prev_hash` does not match the line before.
    pub chain_breaks: Vec<u64>,
    /// Why the head does not hold for the log, if it does not.
    pub anchor_mismatch: Option<AnchorMismatchKind>,
    /// An anchored append began and never advanced the head.
    pub anchor_dirty: bool,
    /// An earlier `mur channel repair` never finished (`repair_marker`).
    pub repair_pending: bool,
    /// The agent whose key sealed the head, when it was not the router's:
    /// a legacy head written by an agent's own anchored append (H1). Not
    /// damage — it is reported so the operator can see who vouches for it.
    pub legacy_sealer: Option<String>,
    /// The head file exists but could not be read or parsed. When set the
    /// other checks were skipped, because the reader refuses such a channel.
    pub head_error: Option<String>,
    /// The channel's human-owner state (B4). Default on a channel with no
    /// owner record, which is every legacy channel.
    pub owner: OwnerReport,
}

/// What integrity sees of the owner record (B2's [`HumanTrust`]).
///
/// [`HumanTrust`]: mur_channel::verify::human::HumanTrust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerReport {
    /// `seq` of the live `owner-key-enrolled` record, if any.
    pub live_record_seq: Option<u64>,
    /// `seq` of records that do not verify under the local human chain
    /// (another machine's key, a retired key). Reported, never damage (D-7).
    pub ignored_records: Vec<u64>,
    /// The live record is not covered by the signed head (D-3). Damage.
    pub unanchored: bool,
    /// `human_cut` points past the head or at a line that does not hash to
    /// it (D-6). Damage.
    pub cut_mismatch: bool,
}

impl OwnerReport {
    fn from_trust(trust: &mur_channel::verify::human::HumanTrust) -> Self {
        use mur_channel::verify::human::OwnerDamage;
        let has = |d: OwnerDamage| trust.damage().contains(&d);
        Self {
            live_record_seq: trust.live_record().map(|r| r.seq),
            ignored_records: trust.ignored_records().to_vec(),
            unanchored: has(OwnerDamage::Unanchored),
            cut_mismatch: has(OwnerDamage::CutBeyondHead) || has(OwnerDamage::CutLineHashMismatch),
        }
    }

    fn is_damaged(&self) -> bool {
        self.unanchored || self.cut_mismatch
    }
}

impl IntegrityReport {
    /// No damage of any kind. A channel with no head at all can still be
    /// intact — it is merely unanchored — so that is not a failure here.
    pub fn is_intact(&self) -> bool {
        self.unparseable_lines.is_empty()
            && self.unverified_seqs.is_empty()
            && self.chain_breaks.is_empty()
            && self.anchor_mismatch.is_none()
            && !self.anchor_dirty
            && !self.repair_pending
            && self.head_error.is_none()
            && !self.owner.is_damaged()
    }

    pub fn exit_code(&self) -> i32 {
        if self.is_intact() {
            EXIT_INTACT
        } else {
            EXIT_DAMAGED
        }
    }

    /// Human-readable report, one finding per line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verdict = if self.is_intact() {
            "intact"
        } else {
            "DAMAGED"
        };
        // An unreadable head stops the check before events are loaded, so
        // `events` is unknown there, not zero; say only what was counted.
        let counts = if self.head_error.is_some() {
            format!("{} lines", self.log_lines)
        } else {
            format!("{} events, {} lines", self.events, self.log_lines)
        };
        out.push_str(&format!(
            "channel {}: {verdict} ({counts})\n",
            self.channel_id
        ));
        match (self.anchored_lines, &self.head_error) {
            (_, Some(e)) => out.push_str(&format!(
                "  head: unreadable ({e}); the other checks were skipped\n"
            )),
            (Some(n), None) => {
                let past = self.log_lines.saturating_sub(n);
                out.push_str(&format!(
                    "  head: covers {n} lines{}{}\n",
                    if past > 0 && self.anchor_mismatch.is_none() {
                        format!(" ({past} written past it, not yet anchored)")
                    } else {
                        String::new()
                    },
                    match &self.legacy_sealer {
                        Some(agent) => format!(", sealed by agent `{agent}` (legacy, pre-router)"),
                        None => String::new(),
                    }
                ));
            }
            (None, None) => out.push_str(
                "  head: none — the log is not anchored, so truncation of its tail cannot be detected\n",
            ),
        }
        if let Some(kind) = self.anchor_mismatch {
            out.push_str(&format!("  head does not hold: {}\n", describe(kind)));
        }
        if self.anchor_dirty {
            out.push_str(
                "  head is stale: an anchored append began and never advanced it \
                 (a crash right before the write also leaves this marker)\n",
            );
        }
        if self.repair_pending {
            out.push_str(
                "  an earlier `mur channel repair` did not finish; rerun it to complete\n",
            );
        }
        push_list(&mut out, "unparseable lines", &self.unparseable_lines);
        push_list(&mut out, "bad signature on seq", &self.unverified_seqs);
        if let Some(seq) = self.owner.live_record_seq {
            out.push_str(&format!("  owner: live record at seq {seq}\n"));
        }
        if self.owner.unanchored {
            out.push_str("  owner record is not covered by the signed head\n");
        }
        if self.owner.cut_mismatch {
            out.push_str(
                "  owner record's human_cut does not match the log (past the head, or the line was rewritten)\n",
            );
        }
        push_list(
            &mut out,
            "note: owner records under a key this machine does not hold (ignored, not damage) at seq",
            &self.owner.ignored_records,
        );
        push_list(&mut out, "broken hash chain at seq", &self.chain_breaks);
        push_list(
            &mut out,
            "note: written by a newer MUR (kind unknown to this binary, not damage; upgrade to read) at lines",
            &self.newer_kind_lines,
        );
        out
    }
}

fn describe(kind: AnchorMismatchKind) -> &'static str {
    match kind {
        AnchorMismatchKind::BadSignature => {
            "its signature does not verify under the router key (the head file was forged or edited)"
        }
        AnchorMismatchKind::WrongChannel => {
            "it is validly signed but for a different channel (copied in from elsewhere)"
        }
        AnchorMismatchKind::Truncated => "the log has fewer lines than the head covers",
        AnchorMismatchKind::Rewritten => {
            "the log's leading lines no longer hash to what the head signed"
        }
    }
}

fn push_list<T: std::fmt::Display>(out: &mut String, label: &str, items: &[T]) {
    if items.is_empty() {
        return;
    }
    let joined: Vec<String> = items.iter().map(ToString::to_string).collect();
    out.push_str(&format!("  {label}: {}\n", joined.join(", ")));
}

/// Placeholder for `load_events_with_damage`'s bare-key parameter when the
/// channel has no signed head (so nothing is checked against it). Not a key.
const NO_HEAD_INERT_KEY: [u8; 32] = [0u8; 32];

/// Check `channel_id` under `mur_home`. Errors only when the check cannot run
/// (no such channel, no router key); damage is a successful report.
pub fn check(mur_home: &Path, channel_id: &str) -> Result<IntegrityReport> {
    check_with(
        mur_home,
        channel_id,
        crate::channel_verify::require_sig_from_env(),
    )
}

/// [`check`] with the signature policy passed in rather than read from the
/// environment, so a test of a keyless channel means the same thing whether
/// or not `MUR_CHANNEL_REQUIRE_SIG` is set in the test process (#2145).
fn check_with(mur_home: &Path, channel_id: &str, require_sig: bool) -> Result<IntegrityReport> {
    mur_channel::check_channel_id(channel_id)?;
    // The store, not `ChannelService`: the service also opens the SQLite
    // index (creating dirs, migrating, enabling WAL), which would break the
    // "writes nothing" promise and tie the check to an unrelated read-model.
    let store = ChannelStore::new(mur_home);
    let log_path = store.events_path(channel_id)?;
    let heads = mur_channel::anchor::heads_dir(mur_home);

    // The log is read here for the line count and again inside
    // `load_events_with_damage`. No lock is taken (the check is read-only), so
    // an append landing between the two reads can make `log_lines` and
    // `events` disagree by that append; rerun for an exact snapshot.
    let log = match std::fs::read(&log_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("read {}", log_path.display())),
    };
    let mut report = IntegrityReport {
        channel_id: channel_id.to_owned(),
        log_lines: log
            .as_deref()
            .map_or(0, |b| mur_channel::head::chain_log(b).lines),
        ..Default::default()
    };

    let head = match mur_channel::head_io::read_head(&heads, channel_id) {
        Ok(head) => head,
        Err(e) => {
            report.head_error = Some(format!("{e:#}"));
            return Ok(report);
        }
    };
    // A freshly created channel has a manifest and nothing else, so the
    // manifest counts as existence too.
    if log.is_none()
        && head.is_none()
        && !store.has_manifest(channel_id)
        && !mur_channel::anchor::is_dirty(mur_home, channel_id)
        && !mur_channel::repair_marker::is_pending(mur_home, channel_id)
    {
        bail!("no such channel: {channel_id} (no manifest, no log, no signed head)");
    }
    report.anchored_lines = head.as_ref().map(|h| h.lines);

    // The head is signed by the router, whatever actors wrote the events.
    // Only the head needs it, so a channel with no head (a keyless, legacy
    // router) is still checkable without one; a head we cannot verify is not.
    let router_key = crate::channel_verify::actor_pubkey(mur_home, &ChannelActor::System, None);
    if router_key.is_none() && head.is_some() {
        bail!("cannot read the router's key, so the signed head cannot be checked");
    }
    // `load_events_with_damage` takes a bare key, not an `Option`. With no
    // head it never reaches the head check, and its per-event verdicts are
    // ignored (the per-actor pass below decides), so the key is inert here.
    let router_key = router_key.unwrap_or(NO_HEAD_INERT_KEY);
    // `require_sig` is applied by the per-actor pass below, not here.
    let (events, mut damage) = store.load_events_with_damage(channel_id, &router_key, false)?;
    // A head that fails under the router key may be a legacy one an agent
    // sealed with its own key (H1): re-check it under that writer's key
    // before calling it forged. Only the anchor verdict is taken from the
    // second read; events and their per-actor verdicts stay from the first.
    if let Some((agent, key)) =
        legacy_sealer_key(mur_home, &store, channel_id, &events, &damage, &router_key)
    {
        let (_, again) = store.load_events_with_damage(channel_id, &key, false)?;
        if again.anchor_mismatch.is_none() {
            report.legacy_sealer = Some(agent);
        }
        damage.anchor_mismatch = again.anchor_mismatch;
    }

    // Owner state from the same bytes the line count came from (D-6 hashes
    // the raw line). A non-UTF-8 log has unparseable lines already counted.
    let trust = mur_channel::verify::human::HumanTrust::from_log(
        mur_home,
        channel_id,
        &String::from_utf8_lossy(log.as_deref().unwrap_or_default()),
        head.as_ref(),
    );
    report.owner = OwnerReport::from_trust(&trust);
    let ignored = trust.ignored_records();
    let card_signers = card_signers(&store, channel_id);

    // A commander directive is a `System` line under a pinned commander key,
    // not the router's: it verifies the way `fold_governance` verifies it.
    let commander_keys = crate::cmd::commander::accepted_pubkeys(mur_home);
    report.events = events.len();
    report.unverified_seqs = events
        .iter()
        .filter(|ev| !crate::cmd::commander::is_signed_directive(&commander_keys, channel_id, ev))
        // A foreign owner record fails its signature by construction; it is
        // reported under `owner`, not as damage (D-7).
        .filter(|ev| !(ev.kind == EventKind::OwnerKeyEnrolled && ignored.contains(&ev.seq)))
        .filter(|ev| !is_session_card(mur_home, channel_id, &card_signers, ev))
        .filter(|ev| !crate::channel_verify::verify_event(mur_home, channel_id, ev, require_sig))
        .map(|ev| ev.seq)
        .chain(
            // An unknown kind is no pass on the signature (#2287).
            damage
                .newer_kind_events
                .iter()
                .filter(|ev| {
                    !mur_channel::verify::verify_newer_kind_event(
                        mur_home,
                        channel_id,
                        ev,
                        require_sig,
                    )
                })
                .map(|ev| ev.seq),
        )
        .collect();
    report.unparseable_lines = damage.unparseable_lines;
    report.newer_kind_lines = damage.newer_kind_lines;
    report.chain_breaks = damage.chain_breaks;
    report.anchor_mismatch = damage.anchor_mismatch.map(|m| m.kind);
    report.anchor_dirty = damage.anchor_dirty;
    report.repair_pending = damage.repair_pending;
    Ok(report)
}

/// The writer key a legacy agent-sealed head would verify under, when the
/// head failed its signature under the router key.
///
/// Before H1 an agent's anchored append sealed the head with the same key
/// that signed the event, so the sealer of a legacy head is the writer of
/// the event at `head.seq`. For an `Agent` actor that is the agent itself.
/// A `Human` or `System` line there was written by the session agent too
/// (murmur's typed turn and `!cmd` card, before C2), so on a channel with no
/// owner record yet the channel's agent participants are tried in turn. On
/// an owned channel heads are router-sealed (H1) and nothing is tried.
fn legacy_sealer_key(
    mur_home: &Path,
    store: &ChannelStore,
    channel_id: &str,
    events: &[mur_common::channel::ChannelEvent],
    damage: &mur_channel::store::DamageReport,
    tried: &[u8; 32],
) -> Option<(String, [u8; 32])> {
    let m = damage.anchor_mismatch.as_ref()?;
    if m.kind != AnchorMismatchKind::BadSignature {
        return None;
    }
    let sealed = events.iter().find(|ev| ev.seq == m.head.seq)?;
    let candidates: Vec<String> = match &sealed.actor {
        ChannelActor::Agent { id } => vec![id.clone()],
        _ if events.iter().any(|e| e.kind == EventKind::OwnerKeyEnrolled) => return None,
        _ => store
            .load_manifest(channel_id)
            .ok()?
            .participants
            .into_iter()
            .filter_map(|p| match p.actor {
                ChannelActor::Agent { id } => Some(id),
                _ => None,
            })
            .collect(),
    };
    candidates.into_iter().find_map(|id| {
        let actor = ChannelActor::Agent { id: id.clone() };
        let key = crate::channel_verify::actor_pubkey(mur_home, &actor, m.head.key_version)?;
        (key != *tried && m.head.verify(&key)).then_some((id, key))
    })
}

/// The agents whose key may sign a `!cmd` card on this channel: the agent
/// participants of a conversation. Empty for a fleet or workflow channel,
/// where every `System` line is the control plane's, and when the manifest
/// cannot be read.
fn card_signers(store: &ChannelStore, channel_id: &str) -> Vec<String> {
    let Ok(ch) = store.load_manifest(channel_id) else {
        return Vec::new();
    };
    if mur_channel::purpose::effective_purpose(&ch) != ChannelPurpose::Conversation {
        return Vec::new();
    }
    ch.participants
        .into_iter()
        .filter_map(|p| match p.actor {
            ChannelActor::Agent { id } => Some(id),
            _ => None,
        })
        .collect()
}

/// True when `ev` is a murmur `!cmd` card one of `signers` signed.
///
/// The card is a `System` note written under the session agent's key, not the
/// router's (`cmd/agent/cli/persist.rs`), and the conversation watermark
/// reads it under that key (`mur_channel::conversation::assess`, S1). The
/// per-actor check holds `System` to the router, so for any session agent but
/// the router itself it would call every card a bad signature. The signature
/// is always required here: an unsigned line is left to the per-actor check.
fn is_session_card(
    mur_home: &Path,
    channel_id: &str,
    signers: &[String],
    ev: &mur_common::channel::ChannelEvent,
) -> bool {
    ev.sig.is_some()
        && matches!(ev.actor, ChannelActor::System)
        && ev.kind == EventKind::Note
        && !mur_channel::repair_record::is_repair_record(ev)
        && signers.iter().any(|id| {
            let agent = ChannelActor::Agent { id: id.clone() };
            crate::channel_verify::actor_pubkey(mur_home, &agent, ev.key_version)
                .is_some_and(|pk| mur_channel::sign::verify_one(channel_id, ev, &pk, true))
        })
}

/// Run the command: print the report, return the process exit code.
pub fn run(mur_home: &Path, channel_id: &str) -> Result<i32> {
    let report = check(mur_home, channel_id)?;
    print!("{}", report.render());
    Ok(report.exit_code())
}

#[cfg(test)]
#[path = "channel_integrity_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "channel_integrity_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "channel_integrity_shell_card_tests.rs"]
mod shell_card_tests;
