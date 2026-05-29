# `Retrievable` Trait Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a `Retrievable` trait from the Pattern-coupled hybrid scorer in `mur-core/src/retrieve/scoring.rs` so `Pattern`, `Skill`, and (later) `Note` can share one scorer — **as a purely additive refactor with zero behavior change**: all 11 call sites and ~20 existing tests must pass unchanged.

**Architecture:** Introduce `Retrievable` in `scoring.rs` (lives where it is used). Generalize the inner scoring loop to `score_and_rank_inner<T: Retrievable>`. Generalize `LowerCache` and the timestamp-driven helpers (`recency_score`, `time_decay_score`). Replace `ScoredPattern` with a generic `Scored<T>` + type alias `pub type ScoredPattern = Scored<Pattern>;` so external callers (`interactive.rs`, `server/search.rs`) keep compiling. Pattern-specific boosts (`scope_mult`, `lang_mult`, `kind_score_boost`) move into `Pattern`'s `impl Retrievable` as the optional `adjust_score` trait method; other items get an identity default. The public Pattern-typed entry points (`score_and_rank`, `score_and_rank_hybrid`, `score_and_rank_*_with_*`) become thin wrappers that call the generic — no caller edits anywhere.

**Tech Stack:** Rust 2024 edition, cargo workspace, existing `mur-core` crate, existing `tracing`/`chrono`/`serde` deps. No new dependencies.

**Out of scope (sequenced to later plans):** Pattern removal, `Skill`/`Note` impls of `Retrievable`, `ContentMode::Note`, per-skill `events.jsonl` + reducer + `mur skill evolve`. This plan only de-risks the type migration.

---

## File map

- **Modify:** `mur-core/src/retrieve/scoring.rs` — add trait, generic inner, Pattern impl, type alias. All other content (boost helpers, tests) stays in place.
- **No other files change.** All call sites continue to use the existing Pattern-typed API.
- **No new files.** The trait lives next to its sole consumer.

**Convention check:** mur-core favors a few large modules over many small files (e.g. `scoring.rs` is 921 lines). Adding the trait + generic inner inline is consistent with the established pattern; do not split scoring.rs into submodules in this plan.

---

## Task 1: Define `Retrievable` trait with Pattern impl

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` (insert trait + impl after `ScopeContext`, before `ScoredPattern`)
- Test: `mur-core/src/retrieve/scoring.rs` (test module at line 501)

- [ ] **Step 1: Write the failing test**

Append to `mod tests` (find the `#[test]` block, add this test alongside existing tests):

```rust
#[test]
fn pattern_implements_retrievable_with_expected_accessors() {
    use super::Retrievable;
    let p = make_pattern("alpha", "alpha body");
    assert_eq!(p.name(), "alpha");
    assert_eq!(p.description(), p.description.as_str());
    assert_eq!(&*p.text(), &*p.content.as_text());
    assert_eq!(p.importance(), p.importance);
    assert_eq!(p.effectiveness(), p.evidence.effectiveness());
    assert_eq!(p.tier(), p.tier);
    assert_eq!(p.created_at(), p.created_at);
    assert!(p.is_active());
    assert_eq!(p.decay_half_life_days(), p.tier.decay_half_life_days() as f64);
}
```

(`make_pattern` is the existing test helper at line ~505 — re-use it.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::pattern_implements_retrievable_with_expected_accessors -- --nocapture`
Expected: FAIL with `unresolved import 'super::Retrievable'`.

- [ ] **Step 3: Add the trait and Pattern impl**

Insert in `scoring.rs` immediately after the `ScopeContext` struct (line ~17), before `ScoredPattern`:

```rust
use std::borrow::Cow;

/// A retrievable knowledge item. The hybrid scorer is generic over this trait so
/// `Pattern`, `Skill`, and (later) `Note` share one scoring pipeline.
///
/// Default `adjust_score` is the identity; `Pattern` overrides it to apply
/// scope/language/kind boosts so existing Pattern behavior is preserved exactly.
pub trait Retrievable {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn text(&self) -> Cow<'_, str>;
    fn tag_terms(&self) -> Vec<&str>;
    fn importance(&self) -> f64;
    fn effectiveness(&self) -> f64;
    fn tier(&self) -> Tier;
    fn created_at(&self) -> chrono::DateTime<chrono::Utc>;
    fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    fn decay_half_life_days(&self) -> f64;
    /// Filter predicate: items where this returns false are dropped before scoring.
    fn is_active(&self) -> bool;

    /// Hook for item-specific score adjustment. Default: identity.
    /// Pattern overrides to apply `scope_mult`, `kind_score_boost`, `lang_mult`.
    fn adjust_score(
        &self,
        weighted_sum: f64,
        _query_words: &[&str],
        _scope: Option<&ScopeContext>,
        _project_language: Option<&str>,
    ) -> f64 {
        weighted_sum
    }
}

impl Retrievable for Pattern {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn text(&self) -> Cow<'_, str> { self.content.as_text() }
    fn tag_terms(&self) -> Vec<&str> {
        self.tags
            .topics
            .iter()
            .chain(self.tags.languages.iter())
            .map(String::as_str)
            .collect()
    }
    fn importance(&self) -> f64 { self.importance }
    fn effectiveness(&self) -> f64 { self.evidence.effectiveness() }
    fn tier(&self) -> Tier { self.tier }
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> { self.created_at }
    fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.lifecycle.last_injected.or(self.evidence.last_validated)
    }
    fn decay_half_life_days(&self) -> f64 {
        self.lifecycle
            .decay_half_life
            .unwrap_or_else(|| self.tier.decay_half_life_days()) as f64
    }
    fn is_active(&self) -> bool {
        !self.lifecycle.muted
            && self.lifecycle.status == mur_common::pattern::LifecycleStatus::Active
    }
    // adjust_score implemented in Task 7 (currently inherits identity default)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core retrieve::scoring::tests::pattern_implements_retrievable_with_expected_accessors`
Expected: PASS.

- [ ] **Step 5: Run existing retrieve tests to confirm no regression**

Run: `cargo test -p mur-core retrieve::scoring`
Expected: All existing tests still PASS. The trait is additive — nothing uses it yet.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "refactor(retrieve): add Retrievable trait with Pattern impl (additive)

Foundation for sharing the hybrid scorer across Pattern/Skill/Note.
Pattern impl is complete except adjust_score (added in a later commit).
Zero behavior change — no consumer uses the trait yet."
```

---

## Task 2: Generalize `LowerCache` over `Retrievable`

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` lines 271-295 (`LowerCache` struct + `from_pattern`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn lower_cache_builds_from_retrievable_with_lowered_fields() {
    let p = make_pattern("AlphaBeta", "AlphaBeta Body Content");
    let cache = LowerCache::from_item(&p);
    assert_eq!(cache.name, "alphabeta");
    assert!(cache.description.chars().all(|c| !c.is_uppercase()));
    assert!(cache.content.chars().all(|c| !c.is_uppercase()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::lower_cache_builds_from_retrievable_with_lowered_fields`
Expected: FAIL with `no function 'from_item' found for struct 'LowerCache'`.

- [ ] **Step 3: Generalize `LowerCache::from_pattern` to `from_item`**

Replace the existing `impl LowerCache` block (lines 278-295):

```rust
impl LowerCache {
    fn from_item<T: Retrievable + ?Sized>(item: &T) -> Self {
        let tags_text: String = item
            .tag_terms()
            .iter()
            .map(|t| t.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            name: item.name().to_lowercase(),
            description: item.description().to_lowercase(),
            content: item.text().to_lowercase(),
            tags_text,
        }
    }
}
```

The old method name `from_pattern` is removed; the only caller (Step 4 below) updates with it.

- [ ] **Step 4: Update the single caller**

In `scoring.rs` line ~303 (`fn keyword_relevance`):

```rust
fn keyword_relevance(query_words: &[&str], pattern: &Pattern) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }
    let cache = LowerCache::from_item(pattern);
    keyword_relevance_cached(query_words, &cache)
}
```

(Only the `from_pattern` call changes to `from_item`. Signature unchanged.)

- [ ] **Step 5: Run all retrieve tests**

Run: `cargo test -p mur-core retrieve::scoring`
Expected: All tests PASS (new + existing).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "refactor(retrieve): generalize LowerCache over Retrievable"
```

---

## Task 3: Generalize timestamp-driven helpers over `Retrievable`

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` lines 341-364 (`recency_score`, `time_decay_score`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn recency_and_decay_use_retrievable_accessors() {
    use chrono::{Duration, Utc};
    let mut p = make_pattern("alpha", "alpha body");
    p.lifecycle.last_injected = Some(Utc::now() - Duration::days(1));
    // Generic helpers must take any Retrievable and produce identical numbers
    // to the legacy Pattern-typed call.
    let r_generic = recency_score_for(&p as &dyn Retrievable);
    let d_generic = time_decay_score_for(&p as &dyn Retrievable);
    assert!((r_generic - 0.93_f64).abs() < 0.02);
    assert!(d_generic > 0.5 && d_generic <= 1.0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::recency_and_decay_use_retrievable_accessors`
Expected: FAIL with `cannot find function 'recency_score_for' in this scope`.

- [ ] **Step 3: Add generic helpers; keep Pattern-typed ones as wrappers**

Replace the two existing functions (lines 341-364) with:

```rust
/// Recency score: exp(-days / 14). Generic over Retrievable.
fn recency_score_for<T: Retrievable + ?Sized>(item: &T) -> f64 {
    let last = item.last_activity().unwrap_or_else(|| item.created_at());
    let days = (Utc::now() - last).num_days().max(0) as f64;
    (-days / 14.0).exp()
}

/// Time decay: 0.5 + 0.5 * exp(-days / half_life). Generic over Retrievable.
fn time_decay_score_for<T: Retrievable + ?Sized>(item: &T) -> f64 {
    let half_life = item.decay_half_life_days();
    let last = item.last_activity().unwrap_or_else(|| item.created_at());
    let days = (Utc::now() - last).num_days().max(0) as f64;
    0.5 + 0.5 * (-days / half_life).exp()
}

// Pattern-typed shims preserved for the existing inner-scoring call sites;
// removed in Task 6 when score_and_rank_inner becomes generic.
fn recency_score(pattern: &Pattern) -> f64 { recency_score_for(pattern) }
fn time_decay_score(pattern: &Pattern) -> f64 { time_decay_score_for(pattern) }
```

- [ ] **Step 4: Run all retrieve tests**

Run: `cargo test -p mur-core retrieve::scoring`
Expected: All PASS. The shims keep call sites byte-identical.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "refactor(retrieve): generic recency_score_for / time_decay_score_for"
```

---

## Task 4: Introduce generic `Scored<T>` with `ScoredPattern` alias

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` lines 20-25 (`ScoredPattern` struct definition)

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn scored_pattern_is_alias_of_scored_pattern_generic() {
    fn _accepts_alias(_: ScoredPattern) {}
    fn _accepts_generic(_: Scored<Pattern>) {}
    let p = make_pattern("alpha", "alpha body");
    let s: Scored<Pattern> = Scored { item: p, score: 1.0, relevance: 1.0 };
    _accepts_alias(s);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::scored_pattern_is_alias_of_scored_pattern_generic`
Expected: FAIL with `cannot find type 'Scored' in this scope`.

- [ ] **Step 3: Replace `ScoredPattern` struct with generic + alias**

Replace lines 19-25 in `scoring.rs`:

```rust
/// A retrieved item with its computed relevance score.
#[derive(Debug, Clone)]
pub struct Scored<T> {
    pub item: T,
    pub score: f64,
    pub relevance: f64,
}

/// Backward-compatible alias for existing call sites.
/// New code should prefer `Scored<T>`.
pub type ScoredPattern = Scored<Pattern>;
```

- [ ] **Step 4: Update field access from `.pattern` to `.item`**

External call sites that access `.pattern` on a `ScoredPattern` must change to `.item`. Grep first:

```bash
grep -rn "\.pattern" mur-core/src/retrieve/scoring.rs \
  mur-core/src/interactive.rs \
  mur-core/src/server/search.rs \
  mur-core/src/cmd/context.rs \
  mur-core/src/context_api/mod.rs
```

In `scoring.rs` itself (~lines 230-263): replace every `pattern: p,` (struct construction) with `item: p,` and every `sp.pattern.` or `b.pattern.` (field read in sort/budget code) with `sp.item.` / `b.item.`. The compiler will pinpoint each one.

In external files, the same `.pattern` → `.item` rename applies wherever it accesses a `ScoredPattern`. Use the compiler to drive: change `scoring.rs` first, then `cargo check` and walk the errors.

- [ ] **Step 5: Run the workspace build and full retrieve tests**

```bash
cargo check --workspace
cargo test -p mur-core retrieve::scoring
```

Expected: build clean, all tests PASS.

- [ ] **Step 6: Run the full mur-core test suite to catch external regressions**

Run: `cargo test -p mur-core`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "refactor(retrieve): generic Scored<T>; ScoredPattern is now an alias

External call sites use .item instead of .pattern; behavior unchanged."
```

---

## Task 5: Add `adjust_score` to `impl Retrievable for Pattern` (preserve all boosts)

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` — extend the Pattern `Retrievable` impl from Task 1 with `adjust_score`.

- [ ] **Step 1: Write a failing test asserting the current boosted output for a known input**

The existing kind-boost tests (e.g. `test_kind_boost_preference_with_matching_scope` at line ~757) already pin numerical behavior. Add one explicit equivalence test:

```rust
#[test]
fn pattern_adjust_score_preserves_scope_kind_lang_combination() {
    use super::Retrievable;
    let mut p = make_pattern("rust-error", "rust error body");
    p.applies.languages = vec!["rust".into()];
    // Replicate inline computation matching legacy score_and_rank_inner output
    let query_words = ["rust", "error"];
    let scope = ScopeContext { user: None, platform: None, task: None };
    let weighted_sum = 0.42;
    // After Task 5 the Pattern impl applies scope_mult (1.0, applies nonempty),
    // kind_boost (0.0 for Technical default), lang_mult (1.2, matching language).
    let adjusted = p.adjust_score(weighted_sum, &query_words, Some(&scope), Some("rust"));
    assert!((adjusted - (0.42 * 1.0 + 0.0) * 1.2).abs() < 1e-9);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core retrieve::scoring::tests::pattern_adjust_score_preserves_scope_kind_lang_combination`
Expected: FAIL — the default `adjust_score` returns the raw sum (no boosts applied).

- [ ] **Step 3: Implement `adjust_score` on `impl Retrievable for Pattern`**

Append to the `impl Retrievable for Pattern` block (added in Task 1):

```rust
fn adjust_score(
    &self,
    weighted_sum: f64,
    query_words: &[&str],
    scope: Option<&ScopeContext>,
    project_language: Option<&str>,
) -> f64 {
    let scope_mult = if self.applies.projects.is_empty()
        && self.applies.languages.is_empty()
        && self.applies.tools.is_empty()
    {
        0.7
    } else {
        1.0
    };
    let lang_mult = if let Some(proj_lang) = project_language {
        if !self.applies.languages.is_empty() {
            let proj_lang_lower = proj_lang.to_lowercase();
            let matches = self
                .applies
                .languages
                .iter()
                .any(|l| l.to_lowercase() == proj_lang_lower);
            if matches { 1.2 } else { 0.05 }
        } else {
            1.0
        }
    } else {
        1.0
    };
    let kind_boost = kind_score_boost(self, query_words, scope);
    (weighted_sum * scope_mult + kind_boost) * lang_mult
}
```

`kind_score_boost`, `scope_matches_origin`, and `is_task_query` (the helpers it depends on) stay where they are — they're still called from inside the Pattern impl.

- [ ] **Step 4: Run all retrieve tests**

Run: `cargo test -p mur-core retrieve::scoring`
Expected: PASS — including the new test and all existing kind-boost tests. (Note: `score_and_rank_inner` still inlines the same logic; it is replaced in Task 6.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "refactor(retrieve): Pattern impl Retrievable::adjust_score (boosts preserved)"
```

---

## Task 6: Genericize `score_and_rank_inner` over `T: Retrievable`

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` lines 162-267 (`score_and_rank_inner` body) and the Pattern-typed entry functions (lines 46-160).

- [ ] **Step 1: Note the test surface**

No new test is required for this task — the existing ~20 tests in `mod tests` exercise every code path. They are the regression net. If they all pass after this refactor, behavior is preserved by construction.

- [ ] **Step 2: Genericize `score_and_rank_inner`**

Replace the existing `score_and_rank_inner` (lines 162-267) with:

```rust
fn score_and_rank_inner<T, F>(
    query_words: &[&str],
    candidates: Vec<T>,
    scope: Option<&ScopeContext>,
    project_language: Option<&str>,
    config: Option<&RetrievalConfig>,
    relevance_fn: F,
) -> Vec<Scored<T>>
where
    T: Retrievable,
    F: Fn(&[&str], &T) -> f64,
{
    let score_floor = config.map_or(SCORE_FLOOR, |c| c.min_score);
    let max_patterns = config.map_or(MAX_PATTERNS, |c| c.max_patterns);
    let max_tokens = config.map_or(MAX_TOKENS, |c| c.max_tokens);

    let mut scored: Vec<Scored<T>> = candidates
        .into_iter()
        .filter(Retrievable::is_active)
        .map(|item| {
            let relevance = relevance_fn(query_words, &item);
            let recency = recency_score_for(&item);
            let effectiveness = item.effectiveness();
            let importance = item.importance();
            let time_decay = time_decay_score_for(&item);
            let content_len = item.text().len();
            let length_norm = length_norm_score_from_len(content_len);

            let weighted_sum = relevance * W_RELEVANCE
                + recency * W_RECENCY
                + effectiveness * W_EFFECTIVENESS
                + importance * W_IMPORTANCE
                + time_decay * W_TIME_DECAY
                + length_norm * W_LENGTH_NORM;

            let score = item.adjust_score(weighted_sum, query_words, scope, project_language);

            Scored { item, score, relevance }
        })
        .filter(|sp| sp.score >= score_floor)
        .collect();

    // Sort by score descending, with tier priority as tiebreaker.
    scored.sort_by(|a, b| {
        let score_diff = (a.score - b.score).abs();
        if score_diff < 0.05 {
            tier_priority(&b.item.tier()).cmp(&tier_priority(&a.item.tier()))
        } else {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    // Budget: max items and max tokens.
    let mut result = Vec::new();
    let mut token_count = 0;
    for sp in scored {
        if result.len() >= max_patterns {
            break;
        }
        let est_tokens = sp.item.text().len() / 4;
        if token_count + est_tokens > max_tokens && !result.is_empty() {
            break;
        }
        token_count += est_tokens;
        result.push(sp);
    }
    result
}
```

Note: the in-loop `scope_mult` / `lang_mult` / `kind_boost` computation has moved into `Pattern::adjust_score` (Task 5). The body is shorter now.

- [ ] **Step 3: Update `keyword_relevance` to be generic**

Replace the existing `keyword_relevance` (lines 297-305):

```rust
fn keyword_relevance<T: Retrievable + ?Sized>(query_words: &[&str], item: &T) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }
    let cache = LowerCache::from_item(item);
    keyword_relevance_cached(query_words, &cache)
}
```

`keyword_relevance_cached` is already generic over a `LowerCache`; no change.

- [ ] **Step 4: Public Pattern-typed entry functions keep their signatures**

Functions `score_and_rank`, `score_and_rank_with_config`, `score_and_rank_with_scope`, `score_and_rank_with_scope_and_config`, `score_and_rank_hybrid`, `score_and_rank_hybrid_with_config`, `score_and_rank_hybrid_with_scope`, `score_and_rank_hybrid_with_scope_and_config` (lines 46-160) **keep their existing signatures and bodies**. Their internal call `score_and_rank_inner(...)` now resolves to the generic version with `T = Pattern` via type inference — no edits.

For the hybrid variants, the closure `|words, p| { … vector_scores.get(&p.name) … }` already uses `p.name` — which Rust will resolve to the `Pattern.name` field. After Task 1, `Retrievable::name(&p)` is also in scope; field access still wins. No change needed, but if the compiler reports an ambiguity, write `&p.name` explicitly (which is what's there already).

- [ ] **Step 5: Build and run the full retrieve test suite**

```bash
cargo check -p mur-core
cargo test -p mur-core retrieve::scoring
```

Expected: clean build, all ~20 existing tests + the 4 new ones from Tasks 1-5 PASS. **This is the keystone regression gate.**

- [ ] **Step 6: Remove the now-dead Pattern-typed shims**

In `scoring.rs`, remove the two shim functions added in Task 3:

```rust
// DELETE these:
fn recency_score(pattern: &Pattern) -> f64 { recency_score_for(pattern) }
fn time_decay_score(pattern: &Pattern) -> f64 { time_decay_score_for(pattern) }
```

Nothing inside `scoring.rs` calls them anymore; the generic `*_for` functions replaced them in `score_and_rank_inner`.

- [ ] **Step 7: Run the workspace build to catch any external use of the dropped names**

Run: `cargo check --workspace`
Expected: clean. (The two shim names were private to `scoring.rs`, so external code cannot reference them.)

- [ ] **Step 8: Run the full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. Watch for any failure outside `retrieve::scoring` — that would mean an external `.pattern` field access was missed in Task 4.

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "refactor(retrieve): genericize score_and_rank_inner over T: Retrievable

Pattern-specific boosts moved into Pattern::adjust_score; the inner loop is
now item-agnostic. Public Pattern-typed entry functions unchanged. All
existing retrieve tests pass — behavior preserved by construction."
```

---

## Task 7: Add a synthetic `Retrievable` impl to lock down the generic path

**Files:**
- Modify: `mur-core/src/retrieve/scoring.rs` `mod tests` — add a `FakeItem` test type that proves the generic path works without any Pattern-specific machinery.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn generic_scorer_works_for_non_pattern_retrievable() {
    use super::Retrievable;
    use chrono::{Duration, Utc};
    use std::borrow::Cow;

    struct FakeItem {
        name: String,
        description: String,
        body: String,
        importance: f64,
    }
    impl Retrievable for FakeItem {
        fn name(&self) -> &str { &self.name }
        fn description(&self) -> &str { &self.description }
        fn text(&self) -> Cow<'_, str> { Cow::Borrowed(&self.body) }
        fn tag_terms(&self) -> Vec<&str> { vec![] }
        fn importance(&self) -> f64 { self.importance }
        fn effectiveness(&self) -> f64 { 1.0 }
        fn tier(&self) -> Tier { Tier::Project }
        fn created_at(&self) -> chrono::DateTime<chrono::Utc> { Utc::now() - Duration::days(1) }
        fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> { Some(Utc::now() - Duration::days(1)) }
        fn decay_half_life_days(&self) -> f64 { 90.0 }
        fn is_active(&self) -> bool { true }
    }

    let items = vec![FakeItem {
        name: "fly-deploy".into(),
        description: "Deploy to Fly.io".into(),
        body: "Run fly deploy in the project root.".into(),
        importance: 0.8,
    }];

    // Generic inner call with no scope, no project lang, default config.
    let scored = score_and_rank_inner(
        &["fly", "deploy"],
        items,
        None,
        None,
        None,
        |words, item: &FakeItem| keyword_relevance(words, item),
    );

    assert!(!scored.is_empty(), "fake item should be retrievable through the generic path");
    assert_eq!(scored[0].item.name, "fly-deploy");
    assert!(scored[0].score > 0.0);
}
```

- [ ] **Step 2: Run test to verify it passes immediately**

Run: `cargo test -p mur-core retrieve::scoring::tests::generic_scorer_works_for_non_pattern_retrievable`
Expected: PASS. (This test is the proof that the generic path is callable from outside the Pattern world; failure here means an accessor signature is too tight or a private item leaked.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/retrieve/scoring.rs
git commit -m "test(retrieve): lock down generic path with a non-Pattern Retrievable"
```

---

## Task 8: Verification gate — full workspace and lints

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: every test passes. Pay attention to test counts in `mur-core retrieve::scoring` — it should be the original count plus the 6 added in this plan (Tasks 1, 2, 3, 4, 5, 7 add one each).

- [ ] **Step 2: Run clippy with `-D warnings`**

Run: `cargo clippy --workspace -- -D warnings`
Expected: clean. The `Retrievable` trait may trigger `clippy::needless_pass_by_value` or similar — fix to clean output before merging.

- [ ] **Step 3: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: clean. If not, run `cargo fmt` and amend the previous commit:

```bash
cargo fmt
git add -u
git commit --amend --no-edit
```

- [ ] **Step 4: Confirm no caller edits leaked into external files**

Run: `git diff --stat origin/main..HEAD`
Expected: the only modified file is `mur-core/src/retrieve/scoring.rs`. If anything else appears, that file's change must be reviewed — it likely fell out of Task 4's `.pattern` → `.item` rename and is fine; just confirm it is exactly that and nothing else.

- [ ] **Step 5: Final commit (if any cleanup needed)**

If Steps 2-3 required fixes, the amend above handles it. Otherwise no extra commit needed.

---

## Done state

After this plan:

- `Retrievable` trait exists in `mur-core/src/retrieve/scoring.rs` with `Pattern` as its first impl.
- `score_and_rank_inner<T: Retrievable>` operates on any retrievable item.
- `Scored<T>` is the generic result type; `ScoredPattern = Scored<Pattern>` keeps every external caller compiling.
- All ~20 existing retrieve tests + 6 new tests pass. **Behavior is byte-identical for the Pattern path.**
- Adding `impl Retrievable for Skill` (later plan) is now a 12-line block; `impl Retrievable for Note` likewise. The largest under-estimated item in both specs is resolved.

**What is NOT done (sequenced to later plans):**
- `Skill` / `Note` impls of `Retrievable` — needs the `Note` variant work first.
- Pattern *removal* — separate plan once Skills are the live retrieval corpus.
- `ContentMode::Note` extension — separate foundation slice.
- Per-skill `events.jsonl` reducer + `mur skill evolve` sweep — separate plan.
