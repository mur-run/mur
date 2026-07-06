# murmur Panel Input-Driven Suggestions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** As the user types in murmur's message input, the Hub Panel's Recommendations block updates live — debounced snapshots flow murmur→Hub over the existing panel socket; suggestions stay insert-only.

**Architecture:** One new frame `PanelFrame::InputChanged { text }` (debounced 200 ms, redacted, tail-truncated) on the existing Unix-socket panel bridge. Hub stores the latest snapshot per pid in `PanelState` (raw text never enters the webview), pings the Panel webview, which re-queries a new `panel_recommend_input` command. Ranking = adaptive query→picked history > name prefix matches > existing retrieval pipeline, stable name tie-break.

**Tech Stack:** Rust (edition 2024, let-chains OK), tokio, serde/serde_yaml, Tauri 2, React (Panel webview).

**Spec:** `docs/superpowers/specs/2026-07-06-murmur-panel-input-driven-suggestions-design.md`

## Global Constraints

- No hardcoded values: all tunables are constants in `mur_common::panel` — `INPUT_DEBOUNCE_MS = 200`, `MIN_QUERY_CHARS = 2`, `INPUT_SNAPSHOT_MAX_CHARS = 2000`; adaptive params in `mur-core/src/recommend.rs` — `ADAPTIVE_DAILY_DECAY = 0.975`, `ADAPTIVE_EXPIRE_DAYS = 90.0`, `ADAPTIVE_USE_CAP = 10.0`. Result cap 5 and score floor 0.42 are already in code — do not duplicate.
- `PANEL_PROTO_VERSION` bumps `1 → 2`. Unknown frames must stay tolerated on both sides (`decode_line` → `None`).
- Privacy (spec §3.2): `InputChanged` never persisted, never forwarded to relay/mobile/channel; snapshots not per-keystroke deltas; redact secrets before sending; Hub keeps only latest snapshot per pid, dropped on `SessionDown`; never logged above trace.
- Hub→murmur stays `HubFrame::Insert`-only. Nothing auto-executes.
- Single source file ≤ 800 lines (all touched files are well under after edits).
- Build env for mur-core work: `export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and use toolchain cargo if the proxy is broken (`~/.rustup/toolchains/stable-*/bin`). Use `cargo test -p <crate>` (plain `--workspace` has known flakes).
- Before every commit: `git branch --show-current` must be the feature branch (main advances mid-session), then `cargo fmt --all` (CI rustfmt is newer than local).
- Work on branch `feat/panel-input-suggestions` off `main`.

---

### Task 0: Branch

- [ ] **Step 1: Create the feature branch**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git checkout main && git pull && git checkout -b feat/panel-input-suggestions
```

---

### Task 1: Protocol — `InputChanged` frame, constants, redaction (`mur-common`)

**Files:**
- Modify: `mur-common/src/panel.rs`

**Interfaces:**
- Produces: `PanelFrame::InputChanged { text: String }`; `pub const INPUT_DEBOUNCE_MS: u64 = 200`, `pub const MIN_QUERY_CHARS: usize = 2`, `pub const INPUT_SNAPSHOT_MAX_CHARS: usize = 2000`; `pub fn input_snapshot(text: &str) -> String` (redact + tail-truncate — the ONLY sanctioned way to build the frame payload).

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `mur-common/src/panel.rs`:

```rust
    #[test]
    fn input_changed_round_trips() {
        let line = serde_json::to_string(&PanelFrame::InputChanged {
            text: "run book".into(),
        })
        .unwrap();
        assert!(line.contains("\"type\":\"input_changed\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::InputChanged { text }) if text == "run book"
        ));
    }

    #[test]
    fn snapshot_redacts_secrets() {
        // OpenAI-style key
        let s = input_snapshot("use sk-abcdefghijklmnop1234 please");
        assert_eq!(s, "use [redacted] please");
        // AWS access key id
        let s = input_snapshot("key AKIAIOSFODNN7EXAMPLE here");
        assert_eq!(s, "key [redacted] here");
        // 32+ char hex run
        let s = input_snapshot("token 0123456789abcdef0123456789abcdef end");
        assert_eq!(s, "token [redacted] end");
        // Normal prose untouched, including 31-char hex (below threshold)
        assert_eq!(input_snapshot("fix the panel"), "fix the panel");
        let hex31 = "0123456789abcdef0123456789abcde";
        assert_eq!(input_snapshot(hex31), hex31);
    }

    #[test]
    fn snapshot_truncates_to_tail() {
        let long = "a".repeat(INPUT_SNAPSHOT_MAX_CHARS + 100) + " tail";
        let s = input_snapshot(&long);
        assert!(s.chars().count() <= INPUT_SNAPSHOT_MAX_CHARS);
        assert!(s.ends_with(" tail")); // keep what the user is typing now
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common panel`
Expected: FAIL — `InputChanged` variant and `input_snapshot` not found.

- [ ] **Step 3: Implement** — in `mur-common/src/panel.rs`:

Bump the version and add constants (replace the existing `PANEL_PROTO_VERSION` line):

```rust
pub const PANEL_PROTO_VERSION: u32 = 2;

/// Debounce for `InputChanged` snapshots (murmur side). ~200 ms is the
/// autocomplete sweet spot; >300 ms degrades UX (Algolia guidance).
pub const INPUT_DEBOUNCE_MS: u64 = 200;
/// Below this many trimmed chars the Hub falls back to cwd recommendations.
pub const MIN_QUERY_CHARS: usize = 2;
/// Snapshot size bound; the tail (what the user is typing now) is kept.
pub const INPUT_SNAPSHOT_MAX_CHARS: usize = 2000;
```

Add the variant to `PanelFrame` (after `Stream`, before `Bye`):

```rust
    /// Debounced snapshot of the message input line (never per-keystroke
    /// deltas). Local-socket only; must never be persisted or forwarded
    /// (spec §3.2). Build the payload with [`input_snapshot`].
    InputChanged {
        text: String,
    },
```

Add the sanitizer (above the `tests` module):

```rust
/// Build a privacy-safe `InputChanged` payload: redact secret-looking
/// tokens, then keep only the trailing `INPUT_SNAPSHOT_MAX_CHARS` chars
/// (the tail is what the user is currently typing).
pub fn input_snapshot(text: &str) -> String {
    let redacted: Vec<String> = text
        .split(' ')
        .map(|tok| {
            if looks_secret(tok) {
                "[redacted]".to_string()
            } else {
                tok.to_string()
            }
        })
        .collect();
    let joined = redacted.join(" ");
    let n = joined.chars().count();
    if n <= INPUT_SNAPSHOT_MAX_CHARS {
        joined
    } else {
        joined.chars().skip(n - INPUT_SNAPSHOT_MAX_CHARS).collect()
    }
}

/// Conservative secret heuristics: `sk-` API keys, AWS `AKIA` key ids,
/// and long (32+) unbroken hex/base64-ish runs.
fn looks_secret(tok: &str) -> bool {
    let is_b64ish = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '_' || c == '-')
    };
    if let Some(rest) = tok.strip_prefix("sk-")
        && rest.len() >= 16
        && is_b64ish(rest)
    {
        return true;
    }
    if let Some(rest) = tok.strip_prefix("AKIA")
        && rest.len() == 16
        && rest.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    tok.len() >= 32 && is_b64ish(tok) && tok.chars().any(|c| c.is_ascii_digit())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common panel`
Expected: PASS (including the pre-existing `frames_round_trip`, `unknown_frames_are_none`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-common/src/panel.rs
git commit -m "feat(panel): InputChanged frame, input_snapshot redaction, proto v2"
```

---

### Task 2: Retrieval — `recommend_for_input` tiered ranking (`mur-core`)

**Files:**
- Modify: `mur-core/src/recommend.rs`

**Interfaces:**
- Consumes: existing `recommend_for_cwd`, `cwd_query`, `Recommendation`, `score_and_rank_generic`, `load_skill_candidates`, `filter_by_scope`, `WorkflowYamlStore`.
- Produces: `pub fn recommend_for_input(cwd: &Path, input: &str, limit: usize) -> Vec<Recommendation>`; `pub(crate) fn rank_input(query: &str, mut recs: Vec<Recommendation>) -> Vec<Recommendation>` (pure, testable). Task 3 inserts the adaptive tier at the top of `recommend_for_input`.

- [ ] **Step 1: Write the failing tests** — append to `tests` in `mur-core/src/recommend.rs`:

```rust
    fn rec(name: &str, score: f32) -> Recommendation {
        Recommendation {
            name: name.into(),
            kind: "skill".into(),
            score,
            description: String::new(),
            command: format!("mur skill show {name}"),
        }
    }

    #[test]
    fn short_input_falls_back_to_cwd() {
        // 1 trimmed char < MIN_QUERY_CHARS → identical to recommend_for_cwd
        // (fail-soft: both empty in a test sandbox, and neither panics).
        let a = recommend_for_input(Path::new("/nonexistent/dir"), "x", 5);
        let b = recommend_for_cwd(Path::new("/nonexistent/dir"), 5);
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn prefix_matches_outrank_score() {
        let recs = vec![rec("zeta-high", 0.9), rec("book-search", 0.5)];
        let out = rank_input("book", recs);
        // prefix match beats a higher retrieval score
        assert_eq!(out[0].name, "book-search");
        assert_eq!(out[1].name, "zeta-high");
    }

    #[test]
    fn ties_break_by_name_ascending() {
        let recs = vec![rec("bbb", 0.5), rec("aaa", 0.5)];
        let out = rank_input("zzz", recs);
        assert_eq!(out[0].name, "aaa");
        assert_eq!(out[1].name, "bbb");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core recommend`
Expected: FAIL — `recommend_for_input` / `rank_input` not found.

- [ ] **Step 3: Implement** — append to `mur-core/src/recommend.rs`:

```rust
/// Rank tier for input-driven suggestions: name prefix/exact matches first
/// (VS Code Quick Open lesson — exact beats fuzzy), then retrieval score,
/// then stable name tie-break.
pub(crate) fn rank_input(query: &str, mut recs: Vec<Recommendation>) -> Vec<Recommendation> {
    let q = query.trim().to_ascii_lowercase();
    recs.sort_by(|a, b| {
        let ap = a.name.to_ascii_lowercase().starts_with(&q);
        let bp = b.name.to_ascii_lowercase().starts_with(&q);
        bp.cmp(&ap) // prefix matches first
            .then(b.score.total_cmp(&a.score))
            .then(a.name.cmp(&b.name))
    });
    recs
}

/// Input-driven recommendations (spec §3.3). Input text is the query; cwd
/// terms are appended as low-weight context. Below `MIN_QUERY_CHARS` this
/// is exactly `recommend_for_cwd`. Fail-soft like everything else here.
pub fn recommend_for_input(cwd: &Path, input: &str, limit: usize) -> Vec<Recommendation> {
    let trimmed = input.trim();
    if trimmed.chars().count() < mur_common::panel::MIN_QUERY_CHARS {
        return recommend_for_cwd(cwd, limit);
    }
    // Input words first so they dominate the word-overlap scoring; cwd
    // terms trail as context.
    let query = format!("{} {}", trimmed, cwd_query(cwd));
    let mut out = rank_input(trimmed, recommend_with_query(&query, limit * 2));
    out.truncate(limit);
    out
}
```

And refactor the body of `recommend_for_cwd` so both paths share one query
runner — extract everything after the `query` construction into:

```rust
/// Run the skill + workflow retrieval for an arbitrary query string.
/// (Shared by `recommend_for_cwd` and `recommend_for_input`.)
fn recommend_with_query(query: &str, limit: usize) -> Vec<Recommendation> {
    // ... body is the existing recommend_for_cwd code from `let mur_dir = ...`
    // down to `out.truncate(limit); out`, verbatim, with `query: &str` as the
    // parameter instead of the local built from cwd.
}
```

`recommend_for_cwd` becomes:

```rust
pub fn recommend_for_cwd(cwd: &Path, limit: usize) -> Vec<Recommendation> {
    let query = cwd_query(cwd);
    if query.trim().is_empty() {
        return Vec::new();
    }
    recommend_with_query(&query, limit)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core recommend`
Expected: PASS — all new tests plus the two pre-existing ones (`query_uses_trailing_path_components`, `recommend_is_fail_soft_on_empty_home`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/recommend.rs
git commit -m "feat(recommend): input-driven query with prefix-first ranking"
```

---

### Task 3: Adaptive query→picked history (`mur-core`)

**Files:**
- Modify: `mur-core/src/recommend.rs`

**Interfaces:**
- Consumes: `recommend_for_input` (Task 2), `Recommendation`.
- Produces: `pub fn record_pick(mur_home: &Path, query: &str, picked: &str)`; `pub(crate) fn adaptive_best(entries: &[AdaptiveEntry], query: &str, now_secs: u64) -> Option<String>`; storage at `<mur_home>/panel/adaptive.yaml`. `recommend_for_input` now promotes the adaptive hit to rank 0. Task 5's `panel_insert` calls `record_pick`.

- [ ] **Step 1: Write the failing tests** — append to `tests`:

```rust
    const DAY: u64 = 86_400;

    fn entry(q: &str, p: &str, count: f32, last: u64) -> AdaptiveEntry {
        AdaptiveEntry {
            query: q.into(),
            picked: p.into(),
            use_count: count,
            last_used: last,
        }
    }

    #[test]
    fn adaptive_prefix_match_wins() {
        let now = 100 * DAY;
        let es = vec![
            entry("run boo", "book-search", 3.0, now - DAY),
            entry("deploy", "deployer", 9.0, now - DAY),
        ];
        // typed query extends the stored one → match
        assert_eq!(adaptive_best(&es, "run book", now).as_deref(), Some("book-search"));
        // stored query extends the typed one → also match
        assert_eq!(adaptive_best(&es, "run b", now).as_deref(), Some("book-search"));
        assert_eq!(adaptive_best(&es, "xyz", now), None);
    }

    #[test]
    fn adaptive_decays_and_expires() {
        let now = 200 * DAY;
        // 90+ days unused → expired
        let es = vec![entry("run boo", "book-search", 9.0, now - 91 * DAY)];
        assert_eq!(adaptive_best(&es, "run boo", now), None);
        // Decay: fresher-but-smaller beats stale-but-bigger
        let es = vec![
            entry("run boo", "old-pick", 5.0, now - 60 * DAY), // 5 * 0.975^60 ≈ 1.1
            entry("run boo", "new-pick", 2.0, now - DAY),      // 2 * 0.975   ≈ 1.95
        ];
        assert_eq!(adaptive_best(&es, "run boo", now).as_deref(), Some("new-pick"));
    }

    #[test]
    fn record_pick_saturates_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..30 {
            record_pick(dir.path(), "Run Book", "book-search");
        }
        let es = load_adaptive(dir.path());
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].query, "run book"); // normalized
        assert!(es[0].use_count <= ADAPTIVE_USE_CAP);
        assert!(es[0].use_count > 9.0); // converges toward the cap
    }
```

(`tempfile` is already a dev-dependency of mur-core; if not, add `tempfile = "3"` to `[dev-dependencies]`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core recommend`
Expected: FAIL — `AdaptiveEntry`, `adaptive_best`, `record_pick`, `load_adaptive` not found.

- [ ] **Step 3: Implement** — append to `mur-core/src/recommend.rs`:

```rust
// ── Adaptive query→picked history (Firefox urlbar parameters) ─────────────

/// use_count = use_count * 0.9 + 1 on pick, saturating here.
pub(crate) const ADAPTIVE_USE_CAP: f32 = 10.0;
/// Effective score decays 0.975/day since last use.
const ADAPTIVE_DAILY_DECAY: f32 = 0.975;
/// Entries unused this long are dropped.
const ADAPTIVE_EXPIRE_DAYS: f32 = 90.0;
/// Normalized query length bound.
const ADAPTIVE_QUERY_MAX: usize = 64;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AdaptiveEntry {
    pub query: String,
    pub picked: String,
    pub use_count: f32,
    /// Unix seconds.
    pub last_used: u64,
}

fn adaptive_path(mur_home: &Path) -> std::path::PathBuf {
    mur_home.join("panel").join("adaptive.yaml")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn normalize_query(q: &str) -> String {
    q.trim()
        .to_lowercase()
        .chars()
        .take(ADAPTIVE_QUERY_MAX)
        .collect()
}

pub(crate) fn load_adaptive(mur_home: &Path) -> Vec<AdaptiveEntry> {
    std::fs::read_to_string(adaptive_path(mur_home))
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Best adaptive pick for `query`: prefix match either direction, expired
/// entries skipped, ranked by decayed use_count.
pub(crate) fn adaptive_best(
    entries: &[AdaptiveEntry],
    query: &str,
    now_secs: u64,
) -> Option<String> {
    let q = normalize_query(query);
    if q.is_empty() {
        return None;
    }
    entries
        .iter()
        .filter_map(|e| {
            let days = now_secs.saturating_sub(e.last_used) as f32 / 86_400.0;
            if days > ADAPTIVE_EXPIRE_DAYS {
                return None;
            }
            if !(q.starts_with(&e.query) || e.query.starts_with(&q)) {
                return None;
            }
            Some((e.use_count * ADAPTIVE_DAILY_DECAY.powf(days), &e.picked))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, picked)| picked.clone())
}

/// Record that the user picked `picked` after typing `query`. Fail-soft:
/// any I/O error is swallowed (suggestion quality, not correctness).
pub fn record_pick(mur_home: &Path, query: &str, picked: &str) {
    let q = normalize_query(query);
    if q.is_empty() || picked.is_empty() {
        return;
    }
    let mut entries = load_adaptive(mur_home);
    let now = now_secs();
    if let Some(e) = entries
        .iter_mut()
        .find(|e| e.query == q && e.picked == picked)
    {
        e.use_count = (e.use_count * 0.9 + 1.0).min(ADAPTIVE_USE_CAP);
        e.last_used = now;
    } else {
        entries.push(AdaptiveEntry {
            query: q,
            picked: picked.to_string(),
            use_count: 1.0,
            last_used: now,
        });
    }
    // Expire on write so the file can't grow unbounded.
    entries.retain(|e| now.saturating_sub(e.last_used) as f32 / 86_400.0 <= ADAPTIVE_EXPIRE_DAYS);
    let dir = adaptive_path(mur_home);
    if let Some(parent) = dir.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && let Ok(s) = serde_yaml::to_string(&entries)
    {
        // temp + rename for atomicity, same convention as store/yaml.rs
        let tmp = dir.with_extension("yaml.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &dir);
        }
    }
}
```

Then wire the tier into `recommend_for_input` — insert after the `let trimmed = ...; if ... return recommend_for_cwd(...)` guard and before the ranking:

```rust
    let mur_dir = mur_common::trust::mur_home();
    let adaptive = adaptive_best(&load_adaptive(&mur_dir), trimmed, now_secs());
```

and after `let mut out = rank_input(...)`:

```rust
    // Adaptive history outranks everything (Firefox: "infinite frecency").
    if let Some(name) = adaptive
        && let Some(idx) = out.iter().position(|r| r.name == name)
        && idx > 0
    {
        let hit = out.remove(idx);
        out.insert(0, hit);
    }
    out.truncate(limit);
    out
```

(the plain `out.truncate(limit); out` from Task 2 is replaced by this block).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core recommend`
Expected: PASS — all recommend tests green.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/recommend.rs mur-core/Cargo.toml
git commit -m "feat(recommend): adaptive query->picked history (Firefox urlbar params)"
```

---

### Task 4: murmur TUI — debounced `InputChanged` sender

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (App fields + pure decision fn)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (event-loop wiring)

**Interfaces:**
- Consumes: `mur_common::panel::{PanelFrame, input_snapshot, INPUT_DEBOUNCE_MS}`; `App.panel: Option<PanelHandle>`; `App.input_text()`; the `tokio::select!` in `event_loop`.
- Produces: `App` fields `panel_input_seen: String`, `panel_input_sent: String`, `panel_input_deadline: Option<std::time::Instant>`; `pub(crate) fn arm_input_debounce(app: &mut App, now: std::time::Instant)` and `pub(crate) fn take_due_input(app: &mut App, now: std::time::Instant) -> Option<String>` (both pure over App state, unit-tested).

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `mur-core/src/cmd/agent/cli/app.rs`:

```rust
    #[test]
    fn input_debounce_arms_and_fires_once() {
        use std::time::{Duration, Instant};
        let mut app = App::new_for_test(); // existing test constructor; if the
        // module uses a different helper (grep `fn new_for_test` / how sibling
        // tests build an App), use that one.
        let t0 = Instant::now();

        // No edit → nothing armed, nothing due.
        arm_input_debounce(&mut app, t0);
        assert!(take_due_input(&mut app, t0 + Duration::from_secs(1)).is_none());

        // Edit arms the deadline; before expiry nothing fires.
        app.set_input("run boo");
        arm_input_debounce(&mut app, t0);
        assert!(take_due_input(&mut app, t0).is_none());

        // Continued typing re-arms (debounce reset).
        app.set_input("run book");
        arm_input_debounce(&mut app, t0 + Duration::from_millis(100));

        // After the (re-armed) deadline the latest snapshot fires exactly once.
        let due = t0 + Duration::from_millis(100 + mur_common::panel::INPUT_DEBOUNCE_MS + 1);
        assert_eq!(take_due_input(&mut app, due).as_deref(), Some("run book"));
        assert!(take_due_input(&mut app, due).is_none()); // no repeat

        // Unchanged text never re-fires even after another arm pass.
        arm_input_debounce(&mut app, due);
        assert!(take_due_input(&mut app, due + Duration::from_secs(1)).is_none());

        // Clearing the input fires an empty snapshot (panel resets to cwd mode).
        app.clear_input();
        arm_input_debounce(&mut app, due);
        let later = due + Duration::from_millis(mur_common::panel::INPUT_DEBOUNCE_MS + 1);
        assert_eq!(take_due_input(&mut app, later).as_deref(), Some(""));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core input_debounce`
Expected: FAIL — fields/functions not defined.

- [ ] **Step 3: Implement** — in `mur-core/src/cmd/agent/cli/app.rs`:

Add fields to `App` (next to the existing `pub panel: Option<super::panel::PanelHandle>` around line 260):

```rust
    /// Input-driven suggestions (spec §3.5): last input text observed by the
    /// debounce, last snapshot actually sent, and the pending deadline.
    pub panel_input_seen: String,
    pub panel_input_sent: String,
    pub panel_input_deadline: Option<std::time::Instant>,
```

Initialize in the `App` constructor (where `input: new_input(),` is set):

```rust
            panel_input_seen: String::new(),
            panel_input_sent: String::new(),
            panel_input_deadline: None,
```

Add the two pure functions (module level, near the bottom before `tests`):

```rust
/// Arm/reset the InputChanged debounce when the input text changed since the
/// last observation. Called every event-loop iteration.
pub(crate) fn arm_input_debounce(app: &mut App, now: std::time::Instant) {
    let cur = app.input_text();
    if cur != app.panel_input_seen {
        app.panel_input_seen = cur;
        app.panel_input_deadline =
            Some(now + std::time::Duration::from_millis(mur_common::panel::INPUT_DEBOUNCE_MS));
    }
}

/// If the debounce deadline has passed and the text differs from the last
/// sent snapshot, consume the deadline and return the raw text to send.
pub(crate) fn take_due_input(app: &mut App, now: std::time::Instant) -> Option<String> {
    if app.panel_input_deadline.is_some_and(|d| now >= d) {
        app.panel_input_deadline = None;
        if app.panel_input_seen != app.panel_input_sent {
            app.panel_input_sent = app.panel_input_seen.clone();
            return Some(app.panel_input_sent.clone());
        }
    }
    None
}
```

In `mur-core/src/cmd/agent/cli/mod.rs`, wire the event loop (`event_loop`, ~line 368):

At the top of the `loop`, right after `app.sync_input_block();`:

```rust
        super::cli::app::arm_input_debounce(app, std::time::Instant::now());
        // ^ adjust the path to match how mod.rs already refers to app.rs items
        //   (same module tree — likely just `app_mod::` or a direct import).
```

Compute the sleep target just before `tokio::select!` (next to `blink_at`):

```rust
        let input_due = app
            .panel_input_deadline
            .map(TokioInstant::from_std)
            .unwrap_or_else(|| TokioInstant::from_std(StdInstant::now()));
        let input_armed = app.panel_input_deadline.is_some();
```

Add a select arm (after the `panel_rx` arm):

```rust
            _ = tokio::time::sleep_until(input_due), if input_armed => {
                if let Some(raw) = app::take_due_input(app, StdInstant::now())
                    && let Some(p) = &app.panel
                {
                    p.send(mur_common::panel::PanelFrame::InputChanged {
                        text: mur_common::panel::input_snapshot(&raw),
                    });
                }
            }
```

(`PanelHandle::send` is already fire-and-forget with a 64-frame cap and drops
frames when no Hub is connected — zero cost without a panel.)

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p mur-core input_debounce && cargo clippy -p mur-core -- -D warnings`
Expected: PASS / clean. (Remember `ORT_STRATEGY=download MUR_WEB_DIST=...`.)

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(murmur): debounced InputChanged frames to the panel bridge"
```

---

### Task 5: Hub backend — snapshot state, ping event, input-aware commands

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs`
- Modify: `mur-hub-gui/src-tauri/src/panel/data.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register one new command)

**Interfaces:**
- Consumes: `PanelFrame::InputChanged` (Task 1), `mur_core::recommend::{recommend_for_input, record_pick}` (Tasks 2–3), `PanelState`, `mur_home_path()`.
- Produces: `PanelState.inputs: Mutex<HashMap<u32, String>>`; webview event `panel-input-changed { pid }` (**no text — raw input never enters the webview**); Tauri command `panel_recommend_input(pid: u32, cwd: String) -> Vec<Recommendation>`; `panel_insert` gains `picked: Option<String>` and records the adaptive pair server-side.

- [ ] **Step 1: Extend `PanelState` and `on_frame`** — in `panel/mod.rs`:

```rust
#[derive(Default)]
pub struct PanelState {
    bridge: Mutex<Option<PanelBridge>>,
    sessions: Mutex<HashMap<u32, PanelSession>>,
    /// Latest InputChanged snapshot per session. Local-only: never persisted,
    /// never sent to the webview (it gets a pid-only ping), dropped on
    /// SessionDown. Spec §3.2.
    inputs: Mutex<HashMap<u32, String>>,
}
```

In `on_frame`, add a match arm (before `PanelFrame::Bye`):

```rust
        PanelFrame::InputChanged { text } => {
            app.state::<PanelState>()
                .inputs
                .lock()
                .unwrap()
                .insert(pid, text);
            let _ = app.emit("panel-input-changed", serde_json::json!({ "pid": pid }));
        }
```

In `spawn_bridge`'s `PanelEvent::SessionDown` arm, also drop the snapshot:

```rust
                PanelEvent::SessionDown { pid } => {
                    let st = app.state::<PanelState>();
                    st.sessions.lock().unwrap().remove(&pid);
                    st.inputs.lock().unwrap().remove(&pid);
                    emit_sessions(&app);
                }
```

- [ ] **Step 2: Input-aware command + adaptive recording** — in `panel/data.rs` add:

```rust
/// Recommendations driven by the live murmur input snapshot for `pid`
/// (falls back to cwd-only when there is no/short input). The raw snapshot
/// stays in Rust; the webview only ever sees ranked results.
#[tauri::command]
pub fn panel_recommend_input(
    pid: u32,
    cwd: String,
    state: tauri::State<crate::panel::PanelState>,
) -> Vec<mur_core::recommend::Recommendation> {
    let input = state.inputs_snapshot(pid).unwrap_or_default();
    mur_core::recommend::recommend_for_input(Path::new(&cwd), &input, 5)
}
```

In `panel/mod.rs`, give `PanelState` the accessor (fields stay private):

```rust
impl PanelState {
    pub(crate) fn inputs_snapshot(&self, pid: u32) -> Option<String> {
        self.inputs.lock().unwrap().get(&pid).cloned()
    }
}
```

Extend `panel_insert` in `panel/mod.rs` to record the adaptive pick:

```rust
#[tauri::command]
pub fn panel_insert(
    pid: u32,
    text: String,
    picked: Option<String>,
    state: State<PanelState>,
) -> Result<(), String> {
    // Record BEFORE inserting: the insert clears/replaces the input, and the
    // pairing must use the query that produced the suggestion.
    if let Some(name) = picked.filter(|n| !n.is_empty())
        && let Some(query) = state.inputs_snapshot(pid)
    {
        mur_core::recommend::record_pick(&crate::mur_home_path(), &query, &name);
    }
    let ok = state
        .bridge
        .lock()
        .unwrap()
        .as_ref()
        .map(|b| b.insert(pid, text))
        .unwrap_or(false);
    if ok { Ok(()) } else { Err("session gone".into()) }
}
```

(Existing UI callers pass no `picked` → `Option` defaults to `None`; Tauri
treats missing optional args as `null`. Verify with the existing HITL/proposal
buttons still compiling in Task 6.)

- [ ] **Step 3: Register the command** — in `lib.rs`, next to `panel::data::panel_recommend` (line ~591):

```rust
            panel::data::panel_recommend_input,
```

- [ ] **Step 4: Build the Hub crate**

```bash
# ui/dist must exist for tauri generate_context! (known gotcha):
test -f mur-hub-gui/ui/dist/index.html || (cd mur-hub-gui/ui && npm ci && npm run build)
cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings
```

Expected: clean. (Hub CI clippy is `--lib` on a fresh compile — don't trust a cached local pass.)

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/panel/mod.rs mur-hub-gui/src-tauri/src/panel/data.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): input-driven panel recommendations + adaptive pick recording"
```

---

### Task 6: Panel webview — live re-query with stale-response guard

**Files:**
- Modify: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx`

**Interfaces:**
- Consumes: event `panel-input-changed { pid }`, commands `panel_recommend_input(pid, cwd)`, `panel_insert(pid, text, picked?)`.
- Produces: Recommendations block updates live; suggestion clicks pass `picked`; other `insert()` callers (HITL, proposals) unchanged.

- [ ] **Step 1: Implement** — in `PanelWindow.tsx`:

Add a generation ref next to the existing state (after `const [recommendations, ...]`):

```tsx
  const recGen = useRef(0);
```

Replace the recommendations fetch inside `fetchTabData.current` (the
`panel_recommend` invoke at line ~202) with the input-aware command plus
stale-drop:

```tsx
      const gen = ++recGen.current;
      void invoke<Recommendation[]>("panel_recommend_input", {
        pid: sess.pid,
        cwd: sess.cwd,
      }).then((r) => {
        if (gen === recGen.current) setRecommendations(r);
      });
```

Add the listener in the mount `useEffect` (next to `unFocus`/`unPreview`):

```tsx
    const unInput = listen<{ pid: number }>("panel-input-changed", () => {
      // Re-rank for whichever session is active; fetchTabData reads current
      // state and the generation counter drops stale responses. Keep the
      // current list rendered until the new one lands (no flash-to-empty).
      fetchTabData.current();
    });
```

and in the cleanup: `unInput.then((f) => f());`

Update `insert` to carry the picked suggestion (default keeps old callers):

```tsx
  const insert = (text: string, picked?: string) => {
    if (pid !== null) void invoke("panel_insert", { pid, text, picked: picked ?? null });
  };
```

Update only the recommendation click (line ~356) to pass the name:

```tsx
                    onClick={() => insert(r.command, r.name)}
```

HITL (`~:394`) and proposal (`~:414`) buttons stay `insert(text)`.

- [ ] **Step 2: Typecheck/lint + rebuild dist**

```bash
cd mur-hub-gui/ui && npm run build
```

Expected: clean build (tsc + vite).

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/PanelWindow.tsx
git commit -m "feat(hub-ui): live input-driven recommendations with stale-response guard"
```

---

### Task 7: Full verification + docs

**Files:**
- Modify: `docs/architecture/runtime-overview.md` (murmur Panel section, ~line 172)

- [ ] **Step 1: Full test + lint pass**

```bash
export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
cargo test -p mur-common && cargo test -p mur-core
cargo clippy --workspace -- -D warnings
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings
cargo fmt --all --check
cargo fmt --check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```

Expected: all green.

- [ ] **Step 2: Live verify (manual, tmux + local Hub build)** — the PR #639
recipe: build murmur (`cargo build -p mur-core`), run the local Hub .app,
open `murmur` in a tmux pane, open the Panel, then:

1. Type `run book` slowly → Panel Recommendations updates within ~400 ms of
   pausing; caption shows input-driven mode; no flash-to-empty.
2. Clear the input → recommendations return to cwd-based list.
3. Click a suggestion → text lands in murmur's input line (NOT executed);
   `~/.mur/panel/adaptive.yaml` gains/updates the pair.
4. Retype the same prefix → the previously picked item is now rank 0.
5. Type `sk-aaaaaaaaaaaaaaaaaaaa` → `~/.mur` contains no such string anywhere
   (`grep -r sk-aaaa ~/.mur` empty) and the Hub trace (if enabled) shows
   `[redacted]`.

- [ ] **Step 3: Update docs** — in `docs/architecture/runtime-overview.md`
murmur Panel section, append one paragraph:

```markdown
P6 (input-driven suggestions): murmur debounces the message input (200 ms)
and pushes redacted `PanelFrame::InputChanged` snapshots over the panel
socket (proto v2). The Hub keeps the latest snapshot per pid in memory only
and pings the Panel webview (pid-only — raw text never enters the webview),
which re-queries `panel_recommend_input`: adaptive query→picked history
(Firefox urlbar decay parameters, `~/.mur/panel/adaptive.yaml`) > name-prefix
matches > the standard retrieval ranking, capped at 5. Clicking a suggestion
stays insert-only and records the adaptive pair server-side. Spec:
`docs/superpowers/specs/2026-07-06-murmur-panel-input-driven-suggestions-design.md`.
```

- [ ] **Step 4: Commit + PR**

```bash
git add docs/architecture/runtime-overview.md
git commit -m "docs(panel): input-driven suggestions (P6) runtime overview"
git push -u origin feat/panel-input-suggestions
gh pr create --title "feat(panel): input-driven dynamic suggestions (murmur → Hub, insert-only)" --body "Implements docs/superpowers/specs/2026-07-06-murmur-panel-input-driven-suggestions-design.md

- PanelFrame::InputChanged (proto v2), 200ms debounce, secret redaction, 2000-char tail cap
- recommend_for_input: adaptive history > prefix > retrieval, stable tie-break
- Hub keeps snapshots in Rust only; webview gets pid-only pings
- insert-only preserved; suggestion clicks record adaptive picks

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```
