# mur Conversations Phase 3.3 — Multi-turn `mur ask --continue` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add conversational memory to Mode C `mur ask` via a single persistent JSONL session file, LangChain-style condense rewriter, rolling 3-turn window, graceful degradation.

**Architecture:** New `ask/session.rs` (SessionStore + TurnRecord) persists turns as append-only JSONL at `~/.mur/conversations/ask-session.jsonl`. New `ask/rewriter.rs` issues one Ollama call per follow-up turn with the canonical LangChain condense prompt. `cmd_ask` orchestrates: load/archive session, rewrite question (if `--continue` and prior turns exist), retrieve via `gather_hits` using the rewritten query, generate via extended `prompt::render` (now carries a `## Chat History` section), append the `TurnRecord` on success or degraded fallback. New CLI flags: `--continue` / `--new` / `--show-session` (mutex via clap). Mock's `mock_generate` gains a branch for `"Standalone question:"` that returns the raw latest question (identity rewrite — matches the LangChain "return as is" fallback).

**Tech Stack:** Rust 2024 · tokio · serde_json (existing) · chrono · no new crates.

**Spec:** `docs/superpowers/specs/2026-04-21-mur-conversations-phase-3-3-design.md` (commit `d39b10c`).
**Depends on:** Phase 3.2 shipped (merge `e207b09`).

---

## File Structure

**Create:**

```
mur-core/src/conversations/ask/session.rs             new — SessionStore, TurnRecord, RewriterStatus, Session
mur-core/src/conversations/ask/rewriter.rs            new — rewrite() + CONDENSE_PROMPT + history rendering helper
```

**Modify:**

```
mur-common/src/config.rs                              + AskConfig.continue_history_turns field + default fn
mur-core/src/conversations/ask/mod.rs                 + pub mod session; pub mod rewriter; extend AskRequest + AskResponse with prior_turns/rewritten_question/rewriter_status; extend ask_stream to use rewritten query + prior_turns
mur-core/src/conversations/ask/prompt.rs              render() accepts prior_turns &[TurnRecord]; adds "## Chat History" section; drops oldest history on overflow before hit-shrinking
mur-core/src/conversations/ollama.rs                  + mock_generate branch on "Standalone question:"
mur-core/src/conversations/paths.rs                   + ask_session_path, ask_session_history_dir
mur-core/src/cmd/conversations_cmd.rs                 AskArgs gets continue_flag / new_flag / show_session flags; cmd_ask rewritten to orchestrate session load → rewrite → retrieval → generation → append
mur-core/src/main.rs                                  Commands::Ask variant gains the three new bool flags with clap mutex/exclusive; dispatch arm extended
mur-core/tests/cli_conversations.rs                   + 4 integration tests (continue appends, new archives, show-session prints, continue without prior errors)
scripts/golden-path-conversations.sh                  Steps 16 + 17; banner 15 → 17
```

No new Cargo dependencies. No LanceDB schema changes. No commander sync extension.

---

## Task Overview (9 tasks)

| # | Task | Model | Depends on |
|---|------|-------|------------|
| 1 | Config: `AskConfig.continue_history_turns` + plumbing | haiku | — |
| 2 | Paths: `ask_session_path` + `ask_session_history_dir` | haiku | — |
| 3 | Session store: `TurnRecord` + `Session` + `SessionStore` | haiku | 2 |
| 4 | Rewriter module: `rewrite()` + `CONDENSE_PROMPT` + history rendering | haiku | 3 |
| 5 | Ollama mock branch for `"Standalone question:"` | haiku | — |
| 6 | Prompt render extension — chat history section + budget priority | sonnet | 3 |
| 7 | cmd_ask wiring + main.rs Ask flags + ask_stream extension + --show-session | sonnet | 1, 3, 4, 5, 6 |
| 8 | Integration tests in `cli_conversations.rs` | haiku | 7 |
| 9 | Golden path Steps 16 & 17 | haiku | 7 |

---

## Task 1: Foundations — `AskConfig.continue_history_turns`

**Files:**
- Modify: `mur-common/src/config.rs`

- [ ] **Step 1: Failing test** — append to the existing `#[cfg(test)] mod conversations_tests` in `mur-common/src/config.rs`:

```rust
    #[test]
    fn ask_config_default_continue_history_turns_is_3() {
        let c = AskConfig::default();
        assert_eq!(c.continue_history_turns, 3);
    }
```

- [ ] **Step 2: Run — must fail** with "no field `continue_history_turns` on type `AskConfig`":

```
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/conversations-phase-3-3
cargo test -p mur-common conversations_tests::ask_config_default_continue_history_turns
```

- [ ] **Step 3: Implement** — in `mur-common/src/config.rs`, inside the existing `pub struct AskConfig` (around line 319), add a new field at the end (after `min_score`):

```rust
    #[serde(default = "ask_default_continue_history_turns")]
    pub continue_history_turns: u32,
```

Inside `impl Default for AskConfig` (around line 342), add to the struct literal:

```rust
            continue_history_turns: ask_default_continue_history_turns(),
```

After the existing `ask_default_min_score` fn (around line 383), add:

```rust
fn ask_default_continue_history_turns() -> u32 {
    3
}
```

- [ ] **Step 4: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-common conversations_tests::ask_config_default
```

Expected: 1 passed (or more if existing defaults tests also match the prefix).

- [ ] **Step 5: Full-suite sanity + lint**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-common
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

- [ ] **Step 6: Commit**

```
git add mur-common/src/config.rs
git commit -m "$(cat <<'EOF'
feat(common): AskConfig.continue_history_turns (Phase 3.3)

Adds a u32 field (default 3) controlling how many prior turns the
Phase 3.3 rolling window feeds into the generation prompt for
`mur ask --continue`. Research-backed default (LangChain
ConversationBufferWindowMemory; Chroma "Context Rot" 2025).

Plan: Task 1 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §8

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Path helpers — `ask_session_path` + `ask_session_history_dir`

**Files:**
- Modify: `mur-core/src/conversations/paths.rs`

- [ ] **Step 1: Failing tests** — append to `#[cfg(test)] mod tests` in `paths.rs`:

```rust
    #[test]
    fn ask_session_path_shape() {
        let p = ask_session_path(Some("/tmp/mur-test"));
        assert_eq!(
            p,
            std::path::PathBuf::from("/tmp/mur-test/conversations/ask-session.jsonl")
        );
    }

    #[test]
    fn ask_session_history_dir_shape() {
        let p = ask_session_history_dir(Some("/tmp/mur-test"));
        assert_eq!(
            p,
            std::path::PathBuf::from(
                "/tmp/mur-test/conversations/ask-sessions/.history"
            )
        );
    }
```

- [ ] **Step 2: Run — must fail** (fns not defined):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::paths::tests::ask_session
```

- [ ] **Step 3: Implement** — in `mur-core/src/conversations/paths.rs`, after the existing `monthly_history_dir` fn (around line 106), add:

```rust
/// Path to the active multi-turn Ask session file (`ask-session.jsonl`).
/// Phase 3.3: single global session; `--new` archives prior to `.history/`.
pub fn ask_session_path(root_override: Option<&str>) -> PathBuf {
    conversations_root(root_override).join("ask-session.jsonl")
}

/// Directory that holds archived Ask session files (one per `--new` call).
pub fn ask_session_history_dir(root_override: Option<&str>) -> PathBuf {
    conversations_root(root_override)
        .join("ask-sessions")
        .join(".history")
}
```

- [ ] **Step 4: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::paths::tests
```

- [ ] **Step 5: Lint + commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/paths.rs
git commit -m "$(cat <<'EOF'
feat(core): ask-session path helpers (Phase 3.3)

ask_session_path → <root>/.mur/conversations/ask-session.jsonl
ask_session_history_dir → <root>/.mur/conversations/ask-sessions/.history

Used by Task 3's SessionStore for --continue / --new / --show-session.

Plan: Task 2 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Session store — `SessionStore`, `TurnRecord`, `RewriterStatus`

**Files:**
- Create: `mur-core/src/conversations/ask/session.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs` (register `pub mod session;`)

Complex task — multiple sub-sections (3a–3f) each with its own failing test. Uses haiku.

### 3a. Skeleton + types

- [ ] **Step 1: Create the module skeleton**

Create `mur-core/src/conversations/ask/session.rs`:

```rust
//! Multi-turn session persistence for Phase 3.3 `mur ask --continue`.
//!
//! One session = one JSONL file at `~/.mur/conversations/ask-session.jsonl`.
//! Each line is a `TurnRecord`. `SessionStore` provides load / archive / append.
//! No summarization, no named sessions — see spec §2 for deferrals.
#![allow(dead_code)] // wired progressively across Tasks 3-7.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::{Citation, HitInfo};

/// Rewriter disposition for a turn. Stored in `TurnRecord.rewriter_status`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RewriterStatus {
    /// Turn 1, `--continue` not passed, or session empty → no LLM call.
    Skipped,
    /// Rewriter returned a differing standalone question.
    Rewrote,
    /// Rewriter echoed the raw question verbatim (LangChain "return as is").
    NoRewriteNeeded,
    /// Ollama error on rewrite; retrieval used the raw question.
    FailedFellBackToRaw,
}

/// Single turn event in an Ask session. Append-only JSONL; one line per turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub v: u32,
    pub turn_id: u32,
    pub ts: DateTime<Utc>,
    pub question: String,
    pub rewritten_question: Option<String>,
    pub hits_used: Vec<HitInfo>,
    pub answer: String,
    pub citations: Vec<Citation>,
    pub degraded_to_mode_b: bool,
    pub rewriter_status: RewriterStatus,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

/// In-memory view of the Ask session.
pub struct Session {
    pub turns: Vec<TurnRecord>,
    path: PathBuf,
}

impl Session {
    /// Last `n` turns, oldest first. Empty slice if session empty.
    pub fn last_n(&self, n: u32) -> &[TurnRecord] {
        let n = n as usize;
        if self.turns.len() <= n {
            &self.turns[..]
        } else {
            &self.turns[self.turns.len() - n..]
        }
    }

    /// Turn-id for the next `append_turn`.
    pub fn next_turn_id(&self) -> u32 {
        self.turns.last().map(|t| t.turn_id + 1).unwrap_or(1)
    }
}

/// Loader / archiver / appender. All public entry points take `root_override`
/// so tests can point at a tempdir.
pub struct SessionStore;

// Implementations added below in 3b–3e.

#[cfg(test)]
mod tests {
    use super::*;
    // Fixtures + tests added below.
}
```

Register in `mur-core/src/conversations/ask/mod.rs` — find the existing `pub mod` block (lines 7–11 in Phase 3.2 HEAD):

```rust
pub mod cite;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;
```

Add (alphabetically after `retrieve`):

```rust
pub mod rewriter;
pub mod session;
```

Note: `rewriter` is Task 4's file. Add it now (module declaration only) so Task 4 just appends the implementation; alternatively, add it only in Task 4 and leave Task 3 with just `pub mod session;`. **Recommendation: add only `pub mod session;` here in Task 3** — Task 4 adds its own `pub mod rewriter;`. This keeps commits atomic.

Final edit for Task 3 to `mod.rs`:

```rust
pub mod cite;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;
pub mod session;
```

### 3b. `SessionStore::load_latest` — happy path

- [ ] **Step 2: Failing test** — append inside `#[cfg(test)] mod tests` in `session.rs`:

```rust
    fn dummy_turn(id: u32, q: &str) -> TurnRecord {
        TurnRecord {
            v: 1,
            turn_id: id,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: format!("answer for {q}"),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: RewriterStatus::Skipped,
            tokens_in: 100,
            tokens_out: 50,
            duration_ms: 1000,
        }
    }

    #[test]
    fn load_latest_on_missing_file_returns_empty_session() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert!(session.turns.is_empty());
    }

    #[test]
    fn load_latest_parses_valid_jsonl_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let p = crate::conversations::paths::ask_session_path(Some(root));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let t1 = serde_json::to_string(&dummy_turn(1, "q1")).unwrap();
        let t2 = serde_json::to_string(&dummy_turn(2, "q2")).unwrap();
        std::fs::write(&p, format!("{t1}\n{t2}\n")).unwrap();

        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(session.turns.len(), 2);
        assert_eq!(session.turns[0].turn_id, 1);
        assert_eq!(session.turns[1].question, "q2");
    }
```

- [ ] **Step 3: Run — must fail** (`SessionStore::load_latest` not defined):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests::load_latest
```

- [ ] **Step 4: Implement** — append inside the `impl SessionStore` block (replace the `// Implementations added below in 3b–3e.` comment). Add the whole `impl SessionStore` block:

```rust
impl SessionStore {
    /// Load the current session from disk. Returns an empty `Session` if the
    /// file is missing or empty (not an error — caller decides policy).
    /// Malformed lines are skipped with a `tracing::warn!`; the rest load.
    pub fn load_latest(root_override: Option<&str>) -> Result<Session> {
        let path = crate::conversations::paths::ask_session_path(root_override);
        let mut session = Session {
            turns: Vec::new(),
            path: path.clone(),
        };
        if !path.exists() {
            return Ok(session);
        }
        let file = std::fs::File::open(&path)
            .with_context(|| format!("open {path:?}"))?;
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("session line {i}: read error: {e:#}");
                    continue;
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<TurnRecord>(line) {
                Ok(t) => session.turns.push(t),
                Err(e) => tracing::warn!(
                    "session line {i}: malformed TurnRecord, skipping: {e}"
                ),
            }
        }
        Ok(session)
    }
}
```

- [ ] **Step 5: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests::load_latest
```

Expected: 2 passed.

### 3c. `SessionStore::load_latest` — malformed-line resilience

- [ ] **Step 6: Failing test** — append inside `mod tests`:

```rust
    #[test]
    fn load_latest_skips_malformed_lines_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let p = crate::conversations::paths::ask_session_path(Some(root));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let good1 = serde_json::to_string(&dummy_turn(1, "q1")).unwrap();
        let good2 = serde_json::to_string(&dummy_turn(2, "q2")).unwrap();
        std::fs::write(
            &p,
            format!("{good1}\nthis is not JSON at all\n{good2}\n"),
        )
        .unwrap();

        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(
            session.turns.len(),
            2,
            "malformed line should be skipped, both good turns preserved"
        );
        assert_eq!(session.turns[0].turn_id, 1);
        assert_eq!(session.turns[1].turn_id, 2);
    }
```

- [ ] **Step 7: Run — must pass** (the existing implementation already handles this):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests::load_latest_skips_malformed
```

### 3d. `SessionStore::append_turn`

- [ ] **Step 8: Failing test** — append:

```rust
    #[test]
    fn append_turn_creates_file_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        assert!(session.turns.is_empty());

        SessionStore::append_turn(&mut session, dummy_turn(1, "q1")).unwrap();

        let loaded = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(loaded.turns.len(), 1);
        assert_eq!(loaded.turns[0].question, "q1");
    }

    #[test]
    fn append_turn_appends_to_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(1, "q1")).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(2, "q2")).unwrap();

        let loaded = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.turns[1].question, "q2");
    }
```

- [ ] **Step 9: Run — must fail** (`append_turn` not defined).

- [ ] **Step 10: Implement** — inside the `impl SessionStore` block, add:

```rust
    /// Append a turn to the session file + update the in-memory turns vec.
    /// Creates the file (and parent dir) if missing.
    /// `sync_all()` before return to guarantee crash durability of the line.
    pub fn append_turn(session: &mut Session, turn: TurnRecord) -> Result<()> {
        if let Some(parent) = session.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(&turn)
            .context("serialize TurnRecord")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session.path)
            .with_context(|| format!("open session for append {:?}", session.path))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        session.turns.push(turn);
        Ok(())
    }
```

- [ ] **Step 11: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests::append_turn
```

Expected: 2 passed.

### 3e. `SessionStore::archive_and_new` + `prune_history`

- [ ] **Step 12: Failing test** — append:

```rust
    #[test]
    fn archive_and_new_renames_prior_to_history() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // Seed an existing session
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(1, "first")).unwrap();
        assert!(
            crate::conversations::paths::ask_session_path(Some(root)).exists()
        );

        let fresh = SessionStore::archive_and_new(Some(root), 5).unwrap();
        assert!(fresh.turns.is_empty());
        assert!(
            !crate::conversations::paths::ask_session_path(Some(root))
                .exists(),
            "active session should be gone after archive"
        );
        let hist =
            crate::conversations::paths::ask_session_history_dir(Some(root));
        let entries: Vec<_> = std::fs::read_dir(&hist).unwrap().collect();
        assert_eq!(entries.len(), 1, "one archived session expected");
    }

    #[test]
    fn archive_and_new_is_noop_on_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let fresh = SessionStore::archive_and_new(Some(root), 5).unwrap();
        assert!(fresh.turns.is_empty());
        let hist =
            crate::conversations::paths::ask_session_history_dir(Some(root));
        assert!(
            !hist.exists() || std::fs::read_dir(&hist).unwrap().count() == 0
        );
    }

    #[test]
    fn archive_and_new_prunes_history_per_retain_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // Seed 7 rapid archives with retain=5; distinct content so each actually archives.
        for i in 0..7 {
            let mut s = SessionStore::load_latest(Some(root)).unwrap();
            SessionStore::append_turn(&mut s, dummy_turn(1, &format!("q{i}")))
                .unwrap();
            SessionStore::archive_and_new(Some(root), 5).unwrap();
            // Each archive uses seconds-granularity timestamps, so pause briefly
            // to guarantee distinct filenames.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let hist =
            crate::conversations::paths::ask_session_history_dir(Some(root));
        let entries: Vec<_> = std::fs::read_dir(&hist)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 5, "retain=5 should cap history at 5");
    }
```

- [ ] **Step 13: Run — must fail** (`archive_and_new` not defined).

- [ ] **Step 14: Implement** — inside `impl SessionStore`, add:

```rust
    /// Archive the current session into `.history/<utc>.jsonl` and return a
    /// fresh empty `Session`. No-op if the active file is missing or empty.
    /// `retain` caps `.history/` by count (oldest dropped first).
    pub fn archive_and_new(
        root_override: Option<&str>,
        retain: u32,
    ) -> Result<Session> {
        let path = crate::conversations::paths::ask_session_path(root_override);
        let hist_dir =
            crate::conversations::paths::ask_session_history_dir(root_override);

        // Archive only if active file exists and is non-empty.
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > 0 {
            std::fs::create_dir_all(&hist_dir)?;
            let stamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
            let dest = hist_dir.join(format!("{stamp}.jsonl"));
            std::fs::rename(&path, &dest)
                .with_context(|| format!("archive {path:?} -> {dest:?}"))?;
            prune_history(&hist_dir, retain)?;
        }
        Ok(Session {
            turns: Vec::new(),
            path,
        })
    }
}

/// Keep the `retain` most recent files in `hist_dir`; delete older.
/// Sort ascending by name — since files are named `YYYY-MM-DDTHH-MM-SSZ.jsonl`,
/// alphabetical == chronological.
fn prune_history(hist_dir: &std::path::Path, retain: u32) -> Result<()> {
    if !hist_dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(hist_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    if entries.len() <= retain as usize {
        return Ok(());
    }
    entries.sort();
    let drop_count = entries.len() - retain as usize;
    for p in entries.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}
```

- [ ] **Step 15: Run — must pass** (the prune test takes ~8s due to the sleep; be patient):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests::archive
```

Expected: 3 passed.

### 3f. `Session::last_n` + `next_turn_id`

- [ ] **Step 16: Failing test** — append:

```rust
    #[test]
    fn last_n_returns_correct_slice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut s = SessionStore::load_latest(Some(root)).unwrap();
        for i in 1..=5 {
            SessionStore::append_turn(&mut s, dummy_turn(i, &format!("q{i}")))
                .unwrap();
        }
        let last3 = s.last_n(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].turn_id, 3);
        assert_eq!(last3[2].turn_id, 5);

        let last10 = s.last_n(10);
        assert_eq!(
            last10.len(),
            5,
            "asking for more than available returns all"
        );
    }

    #[test]
    fn next_turn_id_is_1_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let s = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(s.next_turn_id(), 1);
    }

    #[test]
    fn next_turn_id_is_last_plus_1() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut s = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut s, dummy_turn(1, "q")).unwrap();
        SessionStore::append_turn(&mut s, dummy_turn(2, "q")).unwrap();
        assert_eq!(s.next_turn_id(), 3);
    }
```

- [ ] **Step 17: Run — must pass** (methods already defined in the skeleton):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::session::tests
```

Expected: 10 tests pass (2+1+2+3+2 across 3b-3f).

### 3g. Commit

- [ ] **Step 18: Full suite + lint + commit**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/session.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): ask::session — SessionStore + TurnRecord (Phase 3.3)

New module managing the multi-turn Ask session file:
  - TurnRecord: per-turn JSONL record (v, turn_id, ts, question,
    rewritten_question, hits_used, answer, citations, degraded flags,
    rewriter_status, tokens, duration_ms).
  - RewriterStatus enum: Skipped | Rewrote | NoRewriteNeeded |
    FailedFellBackToRaw.
  - Session: in-memory view + last_n / next_turn_id helpers.
  - SessionStore::load_latest: parse JSONL, skip malformed lines with
    tracing::warn! (never fail whole load on one bad line).
  - SessionStore::append_turn: atomic append + fsync; creates file +
    parent dir if missing.
  - SessionStore::archive_and_new: rename active → .history/<utc>.jsonl,
    prune by retain config; no-op on empty.

Plan: Task 3 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §4

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Rewriter module — `rewrite()` + `CONDENSE_PROMPT`

**Files:**
- Create: `mur-core/src/conversations/ask/rewriter.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs` (add `pub mod rewriter;`)

### 4a. Skeleton + types

- [ ] **Step 1: Add module declaration** — in `mur-core/src/conversations/ask/mod.rs`, the existing `pub mod` block (after Task 3) should read:

```rust
pub mod cite;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;
pub mod session;
```

Add `rewriter`:

```rust
pub mod cite;
pub mod format;
pub mod generate;
pub mod prompt;
pub mod retrieve;
pub mod rewriter;
pub mod session;
```

- [ ] **Step 2: Create the module skeleton**

Create `mur-core/src/conversations/ask/rewriter.rs`:

```rust
//! Query rewriting for Phase 3.3 `mur ask --continue` (§5 of spec).
//!
//! One Ollama call per follow-up turn. Canonical LangChain
//! "condense question" prompt — see `CONDENSE_PROMPT`.
//! Failure modes (timeout / empty / etc.) fall back to raw question.
#![allow(dead_code)] // wired by Task 7.

use crate::conversations::ollama::{GenerateOptions, GenerateRequest, OllamaClient};
use std::time::Duration;

use super::session::{RewriterStatus, TurnRecord};

/// Canonical LangChain "condense question" prompt. Verbatim from the
/// LangChain docs — widely used across LangChain/LlamaIndex/Haystack.
/// The "return it as is" clause means identity is always a legal output.
pub(crate) const CONDENSE_PROMPT_TEMPLATE: &str = "Given a chat history and the latest user question \
which might reference context in the chat history, formulate a standalone \
question which can be understood without the chat history. Do NOT answer \
the question, just reformulate it if needed and otherwise return it as is.\n\n\
Chat history:\n{history}\n\n\
Latest question: {question}\n\n\
Standalone question:";

/// Max chars of a prior turn's answer to include in the rewriter's `{history}`.
/// Keeps the rewrite prompt bounded; the full answer is not needed to resolve anaphora.
pub(crate) const PRIOR_ANSWER_TRUNCATE_CHARS: usize = 500;

pub struct RewriteInput<'a> {
    pub prior_turns: &'a [TurnRecord],
    pub raw_question: &'a str,
}

pub struct RewriteResult {
    pub rewritten: String,
    pub status: RewriterStatus,
}

// Implementations below in 4b–4d.

#[cfg(test)]
mod tests {
    use super::*;
    // Tests added below.
}
```

### 4b. History rendering helper

- [ ] **Step 3: Failing test** — append inside `mod tests`:

```rust
    fn trec(id: u32, q: &str, a: &str) -> TurnRecord {
        TurnRecord {
            v: 1,
            turn_id: id,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: a.into(),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: RewriterStatus::Skipped,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn render_history_truncates_long_answers() {
        let long_answer = "x".repeat(2000);
        let turns = vec![trec(1, "what?", &long_answer)];
        let rendered = render_history(&turns);
        assert!(rendered.contains("User: what?"));
        // Should contain at most PRIOR_ANSWER_TRUNCATE_CHARS of the answer
        // plus an ellipsis "…".
        assert!(rendered.contains("x".repeat(PRIOR_ANSWER_TRUNCATE_CHARS).as_str()));
        assert!(!rendered.contains("x".repeat(PRIOR_ANSWER_TRUNCATE_CHARS + 1).as_str()));
        assert!(rendered.contains('…'));
    }

    #[test]
    fn render_history_multiple_turns_in_order() {
        let turns = vec![trec(1, "q1", "a1"), trec(2, "q2", "a2")];
        let rendered = render_history(&turns);
        let q1_pos = rendered.find("User: q1").unwrap();
        let q2_pos = rendered.find("User: q2").unwrap();
        assert!(q1_pos < q2_pos);
        assert!(rendered.contains("Assistant: a1"));
        assert!(rendered.contains("Assistant: a2"));
    }
```

- [ ] **Step 4: Run — must fail** (`render_history` not defined):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::rewriter::tests::render_history
```

- [ ] **Step 5: Implement `render_history`** — inside `rewriter.rs`, above the `#[cfg(test)]` block:

```rust
/// Render prior turns into the `{history}` substitution for CONDENSE_PROMPT.
/// Each turn → "User: <q>\nAssistant: <a_truncated>\n".
pub(crate) fn render_history(prior_turns: &[TurnRecord]) -> String {
    let mut out = String::new();
    for t in prior_turns {
        out.push_str("User: ");
        out.push_str(&t.question);
        out.push('\n');
        out.push_str("Assistant: ");
        out.push_str(&truncate_chars(&t.answer, PRIOR_ANSWER_TRUNCATE_CHARS));
        out.push('\n');
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
```

- [ ] **Step 6: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::rewriter::tests::render_history
```

Expected: 2 passed.

### 4c. `rewrite()` — empty prior turns short-circuit

- [ ] **Step 7: Failing test** — append:

```rust
    #[tokio::test]
    async fn empty_prior_turns_returns_identity_without_calling_ollama() {
        // Unreachable endpoint — if we accidentally call Ollama, we'd panic/error.
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(100));
        let input = RewriteInput {
            prior_turns: &[],
            raw_question: "what did I ship?",
        };
        let r = rewrite(&client, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::Skipped);
        assert_eq!(r.rewritten, "what did I ship?");
    }
```

- [ ] **Step 8: Run — must fail** (`rewrite` not defined).

- [ ] **Step 9: Implement** — above `#[cfg(test)]`:

```rust
/// Decontextualize `raw_question` against `prior_turns`.
///
/// Short-circuits to `Skipped` when `prior_turns` is empty — no LLM call.
/// On Ollama error, returns `FailedFellBackToRaw`. On identity echo
/// (trimmed, case-insensitive), returns `NoRewriteNeeded`.
pub async fn rewrite(
    client: &OllamaClient,
    model: &str,
    input: RewriteInput<'_>,
) -> RewriteResult {
    if input.prior_turns.is_empty() {
        return RewriteResult {
            rewritten: input.raw_question.to_string(),
            status: RewriterStatus::Skipped,
        };
    }

    let history = render_history(input.prior_turns);
    let prompt = CONDENSE_PROMPT_TEMPLATE
        .replace("{history}", &history)
        .replace("{question}", input.raw_question);

    let resp = client
        .generate(GenerateRequest {
            model,
            prompt: &prompt,
            system: None,
            stream: false,
            options: GenerateOptions {
                temperature: Some(0.1),
                top_p: Some(0.9),
                num_predict: Some(80),
                stop: vec!["\n".into()],
            },
        })
        .await;

    match resp {
        Err(e) => {
            tracing::warn!("rewriter Ollama error: {e:#}");
            RewriteResult {
                rewritten: input.raw_question.to_string(),
                status: RewriterStatus::FailedFellBackToRaw,
            }
        }
        Ok(r) => {
            let trimmed = r.response.trim().to_string();
            if trimmed.is_empty() {
                tracing::warn!("rewriter returned empty response; falling back to raw");
                return RewriteResult {
                    rewritten: input.raw_question.to_string(),
                    status: RewriterStatus::FailedFellBackToRaw,
                };
            }
            let status = if trimmed.to_lowercase()
                == input.raw_question.trim().to_lowercase()
            {
                RewriterStatus::NoRewriteNeeded
            } else {
                RewriterStatus::Rewrote
            };
            RewriteResult {
                rewritten: if status == RewriterStatus::NoRewriteNeeded {
                    input.raw_question.to_string()
                } else {
                    trimmed
                },
                status,
            }
        }
    }
}
```

- [ ] **Step 10: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::rewriter::tests::empty_prior
```

### 4d. `rewrite()` — failure path test

- [ ] **Step 11: Failing test** — append:

```rust
    #[tokio::test]
    async fn connection_failure_returns_fallback_to_raw() {
        // Real (not mock) mode with unreachable endpoint.
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(200));
        let turns = vec![trec(1, "first q", "first a")];
        let input = RewriteInput {
            prior_turns: &turns,
            raw_question: "follow up",
        };
        let r = rewrite(&client, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::FailedFellBackToRaw);
        assert_eq!(r.rewritten, "follow up");
    }
```

Gate the env removal with the `ENV_LOCK` mutex to avoid racing other tests:

```rust
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn connection_failure_returns_fallback_to_raw() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
        let client = OllamaClient::new("http://127.0.0.1:1", Duration::from_millis(200));
        let turns = vec![trec(1, "first q", "first a")];
        let input = RewriteInput {
            prior_turns: &turns,
            raw_question: "follow up",
        };
        let r = rewrite(&client, "qwen3:14b", input).await;
        assert_eq!(r.status, RewriterStatus::FailedFellBackToRaw);
        assert_eq!(r.rewritten, "follow up");
    }
```

- [ ] **Step 12: Run — must pass** (`rewrite` already handles errors per Step 9):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::rewriter::tests::connection_failure
```

### 4e. Commit

- [ ] **Step 13: Full suite + lint + commit**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/rewriter.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): ask::rewriter — LangChain-style condense rewriter (Phase 3.3)

One Ollama call per --continue follow-up turn. Prompt is the canonical
LangChain condense-question template verbatim. The "if needed and
otherwise return it as is" clause means identity is always legal; the
caller maps identity responses to NoRewriteNeeded status.

Short-circuits to Skipped (no LLM call) when prior_turns is empty.
Connection errors / empty responses fall back to raw with
FailedFellBackToRaw status.

render_history truncates each prior answer to 500 chars — full answer
text isn't needed for anaphora resolution; tighter prompt = lower
latency + lower token cost.

Plan: Task 4 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §5

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Ollama mock branch — `"Standalone question:"`

**Files:**
- Modify: `mur-core/src/conversations/ollama.rs`

Small surgical change. The mock currently branches on `"Extract the 1-3 most informative spans"`, `"narrative paragraph"`, `"[cit:"`. We add `"Standalone question:"` before `"[cit:"` (distinct prefix; rewriter prompts don't contain narrative/extractive markers).

The semantics: mock returns a deterministic "rewrite" for testing. Per spec, `MUR_OLLAMA_MOCK=1` returns the raw latest question (identity — matches LangChain fallback). Tests that want to observe a distinct rewrite should use `MUR_OLLAMA_MOCK=hash` **or** rely on the `NoRewriteNeeded` status that identity produces.

### 5a. Failing test + implementation

- [ ] **Step 1: Failing test** — append to the existing `#[cfg(test)] mod tests` in `ollama.rs` (after `real_call_errors_on_unreachable_endpoint`):

```rust
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mock_returns_identity_for_standalone_question_prompt() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let client = OllamaClient::new("http://unused", Duration::from_secs(1));
        let prompt = "Given a chat history and the latest user question \
                     which might reference context in the chat history, \
                     formulate a standalone question which can be understood \
                     without the chat history. Do NOT answer the question, \
                     just reformulate it if needed and otherwise return it as is.\n\n\
                     Chat history:\nUser: q1\nAssistant: a1\n\n\
                     Latest question: what did I ship yesterday?\n\n\
                     Standalone question:";
        let req = GenerateRequest {
            model: "qwen3:14b",
            prompt,
            system: None,
            stream: false,
            options: GenerateOptions::default(),
        };
        let resp = client.generate(req).await.unwrap();
        assert_eq!(
            resp.response.trim(),
            "what did I ship yesterday?",
            "mock should echo the raw 'Latest question:' as the standalone form"
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
```

- [ ] **Step 2: Run — must fail** (mock currently falls through to the default `"mock response for model=..."` for this prompt):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ollama::tests::mock_returns_identity_for_standalone
```

- [ ] **Step 3: Implement** — in `ollama.rs::mock_generate` (around line 235), the existing branch cascade is:

```rust
fn mock_generate(req: &GenerateRequest<'_>) -> GenerateResponse {
    let response = if req.prompt.contains("Extract the 1-3 most informative spans") {
        r#"[{"role":"user","conv_id":"mock","line_hint":1,"text":"mock extractive span"}]"#.to_string()
    } else if req.prompt.contains("narrative paragraph") {
        if req.prompt.contains("one week") || req.prompt.contains("one-week") {
            "Mock narrative: this week the developer shipped several fixes and refactors.".to_string()
        } else if req.prompt.contains("one month") || req.prompt.contains("one-month") {
            "Mock narrative: this month saw major work on the conversations archive.".to_string()
        } else {
            "Mock narrative: today the developer explored mock compression.".to_string()
        }
    } else if req.prompt.contains("[cit:") {
        "Mock answer about the archive [cit: 2026-04-19 claude-code/mock:L1].".to_string()
    } else {
        format!("mock response for model={}", req.model)
    };
    // ...
```

Insert a new branch **before the `[cit:` branch** (Standalone-question prompts don't contain citation markers):

```rust
    } else if req.prompt.contains("Standalone question:") {
        // Phase 3.3 rewriter: identity (echo the raw latest question).
        // Matches LangChain prompt's "return it as is" fallback.
        extract_latest_question_from_condense_prompt(req.prompt)
    } else if req.prompt.contains("[cit:") {
```

Then add the helper (above `mock_generate`):

```rust
/// Given a CONDENSE-style prompt with "Latest question: <q>\n\nStandalone question:"
/// extract the raw `<q>` for the identity-echo mock path. Returns the literal
/// raw question on any parse failure (matches "return it as is" fallback).
fn extract_latest_question_from_condense_prompt(prompt: &str) -> String {
    let start_tag = "Latest question: ";
    let Some(start) = prompt.find(start_tag) else {
        return prompt.to_string();
    };
    let rest = &prompt[start + start_tag.len()..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}
```

- [ ] **Step 4: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ollama::tests::mock_returns_identity_for_standalone
```

### 5b. Full suite + commit

- [ ] **Step 5: Full suite sanity** (ensure the existing `"narrative paragraph"` + `"[cit:"` branches still fire correctly):

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ollama::tests
```

Expected: 9 passed (8 existing + 1 new).

- [ ] **Step 6: Lint + commit**

```
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ollama.rs
git commit -m "$(cat <<'EOF'
test(core): ollama mock — identity branch for condense prompts (Phase 3.3)

Phase 3.3 rewriter builds prompts containing "Standalone question:".
The mock now routes those prompts through a new identity branch that
echoes the raw "Latest question: <q>" back — matching the LangChain
"return it as is" fallback.

Deterministic + idempotent: same prompt → same response. Required
so rewriter tests + integration tests + golden-path Step 16 can run
under MUR_OLLAMA_MOCK=1 without hitting real Ollama.

Plan: Task 5 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §5.5

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Prompt render extension — `## Chat History` section + budget priority

**Files:**
- Modify: `mur-core/src/conversations/ask/prompt.rs`

Complex task — render() becomes multi-section-aware with priority-based budget math. Uses sonnet.

### 6a. Extend `render` signature + add history section

- [ ] **Step 1: Failing test** — append to `#[cfg(test)] mod tests` in `prompt.rs`:

```rust
    fn turn_rec(q: &str, a: &str) -> super::super::session::TurnRecord {
        super::super::session::TurnRecord {
            v: 1,
            turn_id: 1,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: a.into(),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: super::super::session::RewriterStatus::Skipped,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn render_includes_chat_history_section_when_prior_turns_non_empty() {
        let hits = vec![hit_raw("a", "one")];
        let prior = vec![turn_rec("prev q", "prev a")];
        let r = render("new q?", &prior, &hits, 6000, 1024);
        assert!(
            r.user.contains("## Chat History"),
            "expected '## Chat History' header, got:\n{}",
            r.user
        );
        assert!(r.user.contains("User: prev q"));
        assert!(r.user.contains("Assistant: prev a"));
        // Current question is still in the user section
        assert!(r.user.contains("new q?"));
    }

    #[test]
    fn render_omits_chat_history_section_when_prior_turns_empty() {
        let hits = vec![hit_raw("a", "one")];
        let r = render("q?", &[], &hits, 6000, 1024);
        assert!(!r.user.contains("## Chat History"));
    }
```

- [ ] **Step 2: Run — must fail** (render's signature doesn't include `prior_turns` yet).

- [ ] **Step 3: Extend `render` signature + history rendering**

In `mur-core/src/conversations/ask/prompt.rs`, change the `render` fn signature from:

```rust
pub fn render(
    question: &str,
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
) -> RenderedPrompt {
```

to:

```rust
pub fn render(
    question: &str,
    prior_turns: &[super::session::TurnRecord],
    hits: &[ResolvedHit],
    max_context_tokens: usize,
    response_tokens: usize,
) -> RenderedPrompt {
```

Add a `use` at the top of the file if needed — the `super::session::TurnRecord` path is fully qualified in the signature so no `use` statement is strictly required.

**Constant for prior-answer truncation** — add near the top of `prompt.rs`:

```rust
/// Chars of each prior answer to include in the `## Chat History` section.
/// Matches `rewriter::PRIOR_ANSWER_TRUNCATE_CHARS` to keep behavior consistent.
const HISTORY_ANSWER_TRUNCATE_CHARS: usize = 500;
```

**Helper fn** — above `render`:

```rust
/// Format the prior-turns block for the generation prompt. Empty string
/// if `turns` is empty (caller decides whether to prepend the `## Chat History`
/// header).
fn render_history_block(turns: &[super::session::TurnRecord]) -> String {
    let mut s = String::new();
    for t in turns {
        s.push_str("User: ");
        s.push_str(&t.question);
        s.push('\n');
        s.push_str("Assistant: ");
        s.push_str(&truncate_chars(&t.answer, HISTORY_ANSWER_TRUNCATE_CHARS));
        s.push('\n');
    }
    s
}
```

**Modify `render` body** — after the existing `ctx` + `valid_citations` loop, but before `let truncated_question = ...`, insert:

```rust
    let history_block = if prior_turns.is_empty() {
        String::new()
    } else {
        format!("## Chat History\n\n{}\n", render_history_block(prior_turns))
    };
```

Then change the `user` format from:

```rust
    let mut user = format!("Context:\n{ctx}\nQuestion: {truncated_question}");
```

to:

```rust
    let mut user = format!(
        "{history_block}## Context\n\n{ctx}\n## Question\n\n{truncated_question}"
    );
```

Also update the overflow-path `user` format (around line 61) identically:

```rust
        user = format!(
            "{history_block}## Context\n\n{ctx2}\n## Question\n\n{truncated_question}"
        );
```

Update `tokens_est` to include `history_block.len()`:

```rust
    let tokens_est = (system.len() + user.len()) / 4 + response_tokens + 120;
```

(No change needed — `user.len()` already includes `history_block` after the format change.)

- [ ] **Step 4: Fix all existing callers of `render`** — the only non-test caller is `ask::ask_stream` in `ask/mod.rs` (around line 162). Update it to pass an empty slice:

In `mur-core/src/conversations/ask/mod.rs`, change:

```rust
    let prompt = prompt::render(
        &req.question,
        &hits,
        req.max_context_tokens,
        req.response_tokens,
    );
```

to (temporarily; Task 7 replaces this with real prior_turns):

```rust
    let prompt = prompt::render(
        &req.question,
        &[],
        &hits,
        req.max_context_tokens,
        req.response_tokens,
    );
```

Also fix existing tests in `prompt.rs` that call `render` with the old signature:

- `render_shrinks_hits_on_overflow` (around line 139): insert `&[],` as the second arg.
- `render_lists_valid_citations_in_order` (around line 149): same.

- [ ] **Step 5: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::prompt::tests
```

Expected: existing tests + 2 new pass.

### 6b. Budget priority — drop oldest history first on overflow

- [ ] **Step 6: Failing test** — append:

```rust
    #[test]
    fn render_drops_oldest_history_first_on_budget_overflow() {
        let hits = vec![hit_raw("a", "unique-hit-content")];
        // Three prior turns, each with a recognizable answer. Budget tight
        // enough that history must drop.
        let prior = vec![
            turn_rec("q1-oldest", "aaaa-oldest-ANSWER"),
            turn_rec("q2-middle", "bbbb-middle-ANSWER"),
            turn_rec("q3-newest", "cccc-newest-ANSWER"),
        ];
        // Budget chosen so history doesn't fit but hit does.
        let r = render("new q?", &prior, &hits, 500, 100);
        // The hit must survive.
        assert!(r.user.contains("unique-hit-content"));
        // Oldest history turn must be dropped before middle/newest.
        let has_oldest = r.user.contains("aaaa-oldest-ANSWER");
        let has_newest = r.user.contains("cccc-newest-ANSWER");
        assert!(
            !(has_oldest && !has_newest),
            "if oldest survives, newest should too (invalid ordering)"
        );
        // Under a very tight budget the oldest should be dropped.
        if has_newest {
            // Expected path: newest survived, oldest dropped
            assert!(!has_oldest, "oldest should be dropped first");
        }
    }
```

- [ ] **Step 7: Run — must fail** (current overflow path only trims hits, not history).

- [ ] **Step 8: Implement budget priority**

In `render`, replace the existing hit-overflow `while` loop body. The new order:

1. If `tokens_est > max_context_tokens`, first try dropping history turns **oldest first**.
2. Only if history is fully exhausted AND still over budget, drop hits from the tail.

Replace everything between `let tokens_est = (system.len() + user.len()) / 4 + response_tokens + 120;` and the final `RenderedPrompt { ... }` with:

```rust
    // Overflow handling: drop oldest history turns first, then shrink hits.
    // Rationale: Chroma "Context Rot" research (2025) — hits matter more than
    // distant history for RAG answer quality.
    let mut history_cursor = 0usize; // # of oldest turns dropped so far
    let mut trimmed_hits = hits.len();

    let mut cur_tokens = tokens_est;
    while cur_tokens > max_context_tokens && history_cursor < prior_turns.len() {
        history_cursor += 1;
        let history_block2 = if history_cursor >= prior_turns.len() {
            String::new()
        } else {
            format!(
                "## Chat History\n\n{}\n",
                render_history_block(&prior_turns[history_cursor..])
            )
        };
        user = format!(
            "{history_block2}## Context\n\n{ctx}\n## Question\n\n{truncated_question}"
        );
        cur_tokens = (system.len() + user.len()) / 4 + response_tokens + 120;
    }

    // If still over budget, shrink hits from the tail.
    let remaining_history_block = if history_cursor >= prior_turns.len() {
        String::new()
    } else {
        format!(
            "## Chat History\n\n{}\n",
            render_history_block(&prior_turns[history_cursor..])
        )
    };
    while cur_tokens > max_context_tokens && trimmed_hits > 1 {
        trimmed_hits -= 1;
        let mut ctx2 = String::new();
        valid_citations.clear();
        for h in hits.iter().take(trimmed_hits) {
            let anchor = cite_anchor(h);
            valid_citations.push(anchor.clone());
            ctx2.push_str(&anchor);
            ctx2.push('\n');
            ctx2.push_str("> ");
            ctx2.push_str(&h.snippet.replace('\n', "\n> "));
            ctx2.push_str("\n\n");
        }
        user = format!(
            "{remaining_history_block}## Context\n\n{ctx2}\n## Question\n\n{truncated_question}"
        );
        cur_tokens = (system.len() + user.len()) / 4 + response_tokens + 120;
    }

    RenderedPrompt {
        system,
        user,
        tokens_est: cur_tokens,
        valid_citations,
    }
```

**Remove** the old simple hit-overflow loop (the original `while tokens_est > max_context_tokens && trimmed_hits > 1 { ... }` block at lines 47–62) since the new logic subsumes it. Delete the original loop + the `RenderedPrompt { ..., tokens_est, ... }` that follows it — replaced by the block above.

Also remove the standalone `let mut trimmed_hits = hits.len();` that was before the old loop (the new block declares it itself).

**Important:** the `ctx` variable is built in the initial loop and remains the "full hits" ctx for use when no history dropping is needed. When shrinking hits, `ctx2` overrides; when only history is dropped, `ctx` is retained. The `remaining_history_block` is recomputed once after history-only dropping — subsequent hit-shrinking uses it verbatim.

- [ ] **Step 9: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::prompt::tests
```

Expected: all prompt tests pass (including the new `render_drops_oldest_history_first_on_budget_overflow`).

### 6c. Fall-through test — hits shrink after history exhausted

- [ ] **Step 10: Failing test** — append:

```rust
    #[test]
    fn render_falls_through_to_hit_shrinking_when_history_exhausted() {
        // 5 hits + 2 prior turns + tight budget. Expect: history fully dropped,
        // then hits start shrinking.
        let hits: Vec<_> = (0..5)
            .map(|i| hit_raw(&format!("c{i}"), &"x".repeat(800)))
            .collect();
        let prior = vec![
            turn_rec("q1", &"yyyyy".repeat(100)),
            turn_rec("q2", &"zzzzz".repeat(100)),
        ];
        let r = render("q?", &prior, &hits, 1500, 300);
        // History should be entirely gone.
        assert!(!r.user.contains("## Chat History"), "history should be dropped");
        // Hits should be shrunk (fewer than 5).
        assert!(
            r.valid_citations.len() < hits.len(),
            "expected hit count < {}, got {}",
            hits.len(),
            r.valid_citations.len()
        );
        assert!(!r.valid_citations.is_empty());
    }
```

- [ ] **Step 11: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::prompt::tests
```

### 6d. Commit

- [ ] **Step 12: Full suite + lint + commit**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check -p mur-core
git add mur-core/src/conversations/ask/prompt.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): ask::prompt — chat history section + budget priority (Phase 3.3)

render() now accepts prior_turns and emits a "## Chat History" section
containing User:/Assistant: lines with per-answer truncation to 500
chars. Section is omitted when prior_turns is empty (first turn, no
--continue, or --show-session).

Generation-prompt structure:
  [SYSTEM]  (unchanged)
  ## Chat History                (new, optional)
  ## Context                     (hits, formerly inline)
  ## Question                    (raw user question)

Overflow handling reordered: drop oldest history turns first, THEN
shrink hits from tail. Chroma "Context Rot" research (2025) shows hits
matter more than distant history for RAG accuracy.

All non-test callers updated (ask_stream passes empty slice; Task 7
wires real prior_turns). Existing prompt tests updated to pass &[]
as the new parameter.

Plan: Task 6 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §6

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: cmd_ask wiring + main.rs flags + `--show-session`

**Files:**
- Modify: `mur-core/src/cmd/conversations_cmd.rs`
- Modify: `mur-core/src/main.rs`
- Modify: `mur-core/src/conversations/ask/mod.rs` (extend `AskRequest` + `AskResponse` + `ask_stream`)

Most integration-heavy task. Uses sonnet.

### 7a. Extend `AskRequest` + `AskResponse` + `ask_stream`

- [ ] **Step 1: Extend types** — in `mur-core/src/conversations/ask/mod.rs`:

Add to `AskRequest` struct (at the end):

```rust
    pub prior_turns: Vec<session::TurnRecord>,
    /// The query actually used for retrieval. If `--continue` + rewriter ran,
    /// this differs from `question`. If `Skipped`, equals `question`.
    pub retrieval_query: String,
    pub rewriter_status: session::RewriterStatus,
```

Add to `AskResponse`:

```rust
    pub rewritten_question: Option<String>,
    pub rewriter_status: session::RewriterStatus,
```

**Update `ask_stream`** to use `retrieval_query` for embedding (NOT `req.question`). Around line 106 in `ask/mod.rs`:

```rust
    let query_embedding = match embed_query(&req.question).await {
```

Change to:

```rust
    let query_embedding = match embed_query(&req.retrieval_query).await {
```

**Update the `prompt::render` call** (around line 162 — currently passes `&[]`):

```rust
    let prompt = prompt::render(
        &req.question,
        &req.prior_turns,
        &hits,
        req.max_context_tokens,
        req.response_tokens,
    );
```

**Update `ask` (the non-streaming convenience fn)** — the existing `ask` builds `AskResponse`. After Task 6's `ask_stream` extension, propagate the new fields through. Around line 282:

```rust
    Ok(AskResponse {
        answer,
        citations,
        hits_used,
        degraded_to_mode_b: degraded,
        tokens_in,
        tokens_out,
        duration_ms,
        rewritten_question: match req.rewriter_status {
            session::RewriterStatus::Skipped => None,
            _ => Some(req.retrieval_query.clone()),
        },
        rewriter_status: req.rewriter_status,
    })
```

Wait — `req` was moved into `ask_stream` above. You need to save the values before moving. Update `ask` (around line 254):

```rust
pub async fn ask(req: AskRequest, root_override: Option<&str>) -> Result<AskResponse> {
    let retrieval_query = req.retrieval_query.clone();
    let rewriter_status = req.rewriter_status;
    let mut stream = ask_stream(req, root_override).await?;
    let mut answer = String::new();
    let mut citations = Vec::new();
    let mut hits_used = Vec::new();
    let mut degraded = false;
    let mut tokens_in = 0;
    let mut tokens_out = 0;
    let mut duration_ms = 0;
    while let Some(evt) = stream.next().await {
        match evt? {
            AskEvent::Token(t) => answer.push_str(&t),
            AskEvent::Citation(c) => citations.push(c),
            AskEvent::HitInfo(h) => hits_used.push(h),
            AskEvent::Done {
                tokens_in: ti,
                tokens_out: to,
                degraded: d,
                duration_ms: ms,
            } => {
                tokens_in = ti;
                tokens_out = to;
                degraded = d;
                duration_ms = ms;
            }
            AskEvent::Error(e) => return Err(anyhow::anyhow!(e)),
        }
    }
    Ok(AskResponse {
        answer,
        citations,
        hits_used,
        degraded_to_mode_b: degraded,
        tokens_in,
        tokens_out,
        duration_ms,
        rewritten_question: match rewriter_status {
            session::RewriterStatus::Skipped => None,
            _ => Some(retrieval_query),
        },
        rewriter_status,
    })
}
```

**Update the existing `ask_end_to_end_mock_empty_hits` test** (around line 367) — it constructs an `AskRequest` literal. Add the new fields:

```rust
        let req = AskRequest {
            question: "What did we do yesterday?".into(),
            filters: Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.35,
            },
            k_summary: 5,
            k_raw: 10,
            escalation_threshold: 0.5,
            mmr_threshold: 0.85,
            model: "qwen3:14b".into(),
            endpoint: "http://unused".into(),
            format: Format::Plain,
            max_context_tokens: 6000,
            response_tokens: 256,
            timeout: Duration::from_secs(5),
            no_escalate: false,
            debug_prompt: false,
            strict_citations: false,
            prior_turns: vec![],
            retrieval_query: "What did we do yesterday?".into(),
            rewriter_status: session::RewriterStatus::Skipped,
        };
```

Also update the test assertion to check the new fields don't break anything:

```rust
        let resp = ask(req, Some(root)).await.unwrap();
        assert!(resp.answer.contains("don't cover that"));
        assert!(resp.citations.is_empty());
        assert!(!resp.degraded_to_mode_b);
        assert!(resp.rewritten_question.is_none());
        assert_eq!(resp.rewriter_status, session::RewriterStatus::Skipped);
```

- [ ] **Step 2: Run — existing tests still pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core conversations::ask::tests
```

### 7b. Extend `AskArgs` + wire `--continue` / `--new` / `--show-session`

- [ ] **Step 3: Extend `AskArgs`** — in `mur-core/src/cmd/conversations_cmd.rs` (around line 970):

```rust
pub struct AskArgs {
    pub question: Option<String>,    // was String; now Option because --show-session has no question
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
    pub strict_citations: bool,
    pub continue_flag: bool,
    pub new_flag: bool,
    pub show_session: bool,
}
```

- [ ] **Step 4: Rewrite `cmd_ask` body** — replace the current body (lines 1074–1174):

```rust
pub async fn cmd_ask(args: AskArgs) -> Result<()> {
    use crate::conversations::ask;
    use crate::conversations::ollama::OllamaClient;
    use chrono::{NaiveDate, Utc};
    use futures::StreamExt;
    use std::io::Write;

    let cfg = crate::store::config::load_config().unwrap_or_default();
    let ask_cfg = cfg.conversations.ask.clone();
    let history_retain = cfg.conversations.compact.history_retain;
    let history_turns = ask_cfg.continue_history_turns;

    // --show-session path: no LLM calls, early return
    if args.show_session {
        return cmd_ask_show_session(None);
    }

    let question = match args.question.clone() {
        Some(q) => q,
        None => anyhow::bail!(
            "question is required (or use --show-session to inspect current session)"
        ),
    };

    // Session management
    let mut session = if args.continue_flag {
        let s = ask::session::SessionStore::load_latest(None)?;
        if s.turns.is_empty() {
            anyhow::bail!(
                "no prior session; run without --continue to start a new one"
            );
        }
        s
    } else {
        ask::session::SessionStore::archive_and_new(None, history_retain)?
    };

    // Rewriter call (only if continuing + we have prior turns)
    let prior_slice = session.last_n(history_turns);
    let model = args.model.clone().unwrap_or_else(|| ask_cfg.model.clone());
    let client = OllamaClient::new(
        &ask_cfg.ollama_endpoint,
        std::time::Duration::from_secs(30),
    );
    let rewrite = ask::rewriter::rewrite(
        &client,
        &model,
        ask::rewriter::RewriteInput {
            prior_turns: prior_slice,
            raw_question: &question,
        },
    )
    .await;
    let retrieval_query = rewrite.rewritten.clone();
    let rewriter_status = rewrite.status;

    // Build AskRequest
    let sources = args.src.as_deref().map(parse_sources).unwrap_or_default();
    let filters = ask::Filters {
        source: sources,
        since: args
            .since
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        until: args
            .until
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        min_score: args.min_score.unwrap_or(ask_cfg.min_score),
    };
    let req = ask::AskRequest {
        question: question.clone(),
        filters,
        k_summary: args.k,
        k_raw: args.k * 2,
        escalation_threshold: ask_cfg.escalation_threshold,
        mmr_threshold: ask_cfg.mmr_threshold,
        model,
        endpoint: ask_cfg.ollama_endpoint.clone(),
        format: if args.json {
            ask::Format::Json
        } else {
            ask::Format::Plain
        },
        max_context_tokens: ask_cfg.max_context_tokens as usize,
        response_tokens: ask_cfg.response_tokens as usize,
        timeout: std::time::Duration::from_secs(ask_cfg.timeout_secs as u64),
        no_escalate: args.no_escalate,
        debug_prompt: args.debug_prompt,
        strict_citations: args.strict_citations,
        prior_turns: prior_slice.to_vec(),
        retrieval_query,
        rewriter_status,
    };

    // Generate + collect response
    let resp = if args.json {
        let r = ask::ask(req, None).await?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        r
    } else {
        let mut stream = ask::ask_stream(req, None).await?;
        let mut answer = String::new();
        let mut citations = Vec::new();
        let mut hits_used = Vec::new();
        let mut degraded = false;
        let mut tokens_in = 0;
        let mut tokens_out = 0;
        let mut duration = 0;
        while let Some(evt) = stream.next().await {
            match evt? {
                ask::AskEvent::Token(t) => {
                    print!("{t}");
                    std::io::stdout().flush()?;
                    answer.push_str(&t);
                }
                ask::AskEvent::Citation(c) => citations.push(c),
                ask::AskEvent::HitInfo(h) => hits_used.push(h),
                ask::AskEvent::Done {
                    tokens_in: ti,
                    tokens_out: to,
                    degraded: d,
                    duration_ms,
                } => {
                    tokens_in = ti;
                    tokens_out = to;
                    degraded = d;
                    duration = duration_ms;
                }
                ask::AskEvent::Error(e) => {
                    eprintln!("\nerror: {e}");
                    std::process::exit(1);
                }
            }
        }
        println!();
        print!(
            "{}{}",
            crate::conversations::ask::format::render_citations_block(&citations),
            crate::conversations::ask::format::render_footer(&ask::AskResponse {
                answer: answer.clone(),
                citations: citations.clone(),
                hits_used: hits_used.clone(),
                degraded_to_mode_b: degraded,
                tokens_in,
                tokens_out,
                duration_ms: duration,
                rewritten_question: match rewriter_status {
                    ask::session::RewriterStatus::Skipped => None,
                    _ => Some(rewrite.rewritten.clone()),
                },
                rewriter_status,
            }),
        );
        ask::AskResponse {
            answer,
            citations,
            hits_used,
            degraded_to_mode_b: degraded,
            tokens_in,
            tokens_out,
            duration_ms: duration,
            rewritten_question: match rewriter_status {
                ask::session::RewriterStatus::Skipped => None,
                _ => Some(rewrite.rewritten.clone()),
            },
            rewriter_status,
        }
    };

    // Persist the turn
    let turn = ask::session::TurnRecord {
        v: 1,
        turn_id: session.next_turn_id(),
        ts: Utc::now(),
        question,
        rewritten_question: resp.rewritten_question.clone(),
        hits_used: resp.hits_used.clone(),
        answer: resp.answer.clone(),
        citations: resp.citations.clone(),
        degraded_to_mode_b: resp.degraded_to_mode_b,
        rewriter_status: resp.rewriter_status,
        tokens_in: resp.tokens_in,
        tokens_out: resp.tokens_out,
        duration_ms: resp.duration_ms,
    };
    ask::session::SessionStore::append_turn(&mut session, turn)?;

    Ok(())
}

/// `mur ask --show-session` handler. No LLM calls.
fn cmd_ask_show_session(root_override: Option<&str>) -> Result<()> {
    let session =
        crate::conversations::ask::session::SessionStore::load_latest(root_override)?;
    let path = crate::conversations::paths::ask_session_path(root_override);
    if session.turns.is_empty() {
        println!("session: {}", path.display());
        println!("no active session. run 'mur ask \"question\"' to start one.");
        return Ok(());
    }
    println!("session: {}", path.display());
    println!("turns: {}", session.turns.len());
    let last = session.turns.last().unwrap();
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(last.ts);
    let delta_str = humanize_duration(delta);
    println!("last turn: {} ({delta_str})", last.ts.to_rfc3339());
    let first = &session.turns[0];
    let first_q = truncate_chars_simple(&first.question, 80);
    println!("first question: \"{first_q}\"");
    let degraded = session
        .turns
        .iter()
        .filter(|t| {
            t.degraded_to_mode_b
                || t.rewriter_status
                    == crate::conversations::ask::session::RewriterStatus::FailedFellBackToRaw
        })
        .count();
    println!("degraded turns: {degraded}");
    Ok(())
}

fn humanize_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{} minutes ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hours ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

fn truncate_chars_simple(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
```

### 7c. Extend `main.rs` Ask variant

- [ ] **Step 5: Extend Ask variant** — in `mur-core/src/main.rs` (around line 349):

```rust
    Ask {
        /// Question to ask. Required unless --show-session is passed.
        question: Option<String>,
        /// Filter results to a specific source (e.g. "cc", "cursor").
        /// Phase 3.2 note: source filtering does NOT apply to weekly/monthly
        /// rollup hits (layer=3/4) — rollup rows use synthetic source strings
        /// ("week"/"month") and are always excluded when --src is passed.
        #[arg(long)]
        src: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value = "5")]
        k: usize,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_score: Option<f64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_escalate: bool,
        #[arg(long)]
        debug_prompt: bool,
        #[arg(long)]
        strict_citations: bool,
        /// Append to the current session (multi-turn mode; Phase 3.3).
        #[arg(long = "continue", conflicts_with = "new")]
        continue_flag: bool,
        /// Archive current session and start fresh (default; flag is explicit for scripts).
        #[arg(long = "new", conflicts_with = "continue_flag")]
        new_flag: bool,
        /// Print current session path, turn count, last turn time.
        /// Ignores question if given; no LLM calls.
        #[arg(long)]
        show_session: bool,
    },
```

- [ ] **Step 6: Extend dispatch** — in the same file around line 1259:

```rust
        Commands::Ask {
            question,
            src,
            since,
            until,
            k,
            model,
            min_score,
            json,
            no_escalate,
            debug_prompt,
            strict_citations,
            continue_flag,
            new_flag,
            show_session,
        } => {
            cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
                question,
                src,
                since,
                until,
                k,
                model,
                min_score,
                json,
                no_escalate,
                debug_prompt,
                strict_citations,
                continue_flag,
                new_flag,
                show_session,
            })
            .await?
        }
```

### 7d. Build + manual smoke

- [ ] **Step 7: Build check**

```
cargo build -p mur-core --bin mur 2>&1 | tail -5
```

Expected: compiles cleanly.

- [ ] **Step 8: Full test suite + lint**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

### 7e. Commit

- [ ] **Step 9: Commit**

```
git add mur-core/src/cmd/conversations_cmd.rs mur-core/src/main.rs mur-core/src/conversations/ask/mod.rs
git commit -m "$(cat <<'EOF'
feat(core): mur ask --continue / --new / --show-session (Phase 3.3)

cmd_ask extended end-to-end:
  1. If --show-session: load + print session status, return.
  2. Load session (--continue requires non-empty; default archives
     prior via SessionStore::archive_and_new with history_retain).
  3. rewriter::rewrite on last N turns + raw question → rewritten +
     status (Skipped if no prior turns).
  4. Build AskRequest with prior_turns, retrieval_query,
     rewriter_status populated.
  5. ask_stream uses retrieval_query for embedding + passes
     prior_turns into prompt::render for the new ## Chat History
     section.
  6. On success (or degraded fallback), append TurnRecord to session
     JSONL via SessionStore::append_turn (atomic + fsync).

AskRequest gains: prior_turns, retrieval_query, rewriter_status.
AskResponse gains: rewritten_question, rewriter_status (surfaces in
--json output for transparency).

main.rs Ask variant:
  - question → Option<String> (required unless --show-session).
  - --continue / --new mutex (clap conflicts_with).
  - --show-session → LLM-free introspection.

Plan: Task 7 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §3.4, §7

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Integration tests — cli_conversations.rs

**Files:**
- Modify: `mur-core/tests/cli_conversations.rs`

4 new integration tests exercising the `mur ask` CLI surface end-to-end.

### 8a. Test 1: --continue appends to session

- [ ] **Step 1: Add test** — append to `mur-core/tests/cli_conversations.rs`:

```rust
#[test]
fn mur_ask_continue_appends_to_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // First turn
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "what did I ship this week?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask #1");
    assert!(
        out1.status.success(),
        "first ask stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let session_path = mur_home
        .join("conversations")
        .join("ask-session.jsonl");
    assert!(session_path.exists(), "session file missing after turn 1");
    let turn1_lines = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(turn1_lines.lines().count(), 1);

    // Second turn with --continue
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--continue", "what about last week?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask #2");
    assert!(
        out2.status.success(),
        "continue ask stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    let body = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(
        body.lines().count(),
        2,
        "expected 2 turns after --continue, got:\n{body}"
    );

    // Second line should have rewriter_status != "skipped" (since there was a prior turn)
    let last_line = body.lines().last().unwrap();
    let turn2: serde_json::Value = serde_json::from_str(last_line).unwrap();
    let status = turn2["rewriter_status"].as_str().unwrap();
    assert_ne!(
        status, "skipped",
        "turn 2 should have invoked rewriter, got status={status}"
    );
}
```

- [ ] **Step 2: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_ask_continue_appends_to_session
```

### 8b. Test 2: --new archives prior session

- [ ] **Step 3: Add test** — append:

```rust
#[test]
fn mur_ask_new_archives_prior_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_mur"))
            .args(["ask", "first topic question"])
            .env("MUR_HOME", &mur_home)
            .env("HOME", tmp.path())
            .env("USERPROFILE", tmp.path())
            .env("MUR_OLLAMA_MOCK", "1")
            .output()
            .expect("run mur ask");
        assert!(out.status.success());
    }

    // After 2 bare `mur ask` invocations (default-archive-before-each), we
    // expect 1 file in .history/ (the first ask's session was archived when
    // the second ask started fresh).
    let hist = mur_home
        .join("conversations")
        .join("ask-sessions")
        .join(".history");
    let entries: Vec<_> = std::fs::read_dir(&hist)
        .expect("history dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected 1 archived session, got {}",
        entries.len()
    );

    // Active session has 1 turn (the second ask).
    let active = mur_home
        .join("conversations")
        .join("ask-session.jsonl");
    assert_eq!(
        std::fs::read_to_string(&active).unwrap().lines().count(),
        1
    );
}
```

- [ ] **Step 4: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_ask_new_archives_prior_session
```

### 8c. Test 3: --show-session prints summary without Ollama

- [ ] **Step 5: Add test** — append:

```rust
#[test]
fn mur_ask_show_session_prints_summary_without_ollama() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Seed one turn under mock
    let seed = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("seed ask");
    assert!(seed.status.success());

    // --show-session WITHOUT MUR_OLLAMA_MOCK
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--show-session"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        // NOTE: deliberately NOT setting MUR_OLLAMA_MOCK
        .output()
        .expect("run --show-session");
    assert!(
        out.status.success(),
        "show-session stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("turns: 1"), "got:\n{stdout}");
    assert!(stdout.contains("what did I ship?"), "got:\n{stdout}");
    assert!(stdout.contains("session:"), "got:\n{stdout}");
}
```

- [ ] **Step 6: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_ask_show_session_prints_summary_without_ollama
```

### 8d. Test 4: --continue without prior errors

- [ ] **Step 7: Add test** — append:

```rust
#[test]
fn mur_ask_continue_without_prior_session_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--continue", "follow-up question"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask --continue");
    assert!(
        !out.status.success(),
        "should have exited non-zero on missing prior session"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no prior session"),
        "expected 'no prior session' in stderr, got:\n{stderr}"
    );
}
```

- [ ] **Step 8: Run — must pass**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core --test cli_conversations mur_ask_continue_without_prior_session_errors
```

### 8e. Commit

- [ ] **Step 9: Full suite + lint + commit**

```
MUR_OLLAMA_MOCK=1 cargo test -p mur-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
git add mur-core/tests/cli_conversations.rs
git commit -m "$(cat <<'EOF'
test(core): integration tests for mur ask --continue (Phase 3.3)

Four end-to-end tests exercising the new CLI surface:
  - mur_ask_continue_appends_to_session: two invocations → 2 JSONL
    lines; second turn has non-skipped rewriter_status.
  - mur_ask_new_archives_prior_session: two bare `mur ask` calls →
    .history/ has 1 archived session; active file has 1 turn.
  - mur_ask_show_session_prints_summary_without_ollama: --show-session
    runs WITHOUT MUR_OLLAMA_MOCK set (proves LLM-free path).
  - mur_ask_continue_without_prior_session_errors: clean state +
    --continue → non-zero exit + "no prior session" message.

Plan: Task 8 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §9.2

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Golden path Steps 16 & 17

**Files:**
- Modify: `scripts/golden-path-conversations.sh`

### 9a. Insert Steps 16 & 17 before banner

- [ ] **Step 1: Inspect current end**

```
tail -30 scripts/golden-path-conversations.sh
```

Current last line: `echo "=== ALL 15 STEPS GREEN ==="`.

- [ ] **Step 2: Insert Steps 16 + 17 before the banner**

Open `scripts/golden-path-conversations.sh`. Find the line `echo "=== ALL 15 STEPS GREEN ==="`. BEFORE it, insert:

```bash
# ── Step 16: mur ask --continue appends follow-up turn ─────────────────
echo "--- step 16: mur ask --continue (multi-turn) ---"
MUR_OLLAMA_MOCK=1 "$MUR" ask "what did I ship this week?" > /tmp/gp-step-16a.txt 2>&1
MUR_OLLAMA_MOCK=1 "$MUR" ask --continue "what about the prior week?" > /tmp/gp-step-16b.txt 2>&1
test -f "$TMPHOME/.mur/conversations/ask-session.jsonl" \
  || { echo "FAIL step 16: ask-session.jsonl missing"; exit 1; }
lines=$(wc -l < "$TMPHOME/.mur/conversations/ask-session.jsonl")
[ "$lines" -eq 2 ] \
  || { echo "FAIL step 16: expected 2 turns in session, got $lines"; exit 1; }
# Second turn must have non-null rewritten_question
jq -e '.rewritten_question != null' \
  <(tail -1 "$TMPHOME/.mur/conversations/ask-session.jsonl") > /dev/null \
  || { echo "FAIL step 16: second turn missing rewritten_question"; exit 1; }
# Second turn must have non-skipped rewriter_status
status=$(jq -r '.rewriter_status' \
  <(tail -1 "$TMPHOME/.mur/conversations/ask-session.jsonl"))
[ "$status" != "skipped" ] \
  || { echo "FAIL step 16: second turn rewriter_status is 'skipped', expected rewrote/no_rewrite_needed/failed"; exit 1; }

# ── Step 17: mur ask --show-session prints summary ─────────────────────
echo "--- step 17: mur ask --show-session ---"
"$MUR" ask --show-session 2>&1 | tee /tmp/gp-step-17.txt
grep -q "turns: 2" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not report turn count"; exit 1; }
grep -q "what did I ship this week" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not echo first question"; exit 1; }
grep -q "session:" /tmp/gp-step-17.txt \
  || { echo "FAIL step 17: show-session did not print session path"; exit 1; }

```

Also update the final banner from `ALL 15 STEPS GREEN` → `ALL 17 STEPS GREEN`.

- [ ] **Step 3: Build binary**

```
cargo build -p mur-core --bin mur 2>&1 | tail -3
```

- [ ] **Step 4: Run golden path**

```
./scripts/golden-path-conversations.sh 2>&1 | tail -30
```

Expected final line: `=== ALL 17 STEPS GREEN ===`. No `FAIL` lines.

**Likely issues and fixes:**

- If Step 16's `lines == 2` fails: verify both `mur ask` calls succeeded. Check `/tmp/gp-step-16a.txt` / `/tmp/gp-step-16b.txt` for errors.
- If Step 16's `rewriter_status != "skipped"` fails: the mock might be returning verbatim-identical text, which maps to `no_rewrite_needed` (acceptable — not skipped). The assertion correctly allows this.
- If Step 17's `turns: 2` fails: confirm Step 16 successfully wrote 2 lines.
- If `show-session` exits non-zero: look at stderr; may need `MUR_OLLAMA_MOCK=1` if any upstream config loading hits Ollama.

### 9b. Rust test suite sanity + commit

- [ ] **Step 5: Full Rust test suite + workspace lint**

```
MUR_OLLAMA_MOCK=1 cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All green.

- [ ] **Step 6: Commit**

```
git add scripts/golden-path-conversations.sh
git commit -m "$(cat <<'EOF'
test(core): golden-path Steps 16 & 17 (Phase 3.3)

Step 16: two sequential `mur ask` invocations — first bare, second
with --continue. Asserts ask-session.jsonl has 2 lines, second line's
rewritten_question is non-null, rewriter_status is non-skipped
(matches any of: rewrote, no_rewrite_needed, failed_fell_back_to_raw).

Step 17: `mur ask --show-session` (no --mock) prints "turns: 2",
echoes the first question, prints session path.

Banner: 15 → 17 steps.

Plan: Task 9 of docs/superpowers/plans/2026-04-21-mur-conversations-phase-3-3.md
Spec: §9.3

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## 🏁 End of Phase 3.3

Single-phase plan. After Task 9, open one PR (`feat/conversations-phase-3-3` → `main`), wait for CI green + reviewer approval, then ship.

**Phase 3.4** (LLMLingua-2 / compression / HyDE conditional rewriting) and beyond get their own spec + plan.
