# Agent Export UX — Plan 1: Foundation & Logic Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the data + logic foundation for "give an agent to a friend" — the `.muragent` `model_hint` field, the model-class table, the local-first/cloud-honest resolution decision tree, the export-side secret-leak fix, and the retirement of the toolchain-bound `--format=gui`/`--format=bin` export paths.

**Architecture:** Pure-logic, fully unit-testable layer in `mur-common` (no GUI, no OS integration), plus a small `mur-core` CLI change. This is the substrate the later plans build on: Plan 1b wires `mur-agent-runtime --load` + CLI model resolution onto it; Plan 2 wires the Hub GUI Share command, OS file association, and the GUI model wizard.

**Tech Stack:** Rust (edition 2024), `serde` / `serde_yaml_ng`, `anyhow`, existing `mur-common::muragent` package library and `mur-common::model` registry.

**Spec:** `docs/superpowers/specs/2026-05-29-mur-agent-export-ux-give-to-a-friend-design.md` (Workstreams B-data §7.1–7.2, the §7.3 decision tree, and D §8).

---

## Scope & follow-on plans

This plan covers the parts that are pure-logic and risk-free to specify exactly:

- **In scope:** `model_hint` manifest type + field (§7.1); model-class `classify()` table; writer populates the hint; export sanitization strips `model_ref` and overrides the hint from the registry (§7.2, closes the model-side secret leak); the `recommend()` decision tree (§7.3); retire `--format=gui`/`--format=bin` with a redirecting error and delete the secret-leaking `export_bin` (§8/D).
- **Deferred to Plan 1b** (`mur-agent-runtime`): `--load <path.muragent>` + non-interactive `--model` + CLI interactive resolution prompts (needs supervisor-discovery work).
- **Deferred to Plan 2** (`mur-hub-gui` + `mur-gui-core`): Hub "Share" command/UI (C), tauri.conf file association / deep-link / dmg / notarization + open-routing + onboarding (A), GUI model wizard step (B-wizard GUI surface).

Each later plan depends only on the public types this plan introduces (`ModelHint`, `ModelTier`, `classify`, `Hardware`, `Recommendation`, `recommend`).

---

## File structure

- `mur-common/src/muragent/manifest.rs` — **Modify.** Add `ModelTier` enum, `ModelHint` struct, and the `model_hint: Option<ModelHint>` field on `MuragentManifest`.
- `mur-common/src/muragent/model_class.rs` — **Create.** `classify(provider, name) -> ModelHint` (the static model-class table).
- `mur-common/src/muragent/mod.rs` — **Modify.** `pub mod model_class;`
- `mur-common/src/muragent/writer.rs` — **Modify.** `build_manifest_from_profile` populates `model_hint` from the inline binding.
- `mur-common/src/model_resolve.rs` — **Create.** `Hardware`, `Recommendation`, `recommend(...)` — the §7.3 decision tree.
- `mur-common/src/lib.rs` — **Modify.** `pub mod model_resolve;`
- `mur-core/src/cmd/agent/export.rs` — **Modify.** Sanitize strips `model_ref`; `export_muragent` overrides `model_hint` from the registry when `model_ref` was set; `cmd_export` redirects `bin`/`gui`; delete `export_bin` + `locate_runtime_manifest_dir`.
- `mur-core/src/dispatch.rs` — **Modify.** Route the `gui` format through `cmd_export` (drop the special-case branch).

---

## Task 1: `ModelHint` / `ModelTier` types + manifest field

**Files:**
- Modify: `mur-common/src/muragent/manifest.rs`
- Test: same file (`#[cfg(test)]` at bottom)

- [ ] **Step 1: Write the failing test**

Add to the existing test module (or create one) at the bottom of `manifest.rs`:

```rust
#[cfg(test)]
mod model_hint_tests {
    use super::*;

    #[test]
    fn manifest_round_trips_without_model_hint() {
        let yaml = "\
schema: mur-agent/2
exported_at: '2026-05-29T00:00:00Z'
exporter: { mur_version: 1.0.0, tool: mur }
agent: { slug: coach, display_name: Coach, bundle_id: run.mur.agent.coach, url_scheme: muragent-coach, original_uuid: u1 }
required_surfaces: [hub]
icon: {}
";
        let m: MuragentManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(m.model_hint.is_none());
    }

    #[test]
    fn model_hint_serializes_and_parses() {
        let hint = ModelHint {
            provider: "ollama".into(),
            name: "llama3.2:3b".into(),
            tier: ModelTier::Small,
            min_ram_gb: 8,
            local_capable: true,
        };
        let s = serde_yaml_ng::to_string(&hint).unwrap();
        let back: ModelHint = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(hint, back);
        assert!(s.contains("tier: small"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common model_hint -- --nocapture`
Expected: FAIL — `cannot find type ModelHint` / `no field model_hint`.

- [ ] **Step 3: Add the types and field**

In `manifest.rs`, add after the `Surface` enum:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Small,
    Mid,
    Frontier,
}

/// Declares what kind of model the agent was authored against, so the
/// recipient's first-run wizard can resolve a backend (no weights travel).
/// See spec §7.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelHint {
    pub provider: String,
    pub name: String,
    pub tier: ModelTier,
    #[serde(default)]
    pub min_ram_gb: u32,
    pub local_capable: bool,
}
```

In the `MuragentManifest` struct, add this field (after `assignment`):

```rust
    /// Model backend hint for the recipient's first-run resolution (§7.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<ModelHint>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common model_hint`
Expected: PASS (both tests).

> Note: adding the field breaks the `MuragentManifest { … }` struct literal in `writer.rs::build_manifest_from_profile` (missing field). Task 3 fixes it. If you build the whole crate now it will fail to compile until Task 3 — that is expected; run only the targeted `-p mur-common` lib test for `manifest`, or proceed to Task 3 before a full build.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/muragent/manifest.rs
git commit -m "feat(muragent): add model_hint manifest type + field"
```

---

## Task 2: `classify()` model-class table

**Files:**
- Create: `mur-common/src/muragent/model_class.rs`
- Modify: `mur-common/src/muragent/mod.rs`
- Test: `mur-common/src/muragent/model_class.rs` (inline)

- [ ] **Step 1: Write the failing test**

Create `mur-common/src/muragent/model_class.rs`:

```rust
//! Static model-class table: map a (provider, name) binding to a `ModelHint`
//! (tier, RAM estimate, local-capability). Heuristic + conservative on
//! unknown providers. See spec §7.1.

use crate::muragent::manifest::{ModelHint, ModelTier};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_small_model() {
        let h = classify("ollama", "llama3.2:3b");
        assert_eq!(h.tier, ModelTier::Small);
        assert!(h.local_capable);
        assert_eq!(h.min_ram_gb, 8);
    }

    #[test]
    fn cloud_frontier_model() {
        let h = classify("anthropic", "claude-opus-4-7");
        assert_eq!(h.tier, ModelTier::Frontier);
        assert!(!h.local_capable);
        assert_eq!(h.min_ram_gb, 0);
    }

    #[test]
    fn cloud_small_variant_is_mid() {
        let h = classify("openai", "gpt-4o-mini");
        assert_eq!(h.tier, ModelTier::Mid);
        assert!(!h.local_capable);
    }

    #[test]
    fn unknown_provider_is_conservative_local_mid() {
        let h = classify("acme-llm", "whatever-v2");
        assert_eq!(h.tier, ModelTier::Mid);
        assert!(h.local_capable);
        assert_eq!(h.min_ram_gb, 16);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common model_class`
Expected: FAIL — `cannot find function classify` (and module not declared).

- [ ] **Step 3: Implement `classify` + declare the module**

Add to the top of `model_class.rs` (above the test module):

```rust
const LOCAL_PROVIDERS: &[&str] =
    &["ollama", "mlx", "llamacpp", "llama_cpp", "localai", "lmstudio"];
const CLOUD_PROVIDERS: &[&str] = &[
    "anthropic", "openai", "google", "gemini", "mistral", "groq", "cohere",
    "deepseek", "xai", "openrouter",
];
const SMALL_MARKERS: &[&str] =
    &["1b", "1.5b", "2b", "3b", "mini", "nano", "haiku", "flash", "small", "tiny"];
const LARGE_LOCAL_MARKERS: &[&str] = &["70b", "72b", "405b", "mixtral", "command-r-plus"];

/// Map a model binding to a `ModelHint`. Local providers classify by size
/// markers in the name; known cloud providers are frontier unless a "small"
/// variant marker is present; unknown providers fall back to a conservative
/// local-capable mid tier (the wizard still offers all options).
pub fn classify(provider: &str, name: &str) -> ModelHint {
    let p = provider.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|m| n.contains(m));

    if LOCAL_PROVIDERS.contains(&p.as_str()) {
        let (tier, min_ram_gb) = if has(SMALL_MARKERS) {
            (ModelTier::Small, 8)
        } else if has(LARGE_LOCAL_MARKERS) {
            (ModelTier::Frontier, 64)
        } else {
            (ModelTier::Mid, 16)
        };
        return ModelHint {
            provider: provider.to_string(),
            name: name.to_string(),
            tier,
            min_ram_gb,
            local_capable: true,
        };
    }

    if CLOUD_PROVIDERS.contains(&p.as_str()) {
        let tier = if has(SMALL_MARKERS) {
            ModelTier::Mid
        } else {
            ModelTier::Frontier
        };
        return ModelHint {
            provider: provider.to_string(),
            name: name.to_string(),
            tier,
            min_ram_gb: 0,
            local_capable: false,
        };
    }

    // Unknown provider: conservative — assume a local-capable mid model.
    ModelHint {
        provider: provider.to_string(),
        name: name.to_string(),
        tier: ModelTier::Mid,
        min_ram_gb: 16,
        local_capable: true,
    }
}
```

In `mur-common/src/muragent/mod.rs`, add alongside the other `pub mod` lines:

```rust
pub mod model_class;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common model_class`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/muragent/model_class.rs mur-common/src/muragent/mod.rs
git commit -m "feat(muragent): model-class table for model_hint classification"
```

---

## Task 3: Writer populates `model_hint`

**Files:**
- Modify: `mur-common/src/muragent/writer.rs:171-224` (the `MuragentManifest { … }` literal in `build_manifest_from_profile`)
- Test: `mur-common/src/muragent/writer.rs` (test module at bottom)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `writer.rs`:

```rust
    #[test]
    fn manifest_carries_model_hint_from_inline_binding() {
        let mut profile = AgentProfile::default_for_tests();
        profile.model = crate::agent::ModelConfig {
            provider: "ollama".into(),
            name: "llama3.2:3b".into(),
            params: Default::default(),
        };
        let m = build_manifest_from_profile(&profile, "1.0.0");
        let hint = m.model_hint.expect("model_hint populated");
        assert_eq!(hint.tier, crate::muragent::manifest::ModelTier::Small);
        assert!(hint.local_capable);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common -- writer::tests::manifest_carries_model_hint`
Expected: FAIL — struct literal missing field `model_hint` (compile error) or `model_hint` is `None`.

- [ ] **Step 3: Populate the field in the struct literal**

In `build_manifest_from_profile`, change the tail of the returned literal from:

```rust
        commander: None,
        deployment: None,
        assignment: None,
    }
```

to:

```rust
        commander: None,
        deployment: None,
        assignment: None,
        model_hint: Some(crate::muragent::model_class::classify(
            &profile.model.provider,
            &profile.model.name,
        )),
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common -- writer::tests::manifest_carries_model_hint`
Then a full lib check: `cargo test -p mur-common`
Expected: PASS; the crate now compiles (Task 1's literal gap is closed).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/muragent/writer.rs
git commit -m "feat(muragent): populate model_hint from inline binding on export"
```

---

## Task 4: Sanitize strips `model_ref`

**Files:**
- Modify: `mur-core/src/cmd/agent/export.rs:89-121` (`sanitize_profile_for_export`)
- Test: `mur-core/src/cmd/agent/export.rs` (inline test module)

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of `export.rs`:

```rust
#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use mur_common::AgentProfile;

    #[test]
    fn sanitize_strips_model_ref() {
        let mut p = AgentProfile::default_for_tests();
        p.model_ref = Some("anthropic_opus_4_7".into());
        let removed = sanitize_profile_for_export(&mut p);
        assert!(p.model_ref.is_none(), "model_ref must be stripped");
        assert!(
            removed.iter().any(|r| r == "model_ref"),
            "removed list must record model_ref, got {removed:?}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- sanitize_tests::sanitize_strips_model_ref`
Expected: FAIL — `model_ref` still `Some`, not in removed list.

- [ ] **Step 3: Strip `model_ref` in `sanitize_profile_for_export`**

In `sanitize_profile_for_export`, just before `removed` is returned (after the `transport.socket.auth` block), add:

```rust
    if profile.model_ref.is_some() {
        removed.push("model_ref".to_string());
        profile.model_ref = None;
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core -- sanitize_tests::sanitize_strips_model_ref`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/export.rs
git commit -m "fix(export): strip model_ref from exported profile (no registry/secret leak)"
```

---

## Task 5: Override `model_hint` from the registry when `model_ref` was set

**Files:**
- Modify: `mur-core/src/cmd/agent/export.rs:37-84` (`export_muragent`)
- Test: `mur-core/src/cmd/agent/export.rs` (inline test module)

The inline `model:` block may be a stale default when the agent actually binds via `model_ref`. After building the manifest, if the *original* profile had a `model_ref`, resolve it in the registry and overwrite `model_hint` so the recipient's wizard sees the real tier.

- [ ] **Step 1: Write the failing test**

Add to the `sanitize_tests` module (it already imports what we need; add a registry-backed test):

```rust
    #[test]
    fn model_hint_override_uses_registry_entry() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        let reg = ModelRegistry {
            schema_version: 1,
            models: std::collections::BTreeMap::from([(
                "anthropic_opus_4_7".to_string(),
                ModelEntry {
                    provider: "anthropic".into(),
                    model: "claude-opus-4-7".into(),
                    base_url: None,
                    secret: None,
                    capabilities: vec![],
                    params: serde_json::Value::Null,
                },
            )]),
            roles: Default::default(),
        };
        let hint = model_hint_from_ref("anthropic_opus_4_7", &reg).expect("resolved");
        assert_eq!(hint.tier, mur_common::muragent::manifest::ModelTier::Frontier);
        assert!(!hint.local_capable);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- sanitize_tests::model_hint_override`
Expected: FAIL — `cannot find function model_hint_from_ref`.

- [ ] **Step 3: Add the helper and call it in `export_muragent`**

Add this pure helper near `sanitize_profile_for_export` in `export.rs`:

```rust
/// Resolve a `model_ref` against a registry into a `ModelHint`. Returns
/// `None` when the registry has no such entry (the inline-derived hint
/// then stands).
fn model_hint_from_ref(
    model_ref: &str,
    registry: &mur_common::model::ModelRegistry,
) -> Option<mur_common::muragent::manifest::ModelHint> {
    registry
        .models
        .get(model_ref)
        .map(|e| mur_common::muragent::model_class::classify(&e.provider, &e.model))
}
```

In `export_muragent`, the manifest is built before sanitization. Capture the original `model_ref` and override the hint. Change:

```rust
    let mur_version = env!("CARGO_PKG_VERSION");
    let mut manifest = build_manifest_from_profile(&profile, mur_version);
```

to:

```rust
    let mur_version = env!("CARGO_PKG_VERSION");
    let mut manifest = build_manifest_from_profile(&profile, mur_version);

    // If the agent binds via model_ref, the inline-derived hint may be a
    // stale default — override it from the registry entry (§7.1).
    if let Some(model_ref) = profile.model_ref.as_deref() {
        let reg_path = mur_common::model::ModelRegistry::default_path()?;
        let registry = mur_common::model::ModelRegistry::load_from(&reg_path)?;
        if let Some(hint) = model_hint_from_ref(model_ref, &registry) {
            manifest.model_hint = Some(hint);
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core -- sanitize_tests::model_hint_override`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/export.rs
git commit -m "feat(export): override model_hint from registry when model_ref is set"
```

---

## Task 6: Resolution decision tree (`recommend`)

**Files:**
- Create: `mur-common/src/model_resolve.rs`
- Modify: `mur-common/src/lib.rs`
- Test: `mur-common/src/model_resolve.rs` (inline)

- [ ] **Step 1: Write the failing test**

Create `mur-common/src/model_resolve.rs`:

```rust
//! First-run model-resolution decision tree (spec §7.3). Pure logic shared
//! by the CLI (Plan 1b) and the Hub GUI wizard (Plan 2). Hardware detection
//! and the actual pull/key-entry actions live in the surface layers; this
//! module only decides the recommended default given a hint + hardware.

use crate::muragent::manifest::ModelHint;

/// Recipient hardware snapshot. `apple_silicon` and `ollama_present` inform
/// which options a surface offers; the recommendation itself keys off RAM
/// and the hint's `local_capable` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hardware {
    pub total_ram_gb: u32,
    pub apple_silicon: bool,
    pub ollama_present: bool,
}

/// The highlighted default the wizard should pre-select. All escape hatches
/// (local pull / paste key / endpoint) remain available regardless (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recommendation {
    Local,
    Cloud,
    CloudOrSmallerLocal,
    NeutralMenu,
}

pub fn recommend(hint: Option<&ModelHint>, hw: &Hardware) -> Recommendation {
    match hint {
        None => Recommendation::NeutralMenu,
        Some(h) if !h.local_capable => Recommendation::Cloud,
        Some(h) if hw.total_ram_gb >= h.min_ram_gb => Recommendation::Local,
        Some(_) => Recommendation::CloudOrSmallerLocal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muragent::manifest::ModelTier;

    fn hint(local: bool, min_ram: u32) -> ModelHint {
        ModelHint {
            provider: "p".into(),
            name: "n".into(),
            tier: if local { ModelTier::Small } else { ModelTier::Frontier },
            min_ram_gb: min_ram,
            local_capable: local,
        }
    }
    fn hw(ram: u32) -> Hardware {
        Hardware { total_ram_gb: ram, apple_silicon: true, ollama_present: true }
    }

    #[test]
    fn no_hint_is_neutral() {
        assert_eq!(recommend(None, &hw(16)), Recommendation::NeutralMenu);
    }
    #[test]
    fn frontier_is_cloud() {
        assert_eq!(recommend(Some(&hint(false, 0)), &hw(16)), Recommendation::Cloud);
    }
    #[test]
    fn local_with_enough_ram_is_local() {
        assert_eq!(recommend(Some(&hint(true, 8)), &hw(16)), Recommendation::Local);
    }
    #[test]
    fn local_without_ram_is_cloud_or_smaller() {
        assert_eq!(
            recommend(Some(&hint(true, 16)), &hw(8)),
            Recommendation::CloudOrSmallerLocal
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common model_resolve`
Expected: FAIL — module not declared (`cannot find`).

- [ ] **Step 3: Declare the module**

In `mur-common/src/lib.rs`, add alongside the other top-level `pub mod` lines:

```rust
pub mod model_resolve;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common model_resolve`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/model_resolve.rs mur-common/src/lib.rs
git commit -m "feat(model): first-run resolution decision tree (local-first, cloud-honest)"
```

---

## Task 7: Retire `--format=gui` / `--format=bin`

**Files:**
- Modify: `mur-core/src/cmd/agent/export.rs` (`cmd_export`; delete `export_bin` + `locate_runtime_manifest_dir`)
- Modify: `mur-core/src/dispatch.rs:974-1003` (drop the `gui` special-case)
- Test: `mur-core/src/cmd/agent/export.rs` (inline)

- [ ] **Step 1: Write the failing test**

Add to the `sanitize_tests` module in `export.rs`:

```rust
    #[test]
    fn removed_formats_redirect() {
        for fmt in ["bin", "gui"] {
            let err = cmd_export("coach", "/tmp/out", fmt).unwrap_err().to_string();
            assert!(err.contains(".muragent"), "fmt {fmt}: {err}");
            assert!(err.contains("--load"), "fmt {fmt}: {err}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- sanitize_tests::removed_formats_redirect`
Expected: FAIL — `bin` currently calls `export_bin` (cargo build) and `gui` is unreachable here (handled in dispatch); no redirect error.

- [ ] **Step 3: Add the early redirect; delete `export_bin`**

At the very top of `cmd_export`, before `resolve_mur_home()`:

```rust
pub fn cmd_export(name: &str, out: &str, format: &str) -> Result<()> {
    if matches!(format, "bin" | "gui") {
        bail!(
            "--format={format} is no longer supported.\n\
             Export a portable .muragent (the default) and share that one file:\n  \
             mur agent export {name} -o {name}.muragent\n\
             Recipients open it with the MuR Hub app, or run it headless with:\n  \
             mur-agent-runtime --load {name}.muragent"
        );
    }
    let mur_home = resolve_mur_home()?;
```

Remove the now-dead `"bin" => export_bin(...)` arm from the `match format` block (leave `muragent`, `pkg`, and the `other => bail!(...)` arms). Delete the `export_bin` function and the `locate_runtime_manifest_dir` helper entirely (and any now-unused imports they pulled in, e.g. `PathBuf` if unused — let the compiler guide you).

- [ ] **Step 4: Route `gui` through `cmd_export` in dispatch**

In `mur-core/src/dispatch.rs`, replace the whole `AgentAction::Export { … }` arm (the `if format == "gui" { … } else { … }` block) with:

```rust
        AgentAction::Export { name, out, format, .. } => {
            cmd::agent::cmd_export(&name, &out, &format)?;
        }
```

The `..` drops the gui-only `theme`/`icon`/`clone_identity`/`skip_notarize` bindings (now unused). Remove the `use`/call of `cmd::agent_export_gui` if it is no longer referenced anywhere else (compiler will flag an unused import).

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p mur-core -- sanitize_tests::removed_formats_redirect`
Then: `cargo build -p mur-core`
Expected: test PASS; crate builds (fix any unused-import warnings the deletions surfaced).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/export.rs mur-core/src/dispatch.rs
git commit -m "refactor(export): retire --format=gui/bin with redirect; drop secret-leaking export_bin"
```

---

## Self-Review

**1. Spec coverage (Plan 1 scope):**
- §7.1 `model_hint` field → Task 1; classification table → Task 2; populated on export → Tasks 3 & 5. ✓
- §7.2 sanitize converts `model_ref` away (no registry/secret travels) → Task 4 (strip) + Task 5 (hint from registry). ✓
- §7.3 decision tree → Task 6. ✓
- §8/D retire `gui`/`bin`, remove secret-leaking `export_bin` → Task 7. ✓
- Out-of-scope by design: `--load` (Plan 1b), GUI/distribution (Plan 2) — listed in "Scope & follow-on plans". ✓

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to". Every code step shows the full code. ✓

**3. Type consistency:** `ModelHint { provider, name, tier, min_ram_gb, local_capable }` and `ModelTier { Small, Mid, Frontier }` are used identically in Tasks 1, 2, 3, 5, 6. `classify(&str, &str) -> ModelHint` called the same way in Tasks 3 and 5. `recommend(Option<&ModelHint>, &Hardware) -> Recommendation` self-consistent in Task 6. `ModelEntry`/`ModelRegistry` field names match `mur-common/src/model.rs` (`provider`, `model`, `base_url`, `secret`, `capabilities`, `params`; `schema_version`, `models`, `roles`). ✓

**Verification note:** `AgentProfile::default_for_tests()` is used by existing `writer.rs` tests, so it is available in `mur-common`; for the `mur-core` `export.rs` tests confirm it is re-exported (it is referenced as `mur_common::AgentProfile`). If `default_for_tests` is not public from `mur-core`'s view, construct the profile via `AgentProfile::default()` and set the two fields the tests touch (`model`, `model_ref`).
