# Agent Export UX — Plan 1b: Runtime `--load` + `--model` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `mur-agent-runtime` a toolchain-free, signing-intact "run a `.muragent`" path for developers and servers: `mur-agent-runtime --load <path.muragent>` installs and runs the agent headless, and `--model <ref>` rebinds the model backend non-interactively (for servers that have no GUI/prompt).

**Architecture:** No new format and no embedding. The pre-built (already-signed) runtime reuses the existing `mur-common::muragent::installer` to extract a `.muragent` into `~/.mur/agents/<slug>/`, then runs the normal supervisor for that slug. `--model` sets `profile.model_ref` after profile load; the existing `build_provider_runner` → `resolve_model_entry` path already honors `model_ref` against `~/.mur/models.yaml`, so the backend switches with no further wiring.

**Tech Stack:** Rust (edition 2024), `anyhow`, existing `mur-common::muragent` installer/reader, existing `mur-agent-runtime` supervisor.

**Spec:** `docs/superpowers/specs/2026-05-29-mur-agent-export-ux-give-to-a-friend-design.md` §8 (D — toolchain-free developer replacement) and §7.5 (non-interactive surface).

**Depends on:** Plan 1 (no hard code dependency, but ships after it so `--format=bin` is already retired and `.muragent` is the only export).

---

## Scope & boundaries

- **In scope:** runtime argument parsing (`--load`, `--model`); `--load` install-and-run; `--model` non-interactive rebind; an integration test for the load path.
- **Out of scope (Plan 2):** guided/interactive model resolution (hardware detection, Ollama detect/pull, `recommend()`-driven prompts, auto `mur model add`). Developers using `--model` are expected to have created the registry entry with the existing `mur model add` command. The guided wizard is a Plan 2 deliverable shared by CLI and Hub GUI.

---

## File structure

- `mur-agent-runtime/src/subcommand.rs` — **Modify** (currently a one-line doc stub). Add the pure `flag_value` argument helper.
- `mur-agent-runtime/src/supervisor.rs` — **Modify.** Add a `--load` branch to agent-home resolution (`entrypoint`, around line 62-91) and a `--model` override after `Profile::load` (around line 100-106). Add the `load_muragent_and_home` helper.
- `mur-agent-runtime/tests/load_muragent.rs` — **Create.** Integration test: build a `.muragent`, load it, assert the agent home is materialized.

---

## Task 1: `flag_value` argument helper

**Files:**
- Modify: `mur-agent-runtime/src/subcommand.rs`
- Test: same file (inline)

- [ ] **Step 1: Write the failing test**

Replace the contents of `mur-agent-runtime/src/subcommand.rs` with:

```rust
//! CLI subcommand / flag parsing for the agent runtime binary.

/// Return the value following `flag` in `args`, supporting both
/// `--flag value` and `--flag=value` forms. Returns the first match.
pub fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let eq_prefix = format!("{flag}=");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq_prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn space_separated_form() {
        let a = args(&["mur-agent-runtime", "--load", "coach.muragent"]);
        assert_eq!(flag_value(&a, "--load"), Some("coach.muragent".to_string()));
    }

    #[test]
    fn equals_form() {
        let a = args(&["mur-agent-runtime", "--model=anthropic_opus_4_7"]);
        assert_eq!(flag_value(&a, "--model"), Some("anthropic_opus_4_7".to_string()));
    }

    #[test]
    fn absent_flag_is_none() {
        let a = args(&["mur-agent-runtime"]);
        assert_eq!(flag_value(&a, "--load"), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p mur-agent-runtime subcommand`
Expected: the module compiles and the three tests PASS (this task is the implementation + test together; if `flag_value` had a bug the tests would fail).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/subcommand.rs
git commit -m "feat(runtime): flag_value argument helper"
```

---

## Task 2: `--load <path.muragent>` install-and-run

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs` (`entrypoint`, agent-home resolution block at lines ~62-91; add helper near it)

- [ ] **Step 1: Add the `load_muragent_and_home` helper**

Add this function in `supervisor.rs` (near `resolve_embedded_agent_home`):

```rust
/// `--load` path: install a `.muragent` into `mur_home/agents/<slug>` (reusing
/// the shared installer — validation, trust, extraction) and return that home.
/// Stashes the slug as the expected name so the post-load name check passes.
fn load_muragent_and_home(path: &str, mur_home: &Path) -> anyhow::Result<PathBuf> {
    use mur_common::muragent::installer;
    use mur_common::muragent::reader::MuragentArchive;

    let archive = MuragentArchive::read(Path::new(path))
        .map_err(|e| anyhow::anyhow!("read .muragent at {path}: {e}"))?;
    let outcome = installer::install(&archive, mur_home, "cli")
        .map_err(|e| anyhow::anyhow!("install .muragent: {e}"))?;
    let slug = outcome.manifest.agent.slug.clone();
    // SAFETY: single-threaded startup, before any tokio tasks spawn.
    unsafe {
        std::env::set_var("MUR_RUNTIME_EXPECTED_NAME", &slug);
    }
    Ok(mur_home.join("agents").join(&slug))
}
```

- [ ] **Step 2: Add the `--load` branch to agent-home resolution**

In `entrypoint`, the agent-home resolution currently reads:

```rust
    let embedded_override = std::env::var_os("MUR_AGENT_EXTERNAL_PROFILE").is_some();
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
    let agent_home = if crate::export::bin_embed::has_embedded_agent() && !embedded_override {
```

Insert a `--load` branch as the first condition:

```rust
    let embedded_override = std::env::var_os("MUR_AGENT_EXTERNAL_PROFILE").is_some();
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
    let argv: Vec<String> = std::env::args().collect();
    let load_path = crate::subcommand::flag_value(&argv, "--load");
    let agent_home = if let Some(path) = load_path {
        match load_muragent_and_home(&path, &mur_home) {
            Ok(home) => home,
            Err(e) => {
                eprintln!("error[load]: {e}");
                std::process::exit(1);
            }
        }
    } else if crate::export::bin_embed::has_embedded_agent() && !embedded_override {
```

(The rest of the `if/else` chain is unchanged — the embedded branch and the argv0 branch now follow as `else if` / `else`.)

- [ ] **Step 3: Build**

Run: `cargo build -p mur-agent-runtime`
Expected: compiles. (Behavior is covered by the integration test in Task 4.)

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs
git commit -m "feat(runtime): --load <path.muragent> installs and runs the agent headless"
```

---

## Task 3: `--model <ref>` non-interactive rebind

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs` (`entrypoint`, after `Profile::load` at lines ~100-106)

- [ ] **Step 1: Make the profile mutable and apply the override**

The profile load currently reads:

```rust
    let profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error[profile_invalid]: {e}");
            std::process::exit(1);
        }
    };
```

Change to capture as `mut` and apply `--model`:

```rust
    let mut profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error[profile_invalid]: {e}");
            std::process::exit(1);
        }
    };
    // Non-interactive model rebind for headless/server use (§7.5). Honored
    // by build_provider_runner via resolve_model_entry, which prefers
    // model_ref over the inline model: block.
    if let Some(model_ref) = crate::subcommand::flag_value(&argv, "--model") {
        info!(model_ref = %model_ref, "overriding model binding from --model");
        profile.inner.model_ref = Some(model_ref);
    }
```

(`argv` is already in scope from Task 2. `info!` is already imported in `supervisor.rs`.)

- [ ] **Step 2: Build**

Run: `cargo build -p mur-agent-runtime`
Expected: compiles. If the borrow checker complains that `profile` no longer needs `mut` when `--model` is compiled out, it will not — the assignment inside the `if let` keeps `mut` live.

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs
git commit -m "feat(runtime): --model <ref> rebinds backend non-interactively"
```

---

## Task 4: Integration test for `--load`

**Files:**
- Create: `mur-agent-runtime/tests/load_muragent.rs`

The supervisor's `load_muragent_and_home` is private; test the observable effect through the public installer the same way `--load` does, asserting an agent home is materialized from a freshly-written `.muragent`.

- [ ] **Step 1: Write the test**

Create `mur-agent-runtime/tests/load_muragent.rs`:

```rust
//! `--load` materializes an agent home from a `.muragent`. Mirrors the
//! supervisor's load path (read archive → installer::install → agents/<slug>).

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::installer;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

#[test]
fn load_muragent_materializes_agent_home() {
    let tmp = tempfile::TempDir::new().unwrap();
    let pkg = tmp.path().join("coach.muragent");
    let mur_home = tmp.path().join("murhome");

    // Author a minimal signed .muragent.
    let mut profile = AgentProfile::default_for_tests();
    profile.name = "coach".into();
    profile.display_name = "Coach".into();
    let identity = AgentIdentity::generate();
    let manifest = build_manifest_from_profile(&profile, "1.0.0");
    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    MuragentWriter::new(manifest, profile_yaml, identity)
        .write(&pkg)
        .unwrap();

    // The load path: read + install into mur_home/agents/<slug>.
    let archive = MuragentArchive::read(&pkg).unwrap();
    let outcome = installer::install(&archive, &mur_home, "cli").unwrap();
    assert_eq!(outcome.manifest.agent.slug, "coach");

    let home = mur_home.join("agents").join("coach");
    assert!(home.join("profile.yaml").exists(), "profile.yaml extracted");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-agent-runtime --test load_muragent`
Expected: PASS. (If `MuragentWriter::new` / `write` signatures differ, align with the usage in `mur-common/src/muragent/writer.rs` tests — they construct the writer identically.)

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/tests/load_muragent.rs
git commit -m "test(runtime): --load materializes agent home from .muragent"
```

---

## Self-Review

**1. Spec coverage:** §8 toolchain-free developer replacement (`--load`) → Tasks 2 & 4; §7.5 non-interactive `--model` → Task 3; arg parsing prerequisite → Task 1. Guided resolution explicitly deferred to Plan 2 (see Scope). ✓

**2. Placeholder scan:** No "TBD"/"add handling"/"similar to". Every code step shows full code. ✓

**3. Type consistency:** `flag_value(&[String], &str) -> Option<String>` defined in Task 1 and called identically in Tasks 2 & 3. `load_muragent_and_home(&str, &Path) -> anyhow::Result<PathBuf>` returns the home used by `entrypoint`. `installer::install(&archive, mur_home, "cli") -> InstallOutcome` and `outcome.manifest.agent.slug` match `mur-core/src/cmd/agent/install.rs` usage. `profile.inner.model_ref: Option<String>` matches `AgentProfile` (`mur-common/src/agent.rs:59`). ✓

**Verification note:** `MuragentWriter::new(manifest, profile_yaml, identity)` + `.write(path)` is the constructor used by `mur-core/src/cmd/agent/export.rs::export_muragent`; if the writer test in `mur-common` uses a builder variant, copy that exact form. `AgentProfile::default_for_tests()` is available in `mur-common` test/dev builds (used by `writer.rs` tests); the integration test runs against `mur-common`'s public surface.
