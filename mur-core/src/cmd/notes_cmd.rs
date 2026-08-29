//! `mur notes` CLI handlers — MVP create + search.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use mur_common::skill::lifecycle::NoteKind;
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::store::{global_skill_dir, read_from_dir, write_to_dir};
use mur_common::skill::types::Category;
use mur_common::skill::validate;
use mur_common::telemetry::METHOD_NOTE_RETRIEVED;

/// Author identity stamped onto notes created via the local CLI.
/// Plan-marker: later plans may swap this for a config-driven value.
const DEFAULT_PUBLISHER: &str = "human:local";

/// Build a `category: note` skill at `<mur_home>/skills/<name>/skill.yaml`.
/// Returns the path written.
///
/// Errors:
/// - if the target skill directory already contains a `skill.yaml` (duplicate name)
/// - if the resulting manifest fails `mur_common::skill::validate::validate`
pub fn do_create(
    mur_home: &Path,
    name: &str,
    description: &str,
    body: &str,
    kind: NoteKind,
) -> Result<PathBuf> {
    let dir = global_skill_dir(mur_home, name);
    if dir.join("skill.yaml").exists() {
        bail!("note '{name}' already exists at {}", dir.display());
    }

    let manifest = mur_common::skill::note::note_manifest(&mur_common::skill::note::NoteSpec {
        name,
        description,
        body,
        kind,
        publisher: DEFAULT_PUBLISHER,
    });

    validate(&manifest).with_context(|| format!("validate note '{name}'"))?;
    let written =
        write_to_dir(&dir, &manifest).with_context(|| format!("write skill.yaml for '{name}'"))?;
    Ok(written)
}

use std::io::Read;

use super::agent::resolve_mur_home;
use crate::retrieve::scoring::{Scored, score_and_rank_generic};
use crate::retrieve::skill_candidates::{LoadedSkill, load_skill_candidates};

/// Top-level `mur notes create` handler.
pub fn cmd_create(
    name: &str,
    description: &str,
    body_file: Option<&Path>,
    kind: &str,
) -> Result<()> {
    let kind = parse_note_kind(kind)?;
    let body = match body_file {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("read body file {}", p.display()))?
        }
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read body from stdin")?;
            s
        }
    };
    let home = resolve_mur_home()?;
    let path = do_create(&home, name, description, &body, kind)?;
    println!("Created note '{}' at {}", name, path.display());
    Ok(())
}

/// Top-level `mur notes search` handler.
pub fn cmd_search(query: &str, limit: usize) -> Result<()> {
    let home = resolve_mur_home()?;
    let ranked = do_search(&home, query, limit)?;
    if ranked.is_empty() {
        println!(
            "No global notes match '{query}'. (This searches global notes only; an agent's \
             own memories live under its home — see `mur notes list --agent <name>`.)"
        );
        return Ok(());
    }
    for (i, sp) in ranked.iter().enumerate() {
        println!(
            "{:>2}. {:<40} score={:.3}  {}",
            i + 1,
            sp.item.manifest.name,
            sp.score,
            sp.item.manifest.description
        );
    }

    // Record a retrieval for each surfaced note so it accrues lifecycle usage.
    // Best-effort: a trace-write failure must not fail the search.
    let now = Utc::now();
    for sp in &ranked {
        if let Err(e) = record_retrieval(&home, &sp.item.manifest.name, now) {
            tracing::warn!(note = %sp.item.manifest.name, error = %e, "record retrieval failed");
        }
    }
    Ok(())
}

/// Search `~/.mur/skills/` for `category: note` skills matching `query`.
/// Returns up to `limit` ranked results (Scored<LoadedSkill>).
pub fn do_search(mur_home: &Path, query: &str, limit: usize) -> Result<Vec<Scored<LoadedSkill>>> {
    let skills_dir = mur_home.join("skills");
    let all = load_skill_candidates(&skills_dir, mur_home)?;
    let notes: Vec<LoadedSkill> = all
        .into_iter()
        .filter(|s| s.manifest.category == Category::Note)
        .collect();
    let mut ranked = score_and_rank_generic(query, notes);
    ranked.truncate(limit);
    Ok(ranked)
}

/// Append a retrieval event for `skill_name` to today's trace log so the stats
/// reducer (`reindex_stats`) counts it as a successful usage. The trace log is
/// the source of truth for stats, so retrievals are recorded here rather than
/// written directly to the stats sidecar (which `reindex-stats` would overwrite).
pub fn record_retrieval(mur_home: &Path, skill_name: &str, now: DateTime<Utc>) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_NOTE_RETRIEVED,
        "mur.skill.name": skill_name,
        "mur.skill.outcome": "success",
    });

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    use std::io::Write;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// Top-level `mur notes list` handler.
pub fn cmd_list(maturity: Option<&str>, limit: usize, agent: Option<&str>) -> Result<()> {
    let home = resolve_mur_home()?;
    // `--agent` reuses the `/memories` renderer rather than growing a second
    // one: the TUI already labels agent-local · federated · shared correctly,
    // and two listings of one fact are how they start disagreeing.
    if let Some(a) = agent {
        println!(
            "{}",
            crate::cmd::agent::cli::memory_cmds::memories(&home, a)
        );
        return Ok(());
    }
    let filter = maturity.map(parse_maturity).transpose()?;
    let rows = do_list(&home, filter, limit)?;
    if rows.is_empty() {
        // Naming the scope matters: an agent's own memories live under its
        // home and are invisible here, so a bare "No notes found." reads as
        // "you have no memories" when it means "none are global".
        println!(
            "No global notes found. (An agent's own memories are not listed here — try `mur notes list --agent <name>`.)"
        );
        return Ok(());
    }
    for r in &rows {
        println!(
            "{:<40} {:<11} {}",
            r.name,
            format!("{:?}", r.maturity),
            r.description
        );
    }
    Ok(())
}

/// Top-level `mur notes show` handler.
pub fn cmd_show(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let view = do_show(&home, name)?;
    println!("# {}", view.name);
    println!("{}", view.description);
    println!("maturity: {:?}", view.maturity);
    println!("kind: {:?}\n", view.kind);
    println!("{}", view.body);

    // Viewing a note is a retrieval — best-effort, never fail the read.
    if let Err(e) = record_retrieval(&home, &view.name, Utc::now()) {
        tracing::warn!(note = %view.name, error = %e, "record retrieval failed");
    }
    Ok(())
}

/// One row of `mur notes list`.
#[derive(Debug, Clone)]
pub struct NoteListRow {
    pub name: String,
    pub maturity: LifecycleState,
    pub description: String,
}

/// Parse a CLI `--kind` value. Only the two kinds exist; anything else is a
/// user error, not a default.
pub fn parse_note_kind(s: &str) -> Result<NoteKind> {
    match s.to_ascii_lowercase().as_str() {
        "rule" => Ok(NoteKind::Rule),
        "fact" => Ok(NoteKind::Fact),
        other => bail!("unknown note kind '{other}' (expected: rule | fact)"),
    }
}

/// Parse a `--maturity` value (case-insensitive) into a `LifecycleState`.
pub fn parse_maturity(s: &str) -> Result<LifecycleState> {
    match s.to_lowercase().as_str() {
        "draft" => Ok(LifecycleState::Draft),
        "emerging" => Ok(LifecycleState::Emerging),
        "stable" => Ok(LifecycleState::Stable),
        "canonical" => Ok(LifecycleState::Canonical),
        "deprecated" => Ok(LifecycleState::Deprecated),
        "archived" => Ok(LifecycleState::Archived),
        other => bail!(
            "unknown maturity '{other}' \
             (expected draft|emerging|stable|canonical|deprecated|archived)"
        ),
    }
}

/// `mur notes remove <name> [--agent <a>]`.
///
/// A note IS a `category: note` skill, so removing a global one has always been
/// possible — as `mur skill remove`. That is true, discoverable nowhere, and
/// the command everyone reaches for first (`mur notes remove`) failed with
/// `unrecognized subcommand`. Agent-local memories route to the same demotion
/// `/forget` performs, so a memory removed from the CLI and one removed from a
/// chat pane end in the same state rather than two.
pub fn cmd_remove(name: &str, agent: Option<&str>) -> Result<()> {
    let home = resolve_mur_home()?;
    match agent {
        Some(a) => {
            let msg = crate::cmd::agent::cli::memory_cmds::forget(&home, a, Some(name))?;
            println!("{msg}");
        }
        None => {
            let dir = global_skill_dir(&home, name);
            let manifest = read_from_dir(&dir).map_err(|_| anyhow!("note '{name}' not found"))?;
            if manifest.category != Category::Note {
                bail!(
                    "'{name}' is not a note (category: {:?}) — use `mur skill remove` if you \
                     meant to uninstall the skill",
                    manifest.category
                );
            }
            std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
            println!("removed note '{name}'");
        }
    }
    Ok(())
}

/// A note rendered for `mur notes show`.
#[derive(Debug, Clone)]
pub struct NoteView {
    pub name: String,
    pub description: String,
    pub maturity: LifecycleState,
    pub kind: NoteKind,
    pub body: String,
}

/// Load a single note for display. Errors if the skill is missing or not a note.
pub fn do_show(mur_home: &Path, name: &str) -> Result<NoteView> {
    let dir = global_skill_dir(mur_home, name);
    let manifest = read_from_dir(&dir).map_err(|_| anyhow!("note '{name}' not found"))?;
    if manifest.category != Category::Note {
        bail!("'{name}' is not a note (category: {:?})", manifest.category);
    }
    let maturity = SkillStats::load(&SkillStats::path(mur_home, name))?
        .map(|s| s.lifecycle_state)
        .unwrap_or_default();
    let body = manifest.content.note.clone().unwrap_or_default();
    // Notes always have a kind; Fact is the tagless default.
    let kind = mur_common::skill::lifecycle::note_kind(&manifest).unwrap_or(NoteKind::Fact);
    Ok(NoteView {
        name: manifest.name,
        description: manifest.description,
        maturity,
        kind,
        body,
    })
}

/// List `category: note` skills, optionally filtered by maturity, sorted by name.
pub fn do_list(
    mur_home: &Path,
    maturity: Option<LifecycleState>,
    limit: usize,
) -> Result<Vec<NoteListRow>> {
    let skills_dir = mur_home.join("skills");
    let all = load_skill_candidates(&skills_dir, mur_home)?;
    let mut rows: Vec<NoteListRow> = all
        .into_iter()
        .filter(|s| s.manifest.category == Category::Note)
        .filter(|s| maturity.is_none_or(|m| s.stats.lifecycle_state == m))
        .map(|s| NoteListRow {
            name: s.manifest.name.clone(),
            maturity: s.stats.lifecycle_state,
            description: s.manifest.description.clone(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows.truncate(limit);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::parser::parse_canonical;
    use tempfile::tempdir;

    #[test]
    fn do_create_writes_a_well_formed_note_skill() {
        let tmp = tempdir().unwrap();
        let path = do_create(
            tmp.path(),
            "rust-error-handling",
            "Rust error handling reference",
            "# Rust Error Handling\n\nUse anyhow for app errors.",
            NoteKind::Fact,
        )
        .unwrap();

        assert!(path.ends_with("skills/rust-error-handling/skill.yaml"));
        let yaml = std::fs::read_to_string(&path).unwrap();
        let m = parse_canonical(&yaml).unwrap();

        assert_eq!(m.name, "rust-error-handling");
        assert_eq!(m.category, Category::Note);
        assert_eq!(m.content.r#abstract, "Rust error handling reference");
        assert_eq!(
            m.content.note.as_deref(),
            Some("# Rust Error Handling\n\nUse anyhow for app errors.")
        );
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rule_kind_lands_in_tags_and_roundtrips() {
        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "always-zh",
            "reply language",
            "reply in zh-TW",
            NoteKind::Rule,
        )
        .unwrap();
        let view = do_show(tmp.path(), "always-zh").unwrap();
        assert_eq!(view.kind, NoteKind::Rule);
        // and a plain note stays a fact
        do_create(
            tmp.path(),
            "os-ver",
            "environment fact",
            "macOS 15",
            NoteKind::Fact,
        )
        .unwrap();
        assert_eq!(do_show(tmp.path(), "os-ver").unwrap().kind, NoteKind::Fact);
    }

    #[test]
    fn do_create_rejects_duplicate_name() {
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "dup", "d", "body", NoteKind::Fact).unwrap();
        let err = do_create(tmp.path(), "dup", "d", "body", NoteKind::Fact).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn do_create_rejects_invalid_name() {
        let tmp = tempdir().unwrap();
        // Uppercase letters violate validate_name (ascii_lowercase only).
        let err = do_create(tmp.path(), "BadName", "d", "body", NoteKind::Fact).unwrap_err();
        assert!(err.to_string().contains("validate") || err.to_string().contains("name"));
    }

    #[test]
    fn do_search_filters_out_non_note_skills() {
        use std::fs;
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");

        // A genuine note (created via do_create).
        do_create(
            tmp.path(),
            "deploy-fly",
            "Deploy to Fly.io",
            "# fly deploy steps",
            NoteKind::Fact,
        )
        .unwrap();

        // A non-note (category: context) hand-written to the same skills dir.
        let ctx_dir = skills_dir.join("context-thing");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(
            ctx_dir.join("skill.yaml"),
            "name: context-thing\nversion: 1.0.0\npublisher: human:test\n\
             category: context\ndescription: deploy context\n\
             content:\n  abstract: deploy fly\n  context: details\n",
        )
        .unwrap();

        let ranked = do_search(tmp.path(), "deploy fly", 10).unwrap();
        let names: Vec<_> = ranked
            .iter()
            .map(|s| s.item.manifest.name.clone())
            .collect();
        assert!(names.contains(&"deploy-fly".to_string()));
        assert!(!names.contains(&"context-thing".to_string()));
    }

    #[test]
    fn do_search_respects_limit_and_orders_by_score() {
        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "rust-anyhow",
            "Anyhow for rust apps",
            "# anyhow\nuse anyhow for application errors",
            NoteKind::Fact,
        )
        .unwrap();
        do_create(
            tmp.path(),
            "rust-thiserror",
            "thiserror for libraries",
            "# thiserror\nuse thiserror for library errors",
            NoteKind::Fact,
        )
        .unwrap();
        do_create(
            tmp.path(),
            "unrelated-brew",
            "homebrew update",
            "# brew\nrun brew update weekly",
            NoteKind::Fact,
        )
        .unwrap();

        let ranked = do_search(tmp.path(), "rust anyhow application errors", 2).unwrap();
        assert!(ranked.len() <= 2);
        assert_eq!(
            ranked[0].item.manifest.name, "rust-anyhow",
            "rust-anyhow should rank above rust-thiserror for this query"
        );
        if ranked.len() == 2 {
            assert!(ranked[0].score >= ranked[1].score);
        }
    }

    #[test]
    fn do_search_returns_empty_when_no_notes_exist() {
        let tmp = tempdir().unwrap();
        let ranked = do_search(tmp.path(), "anything", 10).unwrap();
        assert!(ranked.is_empty());
    }

    #[test]
    fn end_to_end_create_two_notes_then_search_returns_them_ranked() {
        let tmp = tempdir().unwrap();

        do_create(
            tmp.path(),
            "fly-deploy",
            "Deploy a Rust app to Fly.io",
            "# Deploy Steps\n1. cargo build --release\n2. fly deploy",
            NoteKind::Fact,
        )
        .unwrap();

        do_create(
            tmp.path(),
            "brew-tips",
            "Homebrew maintenance",
            "# Brew\nRun brew update weekly to keep formulae fresh.",
            NoteKind::Fact,
        )
        .unwrap();

        let ranked = do_search(tmp.path(), "deploy rust fly", 10).unwrap();
        assert!(
            !ranked.is_empty(),
            "search should find at least the deploy note"
        );
        assert_eq!(ranked[0].item.manifest.name, "fly-deploy");
        // brew-tips may or may not pass the score floor; if it did, it must rank below.
        if ranked.len() > 1 {
            assert!(ranked[0].score > ranked[1].score);
        }

        // Re-running create with the same name fails — proves duplicate detection survives a real flow.
        let err = do_create(tmp.path(), "fly-deploy", "x", "y", NoteKind::Fact).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn record_retrieval_appends_a_countable_trace_line() {
        use chrono::Utc;
        let tmp = tempdir().unwrap();
        let now = Utc::now();

        record_retrieval(tmp.path(), "my-note", now).unwrap();

        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("mur.note.retrieved"));

        let line = content.lines().next().unwrap();
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            val.get("mur.skill.name").and_then(|v| v.as_str()),
            Some("my-note")
        );
        assert_eq!(
            val.get("mur.skill.outcome").and_then(|v| v.as_str()),
            Some("success")
        );
    }

    #[test]
    fn record_retrieval_appends_not_overwrites() {
        use chrono::Utc;
        let tmp = tempdir().unwrap();
        let now = Utc::now();
        record_retrieval(tmp.path(), "n", now).unwrap();
        record_retrieval(tmp.path(), "n", now).unwrap();
        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
    }

    #[test]
    fn do_list_returns_notes_sorted_by_name_with_maturity() {
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "zebra", "z note", "body", NoteKind::Fact).unwrap();
        do_create(tmp.path(), "alpha", "a note", "body", NoteKind::Fact).unwrap();

        let rows = do_list(tmp.path(), None, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[1].name, "zebra");
        assert_eq!(rows[0].maturity, LifecycleState::Draft);
    }

    #[test]
    fn do_list_filters_by_maturity() {
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "n1", "d", "body", NoteKind::Fact).unwrap();
        // Fresh notes are Draft.
        assert!(
            do_list(tmp.path(), Some(LifecycleState::Stable), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            do_list(tmp.path(), Some(LifecycleState::Draft), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn do_list_excludes_non_note_skills() {
        use std::fs;
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "real-note", "d", "body", NoteKind::Fact).unwrap();
        let ctx = tmp.path().join("skills").join("ctx");
        fs::create_dir_all(&ctx).unwrap();
        fs::write(
            ctx.join("skill.yaml"),
            "name: ctx\nversion: 1.0.0\npublisher: human:test\n\
             category: context\ndescription: d\ncontent:\n  abstract: a\n  context: c\n",
        )
        .unwrap();

        let rows = do_list(tmp.path(), None, 10).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            vec!["real-note"]
        );
    }

    #[test]
    fn do_show_returns_a_note_view() {
        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "my-note",
            "My description",
            "# Heading\nprose",
            NoteKind::Fact,
        )
        .unwrap();

        let v = do_show(tmp.path(), "my-note").unwrap();
        assert_eq!(v.name, "my-note");
        assert_eq!(v.description, "My description");
        assert_eq!(v.body, "# Heading\nprose");
        assert_eq!(v.maturity, LifecycleState::Draft);
    }

    #[test]
    fn do_show_errors_for_missing_note() {
        let tmp = tempdir().unwrap();
        let err = do_show(tmp.path(), "nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn do_show_errors_for_non_note_skill() {
        use std::fs;
        let tmp = tempdir().unwrap();
        let ctx = tmp.path().join("skills").join("ctx");
        fs::create_dir_all(&ctx).unwrap();
        fs::write(
            ctx.join("skill.yaml"),
            "name: ctx\nversion: 1.0.0\npublisher: human:test\n\
             category: context\ndescription: d\ncontent:\n  abstract: a\n  context: c\n",
        )
        .unwrap();

        let err = do_show(tmp.path(), "ctx").unwrap_err();
        assert!(err.to_string().contains("not a note"));
    }

    #[test]
    fn create_then_list_and_show_compose() {
        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "fly",
            "Deploy to fly",
            "# fly\nsteps",
            NoteKind::Fact,
        )
        .unwrap();
        do_create(
            tmp.path(),
            "brew",
            "Brew tips",
            "# brew\nupdate",
            NoteKind::Fact,
        )
        .unwrap();

        // list is sorted by name
        let rows = do_list(tmp.path(), None, 10).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["brew", "fly"]
        );
        assert!(rows.iter().all(|r| r.maturity == LifecycleState::Draft));

        // show returns the right body
        let v = do_show(tmp.path(), "fly").unwrap();
        assert_eq!(v.body, "# fly\nsteps");
        assert_eq!(v.description, "Deploy to fly");
    }

    #[test]
    fn parse_maturity_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(parse_maturity("Stable").unwrap(), LifecycleState::Stable);
        assert_eq!(
            parse_maturity("emerging").unwrap(),
            LifecycleState::Emerging
        );
        assert!(parse_maturity("bogus").is_err());
    }

    #[tokio::test]
    async fn three_retrievals_promote_a_note_from_draft_to_emerging() {
        use crate::skill_lifecycle::sweep::{SweepOptions, run_sweep};
        use crate::skill_stats::reindex::{ReindexOptions, reindex_stats};
        use chrono::{Duration, Utc};
        use mur_common::skill::stats::{LifecycleState, SkillStats};

        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "rust-errors",
            "Rust error handling",
            "# body\nanyhow",
            NoteKind::Fact,
        )
        .unwrap();

        // Surface the note three times.
        let now = Utc::now();
        for _ in 0..3 {
            record_retrieval(tmp.path(), "rust-errors", now).unwrap();
        }

        // Reduce the trace into stats.
        reindex_stats(
            tmp.path(),
            ReindexOptions {
                skill_filter: Some("rust-errors".into()),
                since: None,
                days_back: 1,
            },
        )
        .unwrap();

        let stats_path = SkillStats::path(tmp.path(), "rust-errors");
        let before = SkillStats::load(&stats_path).unwrap().unwrap();
        assert_eq!(before.usage_count, 3);
        assert_eq!(before.success_count, 3);
        assert_eq!(before.lifecycle_state, LifecycleState::Draft);

        // Sweep with a future `now` so the 24h MIN_DWELL_HOURS gate passes.
        run_sweep(
            tmp.path(),
            SweepOptions {
                filter: Some("rust-errors".into()),
                dry_run: false,
                now: now + Duration::days(2),
                require_human_curation_before_stable: true,
                ..SweepOptions::default()
            },
        )
        .unwrap();

        let after = SkillStats::load(&stats_path).unwrap().unwrap();
        assert_eq!(
            after.lifecycle_state,
            LifecycleState::Emerging,
            "3 retrievals + dwell should promote Draft -> Emerging (PROMOTE_DRAFT_USES=3)"
        );
    }
}
