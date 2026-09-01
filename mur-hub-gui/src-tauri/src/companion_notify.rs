//! System notifications for companion inbox entries (#1125 follow-on).
//!
//! The agent writes `<agent_home>/companion/inbox/<id>.md` when a schedule
//! fires. That makes the message *findable*; this makes it *arrive*.
//!
//! ## Why the Hub raises it and not the agent
//!
//! A MUR agent cannot post a macOS notification. Its `osascript` is refused by
//! TCC, and the runtime's TCC identity is whichever terminal first launched it
//! — a launchd-started agent has no bundle identity of its own to be granted.
//! The Hub is a signed `.app` with its own identity, so the notification is
//! raised from the one process that is allowed to.
//!
//! ## The gap this deliberately makes visible
//!
//! A notification only fires while the Hub is running. Anything that arrived
//! while it was closed would otherwise be silently absorbed into "unread", so
//! [`catch_up`] reports those separately on startup: a missed reminder should
//! read as missed, not as one you have already seen.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Marker holding the ids the Hub has already raised a notification for.
/// Lives beside the other Hub state rather than in an agent's home, because it
/// records what *this machine's Hub* has shown, not anything about the agent.
fn seen_path(mur_home: &Path) -> PathBuf {
    mur_home.join("hub").join("notified-inbox-ids")
}

fn load_seen(mur_home: &Path) -> HashSet<String> {
    std::fs::read_to_string(seen_path(mur_home))
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn mark_seen(mur_home: &Path, id: &str) -> Result<()> {
    let p = seen_path(mur_home);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{id}").context("record a notified inbox id")
}

/// One inbox entry, reduced to what a notification needs.
pub struct InboxEntry {
    pub id: String,
    pub agent: String,
    pub situation: String,
    pub body: String,
}

/// Parse the front matter `inbox::render` writes. Returns `None` for a file
/// that does not look like one of ours rather than guessing at its shape.
fn parse_entry(agent: &str, path: &Path) -> Option<InboxEntry> {
    let raw = std::fs::read_to_string(path).ok()?;
    let rest = raw.strip_prefix("---\n")?;
    let (front, body) = rest.split_once("\n---\n")?;
    let field = |k: &str| -> Option<String> {
        front
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}: ")))
            .map(|v| v.trim().to_string())
    };
    Some(InboxEntry {
        id: field("id")?,
        agent: agent.to_string(),
        situation: field("situation").unwrap_or_default(),
        body: body.trim().trim_end_matches(">>> response: <unset>").trim().to_string(),
    })
}

/// Every agent's inbox directory that exists right now.
fn inbox_dirs(mur_home: &Path) -> Vec<(String, PathBuf)> {
    let agents = mur_home.join("agents");
    let Ok(rd) = std::fs::read_dir(&agents) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let dir = e.path().join("companion/inbox");
            dir.is_dir().then_some((name, dir))
        })
        .collect()
}

/// Raise one notification and record that it was raised.
///
/// Best-effort: a notification that cannot be shown must not stop the next one,
/// and must not mark itself seen — otherwise a transient failure would silently
/// consume the message.
fn raise(app: &AppHandle, mur_home: &Path, e: &InboxEntry, missed: bool) {
    let title = if missed {
        format!("{} — while MUR Hub was closed", e.agent)
    } else {
        e.agent.clone()
    };
    match app
        .notification()
        .builder()
        .title(title)
        .body(&e.body)
        .show()
    {
        Ok(()) => {
            if let Err(err) = mark_seen(mur_home, &e.id) {
                tracing::warn!("notified {} but could not record it: {err:#}", e.id);
            }
        }
        Err(err) => tracing::warn!("could not show notification for {}: {err:#}", e.id),
    }
}

/// Notify for everything that arrived while the Hub was not running.
///
/// Labelled as missed rather than shown as ordinary new mail. A reminder you
/// were never told about at the time is a different fact from one you have just
/// been told, and collapsing the two is how a delivery gap stays invisible.
pub fn catch_up(app: &AppHandle, mur_home: &Path) -> usize {
    let seen = load_seen(mur_home);
    let mut n = 0;
    for (agent, dir) in inbox_dirs(mur_home) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .filter_map(|e| parse_entry(&agent, &e.path()))
            .filter(|e| !seen.contains(&e.id))
            .collect();
        // Oldest first, so a burst reads in the order it happened.
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        for e in entries {
            raise(app, mur_home, &e, true);
            n += 1;
        }
    }
    n
}

/// Watch every agent's inbox and notify as entries appear.
///
/// One watcher per agent rather than a single recursive watch on `agents/`:
/// that tree churns constantly (locks, logs, telemetry) and on Linux inotify
/// would take a descriptor per subdirectory. An agent created after the Hub
/// started is picked up at the next Hub start — which is also when `catch_up`
/// would report anything it missed, so nothing is lost, only delayed.
pub fn watch_inboxes(
    app: AppHandle,
    mur_home: &Path,
) -> Result<Vec<notify::RecommendedWatcher>> {
    let mut watchers = Vec::new();
    for (agent, dir) in inbox_dirs(mur_home) {
        let app = app.clone();
        let home = mur_home.to_path_buf();
        let who = agent.clone();
        let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let seen = load_seen(&home);
            for p in ev.paths.iter().filter(|p| p.extension().is_some_and(|x| x == "md")) {
                if let Some(e) = parse_entry(&who, p)
                    && !seen.contains(&e.id)
                {
                    raise(&app, &home, &e, false);
                }
            }
        })
        .with_context(|| format!("create inbox watcher for {agent}"))?;
        notify::Watcher::watch(&mut w, &dir, notify::RecursiveMode::NonRecursive)
            .with_context(|| format!("watch inbox for {agent}"))?;
        watchers.push(w);
    }
    Ok(watchers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_agent_runtime::companion::inbox::write_inbox_md;
    use mur_agent_runtime::companion::notifier::CompanionMessage;
    use mur_common::companion::Situation;

    fn written(dir: &Path, id: &str, body: &str) -> PathBuf {
        let msg = CompanionMessage {
            id: id.to_string(),
            situation: Situation::Scheduled,
            template_id: "scheduled".into(),
            locale: "zh-TW".into(),
            body: body.to_string(),
            generated_at: chrono::Utc::now(),
        };
        write_inbox_md(dir, &msg).unwrap()
    }

    /// The writer lives in `mur-agent-runtime` and the reader here; nothing
    /// else connects the two. Parsing a hand-written fixture would keep passing
    /// after the real format drifted, so this round-trips through the actual
    /// writer.
    #[test]
    fn an_entry_the_runtime_wrote_parses_back() {
        let tmp = tempfile::tempdir().unwrap();
        let p = written(tmp.path(), "task-abc", "該吃早餐了 🥐");
        let e = parse_entry("mur", &p).expect("the real writer's output must parse");
        assert_eq!(e.id, "task-abc");
        assert_eq!(e.agent, "mur");
        assert_eq!(e.situation, "scheduled");
        assert_eq!(
            e.body, "該吃早餐了 🥐",
            "the response placeholder must not reach the notification body"
        );
    }

    /// A file that is not ours yields `None` rather than a notification built
    /// from guesses.
    #[test]
    fn a_file_that_is_not_ours_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("stray.md");
        std::fs::write(&p, "# just a markdown file\n\nno front matter here").unwrap();
        assert!(parse_entry("mur", &p).is_none());
    }

    /// Only ids that were actually notified are recorded, and `catch_up` skips
    /// them next time. Without this a restart re-announces every reminder the
    /// user has already seen.
    #[test]
    fn a_recorded_id_is_not_offered_again() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(load_seen(home).is_empty());
        mark_seen(home, "task-abc").unwrap();
        mark_seen(home, "task-def").unwrap();
        let seen = load_seen(home);
        assert!(seen.contains("task-abc") && seen.contains("task-def"));
        assert_eq!(seen.len(), 2, "the marker must not accumulate blank lines");
    }

    /// Inbox discovery finds an agent's directory and ignores an agent that has
    /// never had one.
    #[test]
    fn only_agents_with_an_inbox_are_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::fs::create_dir_all(agents.join("has-one/companion/inbox")).unwrap();
        std::fs::create_dir_all(agents.join("has-none")).unwrap();
        let found: Vec<_> = inbox_dirs(tmp.path()).into_iter().map(|(a, _)| a).collect();
        assert_eq!(found, vec!["has-one".to_string()]);
    }
}
