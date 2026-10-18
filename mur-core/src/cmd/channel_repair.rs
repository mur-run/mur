//! `mur channel repair <channel_id>` — the operator's way out of a damaged
//! channel log (#1965).
//!
//! The HITL gate refuses to reuse any decision on a log that fails its
//! integrity check (#1935), and decisions given after the damage land in the
//! same damaged log, so without this the channel re-prompts forever.
//!
//! Repair never re-anchors the old history: blessing "the lines that still
//! verify" could bless tampered state. It always starts a fresh chain:
//!
//! 1. run the `mur channel verify` check; an intact channel is a no-op;
//! 2. confirm (or `--yes`; a non-interactive caller without it is refused);
//! 3. resolve the signing key — refused, touching nothing, when it cannot be
//!    read from disk (never an unsigned fallback);
//! 4. in ONE append-lock acquisition (`ChannelService::restart_chain`): move
//!    the log aside (never delete), drop the signed head and dirty marker,
//!    and write a signed record of the repair as seq 0 with a fresh genesis
//!    head. Releasing the lock in between would let another writer put old,
//!    still-valid records into the new log, sealed as trusted.
//!
//! No HITL decision is carried over — not the old ones, not ones given after
//! the damage. The next gated action asks once, and that answer is reused.
//!
//! Gate B's decision channel (an agent's `hitl-channel`) is also checked
//! against a head sealed by the agent's own key in `hitl-heads/`. It is
//! repaired the same way, but the fresh chain is written with that key and
//! sealed by the runtime's own hook (`channel_repair_gate_b`), so Gate B
//! trusts it again.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_channel::ChannelService;
use mur_channel::service::restart::RestartSigner;
use mur_common::agent::HITL_CHANNEL_FILE;
use mur_common::channel::ChannelActor;

use crate::channel_writer::{KeySource, ROUTER_AGENT, WriterKey, writer_key};
use crate::cmd::channel_integrity::{self, IntegrityReport};
use crate::cmd::channel_repair_gate_b::GateB;

/// `payload.type` of the event that opens a repaired chain.
pub const REPAIR_EVENT_TYPE: &str = "channel-repair";

/// What a repair did.
#[derive(Debug)]
pub enum Outcome {
    /// The channel verified clean; nothing was touched.
    Intact,
    /// The operator declined at the prompt; nothing was touched.
    Declined,
    /// The log was quarantined and a fresh chain started.
    Repaired {
        quarantined: Option<PathBuf>,
        seq: u64,
    },
}

/// How the operator confirms. Injected so tests drive it without a terminal.
pub enum Confirm<'a> {
    /// `--yes`: no prompt.
    Yes,
    /// Ask on this reader; `interactive` says whether a human is there.
    Ask {
        input: &'a mut dyn BufRead,
        interactive: bool,
    },
}

/// Run the command: print what happened and return the process exit code.
pub fn run(mur_home: &Path, channel_id: &str, yes: bool) -> Result<i32> {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut lock = stdin.lock();
    let confirm = if yes {
        Confirm::Yes
    } else {
        Confirm::Ask {
            input: &mut lock,
            interactive,
        }
    };
    match repair(mur_home, channel_id, confirm)? {
        Outcome::Intact => println!("channel {channel_id}: intact; nothing to repair"),
        Outcome::Declined => {
            println!("not repaired");
            return Ok(channel_integrity::EXIT_DAMAGED);
        }
        Outcome::Repaired { quarantined, seq } => {
            match quarantined {
                Some(p) => println!("quarantined the damaged log to {}", p.display()),
                None => println!("there was no log to quarantine"),
            }
            println!(
                "channel {channel_id}: fresh chain started (repair recorded at seq {seq}); \
                 no earlier HITL decision is carried over"
            );
        }
    }
    Ok(channel_integrity::EXIT_INTACT)
}

/// The body of [`run`], with confirmation injected.
pub fn repair(mur_home: &Path, channel_id: &str, confirm: Confirm<'_>) -> Result<Outcome> {
    let report = channel_integrity::check(mur_home, channel_id)?;
    print!("{}", report.render());
    // Gate B also checks its own agent-sealed head, so for its channel that
    // check counts as damage too. The key is loaded before anything is asked
    // or touched: without it the fresh chain could not be sealed.
    let gate_b = gate_b_owner(mur_home, channel_id)
        .map(|agent| GateB::load(mur_home, agent))
        .transpose()?;
    let gate_b_damage = gate_b.as_ref().and_then(|g| g.damage(mur_home, channel_id));
    if let Some(why) = &gate_b_damage {
        println!("  agent-sealed head (Gate B): {why}");
    }
    if report.is_intact() && gate_b_damage.is_none() {
        return Ok(Outcome::Intact);
    }
    // The signer is resolved before anything is asked or touched, and there
    // is no unsigned fallback: a repair that cannot sign would drop the old
    // log and head and leave a chain nothing can verify.
    let writer = match &gate_b {
        Some(_) => None,
        None => Some(router_key(mur_home)?),
    };
    if !confirmed(confirm)? {
        return Ok(Outcome::Declined);
    }

    let heads = mur_channel::anchor::heads_dir(mur_home);
    // Read before the heads are dropped, so the repair event can name them.
    let old_head = mur_channel::head_io::read_head(&heads, channel_id)
        .ok()
        .flatten();
    let old_agent_head = gate_b
        .as_ref()
        .and_then(|g| g.old_head(mur_home, channel_id));
    let now = chrono::Utc::now();
    let mut reason = summary(&report);
    if let Some(why) = &gate_b_damage {
        if !reason.is_empty() {
            reason.push_str("; ");
        }
        reason.push_str(&format!("agent-sealed head does not hold ({why})"));
    }
    let rendered = report.render();
    // Runs under the append lock, after the move and before the record is
    // written: the old log is gone and no other writer can append until the
    // fresh chain's signed first line and head are on disk.
    let prepare = |quarantined: Option<&Path>| -> Result<serde_json::Value> {
        mur_channel::head_io::remove_head(&heads, channel_id)?;
        mur_channel::anchor::clear_dirty(mur_home, channel_id)?;
        if let Some(g) = &gate_b {
            g.drop_head(mur_home, channel_id)?;
        }
        Ok(serde_json::json!({
            "type": REPAIR_EVENT_TYPE,
            "text": format!("channel log repaired: {reason}"),
            "repaired_by": ChannelActor::local_human(),
            "repaired_at": now,
            "reason": reason,
            "report": rendered,
            "quarantined_log": quarantined
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            "old_head": old_head.as_ref().map(|h| serde_json::json!({
                "lines": h.lines,
                "seq": h.seq,
                "chain_hash": h.chain_hash,
            })),
            "old_agent_head": old_agent_head,
        }))
    };
    // The check above ran before the lock (and before the prompt). Run it
    // again under the lock: another repair may have finished meanwhile, and
    // quarantining its fresh chain would throw that repair away. A pending
    // repair marker counts as damage here, so an interrupted run resumes.
    let still_damaged = || -> Result<bool> {
        let again = channel_integrity::check(mur_home, channel_id)?;
        let gate_b_again = gate_b
            .as_ref()
            .and_then(|g| g.damage_locked(mur_home, channel_id));
        Ok(!again.is_intact() || gate_b_again.is_some())
    };
    let done = match (&gate_b, &writer) {
        // The agent's seal hook writes the fresh `hitl-heads/` genesis head.
        (Some(g), _) => g.service(mur_home)?.restart_chain(
            channel_id,
            now,
            g.actor(),
            g.signer(),
            still_damaged,
            prepare,
        ),
        (None, Some(w)) => {
            let key_home = mur_home.join("agents").join(ROUTER_AGENT);
            ChannelService::open(mur_home)?.restart_chain(
                channel_id,
                now,
                ChannelActor::System,
                RestartSigner {
                    identity: &w.id,
                    kv: w.kv,
                    anchor_home: Some(&key_home),
                },
                still_damaged,
                prepare,
            )
        }
        (None, None) => unreachable!("the router key was resolved above"),
    }
    .context("restart the channel on a fresh chain")?;
    let Some(done) = done else {
        // Damaged when checked, healthy under the lock: another repair won.
        return Ok(Outcome::Intact);
    };
    Ok(Outcome::Repaired {
        quarantined: done.quarantined,
        seq: done.event.seq,
    })
}

/// The router's signing key, read from disk. Refused when it is missing,
/// unreadable, or only a sandbox handoff (which may not seal the trusted
/// head): any of those would leave the fresh chain unsigned or unanchored.
fn router_key(mur_home: &Path) -> Result<WriterKey> {
    let refuse = |why: String| {
        anyhow::anyhow!(
            "refusing to repair: the router signing key (agent `{ROUTER_AGENT}`) {why}, so \
             the fresh chain could not be signed and anchored; nothing was touched. Run this \
             outside any agent sandbox, from the `mur` that owns the key."
        )
    };
    match writer_key(mur_home, ROUTER_AGENT) {
        Ok(Some(k)) if k.source == KeySource::Disk => Ok(k),
        Ok(Some(_)) => Err(refuse("is only available as a sandbox handoff".into())),
        Ok(None) => Err(refuse("is missing or unreadable".into())),
        Err(e) => Err(refuse(format!("is unreadable ({e:#})"))),
    }
}

fn confirmed(confirm: Confirm<'_>) -> Result<bool> {
    let (input, interactive) = match confirm {
        Confirm::Yes => return Ok(true),
        Confirm::Ask { input, interactive } => (input, interactive),
    };
    if !interactive {
        bail!("refusing to repair without a terminal to confirm on; pass --yes");
    }
    print!(
        "Repair moves the damaged log aside and starts a fresh chain. No earlier HITL \
         decision on this channel will be trusted. Continue? [y/N] "
    );
    std::io::stdout().flush().ok();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

/// One line naming the damage, for the repair event.
fn summary(r: &IntegrityReport) -> String {
    let mut parts = Vec::new();
    if let Some(e) = &r.head_error {
        parts.push(format!("head unreadable ({e})"));
    }
    if let Some(k) = r.anchor_mismatch {
        parts.push(format!("signed head does not hold ({k:?})"));
    }
    if r.anchor_dirty {
        parts.push("dirty marker set".into());
    }
    if r.repair_pending {
        parts.push("an earlier repair did not finish".into());
    }
    if !r.chain_breaks.is_empty() {
        parts.push(format!("hash chain broken at seq {:?}", r.chain_breaks));
    }
    if !r.unparseable_lines.is_empty() {
        parts.push(format!("unparseable lines {:?}", r.unparseable_lines));
    }
    if !r.unverified_seqs.is_empty() {
        parts.push(format!("bad signature on seq {:?}", r.unverified_seqs));
    }
    parts.join("; ")
}

/// The agent whose Gate B decision channel this is, if any.
fn gate_b_owner(mur_home: &Path, channel_id: &str) -> Option<String> {
    let agents = std::fs::read_dir(mur_home.join("agents")).ok()?;
    agents.flatten().find_map(|entry| {
        let marker = std::fs::read_to_string(entry.path().join(HITL_CHANNEL_FILE)).ok()?;
        (marker.trim() == channel_id).then(|| entry.file_name().to_string_lossy().into_owned())
    })
}

#[cfg(test)]
#[path = "channel_repair_tests.rs"]
mod tests;
