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
//! 3. under the append lock, move the log aside (never delete) and drop the
//!    signed head and dirty marker, so the next anchored append seals a
//!    genesis head;
//! 4. write a signed system event recording the repair as the fresh chain's
//!    first line.
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
use mur_common::agent::HITL_CHANNEL_FILE;
use mur_common::channel::{ChannelActor, EventKind};

use crate::channel_writer::{ROUTER_AGENT, append_as_writer};
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
    let svc = ChannelService::open(mur_home)?;
    let quarantined = svc.store().quarantine_log(channel_id, now, || {
        mur_channel::head_io::remove_head(&heads, channel_id)?;
        mur_channel::anchor::clear_dirty(mur_home, channel_id)?;
        match &gate_b {
            Some(g) => g.drop_head(mur_home, channel_id),
            None => Ok(()),
        }
    })?;
    // Seqs restart at 0; the read-model's fold guard (`seq > last_seq`)
    // would ignore the fresh chain, so drop the row and let the append
    // re-seed it.
    if let Err(e) = svc.index().remove(channel_id) {
        tracing::warn!(channel_id, error = %e, "could not reset the channel index row");
    }

    let mut reason = summary(&report);
    if let Some(why) = &gate_b_damage {
        if !reason.is_empty() {
            reason.push_str("; ");
        }
        reason.push_str(&format!("agent-sealed head does not hold ({why})"));
    }
    let payload = serde_json::json!({
        "type": REPAIR_EVENT_TYPE,
        "text": format!("channel log repaired: {reason}"),
        "repaired_by": ChannelActor::local_human(),
        "repaired_at": now,
        "reason": reason,
        "report": report.render(),
        "quarantined_log": quarantined
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        "old_head": old_head.as_ref().map(|h| serde_json::json!({
            "lines": h.lines,
            "seq": h.seq,
            "chain_hash": h.chain_hash,
        })),
        "old_agent_head": old_agent_head,
    });
    let ev = match &gate_b {
        // The agent's seal hook writes the fresh `hitl-heads/` genesis head.
        Some(g) => g.append(mur_home, channel_id, payload),
        None => append_as_writer(
            &svc,
            mur_home,
            channel_id,
            ROUTER_AGENT,
            ChannelActor::System,
            EventKind::Note,
            payload,
            None,
        ),
    }
    .context("record the repair on the fresh chain")?;
    Ok(Outcome::Repaired {
        quarantined,
        seq: ev.seq,
    })
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
