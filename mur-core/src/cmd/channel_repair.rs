//! `mur channel repair <channel_id>` — recover a channel whose log fails its
//! integrity check (#1965).
//!
//! After #1935 the HITL gate ignores every earlier decision on a damaged
//! channel, and with no way to undamage it, every gated action there asks
//! again forever. Repair is the explicit, operator-only way out:
//!
//! 1. Run the same check as `mur channel verify`; an intact channel is a no-op.
//! 2. Confirm (or `--yes`), since this changes what the gate trusts.
//! 3. Quarantine the log and its signed head under the channel dir (never
//!    deleted), plus any agent-side chat-gate head (`hitl-heads/`) for it.
//! 4. Start a fresh chain whose first event is a router-signed, anchored
//!    `Note` recording the repair, then reseal each moved chat-gate head over
//!    the fresh log with that agent's own key. Without the reseal the chat
//!    gate sees "records but no signed head" and distrusts the channel for
//!    good, because its seal hook refuses to start a head over existing lines.
//!
//! The fresh chain is deliberate. Re-anchoring "the lines that still verify"
//! would sign whatever an attacker left in place: a log with a deny deleted
//! still verifies up to the first break, and that is exactly the state the
//! gate refuses to trust. So NO decision crosses the repair — not those from
//! before the damage, and not those given on the damaged log afterwards. The
//! next gated action asks once; that answer is reused from then on.
//!
//! Costs of a fresh chain, accepted: `seq` restarts at 0 (the index row is
//! dropped so it refolds from the new log), and requests still parked on the
//! old log are gone — their runs ask again.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_channel::ChannelService;
use mur_common::agent::HITL_HEAD_DIR;
use mur_common::channel::{ChannelActor, EventKind};

use crate::channel_writer::ROUTER_AGENT;
use crate::cmd::channel_integrity::{self, IntegrityReport};

/// What a repair did. `None` from [`repair`] means the channel was intact.
#[derive(Debug, Clone)]
pub struct RepairOutcome {
    /// Report on the fresh chain.
    pub after: IntegrityReport,
    /// Where the damaged log and heads were moved.
    pub quarantine: PathBuf,
    pub repair_seq: u64,
}

/// File-name-safe UTC timestamp, used as the quarantine label.
fn label_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string()
}

/// Move every `agents/<agent>/hitl-heads/<id>.head` into `quarantine`.
/// Gate B seals those over the whole log; left in place, one would cover
/// lines the fresh log does not have, and that gate would distrust the
/// channel for good. Returns the agents whose head was moved.
fn quarantine_agent_heads(home: &Path, channel_id: &str, quarantine: &Path) -> Result<Vec<String>> {
    let agents = home.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents) else {
        return Ok(Vec::new());
    };
    let mut moved = Vec::new();
    for entry in entries.flatten() {
        let Some(agent) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let heads = entry.path().join(HITL_HEAD_DIR);
        let src = mur_channel::head_io::head_path(&heads, channel_id)?;
        let dst = quarantine.join(format!("{HITL_HEAD_DIR}-{agent}.head"));
        match std::fs::rename(&src, &dst) {
            Ok(()) => moved.push(agent),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("move {} aside", src.display()));
            }
        }
        // A crashed head write leaves a temp nothing else will clear.
        mur_channel::head_io::remove_head(&heads, channel_id)?;
    }
    Ok(moved)
}

/// Seal `agent`'s chat-gate head over the whole fresh log, as its runtime
/// would after its own write. We run outside the sandbox, so the agent key
/// is readable here.
fn reseal_agent_head(home: &Path, agent: &str, channel_id: &str, seq: u64) -> Result<()> {
    let agent_home = home.join("agents").join(agent);
    let identity = mur_common::identity::AgentIdentity::load(&agent_home).map_err(|e| {
        anyhow::anyhow!("cannot reseal {agent}'s chat-gate head: key unavailable ({e})")
    })?;
    let log_path = mur_channel::ChannelStore::new(home).events_path(channel_id)?;
    let log = std::fs::read(&log_path).with_context(|| format!("read {}", log_path.display()))?;
    let chain = mur_channel::head::chain_log(&log);
    let head = mur_channel::head::Head::seal(
        &identity,
        mur_channel::head::HeadFields {
            channel_id: channel_id.to_owned(),
            seq,
            lines: chain.lines,
            chain_hash: hex::encode(chain.hash),
            key_version: Some(crate::channel_writer::read_key_version(&agent_home)),
            legacy_through: None,
        },
        chrono::Utc::now(),
    );
    mur_channel::head_io::write_head(&agent_home.join(HITL_HEAD_DIR), &head)
}

/// Repair `channel_id` without asking. `None` when the channel is intact.
/// Errors when the check cannot run or the router key is missing (a fresh
/// chain nobody can anchor would be no better than the damaged one).
pub fn repair(home: &Path, channel_id: &str, by: &str) -> Result<Option<RepairOutcome>> {
    let before = channel_integrity::check(home, channel_id)?;
    if before.is_intact() {
        return Ok(None);
    }
    let router_home = home.join("agents").join(ROUTER_AGENT);
    mur_common::identity::AgentIdentity::load(&router_home).map_err(|e| {
        anyhow::anyhow!(
            "cannot repair {channel_id}: the router key ({}) is unavailable ({e}), \
             so the fresh chain could not be anchored",
            router_home.display()
        )
    })?;

    let svc = ChannelService::open(home)?;
    let label = label_now();
    let moved = svc.store().quarantine_log(channel_id, &label)?;
    let agent_heads = quarantine_agent_heads(home, channel_id, &moved.dir)?;
    // `record_event` only folds a seq above the row's `last_seq`; the fresh
    // chain restarts at 0, so drop the row and let the next append re-seed it.
    svc.index().remove(channel_id)?;

    let rel = moved
        .dir
        .strip_prefix(home)
        .unwrap_or(&moved.dir)
        .display()
        .to_string();
    let old_head = before.anchored_lines;
    let payload = serde_json::json!({
        "text": format!(
            "channel repaired by {by}: damaged log quarantined to {rel}; \
             earlier approvals and denials are not carried over"
        ),
        "repair": {
            "by": by,
            "quarantine": rel,
            "quarantined_log": moved.log.is_some(),
            "quarantined_head": moved.head.is_some(),
            "quarantined_agent_heads": agent_heads,
            "old_log_lines": before.log_lines,
            "old_head_lines": old_head,
            "damage": before.render().trim_end(),
        },
    });
    let ev = crate::channel_writer::append_as_writer(
        &svc,
        home,
        channel_id,
        ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        payload,
        Some(format!("repair-{label}")),
    )
    .context("write the repair record")?;

    for agent in &agent_heads {
        reseal_agent_head(home, agent, channel_id, ev.seq)?;
    }

    let after = channel_integrity::check(home, channel_id)?;
    if after.is_intact() && after.anchored_lines.is_none() {
        bail!("repair record written but the fresh chain has no signed head");
    }
    Ok(Some(RepairOutcome {
        after,
        quarantine: moved.dir,
        repair_seq: ev.seq,
    }))
}

fn confirm() -> Result<bool> {
    let mut out = std::io::stdout();
    out.write_all(b"quarantine this log and start a fresh chain? [y/N] ")?;
    out.flush()?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("read confirmation")?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// Run the command. Returns the process exit code: 0 when intact or
/// repaired, [`channel_integrity::EXIT_DAMAGED`] when still damaged after.
pub fn run(home: &Path, channel_id: &str, yes: bool) -> Result<i32> {
    let report = channel_integrity::check(home, channel_id)?;
    print!("{}", report.render());
    if report.is_intact() {
        println!("nothing to repair");
        return Ok(channel_integrity::EXIT_INTACT);
    }
    println!(
        "repair moves the log and its signed head into {}/<timestamp>/ (nothing is deleted)\n\
         and starts a fresh chain. Earlier approvals and denials on this channel are NOT\n\
         carried over: the next gated action asks once.",
        mur_channel::store::QUARANTINE_DIR
    );
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!("refusing to repair without confirmation; rerun with --yes");
        }
        if !confirm()? {
            println!("aborted; nothing changed");
            return Ok(channel_integrity::EXIT_DAMAGED);
        }
    }
    let by = match ChannelActor::local_human() {
        ChannelActor::Human { name } => name,
        _ => "you".into(),
    };
    let Some(done) = repair(home, channel_id, &by)? else {
        println!("channel became intact before the repair ran; nothing changed");
        return Ok(channel_integrity::EXIT_INTACT);
    };
    println!(
        "✓ quarantined to {}\n✓ fresh chain started (repair record seq {})",
        done.quarantine.display(),
        done.repair_seq
    );
    print!("{}", done.after.render());
    Ok(done.after.exit_code())
}

#[cfg(test)]
#[path = "channel_repair_tests.rs"]
mod tests;
