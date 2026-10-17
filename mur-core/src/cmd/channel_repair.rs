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

use mur_channel::repair_marker::{self, RepairMarker};

use crate::channel_writer::ROUTER_AGENT;
use crate::cmd::channel_integrity::{self, IntegrityReport};

/// Name the quarantined log is kept under (`ChannelStore`'s `events.jsonl`).
const QUARANTINED_EVENTS_FILE: &str = "events.jsonl";

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
        // A resumed run may find a head here already (moved by the first
        // run) and a newer one in place: the newer one is not a head over the
        // fresh log either, so it goes too, but beside the first, not over it.
        if dst.exists() {
            mur_channel::head_io::remove_head(&heads, channel_id)?;
            continue;
        }
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

/// Agents holding a chat-gate head for `channel_id`.
fn agents_with_heads(home: &Path, channel_id: &str) -> Result<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(home.join("agents")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(agent) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let heads = entry.path().join(HITL_HEAD_DIR);
        if mur_channel::head_io::head_path(&heads, channel_id)?.exists() {
            out.push(agent);
        }
    }
    out.sort();
    Ok(out)
}

/// Every key the repair will sign with must load BEFORE anything moves: a
/// missing agent key found mid-repair would leave that agent's chat gate
/// with no head over the fresh log, distrusting the channel for good.
fn preflight_keys(home: &Path, channel_id: &str, agents: &[String]) -> Result<()> {
    let router_home = home.join("agents").join(ROUTER_AGENT);
    mur_common::identity::AgentIdentity::load(&router_home).map_err(|e| {
        anyhow::anyhow!(
            "cannot repair {channel_id}: the router key ({}) is unavailable ({e}), \
             so the fresh chain could not be anchored",
            router_home.display()
        )
    })?;
    for agent in agents {
        mur_common::identity::AgentIdentity::load(&home.join("agents").join(agent)).map_err(
            |e| {
                anyhow::anyhow!(
                    "cannot repair {channel_id}: {agent} holds a chat-gate head for it, \
                     but its key is unavailable ({e}), so that head could not be resealed"
                )
            },
        )?;
    }
    Ok(())
}

/// Repair `channel_id` without asking. `None` when the channel is intact.
/// Errors when the check cannot run or a key the repair signs with is
/// missing (a fresh chain nobody can anchor would be no better than the
/// damaged one).
///
/// One exclusive append lock spans the whole transition — re-check, marker,
/// move, first event of the fresh chain, chat-gate reseals, marker removal —
/// so no writer can start the fresh chain first or have a head it just
/// sealed swept into the quarantine. (The runtime seals its chat-gate head
/// inside that same lock.) Our own appends nest inside it.
///
/// Crash-safe and resumable: the marker in `channel-heads/` goes down before
/// anything moves and comes off after the last reseal. Until then the channel
/// reads as damaged, and rerunning this finishes the job from the marker.
pub fn repair(home: &Path, channel_id: &str, by: &str) -> Result<Option<RepairOutcome>> {
    // Cheap pre-filter only: unlocked, it can catch an anchored append
    // mid-flight. The decision that counts is re-made under the lock below.
    if repair_marker::read(home, channel_id)?.is_none()
        && channel_integrity::check(home, channel_id)?.is_intact()
    {
        return Ok(None);
    }
    let svc = ChannelService::open(home)?;
    let _lock = svc.store().lock_exclusive(channel_id)?;

    let marker = match repair_marker::read(home, channel_id)? {
        Some(m) => m,
        None => {
            // Any writer that made the channel look damaged a moment ago has
            // finished by now.
            let before = channel_integrity::check(home, channel_id)?;
            if before.is_intact() {
                return Ok(None);
            }
            RepairMarker {
                label: label_now(),
                by: by.to_owned(),
                before: serde_json::json!({
                    "old_log_lines": before.log_lines,
                    "old_head_lines": before.anchored_lines,
                    "damage": before.render().trim_end(),
                }),
            }
        }
    };
    // Every key we will sign with must load before anything moves; with the
    // lock held, no new chat-gate head can appear after this list is made.
    let quarantine = svc
        .store()
        .events_path(channel_id)?
        .with_file_name(mur_channel::store::QUARANTINE_DIR)
        .join(&marker.label);
    let mut signers = agents_with_heads(home, channel_id)?;
    if quarantine.is_dir() {
        signers.extend(quarantined_head_owners(&quarantine)?);
    }
    preflight_keys(home, channel_id, &signers)?;

    let (moved, ()) = svc
        .store()
        .quarantine_log_if(channel_id, &marker.label, || {
            repair_marker::write(home, channel_id, &marker)?;
            Ok(Some(()))
        })?
        .context("unconditional quarantine moved nothing")?;
    finish(&svc, home, channel_id, moved, &marker).map(Some)
}

/// Everything after the log is out of the way. Each step is idempotent, so a
/// resumed repair simply runs them all again.
fn finish(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    moved: mur_channel::store::Quarantined,
    marker: &RepairMarker,
) -> Result<RepairOutcome> {
    // Heads this or an earlier, interrupted run moved aside; resealed below.
    let mut agent_heads = quarantine_agent_heads(home, channel_id, &moved.dir)?;
    agent_heads.extend(quarantined_head_owners(&moved.dir)?);
    agent_heads.sort();
    agent_heads.dedup();
    // `record_event` only folds a seq above the row's `last_seq`; the fresh
    // chain restarts at 0, so drop the row and let the next append re-seed it.
    svc.index().remove(channel_id)?;

    let rel = moved
        .dir
        .strip_prefix(home)
        .unwrap_or(&moved.dir)
        .display()
        .to_string();
    let mut repair = serde_json::json!({
        "by": marker.by,
        "quarantine": rel,
        "quarantined_log": moved.dir.join(QUARANTINED_EVENTS_FILE).exists(),
        "quarantined_head": moved.dir.join(mur_channel::store::QUARANTINED_HEAD_FILE).exists(),
        "quarantined_agent_heads": agent_heads,
    });
    if let (Some(dst), Some(src)) = (repair.as_object_mut(), marker.before.as_object()) {
        dst.extend(src.clone());
    }
    let payload = serde_json::json!({
        "text": format!(
            "channel repaired by {}: damaged log quarantined to {rel}; \
             earlier approvals and denials are not carried over",
            marker.by
        ),
        "repair": repair,
    });
    // On a resumed run the fresh log an interrupted run started has just been
    // quarantined beside the damaged one (nothing that landed on it while the
    // repair was pending is trusted), so this record opens the chain again.
    let ev = crate::channel_writer::append_as_writer(
        svc,
        home,
        channel_id,
        ROUTER_AGENT,
        ChannelActor::System,
        EventKind::Note,
        payload,
        Some(format!("repair-{}", marker.label)),
    )
    .context("write the repair record")?;

    for agent in &agent_heads {
        reseal_agent_head(home, agent, channel_id, ev.seq)?;
    }
    repair_marker::clear(home, channel_id)?;

    let after = channel_integrity::check(home, channel_id)?;
    if after.is_intact() && after.anchored_lines.is_none() {
        bail!("repair record written but the fresh chain has no signed head");
    }
    Ok(RepairOutcome {
        after,
        quarantine: moved.dir,
        repair_seq: ev.seq,
    })
}

/// Agents whose chat-gate head sits in `quarantine` (`hitl-heads-<agent>.head`).
fn quarantined_head_owners(quarantine: &Path) -> Result<Vec<String>> {
    let prefix = format!("{HITL_HEAD_DIR}-");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(quarantine)
        .with_context(|| format!("list {}", quarantine.display()))?
        .flatten()
    {
        let name = entry.file_name();
        let Some(agent) = name
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(|n| n.strip_suffix(".head"))
        else {
            continue;
        };
        out.push(agent.to_owned());
    }
    Ok(out)
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
    if report.repair_pending {
        // The operator confirmed when this repair began; finish it.
        println!("resuming the interrupted repair");
    } else if report.is_intact() {
        println!("nothing to repair");
        return Ok(channel_integrity::EXIT_INTACT);
    } else {
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
    }
    let by = match ChannelActor::local_human() {
        ChannelActor::Human { name } => name,
        _ => "you".into(),
    };
    let Some(done) = repair(home, channel_id, &by)? else {
        println!("channel became intact before the repair ran; nothing changed");
        return Ok(channel_integrity::EXIT_INTACT);
    };
    report_done(&done)
}

fn report_done(done: &RepairOutcome) -> Result<i32> {
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
