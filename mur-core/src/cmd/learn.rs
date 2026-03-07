use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self};

use crate::evolve;
use crate::llm;
use crate::store::yaml::YamlStore;

pub(crate) async fn cmd_learn_extract(
    file: Option<String>,
    fingerprint: bool,
    use_llm: bool,
) -> Result<()> {
    crate::auth::heartbeat();
    // Read transcript
    let transcript = match file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read transcript file '{}': {}", path, e))?,
        None => {
            let mut buf = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut buf)?;
            buf
        }
    };

    if transcript.trim().is_empty() {
        println!("Empty transcript. Provide a session transcript via --file or stdin.");
        return Ok(());
    }

    // LLM-based pattern extraction
    if use_llm {
        let config = crate::store::config::load_config()?;
        let store = YamlStore::default_store()?;

        println!("Analyzing transcript with LLM ({})...", config.llm.model);

        let system = r#"You are MUR, a pattern extraction engine. Analyze the given AI assistant session transcript and extract reusable patterns.

Return a JSON array of patterns. Each pattern object has:
- "name": kebab-case identifier (e.g. "rust-error-handling")
- "description": one-line summary
- "technical": technical content (the actual pattern/rule/practice)
- "principle": optional higher-level principle behind it
- "tags": array of topic strings
- "tier": "session" | "project" | "core"
- "importance": 0.0-1.0 float

Only extract genuinely reusable patterns — skip trivial or one-off interactions.
Return ONLY the JSON array, no markdown fences or other text."#;

        // Truncate very long transcripts to fit context window
        let max_chars = 100_000;
        let truncated = if transcript.len() > max_chars {
            &transcript[..max_chars]
        } else {
            &transcript
        };

        let prompt = format!("Extract patterns from this session transcript:\n\n{truncated}");

        match llm::llm_complete(&config.llm, system, &prompt).await {
            Ok(response) => {
                let parsed = parse_llm_patterns(&response);
                if parsed.is_empty() {
                    println!("LLM returned no extractable patterns.");
                } else {
                    let mut saved = 0;
                    let existing = store.list_all()?;
                    for p in &parsed {
                        if store.exists(&p.name) {
                            println!("  Skipped '{}' (already exists)", p.name);
                            continue;
                        }
                        store.save(p)?;
                        println!("  Saved pattern: {}", p.name);
                        saved += 1;

                        // Auto-discover links
                        let suggestions = evolve::linker::discover_links(p, &existing);
                        if !suggestions.is_empty() {
                            let mut linked = store.get(&p.name)?;
                            for s in &suggestions {
                                match s.link_type {
                                    evolve::linker::LinkType::Related => {
                                        if !linked.links.related.contains(&s.target_name) {
                                            linked.links.related.push(s.target_name.clone());
                                        }
                                        if let Ok(mut target) = store.get(&s.target_name)
                                            && !target.links.related.contains(&p.name)
                                        {
                                            target.links.related.push(p.name.clone());
                                            let _ = store.save(&target);
                                        }
                                    }
                                    evolve::linker::LinkType::Supersedes => {
                                        if !linked.links.supersedes.contains(&s.target_name) {
                                            linked.links.supersedes.push(s.target_name.clone());
                                        }
                                    }
                                }
                            }
                            store.save(&linked)?;
                            println!("    🔗 Linked to {} patterns", suggestions.len());
                        }
                    }
                    println!(
                        "\nExtracted {} patterns, saved {} new.",
                        parsed.len(),
                        saved
                    );
                }
            }
            Err(e) => {
                println!("LLM extraction failed: {e}");
                println!("Hint: check your API key and config with `mur config`.");
            }
        }
    } else {
        println!(
            "Pattern extraction requires --llm flag. Usage: mur learn extract --llm --file transcript.txt"
        );
    }

    // Fingerprint extraction (no LLM needed)
    if fingerprint {
        use crate::capture::emergence::{
            extract_fingerprints, prune_fingerprints, save_fingerprints,
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let fps = extract_fingerprints(&transcript, &session_id);

        if fps.is_empty() {
            println!("No behavior fingerprints detected in transcript.");
        } else {
            save_fingerprints(&fps)?;
            println!(
                "Extracted {} behavior fingerprints from session {}",
                fps.len(),
                &session_id[..8]
            );

            // Auto-prune fingerprints older than 90 days
            let pruned = prune_fingerprints(90)?;
            if pruned > 0 {
                println!("Pruned {} fingerprints older than 90 days.", pruned);
            }
        }
    }

    Ok(())
}

pub(crate) fn cmd_learn_cross(min_projects: usize, dry_run: bool) -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    println!("🌐 Cross-project pattern analysis\n");

    // Analyze which patterns are used across projects.
    // Use applies.projects and evidence (injection from different project contexts).
    let mut candidates: Vec<(String, usize)> = Vec::new();

    for p in &patterns {
        let project_count = p.applies.projects.len();
        if project_count >= min_projects && p.tier != Tier::Core {
            candidates.push((p.name.clone(), project_count));
        }
    }

    if candidates.is_empty() {
        println!("  No patterns found used in {}+ projects.", min_projects);
        println!("  Patterns learn project associations via injection (mur context/inject).");
        println!("  Tip: add projects manually with `mur edit <pattern> --quick`.");
        return Ok(());
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    println!(
        "  Found {} patterns used across {}+ projects:\n",
        candidates.len(),
        min_projects
    );

    for (name, count) in &candidates {
        let p = store.get(name)?;
        let action = if dry_run { "(would promote)" } else { "" };
        println!(
            "  {} — {} projects, tier: {:?} {}",
            name, count, p.tier, action
        );

        if !dry_run {
            let mut p = p;
            let old_tier = p.tier;
            p.tier = Tier::Core;
            p.updated_at = chrono::Utc::now();
            store.save(&p)?;
            println!("    ⬆️  {:?} → Core", old_tier);
        }
    }

    if dry_run {
        println!("\n(dry run — no changes saved)");
    }

    Ok(())
}

pub(crate) fn cmd_emerge(threshold: usize, dry_run: bool) -> Result<()> {
    use crate::capture::emergence::{detect_emergent, load_fingerprints, prune_fingerprints};

    // Auto-prune old fingerprints first
    let pruned = prune_fingerprints(90)?;
    if pruned > 0 {
        println!("Pruned {} fingerprints older than 90 days.\n", pruned);
    }

    // Load all fingerprints
    let fingerprints = load_fingerprints()?;
    if fingerprints.is_empty() {
        println!(
            "No fingerprints found. Run `mur learn extract --fingerprint` on session transcripts first."
        );
        return Ok(());
    }

    let session_count: std::collections::HashSet<&str> = fingerprints
        .iter()
        .map(|fp| fp.session_id.as_str())
        .collect();

    println!(
        "Loaded {} fingerprints from {} sessions.\n",
        fingerprints.len(),
        session_count.len()
    );

    // Detect emergent patterns
    let candidates = detect_emergent(&fingerprints, threshold);

    if candidates.is_empty() {
        println!(
            "No emergent patterns found (threshold: {} sessions).\nKeep running `mur learn extract --fingerprint` to build up the fingerprint database.",
            threshold
        );
        return Ok(());
    }

    let mode = if dry_run { " (dry run)" } else { "" };
    println!(
        "Found {} emergent patterns from {} sessions{}\n",
        candidates.len(),
        session_count.len(),
        mode
    );

    let store = YamlStore::default_store()?;
    let mut created = 0;

    for (i, candidate) in candidates.iter().enumerate() {
        println!(
            "{}. {} (seen in {} sessions)",
            i + 1,
            candidate.suggested_name,
            candidate.session_count
        );
        println!("   Behavior: {}", candidate.behavior);
        println!("   Keywords: {}", candidate.keywords.join(", "));
        println!("   Sessions: {}", candidate.session_ids.join(", "));

        if !candidate.evidence.is_empty() {
            println!("   Evidence:");
            for ev in &candidate.evidence {
                println!("     - {}", ev);
            }
        }

        if !dry_run {
            // Create a draft pattern
            let name = &candidate.suggested_name;
            if store.exists(name) {
                println!("   -> Pattern '{}' already exists, skipping.", name);
            } else {
                let pattern = Pattern {
                    base: KnowledgeBase {
                        schema: mur_common::pattern::SCHEMA_VERSION,
                        name: name.clone(),
                        description: format!(
                            "Emergent: {} (detected across {} sessions)",
                            candidate.behavior, candidate.session_count
                        ),
                        content: mur_common::pattern::Content::DualLayer {
                            technical: candidate.suggested_content.clone(),
                            principle: None,
                        },
                        tier: mur_common::pattern::Tier::Session,
                        importance: 0.3,
                        confidence: 0.2,
                        tags: mur_common::pattern::Tags {
                            languages: vec![],
                            topics: candidate.keywords.clone(),
                            extra: Default::default(),
                        },
                        evidence: mur_common::pattern::Evidence {
                            source_sessions: candidate.session_ids.clone(),
                            first_seen: Some(chrono::Utc::now()),
                            ..Default::default()
                        },
                        maturity: mur_common::knowledge::Maturity::Draft,
                        ..Default::default()
                    },
                    kind: None,
                    origin: None,
                    attachments: vec![],
                };
                store.save(&pattern)?;
                println!("   -> Created draft pattern: {}", name);
                created += 1;
            }
        }

        println!();
    }

    if dry_run {
        println!("Run without --dry-run to create these as draft patterns.");
    } else if created > 0 {
        println!(
            "Created {} draft patterns (maturity: Draft, confidence: 0.2).",
            created
        );
    }

    Ok(())
}

/// Parse the LLM JSON response into Pattern objects.
pub(crate) fn parse_llm_patterns(response: &str) -> Vec<Pattern> {
    // Strip markdown fences if present
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

    #[derive(serde::Deserialize)]
    struct LlmPattern {
        name: String,
        description: String,
        technical: String,
        #[serde(default)]
        principle: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default = "default_tier_str")]
        tier: String,
        #[serde(default = "default_importance")]
        importance: f64,
    }
    fn default_tier_str() -> String {
        "session".to_string()
    }
    fn default_importance() -> f64 {
        0.5
    }

    let items: Vec<LlmPattern> = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse LLM pattern JSON: {e}");
            return vec![];
        }
    };

    let now = chrono::Utc::now();
    items
        .into_iter()
        .filter(|p| !p.name.is_empty() && !p.technical.is_empty())
        .map(|p| {
            let tier = match p.tier.as_str() {
                "core" => Tier::Core,
                "project" => Tier::Project,
                _ => Tier::Session,
            };
            Pattern {
                base: KnowledgeBase {
                    name: p.name,
                    description: p.description,
                    content: Content::DualLayer {
                        technical: p.technical,
                        principle: p.principle,
                    },
                    tier,
                    importance: p.importance.clamp(0.0, 1.0),
                    confidence: 0.6,
                    tags: Tags {
                        topics: p.tags,
                        ..Default::default()
                    },
                    maturity: mur_common::knowledge::Maturity::Draft,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
                kind: None,
                origin: Some(Origin {
                    source: "llm-extract".to_string(),
                    trigger: OriginTrigger::Automatic,
                    user: None,
                    platform: None,
                    confidence: 0.6,
                }),
                attachments: vec![],
            }
        })
        .collect()
}
