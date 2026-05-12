# mur Agent D2 — First-Memory Onboarding (M2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship a 5-step first-launch onboarding wizard in `mur-agent-gui` that completes in ≤ 2 minutes and produces (a) a populated `companion/relationship.json` with a `first_memory` field, (b) profile-level three-tier proactive layers (warm voice / behavior collection / proactive sends) defaulting to layer 1 only, and (c) round-trippable `extensions.mur.first_memory.{text, established_at}` on any exported `.murcard.yaml`. The day-3 `morning_greeting` situation references the first memory verbatim. Per roadmap §4.2 (D2 First-Memory Onboarding).

**Architecture:** The wizard is a Tauri-side React component that renders on first launch and writes through three thin Tauri commands (`companion_onboarding_*`). The wizard reuses the existing `mur agent companion init` Rust pipeline (`mur-core/src/cmd/agent_companion/init.rs`) — extended with two new fields (`agent_display_name`, `first_memory`) and the third proactive layer toggle. Voice setup (Step 2) defers to M1's opt-in default-off voice subsystem with a "Skip / Enable later" path so the wizard never blocks on a 190 MB-1 GB voice/STT download. Companion picker (`mur-agent-runtime/src/companion/voice.rs`) gains a `{{FIRST_MEMORY}}` template variable, and the seed `morning_greeting` content for day-3+ references it; the existing MockClock harness drives the 72-hour acceptance test.

**Tech Stack:** Rust 2024 (existing companion pipeline), Tauri 2 + React 18 + Vite + Tailwind 4 (existing GUI), `dialoguer` (existing CLI fallback), `chrono` (timestamps), `serde_yaml_ng` (round-trip with comments preserved). No new external crates required.

**Spec:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §4.2 (D2 wizard 5 steps + acceptance), §4.4 (D4 character card extensions for first_memory passthrough).

**Predecessors:**
- M0 hooks: `docs/superpowers/plans/2026-04-30-mur-agent-hooks-a0.md` (PR #44 merged)
- M1 voice: `docs/superpowers/plans/2026-04-30-mur-agent-d1-voice.md` (PRs #48, #49, #50, #56, #57, #53, #54, #55 all merged)
- Companion phase 1.1 spec: `docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md`

**Commit format:** `M2.<n>.<m>: <subject>` so `git log --grep "^M2"` shows progress.

**Branch policy:** All M2 work lands on stacked branches off `main`:
- `feat/mur-agent-d2-onboarding-plan` (this plan)
- `feat/mur-agent-d2-onboarding-m2.1-schema` (schema)
- `feat/mur-agent-d2-onboarding-m2.2-picker` (picker template var)
- `feat/mur-agent-d2-onboarding-m2.3-cli` (CLI 5-step wizard)
- `feat/mur-agent-d2-onboarding-m2.4-tauri` (Tauri commands)
- `feat/mur-agent-d2-onboarding-m2.5-ui` (React wizard)
- `feat/mur-agent-d2-onboarding-m2.6-autolaunch` (first-launch detection)
- `feat/mur-agent-d2-onboarding-m2.7-card` (character card extension)
- `feat/mur-agent-d2-onboarding-m2.8-e2e` (acceptance E2E)

Bases: each subsequent branch stacks on the previous; merge bottom-up via squash + delete-branch + retarget-to-main as the M1 cascade did.

---

## File Structure

```
mur-common/src/
  agent.rs                              # MODIFY: extend OnboardingState + add FirstMemory struct
  companion/
    mod.rs                              # MODIFY: re-export FirstMemory
    voice_template.rs                   # MODIFY: substitute {{FIRST_MEMORY}}
    content/
      morning_greeting.en-US.yaml       # MODIFY: add day-3+ template referencing {{FIRST_MEMORY}}
      morning_greeting.zh-TW.yaml       # MODIFY: same, in zh-TW
    content_seed.rs                     # no change (auto-includes the .yaml above)

mur-agent-runtime/src/companion/
  voice.rs                              # MODIFY: thread first_memory into VoiceInput
  picker.rs                             # MODIFY: surface FirstMemoryAware filter
  outbox.rs                             # MODIFY: load first_memory from relationship.json on tick

mur-core/src/cmd/agent_companion/
  init.rs                               # MODIFY: 5-step wizard + Answers schema extension
  util.rs                               # no change (atomic_write_* already exist)
  mod.rs                                # MODIFY: register new sub-args

mur-core/src/cmd/agent_companion/
  preview.rs                            # MODIFY: hydrate first_memory in preview render

mur-agent-gui/src-tauri/src/
  commands.rs                           # MODIFY: add companion_onboarding_* commands
  main.rs                               # MODIFY: register new commands; first-launch detection
  onboarding/                           # NEW module
    mod.rs                              # CREATE: OnboardingService — orchestrates 5 steps
    state.rs                            # CREATE: in-memory wizard state (validation, persistence)

mur-agent-gui/ui/src/
  onboarding/                           # NEW module
    OnboardingWizard.tsx                # CREATE: 5-step shell with progress + Skip/Back/Next
    Step1AgentName.tsx                  # CREATE: name your agent (display name)
    Step2VoicePick.tsx                  # CREATE: voice picker with "Skip / Enable later"
    Step3Relationship.tsx               # CREATE: friend / coach / accountability / mentor
    Step4FirstMemory.tsx                # CREATE: textarea + 200-char counter
    Step5ProactiveTiers.tsx             # CREATE: three-layer toggle (warm / behavior / proactive)
    types.ts                            # CREATE: shared types
    api.ts                              # CREATE: Tauri command bindings
  App.tsx                               # MODIFY: render <OnboardingWizard /> when not completed
  lib/api.ts                            # MODIFY: add getOnboardingStatus() helper

mur-core/src/character_card/            # NEW module (lands here, even though full D4 is later)
  schema.rs                             # CREATE: minimal CCv3-compatible struct with extensions.mur
  first_memory.rs                       # CREATE: FirstMemoryExt (text, established_at)
  serde_round_trip.rs                   # CREATE: serde_yaml_ng helpers for ccv3_passthrough

mur-core/src/cmd/agent_export/
  card.rs                               # MODIFY: emit extensions.mur.first_memory on export

mur-agent-runtime/tests/
  onboarding_picker_first_memory.rs     # CREATE: picker recognizes {{FIRST_MEMORY}}
  onboarding_morning_greeting_72h.rs    # CREATE: MockClock 72h advance acceptance

mur-core/tests/
  companion_init_5step.rs               # CREATE: --answers YAML round-trip (5 fields)
  companion_preview_first_memory.rs     # CREATE: companion preview --situation morning_greeting

mur-agent-gui/src-tauri/tests/
  onboarding_commands.rs                # CREATE: tauri command unit tests
  onboarding_first_launch.rs            # CREATE: ApplicationLauncher detects no completed_at

scripts/e2e/
  v1-d2-onboarding.sh                   # CREATE: drives the GUI wizard end-to-end (≤ 120s budget)

docs/superpowers/specs/
  2026-04-30-mur-agent-harness-roadmap-design.md   # roadmap §4.2 reference
docs/cookbook/
  first-memory-onboarding.md            # CREATE: end-user setup walkthrough
```

---

## Milestone M2.1 — Schema: FirstMemory + ProactiveTiers + OnboardingState extension

### Task M2.1.1: Add FirstMemory struct to mur-common

**Files:**
- Modify: `mur-common/src/agent.rs:599-605` (OnboardingState region)
- Test: `mur-common/tests/companion_enums.rs` (existing file, append)

- [x] **Step 1: Write failing round-trip test**

```rust
// mur-common/tests/companion_enums.rs (append)
use mur_common::agent::FirstMemory;
use chrono::{TimeZone, Utc};

#[test]
fn first_memory_yaml_roundtrip() {
    let fm = FirstMemory {
        text: "We met on a Sunday in Taipei.".into(),
        established_at: Utc.with_ymd_and_hms(2026, 4, 30, 14, 13, 0).unwrap(),
    };
    let s = serde_yaml_ng::to_string(&fm).unwrap();
    assert!(s.contains("text:"));
    assert!(s.contains("established_at:"));
    let back: FirstMemory = serde_yaml_ng::from_str(&s).unwrap();
    assert_eq!(back, fm);
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-common first_memory_yaml_roundtrip`
Expected: `error[E0432]: unresolved import 'mur_common::agent::FirstMemory'`.

- [x] **Step 3: Add FirstMemory + extend OnboardingState**

Modify `mur-common/src/agent.rs` near line 599:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstMemory {
    pub text: String,
    pub established_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnboardingState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_memory: Option<FirstMemory>,
}
```

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-common first_memory_yaml_roundtrip`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/tests/companion_enums.rs
git commit -m "M2.1.1: add FirstMemory + extend OnboardingState"
```

### Task M2.1.2: Three-tier proactive layer documentation + helper

**Files:**
- Modify: `mur-common/src/agent.rs:615-646` (ProactiveConfig region)
- Test: `mur-common/tests/companion_enums.rs`

The three layers in the spec are *already* expressible:
- Layer 1 = `companion.enabled` (warm voice; default `true` after onboarding)
- Layer 2 = `companion.rhythm.enabled` (behavior collection; default `false`)
- Layer 3 = `companion.proactive.enabled` (proactive sends; default `false`)

This task introduces a `ProactiveTiers` newtype + helper to reify the three-layer toggle as a single value the GUI can show/edit — without changing on-disk schema.

- [x] **Step 1: Write failing test**

```rust
// mur-common/tests/companion_enums.rs (append)
use mur_common::agent::{CompanionConfig, ProactiveTiers};

#[test]
fn proactive_tiers_helper() {
    let mut c = CompanionConfig::default();
    c.enabled = true;
    let t = ProactiveTiers::from_config(&c);
    assert_eq!(t, ProactiveTiers::WarmOnly);

    c.rhythm.enabled = true;
    let t = ProactiveTiers::from_config(&c);
    assert_eq!(t, ProactiveTiers::WarmAndBehavior);

    c.proactive.enabled = true;
    let t = ProactiveTiers::from_config(&c);
    assert_eq!(t, ProactiveTiers::All);
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-common proactive_tiers_helper`
Expected: `unresolved import 'mur_common::agent::ProactiveTiers'`.

- [x] **Step 3: Add ProactiveTiers**

Add at end of `mur-common/src/agent.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProactiveTiers {
    Off,
    WarmOnly,
    WarmAndBehavior,
    All,
}

impl ProactiveTiers {
    pub fn from_config(c: &CompanionConfig) -> Self {
        match (c.enabled, c.rhythm.enabled, c.proactive.enabled) {
            (false, _, _) => Self::Off,
            (true, false, false) => Self::WarmOnly,
            (true, true, false) => Self::WarmAndBehavior,
            (true, _, true) => Self::All,
        }
    }

    pub fn apply(&self, c: &mut CompanionConfig) {
        match self {
            Self::Off => {
                c.enabled = false;
                c.rhythm.enabled = false;
                c.proactive.enabled = false;
            }
            Self::WarmOnly => {
                c.enabled = true;
                c.rhythm.enabled = false;
                c.proactive.enabled = false;
            }
            Self::WarmAndBehavior => {
                c.enabled = true;
                c.rhythm.enabled = true;
                c.proactive.enabled = false;
            }
            Self::All => {
                c.enabled = true;
                c.rhythm.enabled = true;
                c.proactive.enabled = true;
            }
        }
    }
}
```

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-common proactive_tiers_helper`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/tests/companion_enums.rs
git commit -m "M2.1.2: ProactiveTiers helper for three-layer toggle"
```

### Task M2.1.3: Profile YAML round-trip including new fields

**Files:**
- Test: `mur-common/tests/companion_enums.rs`

- [x] **Step 1: Write failing round-trip test**

```rust
// mur-common/tests/companion_enums.rs (append)
use mur_common::agent::{AgentProfile, CompanionConfig, FirstMemory, OnboardingState};

#[test]
fn agent_profile_with_first_memory_roundtrip() {
    let mut p = AgentProfile::default();
    p.companion.enabled = true;
    p.companion.onboarding = OnboardingState {
        completed_at: Some(chrono::Utc::now()),
        version: 1,
        agent_display_name: Some("Mochi".into()),
        first_memory: Some(FirstMemory {
            text: "first day in Taipei".into(),
            established_at: chrono::Utc::now(),
        }),
    };
    let yaml = serde_yaml_ng::to_string(&p).unwrap();
    assert!(yaml.contains("first_memory:"));
    assert!(yaml.contains("agent_display_name: Mochi"));
    let back: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(back.companion.onboarding.agent_display_name.as_deref(), Some("Mochi"));
    assert!(back.companion.onboarding.first_memory.is_some());
}
```

- [x] **Step 2: Run + verify pass (no code change needed — schema landed in M2.1.1)**

Run: `cargo test -p mur-common agent_profile_with_first_memory_roundtrip`
Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add mur-common/tests/companion_enums.rs
git commit -m "M2.1.3: AgentProfile round-trip covers OnboardingState extension"
```

---

## Milestone M2.2 — Companion picker `{{FIRST_MEMORY}}` template var + day-3 morning_greeting

### Task M2.2.1: Wire first_memory through VoiceInput

**Files:**
- Modify: `mur-agent-runtime/src/companion/voice.rs`
- Test: `mur-agent-runtime/tests/onboarding_picker_first_memory.rs` (new)

- [x] **Step 1: Write failing test**

```rust
// mur-agent-runtime/tests/onboarding_picker_first_memory.rs (new file)
use mur_agent_runtime::companion::voice::{compose, VoiceInput};
use mur_common::companion::Relationship;

#[test]
fn voice_template_substitutes_first_memory() {
    // Use a synthetic locale that we'll teach the resolver about; for now,
    // assert that {{FIRST_MEMORY}} placeholder is replaced verbatim if present.
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: Some("Sunday in Taipei"),
    };
    // We won't assert on the embedded template content (that's M2.2.2);
    // instead force a known string via a unit test that exercises the
    // substitution helper directly.
    let out = mur_agent_runtime::companion::voice::substitute_for_test(
        "User mentioned: {{FIRST_MEMORY}}",
        &input,
    );
    assert_eq!(out, "User mentioned: Sunday in Taipei");
}

#[test]
fn voice_template_first_memory_none_collapses() {
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: None,
    };
    let out = mur_agent_runtime::companion::voice::substitute_for_test(
        "Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}",
        &input,
    );
    assert_eq!(out, "Hi David.");
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-agent-runtime --test onboarding_picker_first_memory`
Expected: `field 'first_memory' on VoiceInput not found` and `function 'substitute_for_test' not found`.

- [x] **Step 3: Extend VoiceInput + substitution**

In `mur-agent-runtime/src/companion/voice.rs`, modify `VoiceInput` to add `first_memory: Option<&'a str>`. Inside the existing substitution path (the function around line 74 that does `tpl.replace("{{NAME_FOR_USER}}", ...)`), add:

```rust
let mut tpl = tpl.replace("{{NAME_FOR_USER}}", input.name_for_user);
match input.first_memory {
    Some(fm) => {
        tpl = tpl.replace("{{FIRST_MEMORY}}", fm);
        // Optional paragraph form: " (we ___)" — empty when fm is None.
        tpl = tpl.replace("{{FIRST_MEMORY_PARAGRAPH}}", &format!(" {}", fm));
    }
    None => {
        tpl = tpl.replace("{{FIRST_MEMORY}}", "");
        tpl = tpl.replace("{{FIRST_MEMORY_PARAGRAPH}}", "");
    }
}
tpl
```

Add a thin `pub fn substitute_for_test(tpl: &str, input: &VoiceInput) -> String` exposed under `#[cfg(test)]` or behind a `pub(crate)` for tests. Update the existing call sites (they all take `VoiceInput` by struct literal — pass `first_memory: None` for now).

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-agent-runtime --test onboarding_picker_first_memory`
Expected: PASS (both tests).

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/companion/voice.rs mur-agent-runtime/tests/onboarding_picker_first_memory.rs
git commit -m "M2.2.1: VoiceInput.first_memory + {{FIRST_MEMORY}} substitution"
```

### Task M2.2.2: Day-3+ morning_greeting templates that reference first_memory

**Files:**
- Modify: `mur-common/src/companion/content/morning_greeting.en-US.yaml`
- Modify: `mur-common/src/companion/content/morning_greeting.zh-TW.yaml`
- Test: `mur-common/tests/companion_content.rs` (new)

- [x] **Step 1: Inspect current morning_greeting seed content**

Run: `cat mur-common/src/companion/content/morning_greeting.en-US.yaml`
Expected: existing list of templates without any `{{FIRST_MEMORY}}` reference.

- [x] **Step 2: Write failing test**

```rust
// mur-common/tests/companion_content.rs (new file)
use mur_common::companion::content_seed::{MORNING_GREETING_EN_US, MORNING_GREETING_ZH_TW};

#[test]
fn morning_greeting_has_first_memory_template_en() {
    assert!(MORNING_GREETING_EN_US.contains("{{FIRST_MEMORY}}"),
        "expected at least one template referencing {{{{FIRST_MEMORY}}}}");
}

#[test]
fn morning_greeting_has_first_memory_template_zh_tw() {
    assert!(MORNING_GREETING_ZH_TW.contains("{{FIRST_MEMORY}}"),
        "expected at least one template referencing {{{{FIRST_MEMORY}}}}");
}
```

- [x] **Step 3: Run, confirm fail**

Run: `cargo test -p mur-common --test companion_content`
Expected: both fail.

- [x] **Step 4: Append new templates**

Append to `mur-common/src/companion/content/morning_greeting.en-US.yaml`:

```yaml
- id: mg_first_memory_warm_v1
  min_relationship_age_days: 3
  body: |
    morning — still thinking about {{FIRST_MEMORY}}. how's today shaping up?
  cooldown_hours: 168

- id: mg_first_memory_followup_v1
  min_relationship_age_days: 7
  body: |
    hey {{NAME_FOR_USER}} — remember when you told me {{FIRST_MEMORY}}? what's that turned into?
  cooldown_hours: 240
```

Append to `mur-common/src/companion/content/morning_greeting.zh-TW.yaml`:

```yaml
- id: mg_first_memory_warm_v1_zh
  min_relationship_age_days: 3
  body: |
    早上好——我還記得你說的「{{FIRST_MEMORY}}」。今天還順嗎？
  cooldown_hours: 168

- id: mg_first_memory_followup_v1_zh
  min_relationship_age_days: 7
  body: |
    嘿 {{NAME_FOR_USER}}——你之前提到「{{FIRST_MEMORY}}」，後來有什麼進展嗎？
  cooldown_hours: 240
```

- [x] **Step 5: Run, confirm pass**

Run: `cargo test -p mur-common --test companion_content`
Expected: both PASS.

- [x] **Step 6: Commit**

```bash
git add mur-common/src/companion/content/morning_greeting.*.yaml mur-common/tests/companion_content.rs
git commit -m "M2.2.2: morning_greeting day-3+ templates reference {{FIRST_MEMORY}}"
```

### Task M2.2.3: Outbox loads first_memory from relationship.json on tick

**Files:**
- Modify: `mur-agent-runtime/src/companion/outbox.rs`
- Test: `mur-agent-runtime/tests/onboarding_morning_greeting_72h.rs` (new)

- [x] **Step 1: Write failing acceptance test (MockClock 72h)**

```rust
// mur-agent-runtime/tests/onboarding_morning_greeting_72h.rs (new file)
use mur_agent_runtime::companion::{
    clock::MockClock, outbox::Outbox, situations::Situation,
};
use mur_common::companion::Relationship;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn morning_greeting_after_72h_references_first_memory() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    std::fs::create_dir_all(agent_dir.join("companion/inbox")).unwrap();

    // Seed relationship.json with onboarded_at = T0 - 72h, plus a first_memory.
    let onboarded = chrono::Utc::now() - chrono::Duration::hours(72);
    let rel = serde_json::json!({
        "version": 1,
        "name_for_user": "David",
        "relationship": "friend",
        "locale": "en-US",
        "formality": "casual",
        "extra_instructions": "",
        "onboarded_at": onboarded,
        "first_memory": { "text": "Sunday in Taipei", "established_at": onboarded },
    });
    std::fs::write(
        agent_dir.join("companion/relationship.json"),
        serde_json::to_string_pretty(&rel).unwrap(),
    ).unwrap();

    let clock = Arc::new(MockClock::new(chrono::Utc::now()));
    let outbox = Outbox::test_new(&agent_dir, clock.clone(), Relationship::Friend, "en-US").await;
    // Force pick a morning_greeting situation.
    let result = outbox.tick_with_situation(Situation::MorningGreeting).await.unwrap();
    let body = result.expect("outbox produced a message").body;
    assert!(body.contains("Sunday in Taipei"),
        "expected morning_greeting body to reference first_memory, got: {body}");
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-agent-runtime --test onboarding_morning_greeting_72h`
Expected: fail — `tick_with_situation` not found, or body lacks first_memory.

- [x] **Step 3: Implement first_memory load + threading**

In `mur-agent-runtime/src/companion/outbox.rs`:

1. In the relationship-load helper (search for `relationship.json` parsing), pull the `first_memory.text` field if present.
2. In the place that builds `VoiceInput` for the picked template, set `first_memory: rel.first_memory.as_deref()`.
3. Add a `#[cfg(test)] pub fn tick_with_situation(&self, s: Situation) -> ...` that bypasses the situation picker and forces a known situation, returning the produced message body for assertions. (Keep the production tick path unchanged.)

If a helper already exists (e.g. `Outbox::test_new`), reuse and extend; otherwise add minimal test-only constructors that mirror the production wiring but accept a pre-built `Notifier::Capturing` (in-memory).

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-agent-runtime --test onboarding_morning_greeting_72h`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/companion/outbox.rs mur-agent-runtime/tests/onboarding_morning_greeting_72h.rs
git commit -m "M2.2.3: outbox loads first_memory from relationship.json on tick"
```

---

## Milestone M2.3 — CLI 5-step wizard (`mur agent companion init`)

### Task M2.3.1: Extend Answers schema with the two new fields

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/init.rs:18-28` (Answers struct)

- [x] **Step 1: Write failing test**

```rust
// mur-core/tests/companion_init_5step.rs (new file)
use std::path::Path;
use tempfile::TempDir;

#[test]
fn answers_yaml_supports_first_memory_and_display_name() {
    let yaml = r#"
locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
"#;
    // The Answers struct is private; use a shim that loads + writes profile.
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path();
    std::env::set_var("MUR_HOME", mur_home);
    // Pre-create agent.
    let agent_dir = mur_home.join("agents/test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: test\nidentity:\n  pubkey: z000\n  owner: u\n  algorithm: ed25519\n  key_version: 1\n",
    ).unwrap();
    let answers_path = tmp.path().join("answers.yaml");
    std::fs::write(&answers_path, yaml).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        mur_core::cmd::agent_companion::init::run("test", Some(answers_path), false)
            .await
            .unwrap();
    });

    // Read back.
    let pf: serde_yaml_ng::Value = serde_yaml_ng::from_str(
        &std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap()
    ).unwrap();
    assert_eq!(pf["companion"]["onboarding"]["agent_display_name"].as_str(), Some("Mochi"));
    assert!(pf["companion"]["onboarding"]["first_memory"]["text"].is_string());
    let rel: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(agent_dir.join("companion/relationship.json")).unwrap()
    ).unwrap();
    assert_eq!(rel["first_memory"]["text"].as_str(), Some("Sunday in Taipei"));
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-core --test companion_init_5step`
Expected: deserialize error — unknown field `agent_display_name` on `Answers`.

- [x] **Step 3: Extend Answers + run() schema-write paths**

Modify `mur-core/src/cmd/agent_companion/init.rs`:

```rust
#[derive(Debug, Deserialize)]
struct Answers {
    locale: String,
    name_for_user: String,
    relationship: Relationship,
    #[serde(default)]
    formality: Option<Formality>,
    #[serde(default)]
    extra_instructions: Option<String>,
    #[serde(default)]
    agent_display_name: Option<String>,
    #[serde(default)]
    first_memory: Option<String>,
    #[serde(default)]
    proactive_tier: Option<ProactiveTier>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProactiveTier { Off, WarmOnly, WarmAndBehavior, All }
```

Update `RelationshipFile` to include `first_memory: Option<&'a FirstMemory>` and the run() body to populate `OnboardingState::{agent_display_name, first_memory}` plus apply `ProactiveTier` via `ProactiveTiers::apply` (helper from M2.1.2).

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-core --test companion_init_5step`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent_companion/init.rs mur-core/tests/companion_init_5step.rs
git commit -m "M2.3.1: companion init Answers schema supports first_memory + display name + proactive tier"
```

### Task M2.3.2: Interactive 5-step wizard branches

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/init.rs:121-163` (`run_wizard`)

- [x] **Step 1: Inspect current 3-step wizard**

Run: `grep -n 'fn run_wizard\|Step ' mur-core/src/cmd/agent_companion/init.rs`
Expected: Step 1, Step 2, Step 3 markers in the existing `run_wizard()`.

- [x] **Step 2: Replace with 5 steps**

```rust
fn run_wizard() -> Result<Answers> {
    use std::time::Instant;
    let started = Instant::now();

    // Step 1 — name your agent (display name)
    let agent_display_name: String = Input::new()
        .with_prompt("What should this agent be called? (display name)")
        .interact_text()
        .context("read agent display name")?;

    // Step 2 — voice (we don't pick the voice file here; we just record
    //          intent. The GUI Voice tab + M1 voice manager handles the
    //          actual model download. Skip is the default.)
    let voice_choices = ["Skip — enable voice later in Settings", "Enable voice (opens picker)"];
    let v_idx = Select::new()
        .with_prompt("Voice (we never send audio off your machine)")
        .items(&voice_choices)
        .default(0)
        .interact()
        .context("voice choice")?;
    if v_idx == 1 {
        println!("(open Settings → Voice in the GUI to pick a voice; the CLI just records consent)");
    }

    // Step 3 — relationship + name_for_user
    let locale: String = Input::new()
        .with_prompt("Language (BCP-47, e.g. zh-TW)")
        .default(default_locale())
        .interact_text()
        .context("read locale")?;
    let name_for_user: String = Input::new()
        .with_prompt("What should I call you?")
        .interact_text()
        .context("read name")?;
    let choices = ["Friend", "Coach", "Accountability buddy", "Mentor"];
    let idx = Select::new()
        .with_prompt("How should this agent relate to you?")
        .items(&choices).default(0).interact().context("select relationship")?;
    let relationship = match idx {
        0 => Relationship::Friend,
        1 => Relationship::Coach,
        2 => Relationship::AccountabilityBuddy,
        _ => Relationship::Mentor,
    };

    // Step 4 — first memory
    let first_memory: String = Input::new()
        .with_prompt("Share one fact about you, in a sentence")
        .with_initial_text("")
        .allow_empty(true)
        .interact_text()
        .context("read first_memory")?;
    let first_memory = if first_memory.trim().is_empty() { None } else { Some(first_memory) };

    // Step 5 — three-tier proactive
    let tier_choices = [
        "Warm voice only (recommended)",
        "Warm voice + collect rhythm (no proactive sends)",
        "All — including occasional proactive check-ins",
    ];
    let t_idx = Select::new()
        .with_prompt("How should I behave?")
        .items(&tier_choices).default(0).interact().context("select proactive tier")?;
    let proactive_tier = Some(match t_idx {
        0 => ProactiveTier::WarmOnly,
        1 => ProactiveTier::WarmAndBehavior,
        _ => ProactiveTier::All,
    });

    if let Some(example) = example_greeting(&locale, &relationship) {
        println!("{example}");
    }
    println!("Elapsed {}s.", started.elapsed().as_secs());

    Ok(Answers {
        locale,
        name_for_user,
        relationship,
        formality: Some(Formality::Casual),
        extra_instructions: Some(String::new()),
        agent_display_name: Some(agent_display_name),
        first_memory,
        proactive_tier,
    })
}
```

- [x] **Step 3: Test compile**

Run: `cargo build -p mur-core --bin mur`
Expected: clean build.

- [x] **Step 4: Manual smoke test (interactive — optional)**

Run: `MUR_HOME=$(mktemp -d) cargo run -p mur-core -- agent create test && cargo run -p mur-core -- agent companion init test`
Expected: 5 prompts, fewer than 90 s to complete.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent_companion/init.rs
git commit -m "M2.3.2: interactive 5-step wizard with first memory + proactive tier"
```

### Task M2.3.3: `companion preview` hydrates first_memory

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/preview.rs`
- Test: `mur-core/tests/companion_preview_first_memory.rs` (new)

- [x] **Step 1: Locate current preview path**

Run: `grep -n 'pub async fn run\|first_memory\|VoiceInput' mur-core/src/cmd/agent_companion/preview.rs`
Expected: shows the existing run() but no first_memory threading.

- [x] **Step 2: Write failing test**

```rust
// mur-core/tests/companion_preview_first_memory.rs (new)
use std::process::Command;
use tempfile::TempDir;

#[test]
fn preview_morning_greeting_includes_first_memory() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path();
    let answers = mur_home.join("answers.yaml");
    std::fs::write(&answers, r#"
locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
"#).unwrap();
    let mur = env!("CARGO_BIN_EXE_mur");
    Command::new(mur)
        .env("MUR_HOME", mur_home)
        .args(["agent", "create", "test"]).status().unwrap();
    Command::new(mur)
        .env("MUR_HOME", mur_home)
        .args(["agent", "companion", "init", "test", "--answers", answers.to_str().unwrap()])
        .status().unwrap();
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .args(["agent", "companion", "preview", "test", "--situation", "morning_greeting", "--no-llm"])
        .output().unwrap();
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("Sunday in Taipei"),
        "preview output should include first_memory; got: {body}");
}
```

- [x] **Step 3: Run, confirm fail**

Run: `cargo test -p mur-core --test companion_preview_first_memory`
Expected: missing first_memory in preview body.

- [x] **Step 4: Thread first_memory through preview**

In `mur-core/src/cmd/agent_companion/preview.rs`, after loading `relationship.json`, parse the `first_memory.text` field if present and pass it into the `VoiceInput { first_memory: ..., .. }` struct. (Mirror the runtime outbox logic from M2.2.3.)

- [x] **Step 5: Run, confirm pass**

Run: `cargo test -p mur-core --test companion_preview_first_memory`
Expected: PASS.

- [x] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent_companion/preview.rs mur-core/tests/companion_preview_first_memory.rs
git commit -m "M2.3.3: companion preview includes first_memory in rendered body"
```

---

## Milestone M2.4 — Tauri commands for the GUI wizard

### Task M2.4.1: `companion_onboarding_status` command

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/commands.rs`
- Test: `mur-agent-gui/src-tauri/tests/onboarding_commands.rs` (new)

- [x] **Step 1: Write failing test**

```rust
// mur-agent-gui/src-tauri/tests/onboarding_commands.rs (new)
use tempfile::TempDir;

#[tokio::test]
async fn onboarding_status_returns_pending_for_new_agent() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("MUR_HOME", tmp.path());
    // Pre-create agent.
    let agent_dir = tmp.path().join("agents/test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: test\nidentity:\n  pubkey: z\n  owner: u\n  algorithm: ed25519\n  key_version: 1\n",
    ).unwrap();

    let s = mur_agent_gui::commands::companion_onboarding_status_impl("test").await.unwrap();
    assert!(s.completed_at.is_none(), "fresh agent: not onboarded");
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-agent-gui --test onboarding_commands`
Expected: function not found.

- [x] **Step 3: Add command surface**

In `mur-agent-gui/src-tauri/src/commands.rs`:

```rust
#[derive(serde::Serialize)]
pub struct OnboardingStatus {
    pub completed_at: Option<String>,
    pub agent_display_name: Option<String>,
    pub first_memory_text: Option<String>,
    pub proactive_tier: String,
}

pub async fn companion_onboarding_status_impl(agent: &str) -> anyhow::Result<OnboardingStatus> {
    let dir = mur_core::agent_admin::lifecycle::agent_home_for(agent)?;
    let p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(&tokio::fs::read_to_string(dir.join("profile.yaml")).await?)?;
    Ok(OnboardingStatus {
        completed_at: p.companion.onboarding.completed_at.map(|t| t.to_rfc3339()),
        agent_display_name: p.companion.onboarding.agent_display_name.clone(),
        first_memory_text: p.companion.onboarding.first_memory.as_ref().map(|f| f.text.clone()),
        proactive_tier: format!("{:?}", mur_common::agent::ProactiveTiers::from_config(&p.companion))
            .to_lowercase(),
    })
}

#[tauri::command]
pub async fn companion_onboarding_status(agent: String) -> Result<OnboardingStatus, String> {
    companion_onboarding_status_impl(&agent).await.map_err(|e| e.to_string())
}
```

Register in `mur-agent-gui/src-tauri/src/main.rs` `tauri::generate_handler!` list.

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-agent-gui --test onboarding_commands`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-gui/src-tauri/src/commands.rs mur-agent-gui/src-tauri/src/main.rs mur-agent-gui/src-tauri/tests/onboarding_commands.rs
git commit -m "M2.4.1: companion_onboarding_status Tauri command"
```

### Task M2.4.2: `companion_onboarding_submit` command (writes Answers + invokes init.run())

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/commands.rs`
- Test: append to `mur-agent-gui/src-tauri/tests/onboarding_commands.rs`

- [x] **Step 1: Write failing test**

```rust
// append to onboarding_commands.rs
#[tokio::test]
async fn onboarding_submit_persists_first_memory() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("MUR_HOME", tmp.path());
    let agent_dir = tmp.path().join("agents/t2");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: t2\nidentity:\n  pubkey: z\n  owner: u\n  algorithm: ed25519\n  key_version: 1\n",
    ).unwrap();

    let payload = mur_agent_gui::commands::OnboardingSubmit {
        agent_display_name: "Mochi".into(),
        locale: "en-US".into(),
        name_for_user: "David".into(),
        relationship: "friend".into(),
        first_memory: Some("Sunday in Taipei".into()),
        proactive_tier: "warm_only".into(),
    };
    mur_agent_gui::commands::companion_onboarding_submit_impl("t2", payload).await.unwrap();

    let s = mur_agent_gui::commands::companion_onboarding_status_impl("t2").await.unwrap();
    assert_eq!(s.first_memory_text.as_deref(), Some("Sunday in Taipei"));
    assert_eq!(s.proactive_tier, "warmonly");
}
```

- [x] **Step 2: Run, confirm fail**

Run: `cargo test -p mur-agent-gui --test onboarding_commands onboarding_submit_persists_first_memory`
Expected: type/function not found.

- [x] **Step 3: Add submit command**

```rust
// in commands.rs
#[derive(serde::Deserialize)]
pub struct OnboardingSubmit {
    pub agent_display_name: String,
    pub locale: String,
    pub name_for_user: String,
    pub relationship: String,        // "friend" | "coach" | "accountability_buddy" | "mentor"
    pub first_memory: Option<String>,
    pub proactive_tier: String,      // "off" | "warm_only" | "warm_and_behavior" | "all"
}

pub async fn companion_onboarding_submit_impl(
    agent: &str,
    p: OnboardingSubmit,
) -> anyhow::Result<()> {
    use std::io::Write;
    let tmp = tempfile::NamedTempFile::new()?;
    let yaml = serde_yaml_ng::to_string(&serde_json::json!({
        "locale": p.locale,
        "name_for_user": p.name_for_user,
        "agent_display_name": p.agent_display_name,
        "relationship": p.relationship,
        "formality": "casual",
        "extra_instructions": "",
        "first_memory": p.first_memory,
        "proactive_tier": p.proactive_tier,
    }))?;
    tmp.as_file().write_all(yaml.as_bytes())?;
    mur_core::cmd::agent_companion::init::run(agent, Some(tmp.path().to_path_buf()), false).await
}

#[tauri::command]
pub async fn companion_onboarding_submit(agent: String, payload: OnboardingSubmit) -> Result<(), String> {
    companion_onboarding_submit_impl(&agent, payload).await.map_err(|e| e.to_string())
}
```

Register in `tauri::generate_handler!`.

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-agent-gui --test onboarding_commands`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-gui/src-tauri/src/commands.rs mur-agent-gui/src-tauri/src/main.rs mur-agent-gui/src-tauri/tests/onboarding_commands.rs
git commit -m "M2.4.2: companion_onboarding_submit Tauri command"
```

### Task M2.4.3: `companion_onboarding_skip` command

A convenience that records `completed_at = now`, leaves all Onboarding fields `None`, and applies `ProactiveTiers::WarmOnly`. The wizard's "Skip everything" branch needs this.

**Files:**
- Modify: `mur-agent-gui/src-tauri/src/commands.rs`
- Test: append to `mur-agent-gui/src-tauri/tests/onboarding_commands.rs`

- [x] **Step 1: Write failing test**

```rust
// append to onboarding_commands.rs
#[tokio::test]
async fn onboarding_skip_marks_completed_with_warm_only() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("MUR_HOME", tmp.path());
    let agent_dir = tmp.path().join("agents/t3");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: t3\nidentity:\n  pubkey: z\n  owner: u\n  algorithm: ed25519\n  key_version: 1\n",
    ).unwrap();
    mur_agent_gui::commands::companion_onboarding_skip_impl("t3").await.unwrap();
    let s = mur_agent_gui::commands::companion_onboarding_status_impl("t3").await.unwrap();
    assert!(s.completed_at.is_some());
    assert!(s.first_memory_text.is_none());
    assert_eq!(s.proactive_tier, "warmonly");
}
```

- [x] **Step 2: Add skip command**

```rust
pub async fn companion_onboarding_skip_impl(agent: &str) -> anyhow::Result<()> {
    let dir = mur_core::agent_admin::lifecycle::agent_home_for(agent)?;
    let pf_path = dir.join("profile.yaml");
    let mut p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(&tokio::fs::read_to_string(&pf_path).await?)?;
    mur_common::agent::ProactiveTiers::WarmOnly.apply(&mut p.companion);
    p.companion.onboarding.completed_at = Some(chrono::Utc::now());
    p.companion.onboarding.version = 1;
    let yaml = serde_yaml_ng::to_string(&p)?;
    tokio::fs::write(&pf_path, yaml).await?;
    Ok(())
}

#[tauri::command]
pub async fn companion_onboarding_skip(agent: String) -> Result<(), String> {
    companion_onboarding_skip_impl(&agent).await.map_err(|e| e.to_string())
}
```

- [x] **Step 3: Run, confirm pass**

Run: `cargo test -p mur-agent-gui --test onboarding_commands`
Expected: PASS.

- [x] **Step 4: Commit**

```bash
git add mur-agent-gui/src-tauri/src/commands.rs mur-agent-gui/src-tauri/src/main.rs mur-agent-gui/src-tauri/tests/onboarding_commands.rs
git commit -m "M2.4.3: companion_onboarding_skip Tauri command"
```

---

## Milestone M2.5 — React 5-step wizard component

### Task M2.5.1: API + types layer

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/types.ts`
- Create: `mur-agent-gui/ui/src/onboarding/api.ts`

- [x] **Step 1: Create types.ts**

```typescript
export type Relationship = "friend" | "coach" | "accountability_buddy" | "mentor";
export type ProactiveTier = "off" | "warm_only" | "warm_and_behavior" | "all";

export interface OnboardingStatus {
  completed_at: string | null;
  agent_display_name: string | null;
  first_memory_text: string | null;
  proactive_tier: string;
}

export interface OnboardingSubmit {
  agent_display_name: string;
  locale: string;
  name_for_user: string;
  relationship: Relationship;
  first_memory: string | null;
  proactive_tier: ProactiveTier;
}
```

- [x] **Step 2: Create api.ts**

```typescript
import { invoke } from "@tauri-apps/api/core";
import type { OnboardingStatus, OnboardingSubmit } from "./types";

export const getOnboardingStatus = (agent: string) =>
  invoke<OnboardingStatus>("companion_onboarding_status", { agent });

export const submitOnboarding = (agent: string, payload: OnboardingSubmit) =>
  invoke<void>("companion_onboarding_submit", { agent, payload });

export const skipOnboarding = (agent: string) =>
  invoke<void>("companion_onboarding_skip", { agent });
```

- [x] **Step 3: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/{types,api}.ts
git commit -m "M2.5.1: onboarding API + types"
```

### Task M2.5.2: Step1AgentName.tsx (display name)

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/Step1AgentName.tsx`

- [x] **Step 1: Create the component**

```tsx
import { useState } from "react";

interface Props {
  initial?: string;
  onNext: (displayName: string) => void;
  onSkip: () => void;
}

export default function Step1AgentName({ initial, onNext, onSkip }: Props) {
  const [v, setV] = useState(initial ?? "");
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Name your agent</h2>
      <p className="text-sm opacity-75">
        This is what we'll call your agent in the UI. You can change it later.
      </p>
      <input
        autoFocus
        className="w-full rounded border px-3 py-2"
        style={{ borderColor: "var(--color-border)", background: "var(--color-bg-secondary)" }}
        value={v}
        placeholder="Mochi"
        onChange={(e) => setV(e.target.value)}
      />
      <div className="flex gap-2">
        <button
          className="rounded px-4 py-2 text-sm"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
          disabled={!v.trim()}
          onClick={() => onNext(v.trim())}
        >
          Next
        </button>
        <button className="rounded px-4 py-2 text-sm opacity-75" onClick={onSkip}>
          Skip onboarding
        </button>
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/Step1AgentName.tsx
git commit -m "M2.5.2: Step1AgentName"
```

### Task M2.5.3: Step2VoicePick.tsx (Skip / Open Settings)

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/Step2VoicePick.tsx`

- [x] **Step 1: Create the component**

```tsx
interface Props {
  onSkip: () => void;
  onEnable: () => void; // navigates to Settings → Voice
  onBack: () => void;
}

export default function Step2VoicePick({ onSkip, onEnable, onBack }: Props) {
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Voice (optional)</h2>
      <p className="text-sm opacity-75">
        Voice setup is opt-in and downloads ~190 MB on first use.
        We never send audio off your machine.
      </p>
      <div className="rounded border p-4 text-xs opacity-75"
           style={{ borderColor: "var(--color-border)" }}>
        You can enable voice anytime from <strong>Settings → Voice</strong>.
      </div>
      <div className="flex gap-2">
        <button className="rounded px-4 py-2 text-sm opacity-75" onClick={onBack}>
          Back
        </button>
        <button
          className="rounded px-4 py-2 text-sm"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
          onClick={onSkip}
        >
          Skip — enable later
        </button>
        <button className="rounded px-4 py-2 text-sm border"
                style={{ borderColor: "var(--color-border)" }}
                onClick={onEnable}>
          Open Settings → Voice
        </button>
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/Step2VoicePick.tsx
git commit -m "M2.5.3: Step2VoicePick — Skip / Open Settings"
```

### Task M2.5.4: Step3Relationship.tsx

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/Step3Relationship.tsx`

- [x] **Step 1: Create**

```tsx
import { useState } from "react";
import type { Relationship } from "./types";

const opts: { id: Relationship; label: string; example: string }[] = [
  { id: "friend",                label: "Friend",                example: "\"Hey — how's it going?\"" },
  { id: "coach",                 label: "Coach",                 example: "\"Hey. What's the goal?\"" },
  { id: "accountability_buddy",  label: "Accountability buddy", example: "\"Hi, what's on your plate today?\"" },
  { id: "mentor",                label: "Mentor",                example: "\"Hi. What's been on your mind?\"" },
];

interface Props {
  initialName?: string;
  initialRelationship?: Relationship;
  onNext: (nameForUser: string, relationship: Relationship) => void;
  onBack: () => void;
}

export default function Step3Relationship({ initialName, initialRelationship, onNext, onBack }: Props) {
  const [name, setName] = useState(initialName ?? "");
  const [r, setR] = useState<Relationship>(initialRelationship ?? "friend");
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">How should I relate to you?</h2>
      <input
        className="w-full rounded border px-3 py-2"
        placeholder="What should I call you?"
        value={name}
        onChange={(e) => setName(e.target.value)}
      />
      <div className="grid grid-cols-2 gap-2">
        {opts.map((o) => (
          <button
            key={o.id}
            className="rounded border p-3 text-left text-sm"
            style={{
              borderColor: r === o.id ? "var(--color-accent)" : "var(--color-border)",
              background: r === o.id ? "var(--color-accent)" : "transparent",
              color: r === o.id ? "var(--color-accent-fg)" : "var(--color-fg)",
            }}
            onClick={() => setR(o.id)}
          >
            <div className="font-medium">{o.label}</div>
            <div className="opacity-75 text-xs mt-1">{o.example}</div>
          </button>
        ))}
      </div>
      <div className="flex gap-2">
        <button className="rounded px-4 py-2 text-sm opacity-75" onClick={onBack}>Back</button>
        <button
          className="rounded px-4 py-2 text-sm"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
          disabled={!name.trim()}
          onClick={() => onNext(name.trim(), r)}
        >
          Next
        </button>
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/Step3Relationship.tsx
git commit -m "M2.5.4: Step3Relationship"
```

### Task M2.5.5: Step4FirstMemory.tsx (200-char counter)

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/Step4FirstMemory.tsx`

- [x] **Step 1: Create**

```tsx
import { useState } from "react";

const MAX = 200;

interface Props {
  initial?: string;
  onNext: (memory: string | null) => void;
  onBack: () => void;
}

export default function Step4FirstMemory({ initial, onNext, onBack }: Props) {
  const [v, setV] = useState(initial ?? "");
  const remaining = MAX - v.length;
  const trimmed = v.trim();
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Share one fact about you</h2>
      <p className="text-sm opacity-75">
        Anything — a hobby, a goal, a favourite place. I'll remember it and bring it up later.
        This is the single highest-impact thing in this wizard.
      </p>
      <textarea
        className="w-full h-24 rounded border px-3 py-2"
        style={{ borderColor: "var(--color-border)", background: "var(--color-bg-secondary)" }}
        maxLength={MAX}
        placeholder="e.g. 'I'm learning Rust.' or 'My cat's name is Mochi.'"
        value={v}
        onChange={(e) => setV(e.target.value)}
      />
      <div className="text-xs opacity-75">{remaining} characters left</div>
      <div className="flex gap-2">
        <button className="rounded px-4 py-2 text-sm opacity-75" onClick={onBack}>Back</button>
        <button
          className="rounded px-4 py-2 text-sm"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
          onClick={() => onNext(trimmed.length > 0 ? trimmed : null)}
        >
          Next
        </button>
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/Step4FirstMemory.tsx
git commit -m "M2.5.5: Step4FirstMemory with 200-char cap"
```

### Task M2.5.6: Step5ProactiveTiers.tsx

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/Step5ProactiveTiers.tsx`

- [x] **Step 1: Create**

```tsx
import { useState } from "react";
import type { ProactiveTier } from "./types";

const tiers: { id: ProactiveTier; label: string; sub: string }[] = [
  { id: "warm_only", label: "Warm voice only (recommended)", sub: "Replies with a more relational tone. No proactive sends. No behavior collection." },
  { id: "warm_and_behavior", label: "Warm + behavior collection", sub: "Adds rhythm/usage learning. Still no proactive sends." },
  { id: "all", label: "All — including proactive check-ins", sub: "Daily-cap-bound, quiet-hours respected. You can pause any time." },
];

interface Props {
  initial?: ProactiveTier;
  onSubmit: (t: ProactiveTier) => void;
  onBack: () => void;
}

export default function Step5ProactiveTiers({ initial, onSubmit, onBack }: Props) {
  const [t, setT] = useState<ProactiveTier>(initial ?? "warm_only");
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">How should I behave?</h2>
      <p className="text-sm opacity-75">
        You can change this any time in Settings → Companion. The default is the gentlest option.
      </p>
      <div className="space-y-2">
        {tiers.map((o) => (
          <button
            key={o.id}
            className="w-full rounded border p-3 text-left text-sm"
            style={{
              borderColor: t === o.id ? "var(--color-accent)" : "var(--color-border)",
              background: t === o.id ? "var(--color-accent)" : "transparent",
              color: t === o.id ? "var(--color-accent-fg)" : "var(--color-fg)",
            }}
            onClick={() => setT(o.id)}
          >
            <div className="font-medium">{o.label}</div>
            <div className="opacity-75 text-xs mt-1">{o.sub}</div>
          </button>
        ))}
      </div>
      <div className="flex gap-2">
        <button className="rounded px-4 py-2 text-sm opacity-75" onClick={onBack}>Back</button>
        <button
          className="rounded px-4 py-2 text-sm"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
          onClick={() => onSubmit(t)}
        >
          Finish
        </button>
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/Step5ProactiveTiers.tsx
git commit -m "M2.5.6: Step5ProactiveTiers — three-layer toggle"
```

### Task M2.5.7: OnboardingWizard shell

**Files:**
- Create: `mur-agent-gui/ui/src/onboarding/OnboardingWizard.tsx`

- [x] **Step 1: Create the shell**

```tsx
import { useState } from "react";
import Step1AgentName from "./Step1AgentName";
import Step2VoicePick from "./Step2VoicePick";
import Step3Relationship from "./Step3Relationship";
import Step4FirstMemory from "./Step4FirstMemory";
import Step5ProactiveTiers from "./Step5ProactiveTiers";
import { skipOnboarding, submitOnboarding } from "./api";
import type { ProactiveTier, Relationship } from "./types";

const LOCALE_DEFAULT =
  (typeof navigator !== "undefined" && navigator.language) || "en-US";

interface Props {
  agent: string;
  onComplete: () => void;
  onOpenVoiceSettings: () => void;
}

export default function OnboardingWizard({ agent, onComplete, onOpenVoiceSettings }: Props) {
  const [step, setStep] = useState(1);
  const [agentDisplay, setAgentDisplay] = useState("");
  const [nameForUser, setNameForUser] = useState("");
  const [rel, setRel] = useState<Relationship>("friend");
  const [memory, setMemory] = useState<string | null>(null);

  const skipAll = async () => {
    await skipOnboarding(agent);
    onComplete();
  };

  const finish = async (tier: ProactiveTier) => {
    await submitOnboarding(agent, {
      agent_display_name: agentDisplay,
      locale: LOCALE_DEFAULT,
      name_for_user: nameForUser,
      relationship: rel,
      first_memory: memory,
      proactive_tier: tier,
    });
    onComplete();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center"
         style={{ background: "rgba(0,0,0,0.4)" }}>
      <div className="w-[480px] rounded p-6"
           style={{ background: "var(--color-bg)", color: "var(--color-fg)" }}>
        <div className="text-xs opacity-75 mb-2">Step {step} of 5</div>
        {step === 1 && <Step1AgentName initial={agentDisplay}
          onNext={(d) => { setAgentDisplay(d); setStep(2); }} onSkip={skipAll} />}
        {step === 2 && <Step2VoicePick
          onBack={() => setStep(1)} onSkip={() => setStep(3)}
          onEnable={() => { onOpenVoiceSettings(); setStep(3); }} />}
        {step === 3 && <Step3Relationship
          initialName={nameForUser} initialRelationship={rel}
          onBack={() => setStep(2)}
          onNext={(n, r) => { setNameForUser(n); setRel(r); setStep(4); }} />}
        {step === 4 && <Step4FirstMemory initial={memory ?? ""}
          onBack={() => setStep(3)} onNext={(m) => { setMemory(m); setStep(5); }} />}
        {step === 5 && <Step5ProactiveTiers
          onBack={() => setStep(4)} onSubmit={finish} />}
      </div>
    </div>
  );
}
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/onboarding/OnboardingWizard.tsx
git commit -m "M2.5.7: OnboardingWizard shell — 5-step navigation + Skip All"
```

---

## Milestone M2.6 — First-launch detection + App.tsx integration

### Task M2.6.1: getOnboardingStatus helper in lib/api

**Files:**
- Modify: `mur-agent-gui/ui/src/lib/api.ts`

- [x] **Step 1: Add helper**

```typescript
// append in lib/api.ts
import type { OnboardingStatus } from "../onboarding/types";

export const getOnboardingStatus = (agent: string) =>
  invoke<OnboardingStatus>("companion_onboarding_status", { agent });
```

- [x] **Step 2: Commit**

```bash
git add mur-agent-gui/ui/src/lib/api.ts
git commit -m "M2.6.1: lib/api re-exports getOnboardingStatus"
```

### Task M2.6.2: App.tsx renders OnboardingWizard on first launch

**Files:**
- Modify: `mur-agent-gui/ui/src/App.tsx`

- [x] **Step 1: Inspect current App.tsx**

Run: `wc -l mur-agent-gui/ui/src/App.tsx`
Expected: ~100 lines (matches current snapshot in repo).

- [x] **Step 2: Add wizard hook + conditional render**

Modify `App.tsx`:

1. Read agent name from a constant or query the first agent via existing `listAgents()` — for D2, hard-code `BUNDLED_AGENT` (the export bundle's agent name lives in a known location; reuse `getStatus()` which already runs).
2. Add `useState` for `onboardingDone`, default `null` (loading) → `boolean`.
3. `useEffect` calls `getOnboardingStatus(agent)` → set `onboardingDone = !!status.completed_at`.
4. When `onboardingDone === false`, render `<OnboardingWizard agent={agent} onComplete={...} onOpenVoiceSettings={...} />` overlaying the existing layout.

Code excerpt:

```tsx
import OnboardingWizard from "./onboarding/OnboardingWizard";
import { getOnboardingStatus } from "./lib/api";

// inside App():
const [onboarded, setOnboarded] = useState<boolean | null>(null);
const agent = "default"; // TODO: lift from `getStatus()` when multi-agent lands

useEffect(() => {
  getOnboardingStatus(agent)
    .then((s) => setOnboarded(!!s.completed_at))
    .catch(() => setOnboarded(true)); // tolerate fetch errors — never block UI
}, [agent]);

// in render:
{onboarded === false && (
  <OnboardingWizard
    agent={agent}
    onComplete={() => setOnboarded(true)}
    onOpenVoiceSettings={() => setTab("voice" as any /* M1's voice tab id */)}
  />
)}
```

- [x] **Step 3: Build + smoke (don't auto-launch full GUI in CI)**

Run: `cd mur-agent-gui/ui && npm run build`
Expected: type-check passes; build succeeds.

- [x] **Step 4: Commit**

```bash
git add mur-agent-gui/ui/src/App.tsx
git commit -m "M2.6.2: App.tsx renders OnboardingWizard on first launch"
```

### Task M2.6.3: Tauri-side test: first-launch detection

**Files:**
- Create: `mur-agent-gui/src-tauri/tests/onboarding_first_launch.rs`

- [x] **Step 1: Test**

```rust
use tempfile::TempDir;

#[tokio::test]
async fn first_launch_status_pending_then_completed_after_skip() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("MUR_HOME", tmp.path());
    let agent_dir = tmp.path().join("agents/default");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        "name: default\nidentity:\n  pubkey: z\n  owner: u\n  algorithm: ed25519\n  key_version: 1\n",
    ).unwrap();
    let s = mur_agent_gui::commands::companion_onboarding_status_impl("default").await.unwrap();
    assert!(s.completed_at.is_none(), "fresh agent: pending");

    mur_agent_gui::commands::companion_onboarding_skip_impl("default").await.unwrap();
    let s = mur_agent_gui::commands::companion_onboarding_status_impl("default").await.unwrap();
    assert!(s.completed_at.is_some(), "after skip: complete");
}
```

- [x] **Step 2: Run, confirm pass**

Run: `cargo test -p mur-agent-gui --test onboarding_first_launch`
Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add mur-agent-gui/src-tauri/tests/onboarding_first_launch.rs
git commit -m "M2.6.3: first-launch detection ↔ skip integration test"
```

---

## Milestone M2.7 — Character card export round-trips first_memory

### Task M2.7.1: Minimal `.murcard.yaml` schema with extensions.mur

**Files:**
- Create: `mur-core/src/character_card/schema.rs`
- Create: `mur-core/src/character_card/first_memory.rs`
- Create: `mur-core/src/character_card/serde_round_trip.rs`
- Create: `mur-core/src/character_card/mod.rs`
- Test: `mur-core/tests/character_card_round_trip.rs`

- [x] **Step 1: Add module to lib.rs**

```rust
// mur-core/src/lib.rs (append)
pub mod character_card;
```

- [x] **Step 2: Create the schema**

```rust
// character_card/mod.rs
pub mod first_memory;
pub mod schema;
pub mod serde_round_trip;
```

```rust
// character_card/first_memory.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstMemoryExt {
    pub text: String,
    pub established_at: chrono::DateTime<chrono::Utc>,
}
```

```rust
// character_card/schema.rs
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MurCard {
    pub spec: String,                         // "murcard_v1"
    pub spec_version: String,                 // "1.0"
    pub data: CardData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Extensions>,
    /// CCv3 passthrough: any unknown top-level key gets preserved verbatim.
    #[serde(flatten)]
    pub ccv3_passthrough: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardData {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default, rename = "mur", skip_serializing_if = "Option::is_none")]
    pub mur: Option<MurExt>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MurExt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_memory: Option<super::first_memory::FirstMemoryExt>,
}
```

- [x] **Step 3: Round-trip test**

```rust
// mur-core/tests/character_card_round_trip.rs
use mur_core::character_card::{first_memory::FirstMemoryExt, schema::*};

#[test]
fn round_trip_unknown_v3_field_preserved() {
    let yaml = r#"
spec: murcard_v1
spec_version: "1.0"
data:
  name: Mochi
extensions:
  mur:
    first_memory:
      text: "Sunday in Taipei"
      established_at: "2026-04-30T14:13:00Z"
unknown_v3_field:
  hello: world
"#;
    let c: MurCard = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(c.extensions.as_ref().unwrap().mur.as_ref().unwrap()
                 .first_memory.as_ref().unwrap().text, "Sunday in Taipei");
    let back = serde_yaml_ng::to_string(&c).unwrap();
    assert!(back.contains("unknown_v3_field"));
    assert!(back.contains("hello: world"));
}
```

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-core --test character_card_round_trip`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/character_card/ mur-core/src/lib.rs mur-core/tests/character_card_round_trip.rs
git commit -m "M2.7.1: minimal MurCard schema with extensions.mur + ccv3 passthrough"
```

### Task M2.7.2: Wire into `mur agent export --format card`

> Note: full `mur agent export --format card` is part of D4 (M4). For M2 we only ensure the *helper* is callable from inside the existing export pipeline so D4 won't have to retrofit.

**Files:**
- Modify: `mur-core/src/cmd/agent_export/card.rs` (create the file if missing — **Step 1 below verifies**)
- Test: `mur-core/tests/companion_first_memory_to_card.rs`

- [x] **Step 1: Detect prior file presence**

Run: `ls mur-core/src/cmd/agent_export/card.rs 2>/dev/null && echo present || echo missing`
- If `present`: extend it.
- If `missing`: create it but leave the actual CLI registration for D4. M2 only needs a `pub fn build_card_from_profile(profile: &AgentProfile) -> MurCard`.

- [x] **Step 2: Test (TDD)**

```rust
// mur-core/tests/companion_first_memory_to_card.rs
use mur_common::agent::{AgentProfile, FirstMemory, OnboardingState};

#[test]
fn build_card_includes_first_memory_when_present() {
    let mut p = AgentProfile::default();
    p.companion.onboarding = OnboardingState {
        completed_at: Some(chrono::Utc::now()),
        version: 1,
        agent_display_name: Some("Mochi".into()),
        first_memory: Some(FirstMemory {
            text: "Sunday in Taipei".into(),
            established_at: chrono::Utc::now(),
        }),
    };
    p.name = "test".into();
    let card = mur_core::cmd::agent_export::card::build_card_from_profile(&p);
    let yaml = serde_yaml_ng::to_string(&card).unwrap();
    assert!(yaml.contains("Sunday in Taipei"));
    assert!(yaml.contains("first_memory:"));
}

#[test]
fn build_card_omits_first_memory_when_absent() {
    let mut p = AgentProfile::default();
    p.name = "blank".into();
    let card = mur_core::cmd::agent_export::card::build_card_from_profile(&p);
    let yaml = serde_yaml_ng::to_string(&card).unwrap();
    assert!(!yaml.contains("first_memory:"));
}
```

- [x] **Step 3: Implement build_card_from_profile**

```rust
// mur-core/src/cmd/agent_export/card.rs
use crate::character_card::{first_memory::FirstMemoryExt, schema::*};
use mur_common::agent::AgentProfile;

pub fn build_card_from_profile(p: &AgentProfile) -> MurCard {
    let first_memory = p.companion.onboarding.first_memory.as_ref().map(|fm| FirstMemoryExt {
        text: fm.text.clone(),
        established_at: fm.established_at,
    });
    MurCard {
        spec: "murcard_v1".into(),
        spec_version: "1.0".into(),
        data: CardData {
            name: p.companion.onboarding.agent_display_name.clone().unwrap_or_else(|| p.name.clone()),
            description: String::new(),
        },
        extensions: first_memory.map(|fm| Extensions { mur: Some(MurExt { first_memory: Some(fm) }) }),
        ccv3_passthrough: Default::default(),
    }
}
```

- [x] **Step 4: Run, confirm pass**

Run: `cargo test -p mur-core --test companion_first_memory_to_card`
Expected: both PASS.

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent_export/card.rs mur-core/tests/companion_first_memory_to_card.rs
git commit -m "M2.7.2: build_card_from_profile threads first_memory into MurCard"
```

---

## Milestone M2.8 — E2E acceptance + cookbook

### Task M2.8.1: Acceptance script `v1-d2-onboarding.sh`

**Files:**
- Create: `scripts/e2e/v1-d2-onboarding.sh`

- [x] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# scripts/e2e/v1-d2-onboarding.sh — D2 onboarding wizard E2E acceptance.
#
# Acceptance gates (roadmap §4.2):
# 1. Wizard completes in ≤ 120 seconds (--answers path; non-interactive).
# 2. `companion preview <name> --situation morning_greeting --no-llm`
#    output contains the first_memory text.
# 3. MockClock-driven runtime test advances 72 h and produces a
#    proactive message body containing the first_memory string
#    (the cargo test in onboarding_morning_greeting_72h.rs).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

MUR_HOME=$(mktemp -d)
export MUR_HOME

echo "==> 1/3 build mur (release)"
cargo build --release --bin mur --quiet

MUR=./target/release/mur

echo "==> 2/3 create agent + run wizard via --answers"
ANSWERS=$(mktemp)
cat >"$ANSWERS" <<'YAML'
locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
YAML

t0=$(date +%s)
$MUR agent create d2-test
$MUR agent companion init d2-test --answers "$ANSWERS"
t1=$(date +%s)
elapsed=$((t1 - t0))
echo "wizard --answers elapsed ${elapsed}s"
[[ $elapsed -le 120 ]] || { echo "FAIL: > 120s"; exit 1; }

echo "==> 3/3 preview morning_greeting"
out=$($MUR agent companion preview d2-test --situation morning_greeting --no-llm)
echo "$out" | grep -q "Sunday in Taipei" || {
  echo "FAIL: preview did not include first_memory; got:"
  echo "$out"
  exit 1
}

echo "==> 72h MockClock acceptance (cargo test)"
cargo test -p mur-agent-runtime --test onboarding_morning_greeting_72h --release --quiet

echo "✅ D2 onboarding E2E passed"
```

- [x] **Step 2: chmod + run**

Run:
```bash
chmod +x scripts/e2e/v1-d2-onboarding.sh
./scripts/e2e/v1-d2-onboarding.sh
```
Expected: completes ≤ 120 s, all three gates pass.

- [x] **Step 3: Commit**

```bash
git add scripts/e2e/v1-d2-onboarding.sh
git commit -m "M2.8.1: E2E acceptance script for D2 onboarding"
```

### Task M2.8.2: Hook into run-all.sh

**Files:**
- Modify: `scripts/e2e/run-all.sh:53-54` (add new line after companion-phase11.sh)

- [x] **Step 1: Append D2 line**

Modify `scripts/e2e/run-all.sh` so the section reading

```bash
echo "==> Running companion Phase 1.1 E2E smoke..."
"$REPO_ROOT/scripts/e2e/companion-phase11.sh"
```

becomes

```bash
echo "==> Running companion Phase 1.1 E2E smoke..."
"$REPO_ROOT/scripts/e2e/companion-phase11.sh"

echo "==> Running D2 onboarding E2E smoke..."
"$REPO_ROOT/scripts/e2e/v1-d2-onboarding.sh"
```

- [x] **Step 2: Run end-to-end**

Run: `scripts/e2e/run-all.sh`
Expected: full suite passes including the new D2 step.

- [x] **Step 3: Commit**

```bash
git add scripts/e2e/run-all.sh
git commit -m "M2.8.2: run-all.sh includes D2 onboarding gate"
```

### Task M2.8.3: Cookbook entry

**Files:**
- Create: `docs/cookbook/first-memory-onboarding.md`

- [x] **Step 1: Write the cookbook**

```markdown
# First-Memory Onboarding (D2)

The 5-step wizard runs on first launch of a `mur agent export --format gui` bundle (and via `mur agent companion init <name>` from the CLI).

## Steps

1. **Name your agent** — display name only; the slug from `mur agent create` stays the same.
2. **Voice** — opt-in, default *Skip — enable later*. Voice setup in Settings → Voice triggers a one-time ~190 MB whisper download.
3. **Relationship** — Friend / Coach / Accountability buddy / Mentor.
4. **First memory** — one fact the agent should remember. Surfaced on day-3+ `morning_greeting` templates.
5. **Behavior** — three-layer toggle:
   - *Warm voice only* (default, recommended)
   - *Warm + behavior collection*
   - *All including proactive check-ins*

## Where it's stored

- `~/.mur/agents/<name>/profile.yaml` — `companion.onboarding.{completed_at,agent_display_name,first_memory}`, plus `companion.{enabled,rhythm.enabled,proactive.enabled}` for the three-layer toggle.
- `~/.mur/agents/<name>/companion/relationship.json` — duplicates `first_memory.text` (for runtime + character-card export).

## Re-running

```bash
mur agent companion init <name> --re-init
```

## Character card

`mur agent export --format card` (D4) emits `extensions.mur.first_memory.{text, established_at}` round-trippable per CCv3 passthrough.

## Acceptance gates

```
scripts/e2e/v1-d2-onboarding.sh
```

- Wizard ≤ 120 s
- `mur agent companion preview <name> --situation morning_greeting --no-llm` output references the first_memory string verbatim
- 72-hour MockClock test produces a proactive message containing the first_memory
```

- [x] **Step 2: Commit**

```bash
git add docs/cookbook/first-memory-onboarding.md
git commit -m "M2.8.3: cookbook for D2 first-memory onboarding"
```

---

## Self-Review Checklist

| Spec § | Requirement | Task |
|---|---|---|
| §4.2 step 1 | Name your agent | M2.5.2 (UI) + M2.3.2 (CLI) + M2.1.1 (`agent_display_name` field) |
| §4.2 step 2 | Pick a voice | M2.5.3 (UI defers to M1 voice tab; "Skip / Enable later") |
| §4.2 step 3 | Pick a relationship | M2.5.4 (UI) — reuses existing `Relationship` enum |
| §4.2 step 4 | Share one fact (first_memory) | M2.5.5 (UI) + M2.3.1 (CLI) + M2.1.1 (schema) + M2.7.2 (card export) |
| §4.2 step 5 | Three-layer toggle | M2.5.6 (UI) + M2.3.2 (CLI) + M2.1.2 (`ProactiveTiers` helper) |
| §4.2 — picker recognizes `first_memory` | day-3 morning_greeting auto-references it | M2.2.1 (substitution) + M2.2.2 (templates) + M2.2.3 (outbox load) |
| §4.2 acceptance: ≤ 2 min wizard | M2.8.1 step 1 (≤ 120s gate) |
| §4.2 acceptance: preview references first_memory | M2.8.1 step 3 + M2.3.3 |
| §4.2 acceptance: 72-h MockClock test | M2.2.3 + M2.8.1 step 4 |
| §4.4 — character card extension | M2.7.1 + M2.7.2 |

**Placeholder scan:** none.

**Type consistency:** `OnboardingState`, `FirstMemory`, `ProactiveTiers`, `OnboardingStatus`, `OnboardingSubmit`, `Relationship`, `ProactiveTier` (TS) used consistently across CLI, Tauri commands, and React components.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-30-mur-agent-d2-onboarding.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
