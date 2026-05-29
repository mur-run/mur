# Agent Export UX — Plan 2: Share Command + Model-Resolution Backend

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Rust backend for the consumer giveaway loop's producer + recipient model steps: a Hub "Share this agent" command that produces a `.muragent`, an extension of the import inspection to surface the `model_hint`, host-hardware detection, and a shared `apply_model_choice` mechanism that writes a `~/.mur/models.yaml` entry and rebinds the installed agent — consumed by both the CLI (`mur agent install`) and (in Plan 3) the Hub GUI wizard.

**Architecture:** `mur-hub-gui` already depends on `mur-core`, so the Share command is a thin Tauri wrapper over a newly-public `mur-core` export entry point. Model resolution reuses Plan 1's pure `recommend()` (in `mur-common`) plus new I/O helpers in `mur-core` (`Hardware::detect`, `apply_model_choice`) that both the CLI and the GUI call. No new format; weights never travel.

**Tech Stack:** Rust (edition 2024), Tauri 2, `sysinfo` (new dep in `mur-core` for RAM), existing `mur-common::model` registry + `mur-common::muragent`.

**Spec:** §6 (C — Share), §7.1–7.5 (B — model resolution).

**Depends on:** Plan 1 (`ModelHint`, `ModelTier`, `classify`, `Hardware`, `Recommendation`, `recommend`). Ships after Plan 1.

---

## Scope & boundaries

- **In scope (this plan, Rust + Tauri commands):** public `mur-core` export entry point; Hub `export_muragent_file` command; `MuragentInspection.model_hint`; `Hardware::detect`; `ModelChoice` + `apply_model_choice`; Hub `model_resolution_view` / `apply_model_choice` commands; CLI interactive resolution in `mur agent install`.
- **Out of scope → Plan 3 (frontend + distribution):** the React UI (Share button, import-dialog model-wizard step) in `mur-hub-gui/ui`; `tauri.conf.json` `fileAssociations` / deep-link plugin / `dmg` target / mur Developer-ID notarization; OS open-file/url routing in `lib.rs`; first-launch onboarding (`/Applications`, `lsregister`, doctor). Plan 3 needs its own exploration of the `ui/` React tree and the release/signing pipeline before it can be bite-sized; it is outlined at the end of this file.

---

## File structure

- `mur-core/src/cmd/agent/export.rs` — **Modify.** Add `pub fn export_agent_to_muragent(name, out)` (resolve home + call the existing `export_muragent`).
- `mur-core/src/cmd/agent/model_resolve.rs` — **Create.** `ModelChoice`, `Hardware::detect`, `apply_model_choice`. Re-export Plan 1's `recommend`.
- `mur-core/src/cmd/agent/mod.rs` — **Modify.** `pub mod model_resolve;` + export the new fns.
- `mur-core/Cargo.toml` — **Modify.** Add `sysinfo`.
- `mur-core/src/cmd/agent/install.rs` — **Modify.** Interactive model resolution after install.
- `mur-hub-gui/src-tauri/src/export_muragent.rs` — **Create.** `export_muragent_file` command.
- `mur-hub-gui/src-tauri/src/import_muragent.rs` — **Modify.** Add `model_hint` to `MuragentInspection`; add `model_resolution_view` + `apply_model_choice` commands.
- `mur-hub-gui/src-tauri/src/lib.rs` — **Modify.** Register the three new commands.

---

## Task 1: Public export entry point in `mur-core`

**Files:**
- Modify: `mur-core/src/cmd/agent/export.rs`
- Test: same file (`sanitize_tests` module from Plan 1)

- [ ] **Step 1: Write the failing test**

Add to the `sanitize_tests` module:

```rust
    #[test]
    fn export_entry_point_writes_muragent() {
        use mur_common::identity::AgentIdentity;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("agents").join("coach");
        std::fs::create_dir_all(&home).unwrap();
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.name = "coach".into();
        std::fs::write(home.join("profile.yaml"), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
        AgentIdentity::generate().save(&home).unwrap();

        let out = tmp.path().join("coach.muragent");
        export_muragent("coach", &home, &out).unwrap();
        assert!(out.exists(), ".muragent written");
    }
```

- [ ] **Step 2: Run test to verify it fails, then add the public wrapper**

Run: `cargo test -p mur-core -- sanitize_tests::export_entry_point`
Expected: PASS if `export_muragent` already works against a temp home (it is the existing private fn). If it needs a public, agent-name-only entry point for Hub, add:

```rust
/// Resolve an installed agent's home and export it to `out` as a `.muragent`.
/// Public so the Hub "Share" command (mur-hub-gui) can reuse the exact CLI path.
pub fn export_agent_to_muragent(name: &str, out: &Path) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    export_muragent(name, &agent_home, out)
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p mur-core -- sanitize_tests::export_entry_point`
Expected: PASS.

```bash
git add mur-core/src/cmd/agent/export.rs
git commit -m "feat(export): public export_agent_to_muragent entry point for Hub reuse"
```

---

## Task 2: `MuragentInspection.model_hint`

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/import_muragent.rs`

The import dialog must show what model the agent needs so the recipient can resolve it. Surface `manifest.model_hint` in the inspection struct.

- [ ] **Step 1: Add the field + a view type**

In `import_muragent.rs`, add a serializable view near `DeclaredPermissions`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ModelHintView {
    pub provider: String,
    pub name: String,
    pub tier: String,        // "small" | "mid" | "frontier"
    pub min_ram_gb: u32,
    pub local_capable: bool,
}
```

Add this field to `MuragentInspection` (after `permissions`):

```rust
    /// Model the agent was authored against, for first-run resolution (§7.1).
    pub model_hint: Option<ModelHintView>,
```

- [ ] **Step 2: Populate it in `build_inspection`**

Find where `build_inspection` constructs the `MuragentInspection { … }` literal and add (mapping the manifest field):

```rust
        model_hint: manifest.model_hint.as_ref().map(|h| ModelHintView {
            provider: h.provider.clone(),
            name: h.name.clone(),
            tier: format!("{:?}", h.tier).to_lowercase(),
            min_ram_gb: h.min_ram_gb,
            local_capable: h.local_capable,
        }),
```

- [ ] **Step 3: Build + commit**

Run: `cargo build -p mur-hub-gui` (or `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`)
Expected: compiles.

```bash
git add mur-hub-gui/src-tauri/src/import_muragent.rs
git commit -m "feat(hub): surface model_hint in muragent inspection"
```

---

## Task 3: `ModelChoice` + `Hardware::detect` + `apply_model_choice`

**Files:**
- Create: `mur-core/src/cmd/agent/model_resolve.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`, `mur-core/Cargo.toml`

- [ ] **Step 1: Add `sysinfo` to `mur-core/Cargo.toml`**

Under `[dependencies]`:

```toml
sysinfo = "0.32"
```

- [ ] **Step 2: Write the failing test + module**

Create `mur-core/src/cmd/agent/model_resolve.rs`:

```rust
//! First-run model resolution (I/O side). Pairs with the pure `recommend()`
//! decision tree in `mur_common::model_resolve`. Shared by the CLI
//! (`mur agent install`) and the Hub GUI wizard (Plan 3). Spec §7.3–7.5.

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::model_resolve::Hardware;
use serde::{Deserialize, Serialize};

/// A concrete resolution the user (or a flag) selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Secret ref string (e.g. "env:OPENAI_API_KEY"); recipient-supplied,
    /// never from the package.
    #[serde(default)]
    pub secret: Option<String>,
}

/// Detect host capabilities used by `recommend()` and the wizard UI.
pub fn detect_hardware() -> Hardware {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total_ram_gb = (sys.total_memory() / 1024 / 1024 / 1024) as u32;
    let apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let ollama_present = which_ollama();
    Hardware { total_ram_gb, apple_silicon, ollama_present }
}

fn which_ollama() -> bool {
    std::process::Command::new("ollama")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Stable registry key for a choice, e.g. "ollama_llama3_2_3b".
pub fn choice_ref_name(choice: &ModelChoice) -> String {
    let raw = format!("{}_{}", choice.provider, choice.model);
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

/// Write a `models.yaml` entry for `choice` and set the installed agent's
/// `model_ref` to it. Returns the registry key used.
pub fn apply_model_choice(mur_home: &Path, slug: &str, choice: &ModelChoice) -> Result<String> {
    let agent_home = mur_home.join("agents").join(slug);
    let profile_path = agent_home.join("profile.yaml");
    if !profile_path.exists() {
        bail!("agent '{slug}' not installed at {}", profile_path.display());
    }

    // 1. Upsert the registry entry.
    let reg_path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&reg_path)?;
    let key = choice_ref_name(choice);
    reg.models.insert(
        key.clone(),
        ModelEntry {
            provider: choice.provider.clone(),
            model: choice.model.clone(),
            base_url: choice.base_url.clone(),
            secret: choice.secret.as_deref().map(|s| s.parse()).transpose()?,
            capabilities: vec![],
            params: serde_json::Value::Null,
        },
    );
    reg.save_to(&reg_path)?;

    // 2. Point the agent at it.
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)
            .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.model_ref = Some(key.clone());
    let yaml = serde_yaml_ng::to_string(&profile)?;
    std::fs::write(&profile_path, yaml).with_context(|| format!("write {}", profile_path.display()))?;

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_plausible_ram() {
        let hw = detect_hardware();
        assert!(hw.total_ram_gb > 0, "RAM detection should be non-zero");
    }

    #[test]
    fn choice_ref_name_is_sanitized() {
        let c = ModelChoice {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
        };
        assert_eq!(choice_ref_name(&c), "ollama_llama3_2_3b");
    }

    #[test]
    fn apply_writes_registry_and_sets_model_ref() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path().to_path_buf();
        let home = mur_home.join("agents").join("coach");
        std::fs::create_dir_all(&home).unwrap();
        let p = mur_common::AgentProfile::default_for_tests();
        std::fs::write(home.join("profile.yaml"), serde_yaml_ng::to_string(&p).unwrap()).unwrap();

        // Isolate the registry path to the temp home.
        unsafe { std::env::set_var("MUR_HOME", &mur_home); }
        let choice = ModelChoice {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
        };
        let key = apply_model_choice(&mur_home, "coach", &choice).unwrap();
        assert_eq!(key, "ollama_llama3_2_3b");

        let reloaded: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(home.join("profile.yaml")).unwrap())
                .unwrap();
        assert_eq!(reloaded.model_ref.as_deref(), Some("ollama_llama3_2_3b"));
    }
}
```

> Note: `ModelRegistry::default_path()` honors `MUR_HOME` (confirm in `mur-common/src/model.rs:91`); if it does not read `MUR_HOME`, the test must point the registry path explicitly — adjust the test to call a path-taking variant. Verify before relying on the env override.

- [ ] **Step 3: Declare the module**

In `mur-core/src/cmd/agent/mod.rs`, add:

```rust
pub mod model_resolve;
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-core -- model_resolve`
Expected: 3 tests PASS.

```bash
git add mur-core/src/cmd/agent/model_resolve.rs mur-core/src/cmd/agent/mod.rs mur-core/Cargo.toml
git commit -m "feat(model): hardware detect + apply_model_choice (registry + model_ref)"
```

---

## Task 4: Hub Tauri commands (Share + resolution)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/export_muragent.rs`
- Modify: `mur-hub-gui/src-tauri/src/import_muragent.rs`, `mur-hub-gui/src-tauri/src/lib.rs`

- [ ] **Step 1: Create the Share command**

Create `mur-hub-gui/src-tauri/src/export_muragent.rs`:

```rust
//! `mur agent export` — Hub side. Produces a `.muragent` from an installed
//! agent using the exact CLI path (template/sanitized). Spec §6 (C).

use std::path::Path;

#[tauri::command]
pub fn export_muragent_file(name: String, out_path: String) -> Result<String, String> {
    mur_core::cmd::agent::export::export_agent_to_muragent(&name, Path::new(&out_path))
        .map_err(|e| e.to_string())?;
    Ok(out_path)
}
```

(Adjust the `mur_core::…` path to match how `export_agent_to_muragent` is re-exported; if `cmd` is private, add a `pub use` in `mur-core/src/lib.rs` or call through an existing public façade.)

- [ ] **Step 2: Add resolution commands to `import_muragent.rs`**

```rust
use mur_core::cmd::agent::model_resolve::{ModelChoice, apply_model_choice, detect_hardware};
use mur_common::model_resolve::{Recommendation, recommend};
use mur_common::muragent::manifest::ModelHint;

#[derive(Debug, Clone, Serialize)]
pub struct ResolutionView {
    pub hint: Option<ModelHintView>,
    pub total_ram_gb: u32,
    pub apple_silicon: bool,
    pub ollama_present: bool,
    pub recommendation: String, // "local" | "cloud" | "cloud_or_smaller_local" | "neutral_menu"
}

#[tauri::command]
pub fn model_resolution_view(path: String) -> Result<ResolutionView, String> {
    let archive = MuragentArchive::read(Path::new(&path)).map_err(|e| e.to_string())?;
    let manifest_yaml = archive.get_str("manifest.yaml").map_err(|e| e.to_string())?;
    let manifest: MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml).map_err(|e| e.to_string())?;
    let hw = detect_hardware();
    let hint: Option<ModelHint> = manifest.model_hint.clone();
    let rec = match recommend(hint.as_ref(), &hw) {
        Recommendation::Local => "local",
        Recommendation::Cloud => "cloud",
        Recommendation::CloudOrSmallerLocal => "cloud_or_smaller_local",
        Recommendation::NeutralMenu => "neutral_menu",
    };
    Ok(ResolutionView {
        hint: hint.map(|h| ModelHintView {
            provider: h.provider,
            name: h.name,
            tier: format!("{:?}", h.tier).to_lowercase(),
            min_ram_gb: h.min_ram_gb,
            local_capable: h.local_capable,
        }),
        total_ram_gb: hw.total_ram_gb,
        apple_silicon: hw.apple_silicon,
        ollama_present: hw.ollama_present,
        recommendation: rec.to_string(),
    })
}

#[tauri::command]
pub fn apply_agent_model(slug: String, choice: ModelChoice) -> Result<String, String> {
    let mur_home = trust::mur_home();
    apply_model_choice(&mur_home, &slug, &choice).map_err(|e| e.to_string())
}
```

- [ ] **Step 3: Register the three commands**

In `mur-hub-gui/src-tauri/src/lib.rs`, add `pub mod export_muragent;` near the other module declarations, and add to the `tauri::generate_handler![ … ]` list (after the existing `import_muragent::*` entries):

```rust
            export_muragent::export_muragent_file,
            import_muragent::model_resolution_view,
            import_muragent::apply_agent_model,
```

- [ ] **Step 4: Build + commit**

Run: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles (resolve any `pub use` needed for the `mur_core::cmd::agent::…` paths).

```bash
git add mur-hub-gui/src-tauri/src/export_muragent.rs mur-hub-gui/src-tauri/src/import_muragent.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): Share command + model resolution view/apply Tauri commands"
```

---

## Task 5: CLI interactive resolution in `mur agent install`

**Files:**
- Modify: `mur-core/src/cmd/agent/install.rs`

After a successful install, if the agent has no resolvable model on this machine, run the §7.3 recommendation and prompt. Non-interactive callers can pre-set the binding with `mur model add` + edit, so the prompt only fires on a TTY.

- [ ] **Step 1: Add a post-install resolution step**

In `cmd_install`, after the success `println!`s and before `Ok(())`, add:

```rust
    maybe_resolve_model(&mur_home, &outcome.manifest.agent.slug, &archive)?;
    Ok(())
}

fn maybe_resolve_model(
    mur_home: &Path,
    slug: &str,
    archive: &MuragentArchive,
) -> Result<()> {
    use mur_common::model_resolve::{Recommendation, recommend};
    use crate::cmd::agent::model_resolve::{ModelChoice, apply_model_choice, choice_ref_name, detect_hardware};

    // Read the model hint from the package manifest.
    let manifest_yaml = archive.get_str("manifest.yaml").context("read manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)?;
    let hint = manifest.model_hint.clone();
    let hw = detect_hardware();
    let rec = recommend(hint.as_ref(), &hw);

    // Non-interactive (no TTY): print guidance and leave the binding as-is.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  model: {} — set one with `mur model add` then run the agent with --model <ref>",
            match rec {
                Recommendation::Local => "local recommended (Ollama/MLX)",
                Recommendation::Cloud => "cloud model recommended",
                Recommendation::CloudOrSmallerLocal => "cloud or a smaller local model",
                Recommendation::NeutralMenu => "choose a backend",
            }
        );
        return Ok(());
    }

    // Interactive: offer the recommended default; fall back to a paste-key path.
    println!("\nThis agent needs a model backend (no weights are bundled).");
    if let Some(h) = &hint {
        println!("  Authored for: {}/{} (tier {:?})", h.provider, h.name, h.tier);
    }
    let choice = prompt_model_choice(&rec, hint.as_ref())?;
    if let Some(choice) = choice {
        let key = apply_model_choice(mur_home, slug, &choice)?;
        println!("  bound model_ref = {key}");
    } else {
        println!("  skipped — set one later with `mur model add`");
    }
    Ok(())
}
```

- [ ] **Step 2: Add the prompt helper**

```rust
fn prompt_model_choice(
    rec: &mur_common::model_resolve::Recommendation,
    hint: Option<&mur_common::muragent::manifest::ModelHint>,
) -> Result<Option<crate::cmd::agent::model_resolve::ModelChoice>> {
    use std::io::Write;
    use mur_common::model_resolve::Recommendation;
    use crate::cmd::agent::model_resolve::ModelChoice;

    let default_local = hint.map(|h| (h.provider.clone(), h.name.clone()));
    let prompt = match rec {
        Recommendation::Local => "Pull a local model now? [Y/n] ",
        Recommendation::Cloud | Recommendation::CloudOrSmallerLocal => {
            "Paste an API key for a cloud model? [y/N] "
        }
        Recommendation::NeutralMenu => "Configure a model now? [y/N] ",
    };
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "");

    if matches!(rec, Recommendation::Local) && yes {
        if let Some((provider, model)) = default_local {
            return Ok(Some(ModelChoice { provider, model, base_url: None, secret: None }));
        }
    }
    if !yes {
        return Ok(None);
    }
    // Cloud / neutral: collect provider + model + secret ref.
    let provider = read_field("  provider (e.g. anthropic): ")?;
    let model = read_field("  model (e.g. claude-opus-4-7): ")?;
    let secret = read_field("  secret ref (e.g. env:ANTHROPIC_API_KEY): ")?;
    Ok(Some(ModelChoice {
        provider,
        model,
        base_url: None,
        secret: if secret.is_empty() { None } else { Some(secret) },
    }))
}

fn read_field(prompt: &str) -> Result<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
```

- [ ] **Step 3: Build + manually verify**

Run: `cargo build -p mur-core`
Then manual: `mur agent install some.muragent` on a TTY shows the resolution prompt; piped (`mur agent install x </dev/null`) prints the non-interactive guidance line. (Add the needed `use mur_common::muragent::manifest::MuragentManifest;` / `mur_common::muragent::reader::MuragentArchive` already imported in this file.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/install.rs
git commit -m "feat(install): interactive first-run model resolution (local-first, cloud-honest)"
```

---

## Self-Review

**1. Spec coverage:** §6 Share → Tasks 1 & 4; §7.1 hint surfaced → Task 2; §7.3 hardware + recommend wired → Tasks 3 & 4 & 5; §7.4 apply (registry + model_ref) → Task 3; §7.5 GUI surface (commands) → Task 4, CLI surface → Task 5, non-interactive guidance → Task 5. Frontend UI + distribution explicitly deferred to Plan 3. ✓

**2. Placeholder scan:** No "TBD"/"add handling". Two flagged *verification notes* (the `mur_core::cmd::agent` re-export path; whether `ModelRegistry::default_path` honors `MUR_HOME`) are real "confirm-this-signature" checks, not content gaps — the code is complete and the note says exactly what to confirm. ✓

**3. Type consistency:** `ModelChoice { provider, model, base_url, secret }` defined in Task 3, used identically in Tasks 4 & 5. `apply_model_choice(&Path, &str, &ModelChoice) -> Result<String>`, `detect_hardware() -> Hardware`, `choice_ref_name(&ModelChoice) -> String` consistent across tasks. `recommend(Option<&ModelHint>, &Hardware) -> Recommendation` matches Plan 1. `ModelHintView` fields match `ModelHint` (Plan 1). `ModelEntry`/`ModelRegistry` field names match `mur-common/src/model.rs`. `SecretRef` parsed via `str::parse` (confirm `SecretRef: FromStr` in `mur-common/src/secret.rs`; if not, construct it with its actual constructor). ✓

---

# Plan 3 (outline) — Frontend & Distribution

> Not yet bite-sized: needs exploration of `mur-hub-gui/ui` (React) and the release/signing pipeline before TDD-level tasks can be written without speculation. Captured here so the loop is fully tracked.

**A — Host distribution & OS file association** (`mur-hub-gui/src-tauri/tauri.conf.json`, `lib.rs`, `onboarding/`):
- `tauri.conf.json`: add `bundle.fileAssociations` (`muragent` / `application/vnd.mur.agent`), add `bundle.targets` `dmg`, add macOS signing identity + notarization (mur Developer ID via CI env), Windows Trusted Signing. Keep identifier `run.mur.hub`.
- Add `tauri-plugin-deep-link` dep + plugin init; in `lib.rs` `.setup`/`RunEvent`, route OS open-file (`RunEvent::Opened` on macOS, argv on Windows, `%f` on Linux) + `muragent-*://` URLs to `inspect_muragent_file`. Single-instance (focus existing Host on second open).
- First-launch onboarding: enforce `/Applications`; register handler (`lsregister -f` / registry / `xdg-mime`); run `mur agent doctor`; offer drag-to-import.
- `mur.run/get` DMG produced by mur CI (not a user command).

**Frontend (React, `mur-hub-gui/ui`):**
- "Share" action per agent → save dialog → `invoke("export_muragent_file", {name, outPath})` → success toast.
- Import-dialog model-wizard step: call `model_resolution_view(path)`, render the recommended default + the always-present options (Ollama pull / MLX / paste key / endpoint), call `apply_agent_model(slug, choice)`; show pull progress.

**Exploration needed before writing Plan 3:** the `ui/` component structure + invoke patterns; Tauri 2 deep-link plugin API specifics; the release pipeline + signing-credential custody; an Ollama-pull progress channel.
