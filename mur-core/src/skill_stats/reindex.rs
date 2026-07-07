//! Rebuild `stats.json` sidecars from the JSONL trace log.
//! Stats sidecars are caches; the JSONL trace log is the source of truth.

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::skill::stats::SkillStats;
use std::path::Path;

/// Default trace window for rebuilds (matches the CLI default).
pub const DEFAULT_DAYS_BACK: u32 = 30;

pub struct ReindexOptions {
    pub skill_filter: Option<String>,
    #[allow(dead_code)]
    pub since: Option<DateTime<Utc>>,
    pub days_back: u32,
}

pub struct ReindexReport {
    pub skills_touched: usize,
    pub lines_consumed: usize,
}

/// Rebuild stats for installed skills by scanning trace JSONL files.
pub fn reindex_stats(mur_home: &Path, opts: ReindexOptions) -> Result<ReindexReport> {
    let traces_dir = mur_home.join("traces");
    let today = Utc::now();

    // Enumerate traces from newest day back
    let mut trace_paths: Vec<_> = Vec::new();
    for d in 0..opts.days_back.max(1) {
        let day = today - chrono::Duration::days(d as i64);
        let path = traces_dir
            .join(day.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        if path.exists() {
            trace_paths.push(path);
        }
    }
    // Sort newest first so we process in chronological order
    trace_paths.sort();

    let mut lines_consumed: usize = 0;
    let mut skills_touched: usize = 0;

    // Collect installed skill names
    let skills_dir = mur_home.join("skills");
    let installed_names: Vec<String> = if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    } else {
        Vec::new()
    };

    let target_names: Vec<String> = if let Some(ref filter) = opts.skill_filter {
        // Simple glob: if filter contains * or ?, do glob matching; else exact
        if filter.contains('*') || filter.contains('?') {
            let pat = glob_pattern(filter);
            installed_names
                .into_iter()
                .filter(|n| pat.matches(n))
                .collect()
        } else {
            installed_names
                .into_iter()
                .filter(|n| n == filter)
                .collect()
        }
    } else {
        installed_names
    };

    // For each target skill, find relevant manifest for version/digest
    for skill_name in &target_names {
        let skill_dir = skills_dir.join(skill_name);
        let manifest_path = skill_dir.join("skill.yaml");
        let manifest_digest = if manifest_path.exists() {
            std::fs::read_to_string(&manifest_path)
                .ok()
                .map(|s| mur_common::skill::sha256_hex(s.as_bytes()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let _version = "unknown".to_string(); // could parse manifest, but reindex counts only

        // Rebuild stats from scratch
        let stats_path = SkillStats::path(mur_home, skill_name);
        let mut fresh = SkillStats::new(skill_name, "unknown", &manifest_digest, Utc::now());

        for trace_path in &trace_paths {
            let content = match std::fs::read_to_string(trace_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Count skill executions and note retrievals; both carry
                // mur.skill.name + mur.skill.outcome.
                if !trimmed.contains("mur.skill.executed")
                    && !trimmed.contains("mur.note.retrieved")
                    && !trimmed.contains("mur.skill.curated")
                {
                    continue;
                }
                let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(trimmed) else {
                    continue;
                };
                let event_skill = val
                    .get("mur.skill.name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if event_skill != *skill_name {
                    continue;
                }
                lines_consumed += 1;

                // A curation event records human review, not a usage. Set the
                // watermark and skip the usage/outcome accounting below.
                if trimmed.contains("mur.skill.curated") {
                    if let Some(ts) = val.get("ts").and_then(|v| v.as_str())
                        && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts)
                    {
                        let utc = parsed.with_timezone(&Utc);
                        fresh.curated_at = Some(match fresh.curated_at {
                            Some(e) => e.max(utc),
                            None => utc,
                        });
                    }
                    continue;
                }

                let outcome = val
                    .get("mur.skill.outcome")
                    .and_then(|v| v.as_str())
                    .unwrap_or("not_evaluated");

                fresh.usage_count += 1;
                if outcome == "success" {
                    fresh.success_count += 1;
                } else if outcome == "failure" {
                    fresh.failure_count += 1;
                }
                // Update timestamps from the trace line
                if let Some(ts) = val.get("ts").and_then(|v| v.as_str())
                    && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts)
                {
                    let utc = parsed.with_timezone(&Utc);
                    fresh.last_used_at = Some(match fresh.last_used_at {
                        Some(e) => e.max(utc),
                        None => utc,
                    });
                    if outcome == "success" {
                        fresh.last_success_at = Some(match fresh.last_success_at {
                            Some(e) => e.max(utc),
                            None => utc,
                        });
                        fresh.first_successful_use_at = Some(match fresh.first_successful_use_at {
                            Some(e) => e.min(utc),
                            None => utc,
                        });
                    }
                }
            }
        }

        // Write the rebuilt stats
        let default = || SkillStats::new(skill_name, "unknown", &manifest_digest, Utc::now());
        SkillStats::merge_in_place(&stats_path, default, |existing| {
            existing.usage_count = fresh.usage_count;
            existing.success_count = fresh.success_count;
            existing.failure_count = fresh.failure_count;
            if fresh.last_used_at.is_some() {
                existing.last_used_at = fresh.last_used_at;
            }
            if fresh.last_success_at.is_some() {
                existing.last_success_at = fresh.last_success_at;
            }
            if fresh.first_successful_use_at.is_some() {
                existing.first_successful_use_at = fresh.first_successful_use_at;
            }
            if fresh.curated_at.is_some() {
                existing.curated_at = fresh.curated_at;
            }
            existing.rebuilt_from_trace_through = Some(today);
            Ok(())
        })?;

        skills_touched += 1;
    }

    Ok(ReindexReport {
        skills_touched,
        lines_consumed,
    })
}

/// Simple glob matching for skill name filters.
pub struct GlobPattern {
    pattern: String,
}

impl GlobPattern {
    pub fn matches(&self, name: &str) -> bool {
        // Simple glob: * matches anything, ? matches single char
        let mut chars = self.pattern.chars().peekable();
        let name_chars: Vec<char> = name.chars().collect();
        Self::match_impl(&mut chars, &name_chars, 0)
    }

    fn match_impl(
        chars: &mut std::iter::Peekable<std::str::Chars>,
        name: &[char],
        pos: usize,
    ) -> bool {
        match chars.next() {
            None => pos >= name.len(),
            Some('*') => {
                // Greedy: try matching remainder at each position
                let next = chars.next();
                match next {
                    None => true,
                    Some(c) => {
                        for i in pos..name.len() {
                            if name[i] == c && Self::match_impl(chars, name, i + 1) {
                                return true;
                            }
                        }
                        false
                    }
                }
            }
            Some('?') => pos < name.len() && Self::match_impl(chars, name, pos + 1),
            Some(expected) => {
                pos < name.len() && name[pos] == expected && Self::match_impl(chars, name, pos + 1)
            }
        }
    }
}

pub fn glob_pattern(s: &str) -> GlobPattern {
    GlobPattern {
        pattern: s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact_match() {
        let p = glob_pattern("hello");
        assert!(p.matches("hello"));
        assert!(!p.matches("world"));
    }

    #[test]
    fn glob_star() {
        let p = glob_pattern("research-*");
        assert!(p.matches("research-patterns"));
        assert!(p.matches("research-"));
        assert!(!p.matches("skill-research"));
    }

    #[test]
    fn glob_question() {
        let p = glob_pattern("test-?");
        assert!(p.matches("test-a"));
        assert!(p.matches("test-1"));
        assert!(!p.matches("test-ab"));
    }

    #[test]
    fn reindex_counts_note_retrieval_events_as_usage_and_success() {
        use chrono::Utc;
        use mur_common::skill::stats::SkillStats;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let now = Utc::now();

        // A note skill must exist on disk for reindex to consider it.
        let dir = tmp.path().join("skills").join("my-note");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("skill.yaml"),
            "name: my-note\nversion: 1.0.0\npublisher: human:test\n\
             category: note\ndescription: d\ncontent:\n  abstract: a\n  note: b\n",
        )
        .unwrap();

        // Three retrieval lines in today's trace file.
        let traces_dir = tmp.path().join("traces");
        std::fs::create_dir_all(&traces_dir).unwrap();
        let trace_path = traces_dir
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let line = format!(
            "{{\"ts\":\"{}\",\"method\":\"mur.note.retrieved\",\
             \"mur.skill.name\":\"my-note\",\"mur.skill.outcome\":\"success\"}}",
            now.to_rfc3339()
        );
        std::fs::write(&trace_path, format!("{line}\n{line}\n{line}\n")).unwrap();

        reindex_stats(
            tmp.path(),
            ReindexOptions {
                skill_filter: Some("my-note".into()),
                since: None,
                days_back: 1,
            },
        )
        .unwrap();

        let stats = SkillStats::load(&SkillStats::path(tmp.path(), "my-note"))
            .unwrap()
            .unwrap();
        assert_eq!(stats.usage_count, 3);
        assert_eq!(stats.success_count, 3);
    }

    #[test]
    fn reindex_sets_curated_at_without_counting_usage() {
        use mur_common::skill::stats::SkillStats;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Minimal installed skill dir so reindex enumerates it.
        std::fs::create_dir_all(home.join("skills").join("my-skill")).unwrap();
        std::fs::write(
            home.join("skills").join("my-skill").join("skill.yaml"),
            "name: my-skill\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: note\nprovenance: llm\ncontent:\n  abstract: a\n  note: \"b\"\n",
        )
        .unwrap();

        // One curated event in today's trace log.
        let today = chrono::Utc::now();
        let traces = home.join("traces");
        std::fs::create_dir_all(&traces).unwrap();
        let line = format!(
            "{{\"ts\":\"{}\",\"method\":\"mur.skill.curated\",\"mur.skill.name\":\"my-skill\"}}",
            today.to_rfc3339()
        );
        std::fs::write(
            traces
                .join(today.format("%Y-%m-%d").to_string())
                .with_extension("jsonl"),
            format!("{line}\n"),
        )
        .unwrap();

        reindex_stats(
            home,
            ReindexOptions {
                skill_filter: Some("my-skill".into()),
                since: None,
                days_back: 1,
            },
        )
        .unwrap();

        let stats = SkillStats::load(&SkillStats::path(home, "my-skill"))
            .unwrap()
            .unwrap();
        assert!(
            stats.curated_at.is_some(),
            "curated event should set curated_at"
        );
        assert_eq!(stats.usage_count, 0, "curation is not a usage");
        assert_eq!(stats.success_count, 0);
    }
}
