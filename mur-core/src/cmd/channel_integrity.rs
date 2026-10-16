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

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_channel::ChannelService;
use mur_channel::store::AnchorMismatchKind;
use mur_common::channel::ChannelActor;

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
    /// 1-based numbers of lines that did not parse as an event.
    pub unparseable_lines: Vec<usize>,
    /// `seq` of events whose signature failed under their actor's key.
    pub unverified_seqs: Vec<u64>,
    /// `seq` of events whose `prev_hash` does not match the line before.
    pub chain_breaks: Vec<u64>,
    /// Why the head does not hold for the log, if it does not.
    pub anchor_mismatch: Option<AnchorMismatchKind>,
    /// An anchored append began and never advanced the head.
    pub anchor_dirty: bool,
    /// The head file exists but could not be read or parsed. When set the
    /// other checks were skipped, because the reader refuses such a channel.
    pub head_error: Option<String>,
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
            && self.head_error.is_none()
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
        out.push_str(&format!(
            "channel {}: {verdict} ({} events, {} lines)\n",
            self.channel_id, self.events, self.log_lines
        ));
        match (self.anchored_lines, &self.head_error) {
            (_, Some(e)) => out.push_str(&format!(
                "  head: unreadable ({e}); the other checks were skipped\n"
            )),
            (Some(n), None) => {
                let past = self.log_lines.saturating_sub(n);
                out.push_str(&format!(
                    "  head: covers {n} lines{}\n",
                    if past > 0 && self.anchor_mismatch.is_none() {
                        format!(" ({past} written past it, not yet anchored)")
                    } else {
                        String::new()
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
        push_list(&mut out, "unparseable lines", &self.unparseable_lines);
        push_list(&mut out, "bad signature on seq", &self.unverified_seqs);
        push_list(&mut out, "broken hash chain at seq", &self.chain_breaks);
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

/// A channel id names a directory under `channels/` and a file under
/// `channel-heads/`; refuse anything that could point elsewhere.
fn check_id(channel_id: &str) -> Result<()> {
    let unsafe_id = channel_id.is_empty()
        || channel_id == "."
        || channel_id == ".."
        || channel_id.contains(['/', '\\', '\0']);
    if unsafe_id {
        bail!("not a valid channel id: {channel_id:?}");
    }
    Ok(())
}

/// Placeholder for `load_events_with_damage`'s bare-key parameter when the
/// channel has no signed head (so nothing is checked against it). Not a key.
const NO_HEAD_INERT_KEY: [u8; 32] = [0u8; 32];

/// Check `channel_id` under `mur_home`. Errors only when the check cannot run
/// (no such channel, no router key); damage is a successful report.
pub fn check(mur_home: &Path, channel_id: &str) -> Result<IntegrityReport> {
    check_id(channel_id)?;
    let svc = ChannelService::open(mur_home)?;
    let store = svc.store();
    let log_path = store.events_path(channel_id);
    let heads = mur_channel::anchor::heads_dir(mur_home);

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
    if log.is_none() && head.is_none() && !mur_channel::anchor::is_dirty(mur_home, channel_id) {
        bail!("no such channel: {channel_id} (no log, no signed head)");
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
    let (events, damage) = store.load_events_with_damage(channel_id, &router_key, false)?;

    let require_sig = crate::channel_verify::require_sig_from_env();
    report.events = events.len();
    report.unverified_seqs = events
        .iter()
        .filter(|ev| !crate::channel_verify::verify_event(mur_home, channel_id, ev, require_sig))
        .map(|ev| ev.seq)
        .collect();
    report.unparseable_lines = damage.unparseable_lines;
    report.chain_breaks = damage.chain_breaks;
    report.anchor_mismatch = damage.anchor_mismatch.map(|m| m.kind);
    report.anchor_dirty = damage.anchor_dirty;
    Ok(report)
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
