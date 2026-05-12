# mur-companion Phase 1.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship the profile-driven `companion::` subsystem in `mur-agent-runtime` per spec `docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md` — warm voice in reactive replies, optional proactive outbox with deterministic spacing, durable JSONL ledger, transparent CLI.

**Architecture:** Three-layer opt-in (`enabled` / `rhythm.enabled` reserved / `proactive.enabled`) on top of existing `AgentProfile`. New `mur-agent-runtime/src/durable/` shared primitive (Ledger + rate_limit) consumed by `companion::outbox`. Voice composed in-memory from embedded templates (relationship × locale matrix) with disk-override eject. Outbox tick loop driven by existing supervisor cadence.

**Tech Stack:** Rust 2024, `serde`, `serde_yaml_ng`, `chrono` + `chrono-tz`, `rand` (`WeightedIndex`), new deps: `whatlang`, `insta`, `once_cell`, `seahash`, `ulid`, `fs2` (flock).

**Spec sections referenced as `Spec §X.Y` throughout.**

**Commit message format:** `M<n>.<m>: <subject>` so `git log --grep "^M3"` reveals milestone progress.

---

## File Structure

```
mur-common/src/
  agent.rs                                 # MODIFY: add CompanionConfig field to AgentProfile
  companion/
    mod.rs                                 # CREATE: Relationship, Locale, Situation, Signal, Formality
    voice_template.rs                      # CREATE: include_str! matrix + lookup chain
    content_seed.rs                        # CREATE: include_str! content pool
    fixtures.rs                            # CREATE: do/don't pairs
    templates/                             # CREATE: friend.zh-TW.md / coach.en-US.md / ...
    content/                               # CREATE: morning_greeting.zh-TW.yaml / ...

mur-agent-runtime/src/
  durable/
    mod.rs                                 # CREATE: re-exports
    ledger.rs                              # CREATE: append-only JSONL writer
    resume.rs                              # CREATE: scan + replay helper
    rate_limit.rs                          # CREATE: anthropic-ratelimit-* parser
  llm/
    mod.rs                                 # MODIFY: stub provider gating
    stub.rs                                # CREATE: deterministic test LLM
  companion/
    mod.rs                                 # CREATE: Companion::new
    clock.rs                               # CREATE: Clock trait
    onboarding.rs                          # CREATE: wizard answer ingestion
    voice.rs                               # CREATE: composition + cache
    i18n.rs                                # CREATE: heuristic + translate
    picker.rs                              # CREATE: WeightedIndex + cooldown
    situations.rs                          # CREATE: time-of-day weight table
    schedule.rs                            # CREATE: deterministic interval
    earned_permission.rs                   # CREATE: gates
    outbox.rs                              # CREATE: tick loop
    notifier.rs                            # CREATE: trait + StdoutNotifier
    inbox.rs                               # CREATE: write/read inbox markdown
    linter.rs                              # CREATE: heuristic voice-quality gate
    telemetry.rs                           # CREATE: companion.* events

mur-core/src/cmd/
  agent_companion.rs                       # CREATE: CLI subcommand group
  agent.rs                                 # MODIFY: add `companion` subcommand wiring

scripts/e2e/
  companion-phase11.sh                     # CREATE: E2E runner

tests/                                     # CREATE under each crate as needed
  fixtures/
    profile/v1_minimum.yaml                # frozen for schema-evolution test
    ledger/v1_frozen.jsonl                 # frozen for backwards-compat
    ledger/v_current_full_coverage.jsonl   # current schema, every variant
  golden/                                  # insta snapshots
```

---

## Milestone M1 — CompanionConfig Schema + Onboarding Wizard

**Outcome:** `mur agent companion init` end-to-end works (interactive + `--answers`), writes profile and disk state atomically. No voice/LLM yet.

### Task M1.1: Add workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)

- [x] **Step 1: Add deps to workspace `Cargo.toml`**

```toml
# Append under [workspace.dependencies]
rand = "0.8"
once_cell = "1"
seahash = "4"
whatlang = "0.16"
ulid = { version = "1", features = ["serde"] }
fs2 = "0.4"
chrono-tz = "0.8"
insta = { version = "1", features = ["yaml"] }
```

- [x] **Step 2: Verify workspace builds**

Run: `cargo check --workspace`
Expected: PASS (no other crates use these yet, so no breakage).

- [x] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "M1.1: add workspace deps for companion (rand, whatlang, insta, ulid, fs2, etc.)"
```

### Task M1.2: Create `mur-common/src/companion/mod.rs` skeleton with enums

**Files:**
- Create: `mur-common/src/companion/mod.rs`
- Modify: `mur-common/src/lib.rs` (re-export)

- [x] **Step 1: Write failing test**

Create `mur-common/tests/companion_enums.rs`:

```rust
use mur_common::companion::{Relationship, Formality, Signal, Situation};

#[test] fn relationship_default_is_friend() {
    assert!(matches!(Relationship::default(), Relationship::Friend));
}
#[test] fn relationship_serde_roundtrip() {
    let r = Relationship::Coach;
    let s = serde_json::to_string(&r).unwrap();
    assert_eq!(s, "\"coach\"");
    let r2: Relationship = serde_json::from_str(&s).unwrap();
    assert!(matches!(r2, Relationship::Coach));
}
#[test] fn situation_known_variants() {
    let s: Situation = serde_json::from_str("\"morning_greeting\"").unwrap();
    assert!(matches!(s, Situation::MorningGreeting));
}
```

- [x] **Step 2: Run test, verify FAIL**

Run: `cargo test -p mur-common --test companion_enums`
Expected: compile error (module doesn't exist)

- [x] **Step 3: Implement enums**

Create `mur-common/src/companion/mod.rs`:

```rust
//! Companion subsystem shared types (Phase 1.1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    #[default] Friend,
    Coach,
    AccountabilityBuddy,
    Mentor,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Formality { Casual, #[default] Neutral, Formal }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Situation { MorningGreeting, GentleCheckIn, ShareQuote, ShareLink }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal { Positive, Negative, Dismiss, Sent }
```

Modify `mur-common/src/lib.rs`:

```rust
pub mod companion;
```

- [x] **Step 4: Run test, verify PASS**

Run: `cargo test -p mur-common --test companion_enums`
Expected: 3 passed.

- [x] **Step 5: Commit**

```bash
git add mur-common/src/companion/ mur-common/src/lib.rs mur-common/tests/companion_enums.rs
git commit -m "M1.2: companion enums (Relationship, Formality, Situation, Signal)"
```

### Task M1.3: Add `CompanionConfig` to `AgentProfile`

**Files:**
- Modify: `mur-common/src/agent.rs` (add field + struct)
- Test: `mur-common/tests/companion_profile_roundtrip.rs`

- [x] **Step 1: Write failing test**

```rust
use mur_common::agent::{AgentProfile, CompanionConfig};
use mur_common::companion::Relationship;

#[test] fn profile_without_companion_block_loads_with_defaults() {
    let yaml = std::fs::read_to_string("../tests/fixtures/profile/v1_minimum.yaml").unwrap();
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert!(!p.companion.enabled);
    assert!(matches!(p.companion.relationship, Relationship::Friend));
    assert_eq!(p.companion.proactive.daily_cap, 3);
}

#[test] fn companion_roundtrip_preserves_all_fields() {
    let mut p: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string("../tests/fixtures/profile/v1_minimum.yaml").unwrap()
    ).unwrap();
    p.companion.enabled = true;
    p.companion.locale = "zh-TW".into();
    p.companion.relationship = Relationship::Coach;
    let s = serde_yaml_ng::to_string(&p).unwrap();
    let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
    assert_eq!(p, p2);
}
```

Create `tests/fixtures/profile/v1_minimum.yaml` (place at workspace root) — copy from output of `mur agent create test --provider ollama --model llama3.2:3b -- --print-profile` or hand-craft a minimal valid v1 profile (see existing `mur-agent-runtime/tests/fixtures/`).

- [x] **Step 2: Run test, verify FAIL** (`field 'companion' missing` or similar)

- [x] **Step 3: Implement schema**

In `mur-common/src/agent.rs`, add at top of file:

```rust
use crate::companion::{Relationship, Formality};
```

Add field to `AgentProfile`:

```rust
#[serde(default)]
pub companion: CompanionConfig,
```

Append at end of file:

```rust
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanionConfig {
    #[serde(default)] pub enabled: bool,
    #[serde(default = "default_locale")] pub locale: String,
    #[serde(default)] pub relationship: Relationship,
    #[serde(default)] pub voice_overrides: VoiceOverrides,
    #[serde(default)] pub onboarding: OnboardingState,
    #[serde(default)] pub rhythm: RhythmConfig,
    #[serde(default)] pub proactive: ProactiveConfig,
}

fn default_locale() -> String {
    std::env::var("LANG").ok()
        .and_then(|v| v.split('.').next().map(|s| s.replace('_', "-")))
        .unwrap_or_else(|| "en-US".into())
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub name_for_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub formality: Option<Formality>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub extra_instructions: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnboardingState {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)] pub version: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RhythmConfig {
    #[serde(default)] pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProactiveConfig {
    #[serde(default)] pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub learning_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub quiet_hours: Option<QuietHours>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub active_hours: Option<ActiveHours>,
    #[serde(default = "default_daily_cap")] pub daily_cap: u8,
    #[serde(default = "default_channels")] pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub paused_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            enabled: false, learning_until: None, quiet_hours: None, active_hours: None,
            daily_cap: default_daily_cap(), channels: default_channels(), paused_until: None,
        }
    }
}

fn default_daily_cap() -> u8 { 3 }
fn default_channels() -> Vec<String> { vec!["stdout".into()] }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuietHours { pub start: String, pub end: String }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveHours { pub start: String, pub end: String }
```

Add `chrono = { workspace = true }` to `mur-common/Cargo.toml` if not already present.

- [x] **Step 4: Run tests, verify PASS**

Run: `cargo test -p mur-common --test companion_profile_roundtrip`

- [x] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/Cargo.toml mur-common/tests/companion_profile_roundtrip.rs tests/fixtures/profile/v1_minimum.yaml
git commit -m "M1.3: CompanionConfig schema on AgentProfile (back-compat via #[serde(default)])"
```

### Task M1.4: Schema-evolution test (R12 invariant)

**Files:**
- Create: `tests/fixtures/profile/v1_frozen.yaml` (copy of v1_minimum, frozen forever)
- Create: `mur-common/tests/schema_evolution.rs`

- [x] **Step 1: Freeze the v1 fixture**

```bash
cp tests/fixtures/profile/v1_minimum.yaml tests/fixtures/profile/v1_frozen.yaml
```

- [x] **Step 2: Write the guard test**

```rust
//! Asserts every future profile change still loads v1_frozen.yaml.
use mur_common::agent::AgentProfile;

#[test] fn v1_frozen_profile_still_loads() {
    let yaml = std::fs::read_to_string("../tests/fixtures/profile/v1_frozen.yaml")
        .expect("v1_frozen.yaml must exist — DO NOT DELETE OR EDIT");
    let _p: AgentProfile = serde_yaml_ng::from_str(&yaml)
        .expect("v1_frozen profile must remain readable across all future schema changes");
}
```

- [x] **Step 3: Run test, verify PASS** (`cargo test -p mur-common --test schema_evolution`)

- [x] **Step 4: Commit**

```bash
git add tests/fixtures/profile/v1_frozen.yaml mur-common/tests/schema_evolution.rs
git commit -m "M1.4: freeze v1 profile fixture + schema-evolution guard test (R12)"
```

### Task M1.5: `mur agent companion` CLI scaffold

**Files:**
- Create: `mur-core/src/cmd/agent_companion.rs`
- Modify: `mur-core/src/cmd/agent.rs` (wire subcommand)

- [x] **Step 1: Add module + dispatch**

Append to `mur-core/src/cmd/mod.rs`: `pub mod agent_companion;`

In `mur-core/src/cmd/agent.rs`, add to the `agent` clap subcommand enum:

```rust
/// Manage companion (warm voice + optional proactive messaging).
Companion(crate::cmd::agent_companion::CompanionArgs),
```

And in the dispatch match arm:

```rust
AgentCommand::Companion(args) => crate::cmd::agent_companion::run(args).await,
```

Create `mur-core/src/cmd/agent_companion.rs`:

```rust
//! `mur agent companion ...` subcommands (Phase 1.1).
use clap::Parser;

#[derive(Parser, Debug)]
pub struct CompanionArgs {
    #[command(subcommand)]
    pub cmd: CompanionCmd,
}

#[derive(Parser, Debug)]
pub enum CompanionCmd {
    /// Run onboarding wizard.
    Init { name: String,
           #[arg(long)] answers: Option<std::path::PathBuf>,
           #[arg(long)] re_init: bool },
    /// Stub for later tasks (M6).
    #[command(hide = true)] Placeholder,
}

pub async fn run(args: CompanionArgs) -> anyhow::Result<()> {
    match args.cmd {
        CompanionCmd::Init { name, answers, re_init } => init::run(&name, answers, re_init).await,
        CompanionCmd::Placeholder => Ok(()),
    }
}

mod init;
```

Create `mur-core/src/cmd/agent_companion/init.rs` with stub:

```rust
pub async fn run(_name: &str, _answers: Option<std::path::PathBuf>, _re_init: bool) -> anyhow::Result<()> {
    anyhow::bail!("M1.5 stub — implemented in M1.6");
}
```

- [x] **Step 2: Verify build**

Run: `cargo build -p mur-core` — Expected: PASS.
Run: `cargo run -p mur-core -- agent companion --help` — Expected: shows `init` subcommand.

- [x] **Step 3: Commit**

```bash
git add mur-core/src/cmd/agent_companion.rs mur-core/src/cmd/agent_companion/ mur-core/src/cmd/agent.rs mur-core/src/cmd/mod.rs
git commit -m "M1.5: scaffold mur agent companion CLI subcommand group"
```

### Task M1.6: `init` non-interactive (`--answers`) end-to-end

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/init.rs`
- Create: `mur-core/tests/agent_companion_init.rs`

- [x] **Step 1: Write failing test**

```rust
use std::process::Command;
use tempfile::TempDir;

#[test] fn init_with_answers_writes_profile_and_relationship_json() {
    let home = TempDir::new().unwrap();
    // Pre-create an agent with mur agent create
    let status = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("HOME", home.path()).env("MUR_HOME", home.path().join(".mur"))
        .args(["agent", "create", "darwin", "--provider", "ollama", "--model", "llama3.2:3b", "--non-interactive"])
        .status().unwrap();
    assert!(status.success());

    let answers = home.path().join("answers.yaml");
    std::fs::write(&answers, b"locale: zh-TW\nname_for_user: David\nrelationship: friend\nformality: casual\nextra_instructions: \"\"\n").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("HOME", home.path()).env("MUR_HOME", home.path().join(".mur"))
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers).status().unwrap();
    assert!(status.success());

    // Assertions
    let profile_path = home.path().join(".mur/agents/darwin/profile.yaml");
    let profile_str = std::fs::read_to_string(&profile_path).unwrap();
    assert!(profile_str.contains("relationship: friend"));
    assert!(profile_str.contains("locale: zh-TW"));

    let rel_path = home.path().join(".mur/agents/darwin/companion/relationship.json");
    let rel_json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&rel_path).unwrap()).unwrap();
    assert_eq!(rel_json["name_for_user"], "David");
}
```

- [x] **Step 2: Run test, verify FAIL** (`anyhow!("M1.5 stub")`)

- [x] **Step 3: Implement non-interactive `init`**

Replace `mur-core/src/cmd/agent_companion/init.rs`:

```rust
use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::agent::{AgentProfile, OnboardingState, VoiceOverrides};
use mur_common::companion::{Formality, Relationship};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Answers {
    locale: String,
    name_for_user: String,
    relationship: Relationship,
    #[serde(default)] formality: Option<Formality>,
    #[serde(default)] extra_instructions: Option<String>,
}

pub async fn run(name: &str, answers: Option<PathBuf>, re_init: bool) -> Result<()> {
    let answers = match answers {
        Some(path) => {
            let s = std::fs::read_to_string(&path)
                .with_context(|| format!("read answers: {}", path.display()))?;
            serde_yaml_ng::from_str::<Answers>(&s)?
        }
        None => anyhow::bail!("interactive mode lands in M1.7; for now use --answers"),
    };

    let agent_dir = mur_core::paths::agent_dir(name)?;
    if !agent_dir.exists() {
        anyhow::bail!("agent {name} does not exist; run `mur agent create {name}` first");
    }

    let companion_dir = agent_dir.join("companion");
    std::fs::create_dir_all(&companion_dir)?;

    // flock companion/.init.lock for race-safety (R11)
    let lock_path = companion_dir.join(".init.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true).read(true).write(true).truncate(false).open(&lock_path)?;
    use fs2::FileExt;
    lock_file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another `companion init` is running for this agent"))?;

    // Load + mutate profile
    let profile_path = agent_dir.join("profile.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)?;

    if !re_init && profile.companion.onboarding.completed_at.is_some() {
        anyhow::bail!("companion already initialized for {name}; pass --re-init to re-run");
    }

    profile.companion.enabled = true;
    profile.companion.locale = answers.locale.clone();
    profile.companion.relationship = answers.relationship.clone();
    profile.companion.voice_overrides = VoiceOverrides {
        name_for_user: Some(answers.name_for_user.clone()),
        formality: answers.formality.clone(),
        extra_instructions: answers.extra_instructions.clone(),
    };
    profile.companion.onboarding = OnboardingState { completed_at: Some(Utc::now()), version: 1 };

    // Atomic write profile.yaml
    let tmp = profile_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, serde_yaml_ng::to_string(&profile)?)?;
    std::fs::rename(&tmp, &profile_path)?;

    // Write relationship.json
    let rel = serde_json::json!({
        "version": 1,
        "name_for_user": answers.name_for_user,
        "relationship": answers.relationship,
        "locale": answers.locale,
        "formality": answers.formality,
        "extra_instructions": answers.extra_instructions,
        "onboarded_at": Utc::now(),
    });
    let rel_path = companion_dir.join("relationship.json");
    let rel_tmp = rel_path.with_extension("json.tmp");
    std::fs::write(&rel_tmp, serde_json::to_string_pretty(&rel)?)?;
    std::fs::rename(&rel_tmp, &rel_path)?;

    println!("Companion mode enabled for {name}.");
    println!("Run `mur agent companion proactive enable {name}` when you're ready for occasional check-ins.");
    Ok(())
}
```

In `mur-core/src/lib.rs` ensure `pub mod paths;` exposes `agent_dir(&str)`. If `paths` doesn't yet expose this helper, add it:

```rust
pub fn agent_dir(name: &str) -> anyhow::Result<std::path::PathBuf> {
    Ok(mur_root()?.join("agents").join(name))
}
```

Add `fs2 = { workspace = true }` to `mur-core/Cargo.toml`.

- [x] **Step 4: Run test, verify PASS**

Run: `cargo test -p mur-core --test agent_companion_init`

- [x] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent_companion/init.rs mur-core/src/lib.rs mur-core/src/paths.rs mur-core/Cargo.toml mur-core/tests/agent_companion_init.rs
git commit -m "M1.6: init --answers writes profile + relationship.json + flock guard (R11)"
```

### Task M1.7: Interactive wizard (`dialoguer`)

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/init.rs`
- Test: manual (interactive); also non-interactive path remains covered by M1.6

- [x] **Step 1: Add interactive branch**

Replace `None => anyhow::bail!(...)` arm with:

```rust
None => {
    use dialoguer::{theme::ColorfulTheme, Input, Select};
    let theme = ColorfulTheme::default();
    let detected_locale = mur_common::agent::default_locale();   // expose via pub fn
    let locale: String = Input::with_theme(&theme)
        .with_prompt(format!("Language (BCP-47, e.g. zh-TW)"))
        .default(detected_locale).interact_text()?;
    let name_for_user: String = Input::with_theme(&theme)
        .with_prompt("What should I call you?").interact_text()?;
    let rel_choices = ["Friend", "Coach", "Accountability buddy", "Mentor"];
    let rel_idx = Select::with_theme(&theme)
        .with_prompt("How should this agent relate to you?")
        .items(&rel_choices).default(0).interact()?;
    let relationship = match rel_idx {
        0 => Relationship::Friend, 1 => Relationship::Coach,
        2 => Relationship::AccountabilityBuddy, _ => Relationship::Mentor,
    };
    Answers { locale, name_for_user, relationship, formality: Some(Formality::Casual), extra_instructions: Some(String::new()) }
}
```

In `mur-common/src/agent.rs` make `default_locale` `pub` (rename to public helper):

```rust
pub fn default_locale() -> String { /* unchanged */ }
```

Add `dialoguer = "0.11"` to `mur-core/Cargo.toml` if not present.

- [x] **Step 2: Manual smoke**

Run interactively: `cargo run -p mur-core -- agent companion init darwin`
Expected: 3 prompts, completes successfully.

- [x] **Step 3: Commit**

```bash
git add mur-core/src/cmd/agent_companion/init.rs mur-core/Cargo.toml mur-common/src/agent.rs
git commit -m "M1.7: interactive 3-step wizard via dialoguer"
```

### Task M1.8: `re-init` preserves history (R-recovery)

**Files:**
- Modify: `mur-core/src/cmd/agent_companion/init.rs`
- Test: `mur-core/tests/agent_companion_reinit.rs`

- [x] **Step 1: Write failing test**

```rust
// Setup: init once, write a fake ledger entry + bandit-state, re-init, assert preserved.
#[test] fn re_init_preserves_ledger_inbox_bandit() {
    /* spawn `mur agent companion init darwin --answers a.yaml`,
       write companion/outbox-ledger/2026-04-29.jsonl + bandit-state.json + inbox/01HQ.md,
       run `init darwin --answers b.yaml --re-init`,
       assert files still present, profile.relationship updated */
}
```

- [x] **Step 2: Implement** — current `init.rs` already supports `re_init` via the `if !re_init &&` check; ensure ledger / bandit-state / inbox **are not touched**. Add explicit comments and a test exercising it.

- [x] **Step 3: Commit**

```bash
git add mur-core/tests/agent_companion_reinit.rs
git commit -m "M1.8: re-init preserves ledger/inbox/bandit-state (Spec §3.2)"
```

---

## Milestone M2 — Voice Template System + i18n Heuristic

**Outcome:** Voice composition works in-memory and on disk; i18n heuristic detects locale mismatch.

### Task M2.1: Embed voice template matrix

**Files:**
- Create: `mur-common/src/companion/templates/{friend,coach,accountability_buddy,mentor}.{zh-TW,en-US}.md` (8 files; placeholders per Spec Appendix A)
- Create: `mur-common/src/companion/templates/friend.zh-CN.md` (best-effort)
- Create: `mur-common/src/companion/templates/friend.ja-JP.md` (best-effort)
- Create: `mur-common/src/companion/voice_template.rs`

- [x] **Step 1: Author 10 templates** following Spec Appendix A.1/A.2 format (use `{{NAME_FOR_USER}}`, `{{FORMALITY}}`, `{{EXTRA_INSTRUCTIONS}}`, `{{LOCALE}}` placeholders only).

- [x] **Step 2: Write `voice_template.rs`**

```rust
//! Embedded voice templates with lookup chain.
use crate::companion::Relationship;

pub fn embedded(relationship: &Relationship, locale: &str) -> Option<&'static str> {
    match (relationship, locale) {
        (Relationship::Friend, "zh-TW") => Some(include_str!("templates/friend.zh-TW.md")),
        (Relationship::Friend, "en-US") => Some(include_str!("templates/friend.en-US.md")),
        (Relationship::Friend, "zh-CN") => Some(include_str!("templates/friend.zh-CN.md")),
        (Relationship::Friend, "ja-JP") => Some(include_str!("templates/friend.ja-JP.md")),
        (Relationship::Coach, "zh-TW") => Some(include_str!("templates/coach.zh-TW.md")),
        (Relationship::Coach, "en-US") => Some(include_str!("templates/coach.en-US.md")),
        (Relationship::AccountabilityBuddy, "zh-TW") => Some(include_str!("templates/accountability_buddy.zh-TW.md")),
        (Relationship::AccountabilityBuddy, "en-US") => Some(include_str!("templates/accountability_buddy.en-US.md")),
        (Relationship::Mentor, "zh-TW") => Some(include_str!("templates/mentor.zh-TW.md")),
        (Relationship::Mentor, "en-US") => Some(include_str!("templates/mentor.en-US.md")),
        _ => None,
    }
}

/// Locale fallback chain: exact → language-only → en-US.
pub fn resolve_locale(relationship: &Relationship, locale: &str)
    -> (String, &'static str /* template body */)
{
    if let Some(t) = embedded(relationship, locale) { return (locale.into(), t); }
    let lang = locale.split('-').next().unwrap_or(locale);
    if locale != lang {
        if let Some(t) = embedded(relationship, lang) { return (lang.into(), t); }
    }
    let t = embedded(relationship, "en-US").expect("en-US template must exist for every relationship");
    ("en-US".into(), t)
}
```

- [x] **Step 3: Tests**

```rust
#[test] fn fallback_zh_tw_to_zh_to_en() {
    use mur_common::companion::Relationship;
    let (used, _) = mur_common::companion::voice_template::resolve_locale(&Relationship::Mentor, "zh-CN");
    // mentor zh-CN missing → zh missing → en-US used
    assert_eq!(used, "en-US");
}
```

- [x] **Step 4: Commit**

```bash
git add mur-common/src/companion/templates/ mur-common/src/companion/voice_template.rs mur-common/src/companion/mod.rs
git commit -m "M2.1: embed voice template matrix (4×{zh-TW,en-US} + best-effort) + locale fallback"
```

### Task M2.2: Voice composition engine

**Files:**
- Create: `mur-agent-runtime/src/companion/mod.rs` (skeleton)
- Create: `mur-agent-runtime/src/companion/voice.rs`
- Create: `mur-agent-runtime/src/companion/clock.rs`
- Modify: `mur-agent-runtime/src/lib.rs` (`pub mod companion;`)

- [x] **Step 1: Write Clock + Voice tests**

```rust
// mur-agent-runtime/tests/companion_voice.rs
use mur_agent_runtime::companion::voice::{compose_in_memory, VoiceInput};
use mur_common::companion::Relationship;

#[test] fn placeholders_replaced() {
    let v = compose_in_memory(VoiceInput {
        relationship: Relationship::Friend, locale: "zh-TW",
        name_for_user: "David", formality: "casual", extra_instructions: "",
    });
    assert!(v.contains("David"));
    assert!(!v.contains("{{NAME_FOR_USER}}"));
    assert!(v.contains("zh-TW"));
}
```

- [x] **Step 2: Implement Clock**

```rust
// mur-agent-runtime/src/companion/clock.rs
use chrono::{DateTime, Local, Utc};

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn now_local(&self) -> DateTime<Local>;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> { Utc::now() }
    fn now_local(&self) -> DateTime<Local> { Local::now() }
}

pub struct MockClock { offset: std::sync::Mutex<chrono::Duration>, base: DateTime<Utc> }
impl MockClock {
    pub fn at(base: DateTime<Utc>) -> Self { Self { offset: std::sync::Mutex::new(chrono::Duration::zero()), base } }
    pub fn advance(&self, d: chrono::Duration) { *self.offset.lock().unwrap() = *self.offset.lock().unwrap() + d; }
}
impl Clock for MockClock {
    fn now_utc(&self) -> DateTime<Utc> { self.base + *self.offset.lock().unwrap() }
    fn now_local(&self) -> DateTime<Local> { self.now_utc().with_timezone(&Local) }
}
```

- [x] **Step 3: Implement voice composer**

```rust
// mur-agent-runtime/src/companion/voice.rs
use mur_common::companion::{Relationship, voice_template};

pub struct VoiceInput<'a> {
    pub relationship: Relationship, pub locale: &'a str,
    pub name_for_user: &'a str, pub formality: &'a str, pub extra_instructions: &'a str,
}

pub struct ComposedVoice { pub locale_used: String, pub body: String }

pub fn compose_in_memory(i: VoiceInput) -> String {
    let (locale_used, tpl) = voice_template::resolve_locale(&i.relationship, i.locale);
    tpl.replace("{{NAME_FOR_USER}}", i.name_for_user)
       .replace("{{FORMALITY}}", i.formality)
       .replace("{{EXTRA_INSTRUCTIONS}}", i.extra_instructions)
       .replace("{{LOCALE}}", &locale_used)
}
```

`mur-agent-runtime/src/companion/mod.rs`:

```rust
pub mod clock;
pub mod voice;
```

`mur-agent-runtime/src/lib.rs`: add `pub mod companion;`.

- [x] **Step 4: Run test, verify PASS** + commit

```bash
git add mur-agent-runtime/src/companion/ mur-agent-runtime/src/lib.rs mur-agent-runtime/tests/companion_voice.rs
git commit -m "M2.2: Clock trait + voice composer (in-memory placeholder substitution)"
```

### Task M2.3: Disk override lookup chain

**Files:**
- Modify: `mur-agent-runtime/src/companion/voice.rs`

- [x] **Step 1: Test**

```rust
#[test] fn per_agent_disk_override_wins() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".mur/agents/x/companion/templates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("friend.zh-TW.md"), "OVERRIDE {{NAME_FOR_USER}}").unwrap();
    let body = mur_agent_runtime::companion::voice::compose_with_overrides(
        home.path().join(".mur/agents/x"), VoiceInput { /* ... name=Bob */ }
    );
    assert!(body.contains("OVERRIDE Bob"));
}
```

- [x] **Step 2: Implement `compose_with_overrides`** that checks per-agent dir → user dir (`~/.mur/companion/templates/`) → embedded.

- [x] **Step 3: Commit**

```bash
git commit -am "M2.3: voice template disk override (per-agent → user → embedded)"
```

### Task M2.4: i18n heuristic detector

**Files:**
- Create: `mur-agent-runtime/src/companion/i18n.rs`

- [x] **Step 1: Test**

```rust
#[test] fn cjk_block_detects_zh() {
    use mur_agent_runtime::companion::i18n::heuristic_matches;
    assert!(heuristic_matches("早安 David。今天好嗎？", "zh-TW"));
    assert!(!heuristic_matches("Good morning David.", "zh-TW"));
}
#[test] fn english_target_always_matches() {
    use mur_agent_runtime::companion::i18n::heuristic_matches;
    assert!(heuristic_matches("anything goes", "en-US"));
}
#[test] fn whatlang_for_german() {
    use mur_agent_runtime::companion::i18n::heuristic_matches;
    assert!(heuristic_matches("Guten Morgen, wie geht es dir heute?", "de-DE"));
}
```

- [x] **Step 2: Implement**

```rust
//! Locale-mismatch detection.
pub fn heuristic_matches(text: &str, target: &str) -> bool {
    if target.starts_with("en") { return true; }
    if target.starts_with("zh") { return cjk_ratio(text) >= 0.30; }
    if target.starts_with("ja") { return ja_ratio(text) >= 0.20; }
    if target.starts_with("ko") { return hangul_ratio(text) >= 0.30; }
    // Latin script targets — use whatlang
    match whatlang::detect(text) {
        Some(info) => {
            let lang = info.lang().code();
            target.starts_with(lang)
        }
        None => true,  // unknown → conservative pass
    }
}

fn cjk_ratio(s: &str) -> f32 { ratio(s, |c| matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)) }
fn ja_ratio(s: &str) -> f32 { ratio(s, |c| matches!(c as u32, 0x3040..=0x309F | 0x30A0..=0x30FF)) }
fn hangul_ratio(s: &str) -> f32 { ratio(s, |c| matches!(c as u32, 0xAC00..=0xD7AF)) }
fn ratio(s: &str, f: impl Fn(char) -> bool) -> f32 {
    let total = s.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 { return 0.0; }
    let hits = s.chars().filter(|&c| f(c)).count();
    hits as f32 / total as f32
}
```

Add `whatlang = { workspace = true }` to `mur-agent-runtime/Cargo.toml`.

- [x] **Step 3: Run test, verify PASS** + commit

```bash
git commit -am "M2.4: i18n heuristic_matches (CJK unicode-block + whatlang Latin fallback)"
```

---

## Milestone M3 — durable::Ledger + durable::rate_limit + StubLlm

**Outcome:** Shared primitives ready; companion can append events and survive restart; tests don't depend on real LLM.

### Task M3.1: `durable::ledger` append + scan

**Files:**
- Create: `mur-agent-runtime/src/durable/{mod.rs,ledger.rs,resume.rs}`
- Modify: `mur-agent-runtime/src/lib.rs` (`pub mod durable;`)

- [x] **Step 1: Test** (`mur-agent-runtime/tests/durable_ledger.rs`)

```rust
use mur_agent_runtime::durable::ledger::{Ledger, Event};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(tag = "event")]
enum E { A { x: u32 }, B { s: String } }

#[test] fn append_and_scan_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut led = Ledger::open(dir.path()).unwrap();
    led.append(&E::A { x: 1 }).unwrap();
    led.append(&E::B { s: "hi".into() }).unwrap();
    led.flush().unwrap();
    let evs: Vec<E> = Ledger::scan_days(dir.path(), 7).map(|r| r.unwrap()).collect();
    assert_eq!(evs, vec![E::A { x: 1 }, E::B { s: "hi".into() }]);
}

#[test] fn corrupt_last_line_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    let path = dir.path().join(format!("{today}.jsonl"));
    std::fs::write(&path, b"{\"event\":\"A\",\"x\":1}\n{not json}\n").unwrap();
    let evs: Vec<E> = Ledger::scan_days(dir.path(), 1).map(|r| r.unwrap()).collect();
    assert_eq!(evs, vec![E::A { x: 1 }]);  // corrupt line skipped
}
```

- [x] **Step 2: Implement** — `ledger.rs` opens `<base>/<YYYY-MM-DD>.jsonl`, append serializes JSON + `\n`, debounced fsync (1s coalesce via `tokio::time::Instant`-based check on each append). `scan_days` reads N most-recent files in chronological order, swallows malformed lines with a `tracing::warn!`.

- [x] **Step 3: Commit**

```bash
git commit -am "M3.1: durable::Ledger (per-day JSONL, debounced fsync, corrupt-line skip)"
```

### Task M3.2: `durable::rate_limit` parser

**Files:**
- Create: `mur-agent-runtime/src/durable/rate_limit.rs`

- [x] **Step 1: Test**

```rust
use mur_agent_runtime::durable::rate_limit::*;

#[test] fn retry_after_seconds() {
    let mut h = http::HeaderMap::new();
    h.insert("retry-after", "42".parse().unwrap());
    let s = parse_anthropic_429(&h, chrono::Utc::now(), 429);
    assert!(matches!(s, ResumeStrategy::After(d) if d.num_seconds() == 42));
}

#[test] fn ratelimit_reset_used_when_no_retry_after() {
    let mut h = http::HeaderMap::new();
    let reset = chrono::Utc::now() + chrono::Duration::seconds(120);
    h.insert("anthropic-ratelimit-tokens-reset", reset.to_rfc3339().parse().unwrap());
    let s = parse_anthropic_429(&h, chrono::Utc::now(), 429);
    assert!(matches!(s, ResumeStrategy::AtTimestamp(_)));
}

#[test] fn fallback_full_jitter_backoff() {
    let h = http::HeaderMap::new();
    let s = parse_anthropic_429(&h, chrono::Utc::now(), 429);
    assert!(matches!(s, ResumeStrategy::Backoff { attempt: 0 }));
}

#[test] fn five_two_nine_multiplies_wait() {
    let mut h = http::HeaderMap::new();
    h.insert("retry-after", "10".parse().unwrap());
    let s = parse_anthropic_429(&h, chrono::Utc::now(), 529);
    if let ResumeStrategy::After(d) = s { assert!(d.num_seconds() >= 40); } else { panic!() }
}
```

- [x] **Step 2: Implement** — header parsing per Spec §6.3, 529 multiplier 4×–8× (use 6× as midpoint), full-jitter formula `min(cap, base * 2^attempt) * rand_uniform`.

Add `http = "1"` to `mur-agent-runtime/Cargo.toml`.

- [x] **Step 3: Commit**

```bash
git commit -am "M3.2: durable::rate_limit (retry-after / ratelimit-*-reset / 529 multiplier / full jitter)"
```

### Task M3.3: `llm::stub` deterministic provider

**Files:**
- Create: `mur-agent-runtime/src/llm/stub.rs`
- Create: `mur-agent-runtime/src/llm/stub_scenarios.yaml`
- Modify: `mur-agent-runtime/src/llm/mod.rs`

- [x] **Step 1: Test**

```rust
#[test] fn stub_returns_canned_response_by_input_hash() {
    let stub = StubLlm::with_scenarios(/* test scenarios */);
    let resp = stub.generate(LlmRequest { /* ... morning_greeting prompt */ }).await.unwrap();
    assert!(resp.text.contains("早安"));
}

#[test] fn stub_can_simulate_429() {
    let stub = StubLlm::with_scenarios(/* one entry: fault: rate_limit_429 */);
    let err = stub.generate(/* prompt */).await.unwrap_err();
    assert!(matches!(err, LlmError::RateLimit));
}
```

- [x] **Step 2: Implement** — `StubLlm` parses `stub_scenarios.yaml`; `generate` hashes the request and returns the first matching scenario, or default echo. Selection by `MUR_LLM_PROVIDER=stub` in `llm/mod.rs::client_from_env`.

- [x] **Step 3: Commit**

```bash
git commit -am "M3.3: StubLlm provider for deterministic E2E (selected via MUR_LLM_PROVIDER=stub)"
```

---

## Milestone M4 — Picker + Situations + Schedule

### Task M4.1: Content seed embed + `bandit-state.json` types

**Files:**
- Create: `mur-common/src/companion/content/morning_greeting.zh-TW.yaml` (3+ entries with id/weight/cooldown_days/tags/source/reviewed_by/prompt_seed per Spec Appendix B)
- Create: ditto for `share_quote.zh-TW.yaml`, `gentle_check_in.zh-TW.yaml`, `share_link.zh-TW.yaml`, plus `*.en-US.yaml`
- Create: `mur-common/src/companion/content_seed.rs` (`include_str!` per yaml)
- Create: `mur-agent-runtime/src/companion/picker.rs` (state types only)

- [x] Author seed yamls; ensure each has ≥3 templates per situation per locale.
- [x] Implement `TemplateState` struct (per Spec §3.4) + `BanditState { morning_sent_today, templates: BTreeMap<TemplateId, TemplateState> }`.
- [x] Test: round-trip serde, seed loader produces non-empty pool per situation.
- [x] Commit: `M4.1: content pool seed (per Spec Appendix B) + bandit-state types`.

### Task M4.2: Picker selection (`WeightedIndex` + cooldown)

**Files:**
- Modify: `mur-agent-runtime/src/companion/picker.rs`

- [x] Test: empty pool → None; single eligible → returns it; equal weights → 1:1 ±5% over 200 picks (seeded RNG); weight cap 5.0; weight floor 0.1; cooldown filter excludes recent.
- [x] Implement `Picker::pick`, `Picker::record` exactly per Spec §4.5.
- [x] Commit: `M4.2: picker WeightedIndex + cooldown + record(Signal)`.

### Task M4.3: Situation × time-of-day weights + morning cap

**Files:**
- Create: `mur-agent-runtime/src/companion/situations.rs`

- [x] Test: 06:30 returns morning_greeting eligible; 12:00 morning ineligible; 23:00 returns None (or all zero).
- [x] Implement `pick_for_hour(now_local, morning_sent_today)`.
- [x] Commit: `M4.3: situations weight table + morning_greeting once-per-day cap (Spec §4.6)`.

### Task M4.4: Deterministic-interval `should_send_now`

**Files:**
- Create: `mur-agent-runtime/src/companion/schedule.rs`

- [x] Test: 12h active window, daily_cap=3, no prior send, now=09:00 → desired_interval=4h, should_send=true (since elapsed=∞); after sending, advance 1h → false; advance 4h → true; advance into quiet_hours → false.
- [x] Implement per Spec §4.7. ActiveHours/QuietHours parsing (HH:MM with chrono-tz aware).
- [x] Commit: `M4.4: schedule::should_send_now (deterministic interval + jitter; Spec §4.7)`.

### Task M4.5: `earned_permission` gates

**Files:**
- Create: `mur-agent-runtime/src/companion/earned_permission.rs`

- [x] Test: proactive disabled → blocked; paused_until in future → blocked; learning_until in future → blocked; quiet_hours match → blocked.
- [x] Implement `pub fn check(profile: &CompanionConfig, now_utc, now_local) -> GateOutcome`.
- [x] Commit: `M4.5: earned_permission gate (proactive/paused/learning/quiet)`.

---

## Milestone M5 — Outbox + Notifier + StdoutNotifier

### Task M5.1: OutboxEvent enum

**Files:**
- Create: `mur-agent-runtime/src/companion/telemetry.rs` (event enum) — note this hosts BOTH ledger event variants AND telemetry helpers; keep ledger separate from observability metrics in §M5.5.

- [x] Implement `OutboxEvent` enum exactly per Spec §3.5 (12 variants including `RhythmWiped`).
- [x] Test: every variant serde-roundtrips.
- [x] Commit: `M5.1: OutboxEvent enum (12 variants; frozen schema, Spec §3.5)`.

### Task M5.2: Notifier trait + StdoutNotifier + Inbox

**Files:**
- Create: `mur-agent-runtime/src/companion/notifier.rs`
- Create: `mur-agent-runtime/src/companion/inbox.rs`

- [x] Test: `StdoutNotifier::send` writes `inbox/<ulid>.md` with front-matter + body + `>>> response: <unset>`; second call with same id refuses (`O_CREAT|O_EXCL`).
- [x] Implement Notifier trait per Spec §4.9; inbox markdown format per Spec §3.6.
- [x] Commit: `M5.2: Notifier trait + StdoutNotifier + inbox markdown writer (R16 O_EXCL)`.

### Task M5.3: Outbox tick loop core

**Files:**
- Create: `mur-agent-runtime/src/companion/outbox.rs`

- [x] Test (uses StubLlm + FakeNotifier + MockClock): proactive disabled → 24h sim 0 sends; enabled cap=3 + 12h window → exactly 3 sends.
- [x] Implement steps 1-7 (gates + scheduling) per Spec §4.8. Steps 8-12 land in M5.4-M5.6.
- [x] Commit: `M5.3: outbox tick scheduling core (gates → schedule → ledger MessageScheduled)`.

### Task M5.4: Outbox LLM generation + linter + regenerate-once

**Files:**
- Create: `mur-agent-runtime/src/companion/linter.rs`
- Modify: `mur-agent-runtime/src/companion/outbox.rs`

- [x] Test linter: banned phrases, ≤1 emoji, ≤1 exclamation, length 1-3 sentences, zh-TW preserved-English ratio ≤30%.
- [x] Test outbox: linter pass → MessageGenerated + MessageSent; first fail → regenerate; second fail → MessageDropped { reason: "linter_persistent" }.
- [x] Implement linter + integrate (per Spec §4.8 step 9 and Spec §8.5 C1).
- [x] Commit: `M5.4: linter heuristic + regenerate-once integration in outbox`.

### Task M5.5: Outbox i18n + rate-limit pause/resume

**Files:**
- Modify: `mur-agent-runtime/src/companion/outbox.rs`
- Modify: `mur-agent-runtime/src/companion/i18n.rs`

- [x] Test: stub forces English when locale=zh-TW → translate path runs → MessageSent; translate stub returns 429 → MessagePaused { resume_at }; advance MockClock past resume_at → next tick resumes; 4 retries fail → MessageDropped { reason: "locale_unresolved" }.
- [x] Implement `i18n::ensure_locale` per Spec §6.2 + outbox steps 10-12 + resume step 2.
- [x] Commit: `M5.5: outbox i18n + rate-limit pause/resume + retry queue (Spec §4.8 + §6.2)`.

### Task M5.6: Passive-dismiss sweep

**Files:**
- Modify: `mur-agent-runtime/src/companion/outbox.rs`

- [x] Test: send message at T; advance MockClock 24h+1m; tick → PassiveDismissInferred event + picker.dismiss_count incremented.
- [x] Implement step 3 of `run_tick` (Spec §4.8).
- [x] Commit: `M5.6: outbox passive-dismiss sweep (sent>24h + no signal → infer dismiss)`.

### Task M5.7: Wire Companion into supervisor tick

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`
- Modify: `mur-agent-runtime/src/companion/mod.rs`

- [x] Implement `Companion::new(profile, clock) -> Option<Self>` returning `None` when `!profile.companion.enabled`.
- [x] Add 60s tick from supervisor calling `companion.run_tick(now)` when `Some`.
- [x] Test: agent without companion block → no companion module instantiated (memory ≤200B, Spec Q9).
- [x] Commit: `M5.7: wire Companion into supervisor tick (zero-cost when disabled, Q9)`.

---

## Milestone M6 — CLI Subcommand Group

(Each task ≤30min. Tests are end-to-end via `cargo run -p mur-core -- agent companion ...`.)

### Task M6.1: `proactive enable | disable`

- Mutates `profile.yaml::companion.proactive.enabled`. Atomic temp+rename.
- Test: enable → profile reflects; disable → reflects.
- Commit: `M6.1: mur agent companion proactive enable|disable`.

### Task M6.2: `quiet --for | --until | --off`

- Parses `--for 2h | 30m | 1d` (humantime) or `--until <RFC3339>` or `--off`.
- Writes `profile.yaml::companion.proactive.paused_until`. Appends `QuietRequested` ledger event.
- Test: `--for 2h` → paused_until = now+2h ±1s; `--off` → None.
- Commit: `M6.2: mur agent companion quiet --for/--until/--off`.

### Task M6.3: `voice eject | rebuild | diff`

- `eject` writes composed voice.md to `companion/voice.md`.
- `rebuild [--force]` re-composes; refuses if disk file mtime > onboarding completed_at unless `--force`.
- `diff` runs `diff -u embedded composed disk_voice_md`.
- Commit: `M6.3: mur agent companion voice eject|rebuild|diff`.

### Task M6.4: `templates eject [--scope agent|user] [<rel>.<locale>]`

- Writes embedded template to disk.
- Test: post-eject, `voice rebuild` finds disk template via lookup chain.
- Commit: `M6.4: mur agent companion templates eject`.

### Task M6.5: `content add <situation> [--from-stdin | --file]`

- Appends entry to `companion/content/<situation>.<locale>.yaml`. Validates required fields (`id`, `weight`, `cooldown_days`, `tags`, `source`, `reviewed_by`, `prompt_seed`).
- Commit: `M6.5: mur agent companion content add`.

### Task M6.6: `inbox [--unread-only]` + `ack --good|--bad|--dismiss`

- `inbox` walks `companion/inbox/*.md`, parses front-matter + response line, prints table.
- `ack` rewrites the response line + appends `UserSignal` event + calls picker.record (companion needs a sidecar API for CLI).
- Commit: `M6.6: mur agent companion inbox + ack`.

### Task M6.7: `preview --situation <s> [--no-llm]`

- `--no-llm`: print voice.md + prompt_seed only.
- Default: invoke LLM (real or stub), print rendered body. **Never** writes ledger/inbox/state.
- Commit: `M6.7: mur agent companion preview (read-only)`.

### Task M6.8: `why-did-you-message [<msg-id>]`

- No id: list last 7 days' MessageSent.
- With id: scan ledger for that id, replay events, print event chain (per Spec §9.2).
- Commit: `M6.8: mur agent companion why-did-you-message (full event chain)`.

### Task M6.9: `rhythm wipe`

- Shreds `inbox/`, `outbox-ledger/`, `bandit-state.json`. Clears `paused_until`/`learning_until`. Appends `RhythmWiped` event to fresh ledger. Preserves `companion.{enabled, relationship, locale, voice_overrides}`.
- Commit: `M6.9: mur agent companion rhythm wipe (preserves voice config)`.

### Task M6.10: `agent remove` interaction (R13)

**Files:**
- Modify: existing `mur agent remove` handler

- [x] Refuse if `companion/inbox/*.md` non-empty unless `--force`.
- [x] Test: unread message → remove fails with hint; --force succeeds.
- [x] Commit: `M6.10: agent remove refuses with unread companion inbox (R13)`.

---

## Milestone M7 — Tier 1 + Tier 2 Tests

### Task M7.1: Snapshot fixtures (insta)

- Configure `INSTA_UPDATE=no` for CI. Document in `CONTRIBUTING.md`.
- Add `tests/golden/voice_md/{relationship}.{locale}.md` for each combo (use sentinel placeholder values).
- Add `tests/golden/sys_prompt/{relationship}.{locale}.txt` (composed full prompt).
- Commit: `M7.1: insta golden fixtures for voice + sys_prompt`.

### Task M7.2: Picker math invariants + scenario snapshots

- `tests/picker/distribution_invariants.rs`: equal weights → 1:1 ±5%, 2× weight → 2:1 ±5% over N=10000 picks.
- `tests/golden/picker/<scenario>.txt`: named scenarios (`all_eligible_uniform`, `one_with_negative_weight`, `cooldown_excludes_morning_after_send`) at N=200.
- Commit: `M7.2: picker math-invariant + scenario snapshots`.

### Task M7.3: Ledger replay snapshots + frozen v1 fixture

- `tests/fixtures/ledger/v1_frozen.jsonl` (one of each event variant; never edited).
- `tests/fixtures/ledger/v_current_full_coverage.jsonl` (every variant in current schema).
- Reflective check: `OutboxEvent::variants() ⊆ tags found in v_current`.
- Commit: `M7.3: ledger frozen + current fixtures + reflective coverage check`.

### Task M7.4: Telemetry no-PII sentinel test

- `tests/telemetry_no_pii.rs`: run companion with `name_for_user="Sentinel-User-XYZ"` for a 24h MockClock simulation. Assert no event JSON contains the sentinel.
- Commit: `M7.4: telemetry redaction sentinel test (R12 / Spec §7.3)`.

### Task M7.5: Voice quality linter on 9 fixed samples

- Generate (`StubLlm`) the 9 (relationship × locale × situation) combos.
- Run linter on each; assert all pass C1 criteria.
- Commit attached samples as `tests/fixtures/voice_quality/*.md` for human review (Spec §8.5 C2).
- Commit: `M7.5: voice quality C1 linter test (9 samples) + C2 PR artifacts`.

### Task M7.6: `cargo bench` smoke

- Add `benches/companion.rs`: warm sys_prompt compose, picker pick, ledger append. CI invokes `cargo bench --no-run` to ensure compiles; nightly runs full bench and posts diff if >10× regression.
- Commit: `M7.6: cargo bench smoke for companion warm paths`.

---

## Milestone M8 — Integration + E2E

### Task M8.1: Test harness `Harness { home, runtime, llm: StubLlm, notifier: FakeNotifier, clock: MockClock }`

- Create `mur-agent-runtime/tests/companion_integration_common.rs` (test helper).
- Commit: `M8.1: companion integration test harness (StubLlm + FakeNotifier + MockClock)`.

### Task M8.2-M8.4: Integration tests batch

Cover all 16 cases from Spec §8.3. Suggested grouping (one commit per group of ~5):

- **M8.2**: gating tests (1, 2, 3, 4, 5) — onboarding/disabled/cap/quiet/paused.
- **M8.3**: rate-limit + i18n tests (6, 7, 8, 9) — rate-limit-pause / locale mismatch (proactive + reactive paths).
- **M8.4**: signal + restart tests (10, 11, 12, 13, 14, 15, 16) — passive dismiss / picker persistence / ledger replay / linter / re-init / morning cap.

Each: `M8.<n>: integration tests <group description>`.

### Task M8.5: E2E script `scripts/e2e/companion-phase11.sh`

- Implements the 8 steps from Spec §8.4 using `MUR_LLM_PROVIDER=stub`.
- Time budget < 90s. Add to `scripts/e2e/run-all.sh`.
- Commit: `M8.5: E2E companion-phase11.sh (Spec §8.4) + run-all integration`.

### Task M8.6: Optional Ollama nightly smoke

- `scripts/e2e/companion-ollama-nightly.sh`: same flow but with `--provider ollama`. Failures don't block PR.
- Add nightly GitHub Actions job.
- Commit: `M8.6: nightly Ollama smoke job (R7)`.

---

## Milestone M9 — Docs + Release

### Task M9.1: README updates

- Add "Companion mode" section to top-level `README.md` with quickstart (`mur agent create` → `companion init` → reactive demo + opt-in proactive).
- Commit: `M9.1: README companion-mode quickstart`.

### Task M9.2: CLAUDE.md updates

- Add `companion/` module to architecture section. Add `mur agent companion` subcommand list.
- Commit: `M9.2: CLAUDE.md updates (companion subsystem)`.

### Task M9.3: CONTRIBUTING.md additions

- Document `INSTA_UPDATE=no` CI policy + local `cargo insta review` workflow.
- Document `MUR_LLM_PROVIDER=stub` for fast dev.
- Document content-pool PR review checklist (`source`, `reviewed_by` required).
- Commit: `M9.3: CONTRIBUTING.md (insta workflow + stub LLM + content review)`.

### Task M9.4: Spec cross-reference touch-up

- After implementation reveals any spec drift, append a `## Implementation Notes (post-impl)` section to the spec linking to commits where decisions changed.
- Commit: `M9.4: spec post-impl notes`.

### Task M9.5: PR + retro

- Open PR from `feat/companion-phase-1-1`. PR description includes:
  - 9 voice quality samples (Spec §8.5 C2)
  - Telemetry redaction proof (sentinel test output)
  - Performance bench numbers (M7.6)
  - Acceptance criteria checklist (Spec §8.5 A1-A6, B1-B4, C1-C2)
- Commit: `M9.5: open PR with full acceptance evidence`.

---

## Self-Review Notes

Spec coverage check (every spec section maps to ≥1 task):
- §1 Overview / Non-goals → covered by scope of M1-M9 (no task needed; documented)
- §2 Architecture → M1.3 (schema), M5.7 (supervisor wire), M2.2 (Clock)
- §3 Data Model → M1.3, M1.4, M3.1, M5.1, M5.2
- §4 Components → M2 (voice/i18n), M3 (durable/rate_limit/stub), M4 (picker/situations/schedule), M5 (outbox/notifier)
- §5 CLI → M1.5-M1.7 + M6.*
- §6 LLM Integration → M3.3 (stub), M5.5 (i18n + rate limit)
- §7 Privacy → M1.6 (atomic write), M6.9 (wipe), M7.4 (no-PII sentinel)
- §8 Testing → M7.* + M8.*
- §9 Observability → M5.1 (events), M6.8 (why-did-you-message)
- §10 Risks → R11 M1.6 flock, R12 M1.4 schema-evo test, R13 M6.10, R14 M3.1 debounced fsync, R16 M5.2 O_EXCL
- §11 Open Questions → Q8 has dedicated test (in M8 group), Q9 verified by M5.7
- §12 Frozen contracts → guarded by M1.4 + M7.3
- §13 Harness discipline → enforced by progress checklist below

Type/method consistency check passed: `Picker::pick(&mut self, situation, now)`, `Picker::record(template_id, signal, now)` consistent across M4.2 and M5.3.

---

## Progress Checklist (Spec §13 — git+plan = single source of truth on rate-limit recovery)

**M1 — Schema + Onboarding**
- [x] M1.1: workspace deps
- [x] M1.2: companion enums
- [x] M1.3: CompanionConfig on AgentProfile
- [x] M1.4: schema-evolution guard
- [x] M1.5: CLI scaffold
- [x] M1.6: init --answers (atomic + flock)
- [x] M1.7: interactive wizard
- [x] M1.8: re-init preserves history

**M2 — Voice + i18n**
- [x] M2.1: embed template matrix + locale fallback
- [x] M2.2: Clock + voice composer
- [x] M2.3: disk override lookup chain
- [x] M2.4: i18n heuristic_matches

**M3 — Durable + Stub**
- [x] M3.1: durable::Ledger
- [x] M3.2: durable::rate_limit
- [x] M3.3: StubLlm provider

**M4 — Picker + Situations + Schedule**
- [x] M4.1: content seed + bandit-state types
- [x] M4.2: picker WeightedIndex + cooldown
- [x] M4.3: situations weight table + morning cap
- [x] M4.4: schedule deterministic interval
- [x] M4.5: earned_permission gates

**M5 — Outbox + Notifier**
- [x] M5.1: OutboxEvent enum
- [x] M5.2: Notifier + StdoutNotifier + Inbox
- [x] M5.3: outbox tick core
- [x] M5.4: linter + regenerate-once
- [x] M5.5: i18n + rate-limit pause/resume
- [x] M5.6: passive-dismiss sweep
- [x] M5.7: supervisor wire

**M6 — CLI**
- [x] M6.1: proactive enable/disable
- [x] M6.2: quiet
- [x] M6.3: voice eject/rebuild/diff
- [x] M6.4: templates eject
- [x] M6.5: content add
- [x] M6.6: inbox + ack
- [x] M6.7: preview
- [x] M6.8: why-did-you-message
- [x] M6.9: rhythm wipe
- [x] M6.10: agent remove unread guard

**M7 — Unit + Snapshot**
- [x] M7.1: insta fixtures
- [x] M7.2: picker math + scenario
- [x] M7.3: ledger fixtures + reflective coverage
- [x] M7.4: telemetry no-PII sentinel
- [x] M7.5: voice quality C1
- [x] M7.6: cargo bench smoke

**M8 — Integration + E2E**
- [x] M8.1: harness helpers
- [x] M8.2: gating integration tests (5)
- [x] M8.3: rate-limit + i18n integration tests (4)
- [x] M8.4: signal/restart integration tests (7)
- [x] M8.5: E2E companion-phase11.sh
- [x] M8.6: Ollama nightly smoke

**M9 — Docs**
- [x] M9.1: README quickstart
- [x] M9.2: CLAUDE.md
- [x] M9.3: CONTRIBUTING.md
- [x] M9.4: spec post-impl notes
- [x] M9.5: PR + retro

---

**Recovery procedure** (Spec §13.1 step 6): on Claude-Code interrupt, next session reads `git status` (uncommitted? finish or `git stash`/abandon), then this checklist (next `[ ]`), then continues. Commit messages start with `M<n>.<m>:` so `git log --grep "^M3"` reveals milestone progress at a glance.

