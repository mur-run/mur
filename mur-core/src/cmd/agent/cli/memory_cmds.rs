//! `/remember` `/memories` `/forget` — the TUI surface of the memory
//! behavioral layer (federation P2b). Pure functions that return display
//! strings; the slash dispatch just prints them. Writes stay AGENT-LOCAL:
//! destroying shared knowledge belongs to `mur notes`, not a chat pane.

use anyhow::{Context, Result, bail};
use std::path::Path;

use mur_common::skill::lifecycle::{NoteKind, note_kind};
use mur_common::skill::loader::{SkillScope, load_all};
use mur_common::skill::note::{NoteSpec, note_manifest};
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::store::agent_skill_dir;

/// `/remember [--kind rule|fact] <text…>` — save an agent-local Draft note.
/// The user typed it themselves, so there is no confirmation dance and the
/// provenance is human.
pub fn remember(home: &Path, agent: &str, args: &[String]) -> Result<String> {
    let mut kind = NoteKind::Fact;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter().map(String::as_str);
    while let Some(w) = it.next() {
        if w == "--kind" {
            match it.next() {
                Some("rule") => kind = NoteKind::Rule,
                Some("fact") => kind = NoteKind::Fact,
                other => bail!("--kind expects rule|fact, got {other:?}"),
            }
        } else {
            words.push(w);
        }
    }
    let body = words.join(" ");
    if body.trim().is_empty() {
        bail!("usage: /remember [--kind rule|fact] <text>");
    }
    // Timestamped name: collision-free enough for a human-driven command.
    // ponytail: no slug generation — meaningful names come from agents or
    // `mur notes create`; rename later if it matters.
    let name = format!("note-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let description: String = body.chars().take(60).collect();

    let dir = agent_skill_dir(home, agent).join(&name);
    if dir.join("skill.yaml").exists() {
        bail!("memory '{name}' already exists — try again in a second");
    }
    let manifest = note_manifest(&NoteSpec {
        name: &name,
        description: &description,
        body: &body,
        kind,
        publisher: "human:local",
    });
    mur_common::skill::validate(&manifest).context("invalid note")?;
    mur_common::skill::store::write_to_dir(&dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write note: {e}"))?;
    let stats = SkillStats::new(&name, "1.0.0", "", chrono::Utc::now());
    std::fs::write(
        SkillStats::path_agent(home, agent, &name),
        serde_json::to_string(&stats)?,
    )?;
    Ok(format!(
        "📝 remembered ({}, Draft, agent-local): {description} — undo with /forget {name}",
        kind_str(kind)
    ))
}

/// `/memories` — every note this agent can see, labeled by where it lives
/// (agent-local / federated cache / shared), with kind and maturity.
pub fn memories(home: &Path, agent: &str) -> String {
    let cache_root = home.join("agents").join(agent).join("knowledge_cache");
    let mut rows: Vec<String> = Vec::new();
    for s in load_all(home, agent) {
        let Some(kind) = note_kind(&s.manifest) else {
            continue;
        };
        let (scope_label, stats_path) = match s.scope {
            SkillScope::Agent => ("agent", SkillStats::path_agent(home, agent, &s.name)),
            SkillScope::Global if s.dir.starts_with(&cache_root) => {
                ("federated", SkillStats::path(home, &s.name))
            }
            SkillScope::Global => ("shared", SkillStats::path(home, &s.name)),
        };
        let state = SkillStats::load(&stats_path)
            .ok()
            .flatten()
            .map(|st| st.lifecycle_state)
            .unwrap_or(LifecycleState::Draft);
        if state == LifecycleState::Destroyed {
            continue; // forgotten — stays invisible here too
        }
        rows.push(format!(
            "  {:<28} {:<5} {:<9} {:<10} {}",
            s.name,
            kind_str(kind),
            scope_label,
            format!("{state:?}"),
            s.manifest.description
        ));
    }
    if rows.is_empty() {
        return "no memories yet — /remember <text>, or agents save them mid-chat".into();
    }
    format!(
        "memories visible to this agent (agent-local · federated · shared):\n{}",
        rows.join("\n")
    )
}

/// `/forget <name|last>` — demote an AGENT-LOCAL note to `Destroyed`, which
/// removes it from injection everywhere it is read. Shared notes are
/// deliberately out of reach from a chat pane.
pub fn forget(home: &Path, agent: &str, target: Option<&str>) -> Result<String> {
    let target = target.ok_or_else(|| anyhow::anyhow!("usage: /forget <name|last>"))?;
    let name = if target == "last" {
        load_all(home, agent)
            .into_iter()
            .filter(|s| s.scope == SkillScope::Agent && note_kind(&s.manifest).is_some())
            .filter(|s| {
                SkillStats::load(&SkillStats::path_agent(home, agent, &s.name))
                    .ok()
                    .flatten()
                    .is_none_or(|st| st.lifecycle_state != LifecycleState::Destroyed)
            })
            .max_by_key(|s| s.manifest.updated_at)
            .map(|s| s.name)
            .ok_or_else(|| anyhow::anyhow!("no agent-local memories to forget"))?
    } else {
        target.to_string()
    };
    let stats_path = SkillStats::path_agent(home, agent, &name);
    let mut stats = SkillStats::load(&stats_path)?.ok_or_else(|| {
        anyhow::anyhow!("no agent-local memory named '{name}' (shared notes: use `mur notes`)")
    })?;
    stats.lifecycle_state = LifecycleState::Destroyed;
    stats.lifecycle_changed_at = chrono::Utc::now();
    std::fs::write(&stats_path, serde_json::to_string(&stats)?)?;
    Ok(format!("forgot '{name}' — it will no longer be injected"))
}

fn kind_str(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Rule => "rule",
        NoteKind::Fact => "fact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_memories_forget_cycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();

        let msg = remember(
            home,
            "a1",
            &[
                "--kind".into(),
                "rule".into(),
                "reply".into(),
                "in".into(),
                "zh-TW".into(),
            ],
        )
        .unwrap();
        assert!(msg.contains("rule") && msg.contains("/forget"));

        let listing = memories(home, "a1");
        assert!(listing.contains("reply in zh-TW"));
        assert!(listing.contains("agent"));

        let gone = forget(home, "a1", Some("last")).unwrap();
        assert!(gone.contains("forgot"));
        assert!(
            !memories(home, "a1").contains("reply in zh-TW"),
            "forgotten note must disappear from /memories"
        );
    }

    #[test]
    fn forget_refuses_shared_notes_and_empty_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        assert!(forget(home, "a1", None).is_err());
        // a GLOBAL note exists but no agent-local one: 'last' finds nothing,
        // and naming it directly reports the agent-local miss.
        let dir = home.join("skills/shared-note");
        let m = note_manifest(&NoteSpec {
            name: "shared-note",
            description: "d",
            body: "b",
            kind: NoteKind::Fact,
            publisher: "human:t",
        });
        mur_common::skill::store::write_to_dir(&dir, &m).unwrap();
        assert!(forget(home, "a1", Some("last")).is_err());
        let err = forget(home, "a1", Some("shared-note"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("mur notes"),
            "must point at the right tool: {err}"
        );
    }

    #[test]
    fn remember_rejects_empty_and_bad_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(remember(tmp.path(), "a1", &[]).is_err());
        assert!(
            remember(
                tmp.path(),
                "a1",
                &["--kind".into(), "opinion".into(), "x".into()]
            )
            .is_err()
        );
    }
}
