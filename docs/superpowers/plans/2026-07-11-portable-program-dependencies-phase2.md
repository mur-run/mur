# Portable Program Dependencies — Phase 2 Implementation Plan (Trusted-Publisher Recipes)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a signed bundle from a trusted publisher carry an author-declared, per-platform program install recipe and auto-install it AT IMPORT (consent-gated), reusing the Phase 1 installer + the publisher-trust keyring.

**Architecture:** Add an optional `recipe` field to `ProgramDep` (per-platform, converts to the Phase 1 `CuratedRecipe`). A pure `trust_gate::decide(...)` function maps `(publisher trust, curated?, detect status, platform recipe?)` to a decision. Fleet import and agent `.muragent` install call it after their existing signature verification and, for a `Trusted` publisher, prompt+install via the Phase 1 `installer::install`. Everything else (detect, registry, installer, doctor) is unchanged.

**Tech Stack:** Rust (edition 2024). Reuse: `mur_common::deps::registry::{CuratedRecipe, ArchiveSpec}`, `mur-core` `installer::install`, `mur_common::skill::publisher_trust::{PublisherKeyring, PublisherTrust}`.

**Spec:** `docs/superpowers/specs/2026-07-11-portable-program-dependencies-phase2-design.md`

## Global Constraints

- **Reuse, don't rebuild** — no new download/verify/install code; call Phase 1 `installer::install`. No new trust store; use `PublisherKeyring`.
- **Cross-platform** — author recipes keyed by `<arch>-<os>`; only `current_platform()`'s entry installs.
- **Two required factors + consent** — author SHA-256 (integrity) + valid bundle signature & `classify == Trusted` (authorization) + per-install `[y/N]` (unless `--yes`).
- **Fail-closed** — `Revoked` → refuse; `Unknown` → detect-and-guide (never auto-install); curated key → curated wins (author recipe ignored for that name).
- **Non-blocking** — a declined/failed trusted-install never fails the import.
- Files ≤ 800 lines; brand "MUR"; comments English.
- **Build/test env (mur-core):** `export ORT_STRATEGY=download; export MUR_WEB_DIST=$HOME/Projects/mur-web/dist; export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`. mur-common needs none.

## File Structure

- Modify `mur-common/src/deps/mod.rs` — add `ProgramRecipe`, `PlatformRecipe`, `ProgramDep.recipe` field + `ProgramRecipe::for_platform() -> Option<CuratedRecipe>`.
- Create `mur-core/src/cmd/deps/trust_gate.rs` — pure `GateDecision` + `decide(...)`.
- Modify `mur-core/src/cmd/deps/mod.rs` — `pub mod trust_gate;` + an `install_trusted_recipes_at_import(...)` orchestrator (aggregate → decide → confirm → install).
- Modify `mur-core/src/cmd/fleet/import.rs` — call the orchestrator after signature verify.
- Modify `mur-core/src/cmd/agent/install.rs` — call the orchestrator after `.muragent` install (adaptive; see Task 5).
- Modify `docs/architecture/runtime-overview.md` + the Phase 1 design doc — document Phase 2.

---

### Task 1: `ProgramRecipe` / `PlatformRecipe` + `to_curated`

**Files:**
- Modify: `mur-common/src/deps/mod.rs`

**Interfaces:**
- Consumes: `mur_common::deps::registry::{CuratedRecipe, ArchiveSpec}` (Phase 1).
- Produces: `ProgramRecipe { platforms: BTreeMap<String, PlatformRecipe> }`; `PlatformRecipe { url, sha256, install_to: Option<String>, executable: bool, archive: Option<ArchiveSpec> }`; `ProgramRecipe::for_platform(&self, platform: &str) -> Option<CuratedRecipe>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod recipe_tests {
    use super::*;

    #[test]
    fn program_recipe_for_platform_converts_to_curated() {
        let y = r#"
platforms:
  aarch64-macos: { url: "https://x/tool", sha256: "abc123", install_to: "aura/tool", executable: true }
"#;
        let r: ProgramRecipe = serde_yaml::from_str(y).unwrap();
        let cur = r.for_platform("aarch64-macos").expect("platform present");
        assert_eq!(cur.url, "https://x/tool");
        assert_eq!(cur.sha256, "abc123");
        assert_eq!(cur.install_to.as_deref(), Some("aura/tool"));
        assert!(cur.executable);
        assert!(cur.archive.is_none());
        assert!(r.for_platform("sparc-solaris").is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common deps::recipe_tests`
Expected: FAIL (types undefined).

- [ ] **Step 3: Implement**

Add to `mur-common/src/deps/mod.rs` (it already has `use serde::{Deserialize, Serialize};` and `pub mod registry;`):

```rust
use std::collections::BTreeMap;

/// Author-declared, per-platform install recipe carried on a `ProgramDep`
/// inside a SIGNED bundle. Its integrity flows from the bundle signature; its
/// authorization from the publisher's trust classification at import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramRecipe {
    /// `<arch>-<os>` → recipe. Only the current platform's entry is installed.
    pub platforms: BTreeMap<String, PlatformRecipe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformRecipe {
    pub url: String,
    pub sha256: String,
    /// Bare-binary install target (relative to mur_home); `None` when `archive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_to: Option<String>,
    #[serde(default)]
    pub executable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<registry::ArchiveSpec>,
}

impl ProgramRecipe {
    /// Convert this recipe's entry for `platform` into the Phase 1
    /// `CuratedRecipe` the installer consumes. `None` if no entry for the
    /// platform. `description` is a fixed author-provenance label.
    pub fn for_platform(&self, platform: &str) -> Option<registry::CuratedRecipe> {
        let p = self.platforms.get(platform)?;
        Some(registry::CuratedRecipe {
            description: "author-declared recipe".to_string(),
            url: p.url.clone(),
            sha256: p.sha256.clone(),
            install_to: p.install_to.clone(),
            executable: p.executable,
            archive: p.archive.clone(),
        })
    }
}
```

**Interface note:** confirm `registry::CuratedRecipe`, `registry::ArchiveSpec`, and `RecipeMember` are `pub` and that `CuratedRecipe`'s fields are exactly `{ description: String, url: String, sha256: String, install_to: Option<String>, executable: bool, archive: Option<ArchiveSpec> }` (grep `pub struct CuratedRecipe` in `mur-common/src/deps/registry.rs`). If `CuratedRecipe` is not constructible outside `registry.rs` (private fields), add a `pub fn` constructor there or make the fields `pub` — the Phase 1 struct already has `pub` fields, so a direct literal should work.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common deps::recipe_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/deps/mod.rs
git commit -m "feat(deps): ProgramRecipe/PlatformRecipe + to-curated conversion"
```

---

### Task 2: `recipe` field on `ProgramDep`

**Files:**
- Modify: `mur-common/src/deps/mod.rs`

**Interfaces:**
- Produces: `ProgramDep.recipe: Option<ProgramRecipe>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn program_dep_parses_optional_recipe_and_defaults_none() {
    let with = r#"
name: some-tool
detect: { command: some-tool }
reason: "x"
recipe:
  platforms:
    aarch64-macos: { url: "u", sha256: "s", install_to: "aura/some-tool", executable: true }
"#;
    let d: ProgramDep = serde_yaml::from_str(with).unwrap();
    assert!(d.recipe.is_some());
    assert!(d.recipe.unwrap().for_platform("aarch64-macos").is_some());

    // Phase 1 dep without recipe → None (back-compat).
    let without = "name: x\ndetect: { command: x }\nreason: r\n";
    let d2: ProgramDep = serde_yaml::from_str(without).unwrap();
    assert!(d2.recipe.is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common program_dep_parses_optional_recipe`
Expected: FAIL (field missing).

- [ ] **Step 3: Implement**

Add to the `ProgramDep` struct in `mur-common/src/deps/mod.rs`:

```rust
    /// Author-declared install recipe (Phase 2). Present only in signed
    /// bundles; auto-installed at import ONLY from a trusted publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ProgramRecipe>,
```

Every existing `ProgramDep { ... }` struct-literal construction site (grep `ProgramDep {` across `mur-common/src` and `mur-core/src`) needs `recipe: None,` added. (Phase 1 test fixtures + the `aggregate` MCP-command synthesis in `mur-core/src/cmd/deps/mod.rs` construct `ProgramDep` literals — update them.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common 2>&1 | tail -5` then `cargo build -p mur-core` (with env) — fix every E0063 for the new field.
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/deps/mod.rs mur-core/src/cmd/deps/mod.rs
git commit -m "feat(deps): optional author recipe on ProgramDep"
```

---

### Task 3: Pure trust-gate decision

**Files:**
- Create: `mur-core/src/cmd/deps/trust_gate.rs`
- Modify: `mur-core/src/cmd/deps/mod.rs` (add `pub mod trust_gate;`)

**Interfaces:**
- Consumes: `mur_common::deps::{ProgramDep, DepStatus, registry::{CuratedRecipe, is_curated}}`, `mur_common::skill::publisher_trust::PublisherTrust`.
- Produces: `enum GateDecision { Offer(CuratedRecipe), SkipUntrusted, SkipRevoked, SkipCurated, SkipPresent, SkipNoRecipe, SkipNoPlatformRecipe }`; `fn decide(dep: &ProgramDep, trust: PublisherTrust, status: DepStatus, platform: &str) -> GateDecision`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::deps::{DetectMethod, DepStatus, ProgramDep, ProgramRecipe, PlatformRecipe};
    use mur_common::skill::publisher_trust::PublisherTrust;
    use std::collections::BTreeMap;

    fn dep_with_recipe(name: &str, plat: &str) -> ProgramDep {
        let mut platforms = BTreeMap::new();
        platforms.insert(plat.to_string(), PlatformRecipe {
            url: "u".into(), sha256: "s".into(), install_to: Some("aura/x".into()),
            executable: true, archive: None,
        });
        ProgramDep {
            name: name.into(),
            detect: DetectMethod::Command { command: name.into() },
            reason: "r".into(), hint: None, registry: None,
            recipe: Some(ProgramRecipe { platforms }),
        }
    }

    #[test]
    fn decision_table() {
        let d = dep_with_recipe("some-tool", "aarch64-macos");
        // Trusted + missing + has-platform-recipe + not-curated → Offer
        assert!(matches!(
            decide(&d, PublisherTrust::Trusted, DepStatus::Missing, "aarch64-macos"),
            GateDecision::Offer(_)
        ));
        // Unknown publisher → SkipUntrusted
        assert!(matches!(
            decide(&d, PublisherTrust::Unknown, DepStatus::Missing, "aarch64-macos"),
            GateDecision::SkipUntrusted
        ));
        // Revoked → SkipRevoked (even though Trusted-eligible otherwise)
        assert!(matches!(
            decide(&d, PublisherTrust::Revoked, DepStatus::Missing, "aarch64-macos"),
            GateDecision::SkipRevoked
        ));
        // Present → SkipPresent (never reinstall)
        assert!(matches!(
            decide(&d, PublisherTrust::Trusted, DepStatus::Present, "aarch64-macos"),
            GateDecision::SkipPresent
        ));
        // No recipe entry for this platform → SkipNoPlatformRecipe
        assert!(matches!(
            decide(&d, PublisherTrust::Trusted, DepStatus::Missing, "x86_64-windows"),
            GateDecision::SkipNoPlatformRecipe
        ));
        // No recipe at all → SkipNoRecipe
        let mut d_no = d.clone();
        d_no.recipe = None;
        assert!(matches!(
            decide(&d_no, PublisherTrust::Trusted, DepStatus::Missing, "aarch64-macos"),
            GateDecision::SkipNoRecipe
        ));
        // Curated name → SkipCurated (curated wins). "obscura" is a Phase-1 curated key.
        let mut d_cur = dep_with_recipe("obscura", "aarch64-macos");
        d_cur.registry = None; // name is the key
        assert!(matches!(
            decide(&d_cur, PublisherTrust::Trusted, DepStatus::Missing, "aarch64-macos"),
            GateDecision::SkipCurated
        ));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::trust_gate`
Expected: FAIL.

- [ ] **Step 3: Implement**

Create `mur-core/src/cmd/deps/trust_gate.rs`:

```rust
//! Pure decision for whether to offer a trusted-publisher recipe install at
//! import. No I/O — the caller supplies trust classification + detect status.

use mur_common::deps::registry::{is_curated, CuratedRecipe};
use mur_common::deps::{DepStatus, ProgramDep};
use mur_common::skill::publisher_trust::PublisherTrust;

#[derive(Debug)]
pub enum GateDecision {
    /// Offer to install this (already platform-resolved) recipe.
    Offer(CuratedRecipe),
    /// Publisher not in the keyring — detect-and-guide only.
    SkipUntrusted,
    /// Publisher revoked — refuse.
    SkipRevoked,
    /// Name is a MUR-curated key — curated wins, handled by install-deps.
    SkipCurated,
    /// Already present — never reinstall.
    SkipPresent,
    /// No author recipe declared.
    SkipNoRecipe,
    /// Recipe declared but no entry for the current platform.
    SkipNoPlatformRecipe,
}

/// Decide the gate for one dep. Precedence: present → curated → no-recipe →
/// no-platform → trust (revoked/unknown/trusted).
pub fn decide(
    dep: &ProgramDep,
    trust: PublisherTrust,
    status: DepStatus,
    platform: &str,
) -> GateDecision {
    if status == DepStatus::Present {
        return GateDecision::SkipPresent;
    }
    let key = dep.registry.as_deref().unwrap_or(&dep.name);
    if is_curated(key) {
        return GateDecision::SkipCurated;
    }
    let Some(recipe) = &dep.recipe else {
        return GateDecision::SkipNoRecipe;
    };
    let Some(curated) = recipe.for_platform(platform) else {
        return GateDecision::SkipNoPlatformRecipe;
    };
    match trust {
        PublisherTrust::Revoked => GateDecision::SkipRevoked,
        PublisherTrust::Unknown => GateDecision::SkipUntrusted,
        PublisherTrust::Trusted => GateDecision::Offer(curated),
    }
}
```

Add `pub mod trust_gate;` to `mur-core/src/cmd/deps/mod.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::trust_gate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/trust_gate.rs mur-core/src/cmd/deps/mod.rs
git commit -m "feat(deps): pure trust-gate decision for author recipes"
```

---

### Task 4: Import orchestrator + fleet-import hook

**Files:**
- Modify: `mur-core/src/cmd/deps/mod.rs` (add `install_trusted_recipes_at_import`)
- Modify: `mur-core/src/cmd/fleet/import.rs` (call it after signature verify)

**Interfaces:**
- Consumes: `aggregate_agent`/`aggregate_fleet` (Phase 1), `mur_common::deps::detect::detect`, `current_platform`, `trust_gate::{decide, GateDecision}`, `installer::install`, `PublisherKeyring`, a local `confirm`.
- Produces: `async fn install_trusted_recipes_at_import(mur_home: &Path, deps: &[AggregatedDep], signer_fp: &str, publisher_label: &str, yes: bool)`.

- [ ] **Step 1: Write the failing test** (the orchestrator is I/O-heavy; test the message/label helper that formats an untrusted-skip line, which is pure)

```rust
#[cfg(test)]
mod import_hook_tests {
    use super::*;
    #[test]
    fn untrusted_hint_names_publisher_and_signer_trust_cmd() {
        let msg = untrusted_skip_line("some-tool", "abcd1234", Some("https://x/tool"));
        assert!(msg.contains("some-tool"));
        assert!(msg.contains("abcd1234"));
        assert!(msg.contains("signer-trust"));
        assert!(msg.contains("https://x/tool"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::import_hook_tests`
Expected: FAIL.

- [ ] **Step 3: Implement the orchestrator**

Append to `mur-core/src/cmd/deps/mod.rs`:

```rust
use mur_common::skill::publisher_trust::PublisherKeyring;

/// One-line guidance for a recipe from an untrusted publisher.
pub fn untrusted_skip_line(name: &str, signer_fp: &str, hint: Option<&str>) -> String {
    let tail = hint.map(|h| format!("; or install manually: {h}")).unwrap_or_default();
    format!(
        "{name}: recipe from an untrusted publisher — run `mur agent skill signer-trust {signer_fp}` to trust it{tail}"
    )
}

/// Local y/N (mirrors fleet/import.rs `confirm`).
fn confirm(prompt: &str, yes: bool) -> anyhow::Result<bool> {
    use anyhow::Context;
    if yes { return Ok(true); }
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).context("read stdin")?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// At import of a signed bundle: for each missing, non-curated dep with an
/// author recipe for the current platform, gate on the publisher's trust and
/// (if Trusted) prompt+install via the Phase 1 installer. Best-effort — never
/// returns Err into the import path.
pub async fn install_trusted_recipes_at_import(
    mur_home: &std::path::Path,
    deps: &[AggregatedDep],
    signer_fp: &str,
    publisher_label: &str,
    yes: bool,
) {
    let keyring = match PublisherKeyring::load_or_seed(mur_home) {
        Ok(k) => k,
        Err(_) => return,
    };
    let trust = keyring.classify(signer_fp);
    let platform = mur_common::deps::current_platform();
    for a in deps {
        let status = mur_common::deps::detect::detect(&a.dep, mur_home);
        match crate::cmd::deps::trust_gate::decide(&a.dep, trust, status, &platform) {
            crate::cmd::deps::trust_gate::GateDecision::Offer(recipe) => {
                let prompt = format!(
                    "Install {} from {} (signed by {publisher_label}, sha256 {}) ?",
                    a.dep.name, recipe.url, recipe.sha256
                );
                match confirm(&prompt, yes) {
                    Ok(true) => match crate::cmd::deps::installer::install(&recipe, mur_home).await {
                        Ok(paths) => println!("  installed {} -> {:?}", a.dep.name, paths),
                        Err(e) => println!("  FAILED {}: {e}", a.dep.name),
                    },
                    Ok(false) => println!("  skipped {}.", a.dep.name),
                    Err(_) => {}
                }
            }
            crate::cmd::deps::trust_gate::GateDecision::SkipUntrusted => {
                println!("  {}", untrusted_skip_line(&a.dep.name, signer_fp, a.dep.hint.as_deref()));
            }
            crate::cmd::deps::trust_gate::GateDecision::SkipRevoked => {
                println!("  {}: publisher {signer_fp} is REVOKED — not installing.", a.dep.name);
            }
            // Present / curated / no-recipe / no-platform → silent (Phase 1 doctor covers them).
            _ => {}
        }
    }
}
```

Wire into `mur-core/src/cmd/fleet/import.rs`: after the signature is verified and `derived_fp` is known and the fleet is installed, add a best-effort call (async — `cmd_fleet_import` may be sync; if so, block on it with the codebase's runtime helper, or make the preflight a `tokio` block — grep how other async is called from this sync command, e.g. the Phase 1 preflight, and match it):

```rust
// Phase 2: trusted-publisher recipe install (best-effort, non-blocking).
if let Ok(deps) = crate::cmd::deps::aggregate_fleet(mur_home, &fleet_name) {
    // publisher_label: the manifest's publisher name if present, else the fp.
    let label = /* manifest.publisher.name or derived_fp */;
    <runtime>.block_on(crate::cmd::deps::install_trusted_recipes_at_import(
        mur_home, &deps, &derived_fp, &label, opts.yes,
    ));
}
```

**Interface note:** `cmd_fleet_import` is currently sync (`pub fn`). Determine how to run the async installer — grep the file / dispatch for an existing `Runtime`/`block_on` pattern; the Phase 1 import preflight (Task 9 of Phase 1) added a report here — place this call right after it, reusing whatever async-bridging the surrounding code uses (or `tokio::runtime::Runtime::new()?.block_on(...)` best-effort). `opts.yes` — confirm the import `ImportOpts` has a `yes`/`force` field; if only `--force` exists, thread a `yes` bool from the CLI or default to interactive (false).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::import_hook_tests` then `cargo build -p mur-core`.
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/mod.rs mur-core/src/cmd/fleet/import.rs
git commit -m "feat(deps): trusted-recipe install at fleet import"
```

---

### Task 5: Agent `.muragent` install hook (adaptive)

**Files:**
- Modify: `mur-core/src/cmd/agent/install.rs`

**Interfaces:**
- Consumes: the `install_trusted_recipes_at_import` orchestrator (Task 4); the `.muragent` install outcome's signer fingerprint + installed agent name.

- [ ] **Step 1: Investigate the muragent install outcome**

The agent bundle install runs through `mur_common::muragent::installer` and returns an `InstallOutcome`. Grep to find: (a) the installed agent's name, (b) the signer fingerprint / whether the signer was trusted. Run:
```
grep -n "struct InstallOutcome\|signer\|fingerprint\|trust\|pub name\|pub fn install" mur-common/src/muragent/installer.rs mur-common/src/muragent/manifest.rs
```
If the outcome exposes the signer fingerprint + agent name, proceed. If it does NOT surface the signer fp cleanly, STOP and report DONE_WITH_CONCERNS: implement the fleet path only (Task 4) and note agent-import as a follow-up requiring the muragent installer to expose the signer fp.

- [ ] **Step 2: Wire the hook (if the outcome exposes the fp)**

After a successful `mur agent install <path>.muragent` in `mur-core/src/cmd/agent/install.rs`'s command function, add a best-effort call, mirroring Task 4's fleet wiring but with `aggregate_agent(mur_home, &installed_name)`:
```rust
if let Ok(deps) = crate::cmd::deps::aggregate_agent(mur_home, &installed_name) {
    <runtime>.block_on(crate::cmd::deps::install_trusted_recipes_at_import(
        mur_home, &deps, &signer_fp, &publisher_label, /* yes */ false,
    ));
}
```
Use the signer fp + agent name from the outcome. Non-blocking (a failure never fails the install).

- [ ] **Step 3: Verify**

Run: `cargo build -p mur-core` (compiles). If a focused test is feasible without a real signed bundle, add one; otherwise rely on the Task 3/4 unit tests + the build (the wiring is a few lines).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/install.rs
git commit -m "feat(deps): trusted-recipe install at agent .muragent install"
```

(If Step 1 concluded the outcome doesn't expose the fp: commit nothing here; report the follow-up.)

---

### Task 6: Docs

**Files:**
- Modify: `docs/architecture/runtime-overview.md` (Portable program dependencies section — add the Phase 2 trusted-publisher path)
- Modify: `docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md` (mark §7.2 as implemented)

- [ ] **Step 1:** In the runtime-overview "Portable program dependencies" section, add a short paragraph: a signed bundle from a **trusted** publisher (in `~/.mur/trust/publishers.yaml`) may carry an author-declared `recipe` per program; at **import**, MUR offers to install it (per-item consent showing publisher + url + sha256 + target). Unknown publisher → detect-and-guide (with a `signer-trust` hint); revoked → refused. Trust is anchored at import; `install-deps` stays curated-only.

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/runtime-overview.md docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md
git commit -m "docs(deps): Phase 2 trusted-publisher recipes"
```

---

## Self-Review

**Spec coverage:** §4 recipe field → T1+T2; §6 `ProgramRecipe`/`to_curated` → T1; trust-gate decision table (§5, §8) → T3; import orchestrator + fleet hook (§5) → T4; agent import (§5) → T5 (adaptive); §7 security is enforced by the reused installer + the decision precedence (revoked/unknown never Offer); docs → T6. ✓

**Placeholder scan:** The `<runtime>.block_on`, `label`, and `opts.yes` in T4 Step 3 are flagged interface-confirmations (grep the real async-bridge + ImportOpts field), not code placeholders — the surrounding logic is complete. T5 is deliberately adaptive with an explicit STOP-and-report branch (the muragent outcome shape is unknown until grepped) — an honest investigation task, not a vague one.

**Type consistency:** `ProgramRecipe`/`PlatformRecipe`/`ProgramRecipe::for_platform` (T1) → `ProgramDep.recipe` (T2) → `trust_gate::decide` (T3) → orchestrator (T4). `GateDecision::Offer(CuratedRecipe)` feeds `installer::install(&CuratedRecipe, mur_home)` (Phase 1, unchanged). `PublisherTrust::{Trusted,Revoked,Unknown}` matches the keyring enum verbatim.

**Interface confirmations flagged for the implementer (not placeholders):** `CuratedRecipe` field visibility (T1); every `ProgramDep {` literal site (T2); the async-bridge + `ImportOpts.yes` in fleet import (T4); the muragent `InstallOutcome` signer-fp exposure (T5, with a defined fallback).
