# MuR Skill Ecosystem — M3b.2 (Pattern → Skill Promotion) Implementation Plan

> **For agentic workers:** Use `superpowers:subagent-driven-development` or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** `mur skill from-pattern <pattern-name>` reads a Stable or Canonical pattern from `~/.mur/patterns/`, converts it to a `skill.yaml`, and installs it. Optional `--polish` flag calls an LLM to improve the abstract and suggest procedure steps. Zero dependencies on M3a/M3b.

---

## Codebase Reality Check

Verified against `main`:

| Assumption | Reality |
|---|---|
| Pattern store API | `YamlStore::get(name) -> Result<Pattern>` at `mur-core/src/store/yaml.rs:70`. `list_names()` + `list_all()` also available. Default store path: `~/.mur/patterns/`. |
| Pattern data model | `Pattern` derefs to `KnowledgeBase` (via `#[serde(flatten)]`). Fields: `name`, `description`, `content.technical`, `content.principle`, `tags`, `tier`, `importance`, `confidence`, `lifecycle.maturity`. |
| Maturity enum | `mur_common::knowledge::Maturity { Draft, Emerging, Stable, Canonical }`. Only Stable + Canonical qualify for promotion. |
| Skill CLI surface | `SkillAction` enum in `mur-core/src/cli/skill.rs`. Dispatch in `mur-core/src/dispatch.rs` lines 267-290. New arm: `crate::cli::SkillAction::FromPattern { name, polish }`. |
| Skill install path | `cmd_skill_install` already handles scan + trust + write. `from-pattern` reuses `scan_skill` + `SkillTrustStore::insert` directly; no need to go through the resolver. |
| LLM client (optional polish) | `factory::build_for_stage(cfg, stage) -> Arc<dyn ChatBackend>` (`mur-core/src/conversations/backend/factory.rs:34`). `ChatBackend` (not `LlmClient`) is the in-crate convention — call `generate(ChatRequest<'_>) -> ChatResponse` directly. `LlmClient` lives in `mur-common` but `ChatBackend` does NOT impl it (different shape: structured request vs string prompt, plus required `embed()`). Do not write an adapter. |
| `Tags` shape | `mur_common::pattern::Tags { languages: Vec<String>, topics: Vec<String>, extra: HashMap<String, Vec<String>> }` — **not** a newtype. Tag projection has to flatten all three. |
| `Pattern` import path | `mur_common::pattern::Pattern` (no top-level re-export in `mur-common/src/lib.rs`). |
| Dispatch async context | `dispatch::run` is already `async fn` (`dispatch.rs:20`), and the `Commands::Skill` arm runs inside it. Making `cmd_from_pattern` async and awaiting it from the arm is a one-line change — no upward refactor needed. |

---

## File Structure

**Create:**
- `mur-core/src/cmd/skill_from_pattern.rs` — `cmd_from_pattern` pure fn + `cmd_from_pattern_cli` shim

**Modify:**
- `mur-core/src/cli/skill.rs` — add `FromPattern { name, polish }` variant
- `mur-core/src/dispatch.rs` — add dispatch arm
- `mur-core/src/cmd/mod.rs` — register `pub mod skill_from_pattern;`

---

## Conversion Mapping

```
Pattern                          →  SkillManifest
──────────────────────────────────────────────────
name (kebab-case already)        →  name
description                      →  description
publisher = "agent:from-pattern" →  publisher
category = "workflow"            →  category
content.principle                →  content.abstract (Layer 2)
content.technical                →  content.context   (Layer 3 body)
tags                             →  tags
version = "0.1.0"                →  version
triggers = []                    →  triggers (empty; --polish adds command trigger)
requires = []                    →  requires (empty)
```

Patterns with `maturity < Stable` are rejected with a clear error message listing the current maturity.

---

### Task 1 — Core conversion function

**Files:** `mur-core/src/cmd/skill_from_pattern.rs`, `mur-core/src/cmd/mod.rs`.

- [ ] **1.1** Pure function — no I/O beyond pattern store read:

```rust
//! `mur skill from-pattern <name>` — promote a Stable/Canonical pattern to a skill.

use anyhow::{Context, Result, bail};
use mur_common::knowledge::Maturity;
use mur_common::pattern::Pattern;
use mur_common::skill::{Category, Content, HostId, SkillManifest, validate};

/// Layer 2 abstract budget. Pre-polish, the `principle` field is truncated to
/// this on a UTF-8 char boundary. With `--polish`, the LLM gets the full
/// principle and is asked to compress within the same budget.
const ABSTRACT_CHAR_BUDGET: usize = 250;

pub fn pattern_to_skill(pattern: &Pattern, polish: bool) -> Result<SkillManifest> {
    // Gate: only Stable or Canonical patterns are promoted.
    match pattern.base.maturity {
        Maturity::Stable | Maturity::Canonical => {}
        other => bail!(
            "pattern '{}' is {other:?} — only Stable or Canonical patterns can be promoted.\n\
             Use `mur pattern show {}` to see its current state.",
            pattern.name,
            pattern.name,
        ),
    }

    let abstract_text = if polish {
        // Polish path: leave principle full-length; LLM will rewrite + budget.
        pattern.base.content.principle.clone()
    } else {
        // No-polish: char-safe truncation. `&str[..n]` would panic mid-codepoint
        // on Chinese/emoji content. ABSTRACT_CHAR_BUDGET counts chars, not bytes.
        let p = &pattern.base.content.principle;
        let char_count = p.chars().count();
        if char_count > ABSTRACT_CHAR_BUDGET {
            let mut s: String = p.chars().take(ABSTRACT_CHAR_BUDGET).collect();
            s.push('…');
            s
        } else {
            p.clone()
        }
    };

    // Flatten Tags { languages, topics, extra } → Vec<String>.
    // De-dup preserves first occurrence so the manifest is stable across runs.
    let mut tags: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in pattern.base.tags.languages.iter()
        .chain(pattern.base.tags.topics.iter())
        .chain(pattern.base.tags.extra.values().flatten())
    {
        if seen.insert(t.clone()) {
            tags.push(t.clone());
        }
    }

    let manifest = SkillManifest {
        name: pattern.name.clone(),
        version: "0.1.0".into(),
        publisher: "agent:from-pattern".into(),
        description: pattern.base.description.clone(),
        category: Category::Workflow,
        hosts: vec![HostId::MurAgent],
        content: Content {
            r#abstract: abstract_text,
            context: Some(pattern.base.content.technical.clone()),
            procedure: None,
            command: None,
        },
        requires: vec![],
        tags,
        triggers: vec![],
        priority: Default::default(),
    };

    // Fail fast if the pattern name / charset / length violates skill schema.
    // Cheaper to surface here than at `read_from_dir` after a successful write.
    validate(&manifest).context("derived skill manifest failed schema validation")?;
    Ok(manifest)
}
```

- [ ] **1.2** Tests in the same file:
  1. Stable pattern → produces a valid `SkillManifest` (run `validate()`).
  2. Draft pattern → `bail!` with message containing "Stable or Canonical".
  3. Emerging pattern → same rejection.
  4. Canonical pattern → accepted.
  5. Long principle text (500 chars) without `--polish` → truncated to ≤251 chars (budget + ellipsis), suffix is `'…'`.
  6. **UTF-8 boundary**: principle containing 300 Chinese chars (e.g. "工" repeated) — must not panic, must produce a valid `String`.
  7. With `--polish` flag: principle text preserved at full length.
  8. **Tag flattening**: pattern with `languages: [rust]`, `topics: [cli, refactor]`, `extra: { project: [mur] }` → manifest tags contain all four, de-duped, no panic on empty.
  9. **Invalid name surfaces as error**: pattern with a name that violates skill schema (e.g. trailing whitespace, `..`) → `pattern_to_skill` returns an error from `validate`, not a malformed manifest.

- [ ] **1.3** Commit:
  ```bash
  cargo test -p mur-core skill_from_pattern
  git add mur-core/src/cmd/skill_from_pattern.rs mur-core/src/cmd/mod.rs
  git commit -m "feat(skill): pattern_to_skill conversion with maturity gate"
  ```

---

### Task 2 — CLI integration (no LLM)

**Files:** `mur-core/src/cmd/skill_from_pattern.rs` (add CLI shim), `mur-core/src/cli/skill.rs`, `mur-core/src/dispatch.rs`.

- [ ] **2.1** Add `FromPattern` variant to `SkillAction` enum:

```rust
// In mur-core/src/cli/skill.rs
FromPattern {
    /// Pattern name in ~/.mur/patterns/
    name: String,
    /// Optional LLM polish pass
    #[clap(long)]
    polish: bool,
},
```

- [ ] **2.2** Implementation — `cmd_from_pattern_with_home` does the real work against an explicit `mur_home`; `cmd_from_pattern` is the production shim that resolves `~/.mur` and drives an async runtime if polish is requested. Splitting up-front means the E2E tests in T4 use the same code path as production — no test-only branch.

```rust
use crate::cmd::agent::resolve_mur_home;
use crate::store::yaml::YamlStore;
use mur_common::skill::{
    TrustLevel, content_sha256, global_skill_dir, scan::scan_skill, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
use std::path::Path;

/// Test- and lib-friendly entry point. All I/O is scoped under `mur_home`.
///
/// Note: when `polish=true` this function will make an LLM call (Task 3).
/// Kept `async` so the CLI shim can `.await` it directly without spinning a
/// nested runtime — `dispatch::run` is already async.
pub async fn cmd_from_pattern_with_home(
    mur_home: &Path,
    name: &str,
    polish: bool,
) -> Result<()> {
    let patterns_dir = mur_home.join("patterns");
    let store = YamlStore::new(patterns_dir).context("open pattern store")?;
    let pattern = store
        .get(name)
        .with_context(|| format!("pattern '{name}' not found in {}/patterns/", mur_home.display()))?;

    let mut manifest = pattern_to_skill(&pattern, polish)?;

    if polish {
        // Task 3 fills this in. Until then, surface clearly that --polish is a no-op.
        eprintln!("note: --polish is not yet implemented — proceeding without polish");
    }

    // Scan the (possibly-polished) manifest. Pattern-promoted skills always
    // land at Sandboxed regardless of findings, but findings still get
    // surfaced so the user can decide whether to `mur skill trust` them up.
    let report = scan_skill(&manifest).context("scan derived skill manifest")?;
    if report.has_blocking_findings() {
        eprintln!(
            "warning: derived skill has {} blocking content finding(s) — staying Sandboxed.\n\
             Run `mur skill audit {}` after install for details.",
            report.findings.len(),
            manifest.name,
        );
    }

    let dir = global_skill_dir(mur_home, &manifest.name);
    write_to_dir(&dir, &manifest).context("write skill manifest")?;

    let hash = content_sha256(&manifest).context("hash skill manifest")?;
    let mut trust = SkillTrustStore::load(mur_home)
        .map_err(|e| anyhow::anyhow!("load trust store: {e}"))?;
    trust.insert(
        hash,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: TrustLevel::Sandboxed, // pattern-promoted always Sandboxed
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
        },
    );
    trust
        .save(mur_home)
        .map_err(|e| anyhow::anyhow!("save trust store: {e}"))?;

    println!(
        "promoted: {} v{} (Sandboxed, from {:?})",
        manifest.name, manifest.version, pattern.base.maturity
    );
    if !polish {
        println!("hint: re-run with --polish for LLM-assisted abstract + procedure generation");
    }
    Ok(())
}

/// Production entry point. Thin shim — resolves `~/.mur` and delegates.
pub async fn cmd_from_pattern(name: &str, polish: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    cmd_from_pattern_with_home(&home, name, polish).await
}
```

- [ ] **2.3** Wire into dispatch.rs. `Commands::Skill` runs inside `pub async fn run` (`dispatch.rs:20`), so `.await` works directly:

```rust
crate::cli::SkillAction::FromPattern { name, polish } => {
    cmd::skill_from_pattern::cmd_from_pattern(&name, polish).await?
}
```

- [ ] **2.4** Integration test (unit-level, uses tempdir with a real `YamlStore`):
  1. Save a Stable pattern via `YamlStore::new(home.path().join("patterns"))` → `cmd_from_pattern_with_home(home.path(), name, false).await` → verify `home/skills/<name>/skill.yaml` exists and round-trips through `read_from_dir` + `validate`.
  2. Save a Draft pattern → call → assert error message contains "Stable or Canonical".
  3. Trust entry in `home/trust/skills.json` has `level == TrustLevel::Sandboxed` and `publisher == "agent:from-pattern"`.

- [ ] **2.5** Commit:
  ```bash
  cargo test -p mur-core skill_from_pattern
  git add mur-core/src/cmd/skill_from_pattern.rs mur-core/src/cli/skill.rs mur-core/src/dispatch.rs
  git commit -m "feat(skill): mur skill from-pattern CLI (maturity-gated)"
  ```

---

### Task 3 — Optional LLM polish

**Files:** `mur-core/src/cmd/skill_from_pattern.rs`.

> **Design choice — no `LlmClient` adapter.** `mur_common::llm::LlmClient`
> and `mur-core`'s `ChatBackend` have incompatible shapes (string prompt vs
> structured `ChatRequest`; `LlmClient` also requires `embed()`). Rather than
> writing a brittle adapter, the polish path calls `ChatBackend` directly —
> matching every other LLM call site in `mur-core/src/conversations/` and
> `cmd/skill_generate.rs`. The `LlmClient` trait stays the `mur-common`
> embedding/cross-crate contract only.

- [ ] **3.1** Add a `polish_via_llm` function. Takes `&dyn ChatBackend` so callers can pass the `Arc<dyn ChatBackend>` from `factory::build_for_stage` by deref:

```rust
use crate::conversations::backend::{ChatBackend, ChatRequest};
use mur_common::skill::{Procedure, ProcedureStep, Trigger, TriggerKind};

const POLISH_SYSTEM: &str = r#"
You are a skill editor. Given a pattern extracted from agent behavior,
produce a polished skill.yaml abstract and suggest procedure steps.

The pattern's `technical` content describes what the agent actually did.
The `principle` describes the higher-level rule.

Output JSON only — no markdown fences, no prose:
{
  "abstract": "one-line polished abstract (≤200 chars)",
  "procedure_steps": [
    {"description": "step 1", "tool": "optional.tool.name"}
  ],
  "triggers": [{"type": "command", "pattern": "/suggested-trigger"}]
}

Rules:
- The abstract must capture the pattern's principle at a higher level.
- Procedure steps are derived from the technical content.
- If the technical content is too vague, suggest at most 2 steps.
"#;

const POLISH_MAX_TOKENS: u32 = 1024;
const POLISH_MODEL: &str = "claude-sonnet-4-6"; // budget pick; user can override via config

pub async fn polish_via_llm(
    manifest: &mut SkillManifest,
    pattern: &Pattern,
    backend: &dyn ChatBackend,
) -> Result<()> {
    let user = format!(
        "Pattern name: {}\nDescription: {}\nPrinciple: {}\nTechnical: {}",
        pattern.name,
        pattern.base.description,
        pattern.base.content.principle,
        pattern.base.content.technical,
    );

    let req = ChatRequest {
        model: POLISH_MODEL,
        system: Some(POLISH_SYSTEM),
        user: &user,
        max_tokens: POLISH_MAX_TOKENS,
        temperature: Some(0.2),
        stop: vec![],
        cache_system: true,
        cache_user_prefix: None,
    };
    let resp = backend.generate(req).await.context("polish LLM call failed")?;

    // Defensive: some backends still wrap JSON in fences even when told not to.
    let raw = resp.text.trim();
    let raw = raw.trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(raw)
        .with_context(|| format!("LLM did not return valid JSON; got: {raw:.200}"))?;

    if let Some(a) = v.get("abstract").and_then(|x| x.as_str()) {
        manifest.content.r#abstract = a.to_string();
    }
    if let Some(steps) = v.get("procedure_steps").and_then(|x| x.as_array()) {
        let steps: Vec<ProcedureStep> = steps.iter().filter_map(|s| {
            Some(ProcedureStep {
                description: s.get("description")?.as_str()?.to_string(),
                tool: s.get("tool").and_then(|x| x.as_str()).map(String::from),
            })
        }).collect();
        if !steps.is_empty() {
            manifest.content.procedure = Some(Procedure { variables: vec![], steps });
        }
    }
    if let Some(triggers) = v.get("triggers").and_then(|x| x.as_array()) {
        manifest.triggers = triggers.iter().filter_map(|t| {
            let kind = match t.get("type")?.as_str()? {
                "command" => TriggerKind::Command,
                "keyword" => TriggerKind::Keyword,
                _ => return None,
            };
            Some(Trigger {
                kind,
                pattern: t.get("pattern").and_then(|x| x.as_str()).map(String::from),
            })
        }).collect();
    }
    Ok(())
}
```

> **Adjust field names if `ChatResponse` differs.** Quick check: the
> conversations backend defines `ChatResponse { text: String, usage: Usage }`
> (see `mur-core/src/conversations/backend/mod.rs`). If it's not `.text`, fix
> the one accessor — no other call sites change.

- [ ] **3.2** Wire into `cmd_from_pattern_with_home` (replace the `eprintln!` placeholder added in T2.2). Polish happens **before** scan / write / hash / trust so the on-disk artifact is the polished one and the hash matches:

```rust
let mut manifest = pattern_to_skill(&pattern, polish)?;

if polish {
    use mur_common::config::Config;
    use crate::conversations::backend::factory;

    let cfg_path = mur_home.join("config.yaml");
    let cfg = Config::load_or_default(&cfg_path);
    let backend = factory::build_for_stage(&cfg.synthesize_backend(), "skill.from-pattern")
        .context("build LLM backend for polish")?;
    polish_via_llm(&mut manifest, &pattern, backend.as_ref())
        .await
        .context("polish failed — re-run without --polish to install the unpolished skill")?;
    // Polish mutated the manifest — re-validate before write.
    mur_common::skill::validate(&manifest)
        .context("polished skill manifest failed schema validation")?;
}

// scan → write → hash → trust (unchanged from T2.2, but now operating on
// the post-polish manifest when polish was requested).
```

- [ ] **3.3** Commit:
  ```bash
  cargo check -p mur-core
  git add mur-core/src/cmd/skill_from_pattern.rs
  git commit -m "feat(skill): LLM polish for from-pattern"
  ```

---

### Task 4 — E2E test

**Files:** `mur-core/tests/skill_from_pattern_e2e.rs`.

- [ ] **4.1** Two tests, both driving the production entry point `cmd_from_pattern_with_home` (defined in T2.2 — no test-only shim):

```rust
use mur_common::knowledge::Maturity;
use mur_common::skill::types::TrustLevel;
use mur_common::skill::{read_from_dir, validate};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_from_pattern::cmd_from_pattern_with_home;
use mur_core::store::yaml::YamlStore;

#[tokio::test]
async fn promotes_stable_pattern_to_sandboxed_skill() {
    let home = tempfile::tempdir().unwrap();

    // 1. Create a Stable pattern in tempdir.
    let store = YamlStore::new(home.path().join("patterns")).unwrap();
    let mut p = make_pattern("git-push-flow", "Always pull before push");
    p.base.maturity = Maturity::Stable;
    store.save(&p).unwrap();

    // 2. Run from-pattern (polish=false → no LLM call, no network).
    cmd_from_pattern_with_home(home.path(), "git-push-flow", false)
        .await
        .unwrap();

    // 3. Assert skill.yaml exists and validates.
    let skill_dir = home.path().join("skills/git-push-flow");
    assert!(skill_dir.join("skill.yaml").exists());
    let m = read_from_dir(&skill_dir).unwrap();
    validate(&m).unwrap();
    assert_eq!(m.publisher, "agent:from-pattern");
    assert_eq!(m.version, "0.1.0");

    // 4. Trust entry is Sandboxed and tagged with the right publisher.
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let entry = trust.entries.values().find(|e| e.name == "git-push-flow").unwrap();
    assert!(matches!(entry.level, TrustLevel::Sandboxed));
    assert_eq!(entry.publisher.as_deref(), Some("agent:from-pattern"));
}

#[tokio::test]
async fn rejects_draft_pattern() {
    let home = tempfile::tempdir().unwrap();
    let store = YamlStore::new(home.path().join("patterns")).unwrap();
    let mut p = make_pattern("draft-thing", "...");
    p.base.maturity = Maturity::Draft;
    store.save(&p).unwrap();

    let err = cmd_from_pattern_with_home(home.path(), "draft-thing", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Stable or Canonical"));
}
```

> The `make_pattern` helper builds a minimal valid `Pattern` (kebab-case name, non-empty `content.principle` + `content.technical`, `Maturity::Draft` default). Define it once at the top of the test file.

- [ ] **4.2** Commit:
  ```bash
  cargo test -p mur-core --test skill_from_pattern_e2e
  git add mur-core/tests/
  git commit -m "test(skill): e2e pattern promotion with maturity gate"
  ```

---

## Self-Review

**Spec §8.1 coverage:**

| Item | Status | Task |
|---|---|---|
| `mur skill from-pattern <name>` | ✅ | T2 |
| Pattern maturity gate (Stable/Canonical only) | ✅ | T1 |
| Mechanical Pattern → SkillManifest conversion | ✅ | T1 |
| LLM polish pass (`--polish`) | ✅ | T3 |
| Generated skill enters at Sandboxed trust | ✅ | T2 |
| Security scan runs on output | ✅ | T2 |

**Risks:**

1. **Pattern naming**: Pattern names use the same kebab-case convention as skill names — but the conversion now calls `validate(&manifest)` at the end of `pattern_to_skill` (T1.1), so a name that violates skill schema fails fast with a clear error instead of producing a malformed on-disk skill. If a pattern's name collides with an installed skill, `write_to_dir` overwrites silently — acceptable for an explicit promotion.

2. **LLM backend shape**: Polish uses `ChatBackend::generate(ChatRequest<'_>)` directly — same as every other LLM call site in `mur-core` (`conversations/`, `cmd/skill_generate.rs`). No `LlmClient` adapter, no shim. Cost: this milestone takes a direct dependency on `crate::conversations::backend::{ChatBackend, ChatRequest}` types; that's a fine coupling since `mur-core` already owns both.

3. **`ChatResponse` field name**: The polish code reads `resp.text`. If the type uses a different field name (e.g. `content`, `completion`), Task 3.1 needs one accessor swap — verifiable in 30 seconds before starting T3. Listed as a 1-line precheck rather than a separate task.

4. **No `--dry-run`**: Not in this milestone. The `--polish` flag's LLM call is the only costly operation; to preview, omit `--polish`, inspect the skill.yaml, then re-run with `--polish` (which overwrites + polishes — the second insert into the trust store creates a second hash entry for the polished version; the un-polished entry stays as harmless dead weight).

5. **Async signature change**: `cmd_from_pattern` is `async fn`. Dispatch uses `.await` (T2.3). Confirmed compatible — `dispatch::run` is `pub async fn run` (`mur-core/src/dispatch.rs:20`) and the `Commands::Skill` arm runs inside it.

**Placeholder scan:** The only `eprintln!("note: --polish requires...")` placeholder in T2 is replaced by T3. No other placeholders.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m3b2.md`.

4 tasks, ~300 lines of implementation. Independent of M3a, M3b, and M3c. Suggested branch: `feat/skill-ecosystem-m3b2`.

**30-second precheck before T3** (the only assumption not 100% verified):

```bash
rg -n "pub struct ChatResponse" mur-core/src/conversations/backend/
```

Confirm the response-body field name (`text` vs `content` vs `completion`). If it's not `text`, swap the one accessor in `polish_via_llm`. Everything else in the plan is verified against `main`.
