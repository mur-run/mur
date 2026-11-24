//! `mur channel delete <handle>` — remove one conversation for good (#2564).
//!
//! A conversation lives in five places. [`ChannelService::delete_channel`]
//! owns four of them (store dir, index rows, signed head, dirty marker, head
//! last); the fifth is each agent's own copy of the history,
//! `agents/<agent>/conversations/<id>.json`, which the runtime keeps beside
//! the channel and which nothing else removes. That copy goes first: a crash
//! part-way leaves a conversation the agent has forgotten, which deleting
//! again finishes, never history for a conversation that is gone.
//!
//! Scope, deliberately narrow:
//! - conversations only — fleet and workflow runs are audit records;
//! - CLI only — `delete_channel` writes `channel-heads/`, which the agent
//!   sandbox has no grant for, so no agent or MCP tool reaches this;
//! - a pending HITL request refuses without `--force`. Channel `state` is not
//!   used: every conversation is created `working` and stays there, so it says
//!   nothing about whether a turn is in flight;
//! - an ambiguous handle deletes nothing and lists the matches.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mur_channel::ChannelService;
use mur_channel::handle::{WHOLE_ID_SIGIL, handle_key};
use mur_channel::index::ChannelRow;

/// The index purpose string of a deletable channel.
const CONVERSATION: &str = "conversation";
/// Shortest prefix accepted, the same floor as murmur's `/channels`.
const MIN_PREFIX: usize = 4;
/// Rows scanned to resolve a handle; above any real machine's channel count.
const RESOLVE_SCAN_LIMIT: usize = 100_000;
/// Exit code when the operator answered no.
pub const EXIT_DECLINED: i32 = 2;

/// How the operator confirms. Injected so tests drive it without a terminal.
pub enum Confirm<'a> {
    Yes,
    Ask {
        input: &'a mut dyn BufRead,
        interactive: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Deleted { id: String, histories: Vec<PathBuf> },
    Declined,
}

pub fn run(mur_home: &Path, handle: &str, yes: bool, force: bool) -> Result<i32> {
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
    match delete(mur_home, handle, force, confirm)? {
        Outcome::Declined => {
            println!("not deleted");
            Ok(EXIT_DECLINED)
        }
        Outcome::Deleted { id, histories } => {
            println!(
                "deleted conversation {id} ({} agent history file(s) removed)",
                histories.len()
            );
            Ok(0)
        }
    }
}

/// The body of [`run`], with confirmation injected.
pub fn delete(mur_home: &Path, handle: &str, force: bool, confirm: Confirm<'_>) -> Result<Outcome> {
    let svc = ChannelService::open(mur_home)?;
    let rows = svc.index().list(RESOLVE_SCAN_LIMIT)?;
    let row = resolve(&rows, handle)?;
    if row.purpose != CONVERSATION {
        bail!(
            "channel {} is a {} channel, not a conversation; only conversations can be \
             deleted (run and audit channels are kept)",
            row.id,
            row.purpose
        );
    }
    if row.hitl_pending && !force {
        bail!(
            "conversation {} has a pending approval request; answer it first, or pass --force",
            row.id
        );
    }
    if !confirmed(row, confirm)? {
        return Ok(Outcome::Declined);
    }
    let histories = remove_agent_histories(mur_home, &row.id)?;
    svc.delete_channel(&row.id)?;
    Ok(Outcome::Deleted {
        id: row.id.clone(),
        histories,
    })
}

/// Name one channel from what the user typed: the whole id (optionally
/// behind `#`), or a handle prefix of at least [`MIN_PREFIX`].
fn resolve<'a>(rows: &'a [ChannelRow], typed: &str) -> Result<&'a ChannelRow> {
    let typed = typed.trim();
    let bare = typed.strip_prefix(WHOLE_ID_SIGIL).unwrap_or(typed);
    if let Some(r) = rows.iter().find(|r| r.id.eq_ignore_ascii_case(bare)) {
        return Ok(r);
    }
    if typed.starts_with(WHOLE_ID_SIGIL) {
        bail!("no channel has the id {bare}");
    }
    let Some(key) = handle_key(bare) else {
        bail!("no channel matches {typed:?}");
    };
    if key.len() < MIN_PREFIX {
        bail!("{typed:?} is too short; give at least {MIN_PREFIX} characters of the handle");
    }
    let hits: Vec<&ChannelRow> = rows
        .iter()
        .filter(|r| handle_key(&r.id).is_some_and(|k| k.starts_with(&key)))
        .collect();
    match hits.as_slice() {
        [] => bail!("no channel matches {typed:?}"),
        [one] => Ok(one),
        many => {
            let ids: Vec<&str> = many.iter().map(|r| r.id.as_str()).collect();
            bail!(
                "{typed:?} matches {} channels; nothing deleted. Give more of the id:\n  {}",
                many.len(),
                ids.join("\n  ")
            )
        }
    }
}

/// Every agent's history file for `channel_id`, removed. Every agent is
/// searched, not just today's participants: one that left still holds its
/// copy. The runtime names the file after the channel id verbatim.
fn remove_agent_histories(mur_home: &Path, channel_id: &str) -> Result<Vec<PathBuf>> {
    mur_channel::check_channel_id(channel_id)?;
    let mut removed = Vec::new();
    for agent in crate::a2a_dial::agent_dir_names(mur_home) {
        let dir = mur_home
            .join(mur_common::paths::AGENTS)
            .join(&agent)
            .join("conversations");
        let json = dir.join(format!("{channel_id}.json"));
        for path in [dir.join(format!("{channel_id}.json.tmp")), json.clone()] {
            match std::fs::remove_file(&path) {
                Ok(()) if path == json => removed.push(path),
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("remove agent history {}", path.display())));
                }
            }
        }
    }
    Ok(removed)
}

fn confirmed(row: &ChannelRow, confirm: Confirm<'_>) -> Result<bool> {
    let (input, interactive) = match confirm {
        Confirm::Yes => return Ok(true),
        Confirm::Ask { input, interactive } => (input, interactive),
    };
    if !interactive {
        bail!("refusing to delete without a terminal to confirm on; pass --yes");
    }
    let agents: Vec<String> = serde_json::from_str(&row.agents).unwrap_or_default();
    let title = if row.title.is_empty() {
        "(untitled)"
    } else {
        &row.title
    };
    println!("conversation {}", row.id);
    println!("  title:    {title}");
    println!("  agents:   {}", agents.join(", "));
    println!("  messages: {}", row.msg_count);
    println!("  last:     {}", row.updated_at);
    print!("Delete it and every agent's copy of its history? This cannot be undone. [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

#[cfg(test)]
#[path = "channel_delete_tests.rs"]
mod tests;
