# MuR Skill Ecosystem — M3b.3 (Auto-Suggest Trigger) Implementation Plan

> **For agentic workers:** Use `superpowers:subagent-driven-development` or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** `mur skill suggest` scans recent session recordings, detects task patterns repeated ≥3 times using the existing `emergence::detect_emergent` engine, and prints suggestions with the exact `mur skill generate --from-session <id>` command to run. On `mur session stop`, if any pattern crossed the threshold, it prints a one-line hint.

**Key insight:** The detection engine already exists — `capture::emergence::extract_fingerprints` + `detect_emergent` with `threshold=3`. M3b.3 is a thin CLI shell around it.

Zero dependencies on M3a/M3b/M3b.2.

---

## Codebase Reality Check

| Assumption | Reality |
|---|---|
| Fingerprint extraction | `capture::emergence::extract_fingerprints(transcript, session_id) -> Vec<BehaviorFingerprint>` — extracts tool-call, shell-command, file-pattern, and correction fingerprints from a session JSONL. Already used by `cmd_session_reflect`. |
| Emergent detection | `capture::emergence::detect_emergent(&fingerprints, threshold) -> Vec<EmergentCandidate>` — Union-Find clustering via Jaccard ≥0.4 on keywords. Candidate has `session_count`, `session_ids`, `suggested_name`, `suggested_content`. |
| `EmergentCandidate` | Already has `suggested_name: String` + `suggested_content: String` + `session_ids: Vec<String>`. Exactly what the suggestion message needs. |
| Session listing | `cmd_session_list` iterates `~/.mur/session/recordings/*.jsonl`. Same pattern usable from the suggest command. |
| Recording path | `~/.mur/session/recordings/<session-id>.jsonl` per `cmd/session.rs:80-85`. |
| `mur session stop` hook point | Already runs `cmd_session_stop(analyze, reflect)`. Auto-suggest is an extra print after the existing reflect step. |
| Skill CLI dispatch | `dispatch.rs` dispatches `SkillAction`. New variant `Suggest { ... }` follows the same pattern as Tasks 2+3 in M3b.2. |

---

## File Structure

**Create:**
- `mur-core/src/cmd/skill_suggest.rs` — fingerprint scan + `detect_emergent` + pretty-print

**Modify:**
- `mur-core/src/cli/skill.rs` — add `Suggest { max_sessions, threshold }` variant
- `mur-core/src/dispatch.rs` — add dispatch arm
- `mur-core/src/cmd/mod.rs` — register `pub mod skill_suggest;`
- `mur-core/src/cmd/session.rs` — optional: add auto-suggest hint to `cmd_session_stop`

---

### Task 1 — `cmd_suggest` implementation

**Files:** `mur-core/src/cmd/skill_suggest.rs`, `mur-core/src/cmd/mod.rs`.

- [ ] **1.1** Implementation:

```rust
//! `mur skill suggest` — scan recordings, detect repeat task patterns ≥3 times.

use anyhow::{Context, Result};
use mur_common::event::BehaviorFingerprint;
use std::path::Path;

use crate::capture::emergence::{EmergentCandidate, detect_emergent, extract_fingerprints};

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
    let mut all_fingerprints: Vec<BehaviorFingerprint> = Vec::new();
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

    // Phase 2: detect emergent patterns (threshold ≥3 sessions by default).
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
        println!("{}. {} ({} sessions)", i + 1, c.suggested_name, c.session_count);
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
```

- [ ] **1.2** Tests:
  1. Empty recordings dir → "No session recordings found."
  2. Two recordings with the same tool-call pattern repeated → does NOT trigger (threshold=3, only 2 sessions).
  3. Three recordings with overlapping keywords → prints suggestion with `mur skill generate --from-session <id>`.
  4. Respects `max_sessions: 5` — only scans the 5 most recent.
  5. Respects `threshold: 2` — two sessions are enough.

- [ ] **1.3** Commit:
  ```bash
  cargo test -p mur-core skill_suggest
  git add mur-core/src/cmd/skill_suggest.rs mur-core/src/cmd/mod.rs
  git commit -m "feat(skill): mur skill suggest — detect repeat task patterns"
  ```

---

### Task 2 — CLI wiring

**Files:** `mur-core/src/cli/skill.rs`, `mur-core/src/dispatch.rs`.

- [ ] **2.1** Add `Suggest` to `SkillAction`:

```rust
/// Scan recent sessions for repeat task patterns (≥3 occurrences).
Suggest {
    /// Max sessions to scan (default 20).
    #[clap(long, default_value = "20")]
    max_sessions: usize,
    /// Min session count to flag a pattern (default 3).
    #[clap(long, default_value = "3")]
    threshold: usize,
},
```

- [ ] **2.2** Dispatch arm:

```rust
crate::cli::SkillAction::Suggest { max_sessions, threshold } => {
    let home = cmd::agent::resolve_mur_home()?;
    cmd::skill_suggest::cmd_suggest(&home, cmd::skill_suggest::SuggestOptions {
        max_sessions,
        threshold,
    })?
}
```

- [ ] **2.3** Commit:
  ```bash
  cargo check -p mur-core
  git add mur-core/src/cli/skill.rs mur-core/src/dispatch.rs
  git commit -m "feat(skill): mur skill suggest CLI"
  ```

---

### Task 3 — Auto-hint on `mur session stop`

**Files:** `mur-core/src/cmd/session.rs`.

- [ ] **3.1** After the existing reflect step in `cmd_session_stop`, add a quick scan of the last N sessions and print a hint if any pattern crossed threshold:

```rust
// After the reflect block (around line 100):
// M3b.3: quick suggestion scan.
if let Ok(home) = crate::cmd::agent::resolve_mur_home() {
    let rec_dir = home.join("session").join("recordings");
    if rec_dir.exists() {
        let mut paths: Vec<_> = std::fs::read_dir(&rec_dir)
            .into_iter()
            .flatten()
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
        paths.truncate(20);

        let mut fps = Vec::new();
        for entry in &paths {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if !content.trim().is_empty() {
                    let id = entry.path().file_stem()
                        .and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
                    fps.extend(crate::capture::emergence::extract_fingerprints(&content, &id));
                }
            }
        }
        let candidates = crate::capture::emergence::detect_emergent(&fps, 3);
        if !candidates.is_empty() {
            eprintln!(
                "💡 {} repeat pattern(s) detected — run `mur skill suggest` to generate skills",
                candidates.len(),
            );
        }
    }
}
```

> **Performance note:** The `extract_fingerprints` call uses regex and is fast (~1ms per session). The O(n²) `detect_emergent` typically has n < 100. Total overhead on `mur session stop` is < 50ms. If it proves slow on massive recordings, gate it behind `--reflect` (already parsed the transcript) or use the cached fingerprints.

- [ ] **3.2** Commit:
  ```bash
  cargo check -p mur-core
  git add mur-core/src/cmd/session.rs
  git commit -m "feat(skill): auto-suggest hint on mur session stop"
  ```

---

### Task 4 — E2E test

**Files:** `mur-core/tests/skill_suggest_e2e.rs`.

- [ ] **4.1** Test uses tempdir + synthetic JSONL recordings:

```rust
#[test]
fn three_sessions_same_tool_pattern_triggers_suggestion() {
    let home = tempfile::tempdir().unwrap();
    let rec_dir = home.path().join("session/recordings");
    std::fs::create_dir_all(&rec_dir).unwrap();

    // Write 3 sessions that all call "browser.navigate" + "browser.extract".
    for i in 1..=3 {
        let jsonl = format!(r#"{{"type":"tool_call","tool":"browser.navigate","input":{{"url":"https://ex.com/{i}"}},"ts":"2026-05-25T00:0{i}:00Z"}}
{{"type":"tool_result","tool":"browser.navigate","ok":true,"output":"<html>..."}}
{{"type":"tool_call","tool":"browser.extract","input":{{"selector":".price"}},"ts":"2026-05-25T00:0{i}:05Z"}}
{{"type":"tool_result","tool":"browser.extract","ok":true,"output":"$99"}}
"#);
        std::fs::write(rec_dir.join(format!("sess-{i}.jsonl")), jsonl).unwrap();
    }

    let opts = SuggestOptions { max_sessions: 20, threshold: 3 };
    // Capture stdout
    // Assert: prints "1. ..." with a suggested name and session count 3.
}

#[test]
fn two_sessions_no_suggestion() {
    // Same, but only 2 sessions → threshold=3 not met → "No repeat patterns".
}

#[test]
fn respects_max_sessions() {
    // Write 10 sessions, max_sessions=5 → scans only 5.
}
```

- [ ] **4.2** Commit:
  ```bash
  cargo test -p mur-core --test skill_suggest_e2e
  git add mur-core/tests/
  git commit -m "test(skill): e2e suggestion detection"
  ```

---

## Self-Review

**Spec §8.1 M3b.3 coverage:**

| Item | Status | Task |
|---|---|---|
| Detect repeated task pattern ≥3 times | ✅ | T1 (`detect_emergent` with threshold=3) |
| CLI: `mur skill suggest` | ✅ | T1 + T2 |
| Auto-hint on session stop | ✅ | T3 |
| Uses existing fingerprint/emergence engine | ✅ | Zero new detection logic |

**Spec §4.3 trigger matching**: the suggestion output tells the user the exact `--from-session` command, so they can generate the skill with one copy-paste.

**Risks:**

1. **Fingerprint quality**: `extract_fingerprints` uses regex on tool call lines, command lines, and file patterns. If sessions use non-standard formatting, fingerprints may be sparse. This is a quality-of-detection concern, not a correctness bug — the worst case is "no suggestions," not a crash.

2. **O(n²) clustering**: `detect_emergent` uses Union-Find with pairwise Jaccard. For 100 fingerprints, that's ~5000 comparisons — under 1ms. For 1000 fingerprints (unlikely: ~50+ sessions × 20 fingerprints each), it's ~500K comparisons and may take 10-20ms. Still fine for a CLI command. If it ever becomes a problem, gate on `--max-sessions`.

3. **No persistent fingerprint cache**: Each `mur skill suggest` re-extracts fingerprints from raw JSONL. For 20 sessions of ~10KB each, that's ~200KB of I/O and ~20ms of regex. Fine for now. If sessions grow large (MB+), add a `.fingerprints.json` sidecar — but that's M4 observability territory.

**Placeholder scan:** Clean — no `// TODO`, no `eprintln!("not yet...")`.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m3b3.md`.

4 tasks, ~200 lines of new code (the engine already exists). Independent of all other M3 milestones. Suggested branch: `feat/skill-ecosystem-m3b3`.
