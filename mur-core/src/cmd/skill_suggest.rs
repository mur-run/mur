//! `mur skill suggest` — scan recordings, detect repeat task patterns >=3 times.

use anyhow::{Context, Result};
use std::path::Path;

use crate::capture::emergence::{detect_emergent, extract_fingerprints};

pub struct SuggestOptions {
    /// Max most-recent sessions to scan. Default 20.
    pub max_sessions: usize,
    /// Minimum distinct sessions a pattern must appear in. Default 3.
    pub threshold: usize,
}

pub fn cmd_suggest(home: &Path, opts: SuggestOptions) -> Result<()> {
    let recordings_dir = home.join("session").join("recordings");
    if !recordings_dir.exists() {
        println!("No session recordings found.");
        return Ok(());
    }

    // Collect recent recording paths, sorted by modification time desc.
    let mut paths: Vec<_> = std::fs::read_dir(&recordings_dir)
        .context("read recordings dir")?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    paths.sort_by_key(|e| {
        std::cmp::Reverse(
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    paths.truncate(opts.max_sessions);

    if paths.is_empty() {
        println!("No session recordings found.");
        return Ok(());
    }

    // Phase 1: extract fingerprints from each session.
    let mut all_fingerprints = Vec::new();
    for entry in &paths {
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("read {}", entry.path().display()))?;
        if content.trim().is_empty() {
            continue;
        }
        let id = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let fps = extract_fingerprints(&content, &id);
        all_fingerprints.extend(fps);
    }

    // Phase 2: detect emergent patterns (threshold >=3 sessions by default).
    let candidates = detect_emergent(&all_fingerprints, opts.threshold);

    if candidates.is_empty() {
        println!(
            "No repeat patterns detected across {} sessions (threshold: {}).",
            paths.len(),
            opts.threshold,
        );
        return Ok(());
    }

    // Phase 3: print suggestions.
    println!(
        "Found {} repeat pattern(s) across {} sessions:\n",
        candidates.len(),
        paths.len(),
    );
    for (i, c) in candidates.iter().enumerate() {
        println!(
            "{}. {} ({} sessions)",
            i + 1,
            c.suggested_name,
            c.session_count
        );
        println!("   behavior: {}", c.behavior);
        if !c.keywords.is_empty() {
            println!("   keywords: {}", c.keywords.join(", "));
        }
        if let Some(first_session) = c.session_ids.first() {
            println!(
                "   generate: mur skill generate --from-session {}",
                first_session
            );
        }
        println!();
    }
    Ok(())
}
