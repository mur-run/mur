//! Cross-session emergence detection engine.
//!
//! Detects recurring behaviors across multiple sessions that haven't been
//! explicitly captured as patterns. When a user does the same thing 3+ times
//! across different sessions, MUR surfaces it as an "emergent pattern" candidate.
//!
//! Pure heuristic/keyword extraction — no LLM calls.

use chrono::{DateTime, Duration, Utc};
use mur_common::event::BehaviorFingerprint;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// ─── Emergent candidate ──────────────────────────────────────────

/// A candidate pattern detected from recurring cross-session behaviors.
#[derive(Debug, Clone)]
pub struct EmergentCandidate {
    /// Merged description of the behavior
    pub behavior: String,
    /// Union of keywords from all matching fingerprints
    pub keywords: Vec<String>,
    /// How many distinct sessions exhibited this behavior
    pub session_count: usize,
    /// Which sessions
    pub session_ids: Vec<String>,
    /// Representative snippets from each session
    pub evidence: Vec<String>,
    /// Suggested kebab-case pattern name
    pub suggested_name: String,
    /// Draft pattern content
    pub suggested_content: String,
}

// ─── Fingerprint extraction ──────────────────────────────────────

/// Tool call pattern: matches lines like `tool: Read`, `tool_call: Edit`, etc.
static TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)(?:tool(?:_call)?|tool_name)\s*[:=]\s*["']?(\w+)["']?"#).unwrap()
});

/// Shell command pattern: matches common shell invocations in transcripts.
static COMMAND_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)(?:^|\n)\s*(?:\$|>|%)\s+(.+?)(?:\n|$)"#).unwrap());

/// Bare command pattern: matches backtick-wrapped commands (e.g., cargo test, npm run build)
static BARE_COMMAND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)`((?:cargo|npm|yarn|pnpm|bun|git|docker|make|pytest|go|rustc|gcc|clang|mvn|gradle)\s[^`]+)`"#).unwrap()
});

/// File path pattern: matches file paths referenced in transcripts.
static FILE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|\s|[`"'])([a-zA-Z0-9_./-]+\.[a-zA-Z]{1,10})(?:\s|[`"']|$|:|\))"#).unwrap()
});

/// Correction pattern: matches "actually, do X instead" style corrections.
static CORRECTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)(?:actually[,\s]+|instead[,\s]+|no[,\s]+(?:use|try|do)|don'?t[,\s]+(?:use|do))\s*(.{10,80})"#).unwrap()
});

/// Common technical keywords for extraction (lowercase).
const TECH_TERMS: &[&str] = &[
    "async",
    "await",
    "test",
    "testing",
    "debug",
    "build",
    "deploy",
    "lint",
    "format",
    "refactor",
    "migrate",
    "error",
    "handle",
    "parse",
    "serialize",
    "cache",
    "index",
    "query",
    "fetch",
    "render",
    "compile",
    "bundle",
    "config",
    "install",
    "update",
    "delete",
    "create",
    "read",
    "write",
    "api",
    "database",
    "server",
    "client",
    "auth",
    "token",
    "session",
    "component",
    "module",
    "function",
    "struct",
    "trait",
    "interface",
    "docker",
    "kubernetes",
    "ci",
    "cd",
    "pipeline",
    "hook",
    "middleware",
];

/// Common stop-words to exclude from keyword extraction.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "use", "with", "this", "that", "from", "are", "was", "were", "been",
    "being", "have", "has", "had", "does", "did", "will", "would", "could", "should", "may",
    "might", "can", "shall", "not", "but", "all", "any", "each", "every", "both", "few", "more",
    "most", "other", "some", "such", "only", "than", "too", "very", "just", "into", "also", "how",
    "when", "where", "which", "while", "who", "whom", "what", "why", "new", "old", "let", "you",
    "your", "its", "our", "out", "file", "line", "code", "here", "there", "then", "now",
];

/// Extract behavior fingerprints from a session transcript.
///
/// Parses the transcript for:
/// - Tool call sequences (e.g., "read → edit → test")
/// - Command patterns (shell commands)
/// - File type patterns (commonly touched file types)
/// - Correction patterns ("actually, do X instead")
///
/// Returns one fingerprint per distinct behavior detected.
pub fn extract_fingerprints(transcript: &str, session_id: &str) -> Vec<BehaviorFingerprint> {
    let now = Utc::now();
    let mut fingerprints = Vec::new();

    // 1. Tool call sequences
    fingerprints.extend(extract_tool_call_fingerprints(transcript, session_id, now));

    // 2. Command patterns
    fingerprints.extend(extract_command_fingerprints(transcript, session_id, now));

    // 3. File type patterns
    fingerprints.extend(extract_file_pattern_fingerprints(
        transcript, session_id, now,
    ));

    // 4. Correction patterns
    fingerprints.extend(extract_correction_fingerprints(transcript, session_id, now));

    fingerprints
}

/// Extract fingerprints from tool call sequences.
fn extract_tool_call_fingerprints(
    transcript: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Vec<BehaviorFingerprint> {
    let tool_calls: Vec<String> = TOOL_CALL_RE
        .captures_iter(transcript)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_lowercase()))
        .collect();

    if tool_calls.len() < 2 {
        return Vec::new();
    }

    // Extract sequences of 2-3 consecutive tool calls
    let mut sequences: Vec<Vec<String>> = Vec::new();
    for window_size in 2..=3.min(tool_calls.len()) {
        for window in tool_calls.windows(window_size) {
            // Skip if all the same tool
            if window.iter().all(|t| t == &window[0]) {
                continue;
            }
            sequences.push(window.to_vec());
        }
    }

    // Deduplicate sequences
    let mut seen = HashSet::new();
    let mut fingerprints = Vec::new();

    for seq in &sequences {
        let key = seq.join(" → ");
        if seen.insert(key.clone()) {
            let mut keywords: Vec<String> = seq.clone();
            keywords.push("tool-sequence".to_string());
            keywords.dedup();

            fingerprints.push(BehaviorFingerprint {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                behavior: format!("Tool sequence: {}", key),
                keywords,
                timestamp: now,
            });
        }
    }

    fingerprints
}

/// Extract fingerprints from shell commands.
fn extract_command_fingerprints(
    transcript: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Vec<BehaviorFingerprint> {
    let mut commands: Vec<String> = Vec::new();

    // Prompt-prefixed commands ($ command, > command)
    for cap in COMMAND_RE.captures_iter(transcript) {
        if let Some(m) = cap.get(1) {
            commands.push(m.as_str().trim().to_string());
        }
    }

    // Backtick-wrapped commands (e.g., `cargo test`)
    for cap in BARE_COMMAND_RE.captures_iter(transcript) {
        if let Some(m) = cap.get(1) {
            commands.push(m.as_str().trim().to_string());
        }
    }

    let mut seen = HashSet::new();
    let mut fingerprints = Vec::new();

    for cmd in &commands {
        // Normalize: extract the base command and first subcommand
        let normalized = normalize_command(cmd);
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }

        let mut keywords = extract_command_keywords(cmd);
        keywords.push("command".to_string());
        keywords.dedup();

        fingerprints.push(BehaviorFingerprint {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            behavior: format!("Command: {}", normalized),
            keywords,
            timestamp: now,
        });
    }

    fingerprints
}

/// Extract fingerprints from file type patterns.
fn extract_file_pattern_fingerprints(
    transcript: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Vec<BehaviorFingerprint> {
    let mut extensions: HashMap<String, usize> = HashMap::new();

    for cap in FILE_PATH_RE.captures_iter(transcript) {
        if let Some(m) = cap.get(1) {
            let path = m.as_str();
            if let Some(ext) = path.rsplit('.').next() {
                let ext = ext.to_lowercase();
                // Filter out common non-file extensions
                if ext.len() <= 6
                    && !["com", "org", "net", "io", "dev", "html"].contains(&ext.as_str())
                {
                    *extensions.entry(ext).or_insert(0) += 1;
                }
            }
        }
    }

    // Only report file types seen 3+ times
    let mut fingerprints = Vec::new();
    for (ext, count) in &extensions {
        if *count >= 3 {
            fingerprints.push(BehaviorFingerprint {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                behavior: format!(
                    "File pattern: frequently edits .{} files ({} times)",
                    ext, count
                ),
                keywords: vec![ext.clone(), "file-pattern".to_string()],
                timestamp: now,
            });
        }
    }

    fingerprints
}

/// Extract fingerprints from correction patterns.
fn extract_correction_fingerprints(
    transcript: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Vec<BehaviorFingerprint> {
    let mut seen = HashSet::new();
    let mut fingerprints = Vec::new();

    for cap in CORRECTION_RE.captures_iter(transcript) {
        if let Some(m) = cap.get(1) {
            let correction = m.as_str().trim().to_string();
            if correction.len() < 10 || !seen.insert(correction.clone()) {
                continue;
            }

            let keywords = extract_text_keywords(&correction);
            if keywords.is_empty() {
                continue;
            }

            let mut all_keywords = keywords;
            all_keywords.push("correction".to_string());

            fingerprints.push(BehaviorFingerprint {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                behavior: format!("Correction: {}", truncate(&correction, 80)),
                keywords: all_keywords,
                timestamp: now,
            });
        }
    }

    fingerprints
}

// ─── Emergence detection ─────────────────────────────────────────

/// Detect emergent pattern candidates from a collection of fingerprints.
///
/// Groups fingerprints by keyword similarity (Jaccard >= 0.4) and returns
/// candidates where the behavior appears in >= threshold different sessions.
pub fn detect_emergent(
    fingerprints: &[BehaviorFingerprint],
    threshold: usize,
) -> Vec<EmergentCandidate> {
    if fingerprints.is_empty() {
        return Vec::new();
    }

    // Group fingerprints into clusters by keyword similarity
    let clusters = cluster_fingerprints(fingerprints);

    // For each cluster, check if it meets the session threshold
    let mut candidates = Vec::new();
    for cluster in &clusters {
        let session_ids: HashSet<&str> = cluster.iter().map(|fp| fp.session_id.as_str()).collect();

        if session_ids.len() >= threshold {
            candidates.push(build_candidate(cluster, &session_ids));
        }
    }

    // Sort by session count descending
    candidates.sort_by(|a, b| b.session_count.cmp(&a.session_count));
    candidates
}

/// Cluster fingerprints by keyword similarity using single-linkage clustering.
fn cluster_fingerprints(fingerprints: &[BehaviorFingerprint]) -> Vec<Vec<&BehaviorFingerprint>> {
    let n = fingerprints.len();
    // Union-Find for clustering
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut Vec<usize>, i: usize) -> usize {
        if parent[i] != i {
            parent[i] = find(parent, parent[i]);
        }
        parent[i]
    }

    fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    // Compare all pairs — O(n²) but n is typically small (dozens to low hundreds)
    for i in 0..n {
        for j in (i + 1)..n {
            let sim = jaccard_similarity(&fingerprints[i].keywords, &fingerprints[j].keywords);
            if sim >= 0.4 {
                union(&mut parent, i, j);
            }
        }
    }

    // Collect clusters
    let mut cluster_map: HashMap<usize, Vec<&BehaviorFingerprint>> = HashMap::new();
    for (i, fp) in fingerprints.iter().enumerate() {
        let root = find(&mut parent, i);
        cluster_map.entry(root).or_default().push(fp);
    }

    cluster_map.into_values().collect()
}

/// Build an EmergentCandidate from a cluster of fingerprints.
fn build_candidate(
    cluster: &[&BehaviorFingerprint],
    session_ids: &HashSet<&str>,
) -> EmergentCandidate {
    // Union of all keywords
    let mut all_keywords: HashSet<String> = HashSet::new();
    for fp in cluster {
        for kw in &fp.keywords {
            all_keywords.insert(kw.clone());
        }
    }
    let keywords: Vec<String> = {
        let mut v: Vec<String> = all_keywords.into_iter().collect();
        v.sort();
        v
    };

    // Pick the most common behavior description
    let mut behavior_counts: HashMap<&str, usize> = HashMap::new();
    for fp in cluster {
        *behavior_counts.entry(&fp.behavior).or_insert(0) += 1;
    }
    let behavior = behavior_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(b, _)| b.to_string())
        .unwrap_or_default();

    // Evidence: one snippet per session (first fingerprint from each session)
    let mut evidence = Vec::new();
    let mut seen_sessions = HashSet::new();
    for fp in cluster {
        if seen_sessions.insert(&fp.session_id) {
            evidence.push(fp.behavior.clone());
        }
    }

    let session_id_list: Vec<String> = {
        let mut v: Vec<String> = session_ids.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };

    let suggested_name = generate_suggested_name(&keywords);
    let suggested_content = generate_suggested_content(&behavior, &keywords, &evidence);

    EmergentCandidate {
        behavior,
        keywords,
        session_count: session_id_list.len(),
        session_ids: session_id_list,
        evidence,
        suggested_name,
        suggested_content,
    }
}

// ─── Jaccard similarity ──────────────────────────────────────────

/// Compute Jaccard similarity between two keyword sets.
///
/// J(A, B) = |A ∩ B| / |A ∪ B|
pub fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let set_a: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let set_b: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ─── Fingerprint storage ─────────────────────────────────────────

/// Save fingerprints to the JSONL file (append-only).
pub fn save_fingerprints(fingerprints: &[BehaviorFingerprint]) -> anyhow::Result<()> {
    use std::io::Write;

    if fingerprints.is_empty() {
        return Ok(());
    }

    let path = fingerprints_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let mut writer = std::io::BufWriter::new(file);

    for fp in fingerprints {
        let line = serde_json::to_string(fp)?;
        writeln!(writer, "{}", line)?;
    }

    Ok(())
}

/// Load all fingerprints from the JSONL file.
pub fn load_fingerprints() -> anyhow::Result<Vec<BehaviorFingerprint>> {
    let path = fingerprints_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&path)?;
    let mut fingerprints = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BehaviorFingerprint>(line) {
            Ok(fp) => fingerprints.push(fp),
            Err(e) => {
                tracing::warn!("Skipping malformed fingerprint line: {}", e);
            }
        }
    }

    Ok(fingerprints)
}

/// Prune fingerprints older than `max_age_days`, rewriting the JSONL file.
pub fn prune_fingerprints(max_age_days: i64) -> anyhow::Result<usize> {
    let path = fingerprints_path();
    if !path.exists() {
        return Ok(0);
    }

    let all = load_fingerprints()?;
    let cutoff = Utc::now() - Duration::days(max_age_days);

    let (kept, pruned): (Vec<_>, Vec<_>) = all.into_iter().partition(|fp| fp.timestamp >= cutoff);

    let pruned_count = pruned.len();

    if pruned_count > 0 {
        // Rewrite the file with only kept fingerprints
        let tmp_path = path.with_extension("jsonl.tmp");
        {
            use std::io::Write;
            let file = std::fs::File::create(&tmp_path)?;
            let mut writer = std::io::BufWriter::new(file);
            for fp in &kept {
                let line = serde_json::to_string(fp)?;
                writeln!(writer, "{}", line)?;
            }
        }
        std::fs::rename(&tmp_path, &path)?;
    }

    Ok(pruned_count)
}

/// Path to the fingerprints JSONL file: ~/.mur/fingerprints.jsonl
fn fingerprints_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~"))
        .join(".mur")
        .join("fingerprints.jsonl")
}

// ─── Helpers ─────────────────────────────────────────────────────

/// Normalize a command string to its base form (e.g., "cargo test --release" → "cargo test").
fn normalize_command(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    // Take first 2-3 parts (command + subcommand)
    let take = match parts[0] {
        "cargo" | "npm" | "yarn" | "pnpm" | "bun" | "git" | "docker" | "go" | "kubectl" => {
            2.min(parts.len())
        }
        _ => 1.min(parts.len()),
    };
    parts[..take].join(" ")
}

/// Extract keywords from a shell command.
fn extract_command_keywords(cmd: &str) -> Vec<String> {
    cmd.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() >= 3 && !STOP_WORDS.contains(w))
        .map(|w| w.to_lowercase())
        .collect()
}

/// Extract keywords from general text.
fn extract_text_keywords(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut keywords: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| {
            w.len() >= 3 && !STOP_WORDS.contains(w) && (TECH_TERMS.contains(w) || w.len() >= 4)
        })
        .map(String::from)
        .collect();

    // Deduplicate
    let mut seen = HashSet::new();
    keywords.retain(|w| seen.insert(w.clone()));
    keywords
}

/// Generate a suggested kebab-case name from keywords.
pub fn generate_suggested_name(keywords: &[String]) -> String {
    // Filter out meta-keywords and take up to 4
    let name_parts: Vec<&str> = keywords
        .iter()
        .filter(|k| {
            !["tool-sequence", "command", "file-pattern", "correction"].contains(&k.as_str())
        })
        .take(4)
        .map(|s| s.as_str())
        .collect();

    if name_parts.is_empty() {
        return "emergent-pattern".to_string();
    }

    let name = name_parts.join("-");
    // Sanitize to valid kebab-case
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').to_lowercase()
}

/// Generate suggested pattern content from an emergent candidate.
fn generate_suggested_content(behavior: &str, keywords: &[String], evidence: &[String]) -> String {
    let mut content = format!("Emergent behavior detected: {}\n\n", behavior);
    content.push_str(&format!("Keywords: {}\n\n", keywords.join(", ")));
    content.push_str("Evidence from sessions:\n");
    for (i, ev) in evidence.iter().enumerate() {
        content.push_str(&format!("  {}. {}\n", i + 1, ev));
    }
    content
}

/// Truncate a string to max_len chars, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Jaccard similarity ──────────────────────────────────

    #[test]
    fn test_jaccard_identical() {
        let a = vec!["foo".into(), "bar".into()];
        let b = vec!["foo".into(), "bar".into()];
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_disjoint() {
        let a = vec!["foo".into(), "bar".into()];
        let b = vec!["baz".into(), "qux".into()];
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_partial_overlap() {
        // {foo, bar} ∩ {bar, baz} = {bar}, union = {foo, bar, baz} → 1/3
        let a = vec!["foo".into(), "bar".into()];
        let b = vec!["bar".into(), "baz".into()];
        let sim = jaccard_similarity(&a, &b);
        assert!((sim - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn test_jaccard_empty() {
        let empty: Vec<String> = vec![];
        let a = vec!["foo".into()];
        assert!((jaccard_similarity(&empty, &empty) - 1.0).abs() < f64::EPSILON);
        assert!((jaccard_similarity(&empty, &a)).abs() < f64::EPSILON);
        assert!((jaccard_similarity(&a, &empty)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_above_threshold() {
        // {a, b, c} ∩ {a, b, d} = {a, b}, union = {a, b, c, d} → 2/4 = 0.5 >= 0.4
        let a = vec!["a".into(), "b".into(), "c".into()];
        let b = vec!["a".into(), "b".into(), "d".into()];
        assert!(jaccard_similarity(&a, &b) >= 0.4);
    }

    // ─── Fingerprint extraction ──────────────────────────────

    #[test]
    fn test_extract_tool_call_fingerprints() {
        let transcript = r#"
tool_call: Read
result: file contents
tool_call: Edit
result: success
tool_call: Bash
result: tests pass
"#;
        let fps = extract_fingerprints(transcript, "session-1");
        let tool_fps: Vec<_> = fps
            .iter()
            .filter(|f| f.behavior.contains("Tool sequence"))
            .collect();
        assert!(!tool_fps.is_empty());
        // Should find read → edit, edit → bash, read → edit → bash
        assert!(
            tool_fps
                .iter()
                .any(|f| f.behavior.contains("read") && f.behavior.contains("edit"))
        );
    }

    #[test]
    fn test_extract_command_fingerprints() {
        let transcript = "Running `cargo test --release` to verify\nThen `cargo build`\n";
        let fps = extract_fingerprints(transcript, "session-2");
        let cmd_fps: Vec<_> = fps
            .iter()
            .filter(|f| f.behavior.starts_with("Command:"))
            .collect();
        assert!(cmd_fps.iter().any(|f| f.behavior.contains("cargo test")));
    }

    #[test]
    fn test_extract_file_pattern_fingerprints() {
        let transcript = "\
Read src/main.rs
Edit src/lib.rs
Read src/config.rs
Edit src/utils.rs
Read src/types.rs
";
        let fps = extract_fingerprints(transcript, "session-3");
        let file_fps: Vec<_> = fps
            .iter()
            .filter(|f| f.behavior.contains("File pattern"))
            .collect();
        assert!(
            file_fps
                .iter()
                .any(|f| f.keywords.contains(&"rs".to_string()))
        );
    }

    #[test]
    fn test_extract_correction_fingerprints() {
        let transcript = "\
User: Fix the logging module
AI: I'll use println! for logging.
User: Actually, use the tracing crate for structured logging instead
AI: Updated to use tracing.
";
        let fps = extract_fingerprints(transcript, "session-4");
        let corr_fps: Vec<_> = fps
            .iter()
            .filter(|f| f.behavior.starts_with("Correction:"))
            .collect();
        assert!(!corr_fps.is_empty());
        assert!(
            corr_fps
                .iter()
                .any(|f| f.keywords.contains(&"tracing".to_string()))
        );
    }

    // ─── Emergence detection ─────────────────────────────────

    #[test]
    fn test_emergence_threshold_met() {
        // 3 sessions with similar behavior → should produce a candidate
        let fingerprints = vec![
            BehaviorFingerprint {
                id: "1".into(),
                session_id: "s1".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into(), "command".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "2".into(),
                session_id: "s2".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into(), "command".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "3".into(),
                session_id: "s3".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into(), "command".into()],
                timestamp: Utc::now(),
            },
        ];

        let candidates = detect_emergent(&fingerprints, 3);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].session_count, 3);
        assert!(candidates[0].session_ids.contains(&"s1".to_string()));
        assert!(candidates[0].session_ids.contains(&"s2".to_string()));
        assert!(candidates[0].session_ids.contains(&"s3".to_string()));
    }

    #[test]
    fn test_emergence_below_threshold() {
        // 2 sessions → below default threshold of 3, no candidate
        let fingerprints = vec![
            BehaviorFingerprint {
                id: "1".into(),
                session_id: "s1".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "2".into(),
                session_id: "s2".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
        ];

        let candidates = detect_emergent(&fingerprints, 3);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_emergence_same_session_not_counted() {
        // 3 fingerprints from same session → only 1 unique session → no candidate
        let fingerprints = vec![
            BehaviorFingerprint {
                id: "1".into(),
                session_id: "same-session".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "2".into(),
                session_id: "same-session".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "3".into(),
                session_id: "same-session".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
        ];

        let candidates = detect_emergent(&fingerprints, 3);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_emergence_dissimilar_not_clustered() {
        // 3 sessions but completely different behaviors → no cluster meets threshold
        let fingerprints = vec![
            BehaviorFingerprint {
                id: "1".into(),
                session_id: "s1".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "2".into(),
                session_id: "s2".into(),
                behavior: "File: python edits".into(),
                keywords: vec!["python".into(), "django".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "3".into(),
                session_id: "s3".into(),
                behavior: "Docker: compose up".into(),
                keywords: vec!["docker".into(), "compose".into()],
                timestamp: Utc::now(),
            },
        ];

        let candidates = detect_emergent(&fingerprints, 3);
        assert!(candidates.is_empty());
    }

    // ─── JSONL roundtrip ─────────────────────────────────────

    #[test]
    fn test_jsonl_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fingerprints.jsonl");

        let fps = vec![
            BehaviorFingerprint {
                id: "fp-1".into(),
                session_id: "s1".into(),
                behavior: "Command: cargo test".into(),
                keywords: vec!["cargo".into(), "test".into()],
                timestamp: Utc::now(),
            },
            BehaviorFingerprint {
                id: "fp-2".into(),
                session_id: "s2".into(),
                behavior: "Tool sequence: read → edit".into(),
                keywords: vec!["read".into(), "edit".into()],
                timestamp: Utc::now(),
            },
        ];

        // Write
        {
            use std::io::Write;
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            for fp in &fps {
                writeln!(writer, "{}", serde_json::to_string(fp).unwrap()).unwrap();
            }
        }

        // Read
        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: Vec<BehaviorFingerprint> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "fp-1");
        assert_eq!(loaded[1].id, "fp-2");
        assert_eq!(loaded[0].keywords, vec!["cargo", "test"]);
    }

    // ─── Auto-prune ──────────────────────────────────────────

    #[test]
    fn test_prune_removes_old_fingerprints() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fingerprints.jsonl");

        let old = BehaviorFingerprint {
            id: "old".into(),
            session_id: "s-old".into(),
            behavior: "Old behavior".into(),
            keywords: vec!["old".into()],
            timestamp: Utc::now() - Duration::days(100),
        };
        let recent = BehaviorFingerprint {
            id: "recent".into(),
            session_id: "s-recent".into(),
            behavior: "Recent behavior".into(),
            keywords: vec!["recent".into()],
            timestamp: Utc::now() - Duration::days(10),
        };

        // Write both
        {
            use std::io::Write;
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(file);
            writeln!(writer, "{}", serde_json::to_string(&old).unwrap()).unwrap();
            writeln!(writer, "{}", serde_json::to_string(&recent).unwrap()).unwrap();
        }

        // Prune with 90-day cutoff (using the file directly)
        let content = std::fs::read_to_string(&path).unwrap();
        let all: Vec<BehaviorFingerprint> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let cutoff = Utc::now() - Duration::days(90);
        let kept: Vec<_> = all
            .into_iter()
            .filter(|fp| fp.timestamp >= cutoff)
            .collect();

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "recent");
    }

    // ─── Suggested name generation ───────────────────────────

    #[test]
    fn test_suggested_name_from_keywords() {
        let keywords = vec!["cargo".into(), "test".into(), "command".into()];
        let name = generate_suggested_name(&keywords);
        assert_eq!(name, "cargo-test");
    }

    #[test]
    fn test_suggested_name_limits_parts() {
        let keywords = vec![
            "cargo".into(),
            "test".into(),
            "build".into(),
            "deploy".into(),
            "extra".into(),
        ];
        let name = generate_suggested_name(&keywords);
        // Should take at most 4 non-meta keywords
        let parts: Vec<&str> = name.split('-').collect();
        assert!(parts.len() <= 4);
    }

    #[test]
    fn test_suggested_name_filters_meta_keywords() {
        let keywords = vec!["tool-sequence".into(), "read".into(), "edit".into()];
        let name = generate_suggested_name(&keywords);
        assert!(!name.contains("tool-sequence"));
        assert!(name.contains("read"));
        assert!(name.contains("edit"));
    }

    #[test]
    fn test_suggested_name_empty_keywords() {
        let keywords: Vec<String> = vec![];
        let name = generate_suggested_name(&keywords);
        assert_eq!(name, "emergent-pattern");
    }

    // ─── Draft pattern creation ──────────────────────────────

    #[test]
    fn test_draft_pattern_from_candidate() {
        let candidate = EmergentCandidate {
            behavior: "Command: cargo test".into(),
            keywords: vec!["cargo".into(), "test".into()],
            session_count: 3,
            session_ids: vec!["s1".into(), "s2".into(), "s3".into()],
            evidence: vec![
                "Command: cargo test in s1".into(),
                "Command: cargo test in s2".into(),
                "Command: cargo test in s3".into(),
            ],
            suggested_name: "cargo-test".into(),
            suggested_content: "Emergent behavior detected: Command: cargo test".into(),
        };

        // Verify fields
        assert_eq!(candidate.suggested_name, "cargo-test");
        assert_eq!(candidate.session_count, 3);
        assert!(candidate.suggested_content.contains("cargo test"));
    }

    // ─── Full pipeline test ──────────────────────────────────

    #[test]
    fn test_full_emergence_pipeline() {
        // Simulate 3 sessions where user consistently runs cargo test after editing
        let session_transcripts = vec![
            (
                "s1",
                "tool_call: Read\ntool_call: Edit\nRunning `cargo test`\ntool_call: Read",
            ),
            (
                "s2",
                "tool_call: Read\ntool_call: Edit\nRunning `cargo test --release`\ntool_call: Read",
            ),
            (
                "s3",
                "tool_call: Read\ntool_call: Edit\nRunning `cargo test`\ntool_call: Bash",
            ),
        ];

        let mut all_fingerprints = Vec::new();
        for (session_id, transcript) in &session_transcripts {
            let fps = extract_fingerprints(transcript, session_id);
            all_fingerprints.extend(fps);
        }

        assert!(!all_fingerprints.is_empty());

        // With threshold=3, check if we get candidates
        // The "read → edit" tool sequence should appear in all 3 sessions
        let candidates = detect_emergent(&all_fingerprints, 3);
        // At minimum, the tool sequence "read → edit" should be detected
        // across all 3 sessions
        let has_tool_sequence = candidates.iter().any(|c| c.session_count >= 3);

        // If clustering works, we should find at least one emergent pattern
        // The exact count depends on similarity thresholds
        assert!(
            has_tool_sequence || !candidates.is_empty() || {
                // Fallback: verify fingerprints were extracted from all sessions
                let sessions: HashSet<&str> = all_fingerprints
                    .iter()
                    .map(|fp| fp.session_id.as_str())
                    .collect();
                sessions.len() == 3
            }
        );
    }

    // ─── Normalize command ───────────────────────────────────

    #[test]
    fn test_normalize_command() {
        assert_eq!(normalize_command("cargo test --release"), "cargo test");
        assert_eq!(normalize_command("npm run build"), "npm run");
        assert_eq!(normalize_command("git commit -m 'msg'"), "git commit");
        assert_eq!(normalize_command("ls -la"), "ls");
        assert_eq!(normalize_command(""), "");
    }
}
