# Skill Bundled Assets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve a skill's sibling files (`scripts/`, assets) on import and tell the running agent where its skill lives on disk, so bundled scripts resolve and run.

**Architecture:** Three isolated changes. (1) Import copies non-`SKILL.md` siblings into the installed skill dir with path-escape rejection. (2) `LoadedSkill` gains a `dir` field populated by the loader. (3) Layer-3 injection appends a one-line on-disk path hint when the skill dir carries a bundle. No new subsystem, no new CLI verb, no new security model — execution rides the agent's existing bash tool + trust gates.

**Tech Stack:** Rust (edition 2024), `mur-core` (import), `mur-common` (loader/`LoadedSkill`), `mur-agent-runtime` (injection). Tests via `cargo nextest`.

## Global Constraints

- Rust edition 2024.
- **No hardcoded values.** No hostile-extension blocklist (no threat basis); the shallow scan is path-escape + non-regular-file rejection only.
- `mur-core` / `mur-common` test compile needs env: `ORT_STRATEGY=download` and `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Prefix every test command with them.
- Use `cargo nextest run`, **not** `cargo test` (plain `cargo test --workspace` fails ~7 tests spuriously in this repo).
- Path-safety: a bundle entry must never write outside its own install dir. Reject `..` components, absolute paths, and symlinks resolving outside dest.
- Nothing executes at import or injection time.
- Single source file ≤ 800 lines — if `import.rs` crosses it, split the copy helper into a sibling module.

**Env prefix used throughout (call it `$ENV`):**
```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
```

---

### Task 1: Import preserves skill siblings with path-safety

**Files:**
- Modify: `mur-core/src/cmd/agent/addon/import.rs` (phase-1 collect loop ~115-137; phase-2 write loop ~205-212; add `copy_bundle` helper + tests)

**Interfaces:**
- Consumes: existing `safe_member_name(&str) -> Result<()>`, `write_to_dir(&Path, &SkillManifest)`.
- Produces: `fn copy_bundle(src_dir: &Path, dest_dir: &Path) -> anyhow::Result<()>` — copies every entry of `src_dir` except a top-level `SKILL.md` into `dest_dir`, recursively; returns `Err` on any path escape or symlink.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `import.rs`:

```rust
#[test]
fn copy_bundle_preserves_scripts_skips_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(src.join("scripts")).unwrap();
    std::fs::write(src.join("SKILL.md"), "body").unwrap();
    std::fs::write(src.join("scripts/start-server.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(src.join("helper.js"), "x").unwrap();

    copy_bundle(&src, &dest).unwrap();

    assert!(dest.join("scripts/start-server.sh").is_file());
    assert!(dest.join("helper.js").is_file());
    assert!(!dest.join("SKILL.md").exists(), "SKILL.md must not be copied");
}

#[test]
fn copy_bundle_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    let secret = tmp.path().join("secret.txt");
    std::fs::write(&secret, "s").unwrap();
    std::os::unix::fs::symlink(&secret, src.join("link")).unwrap();

    let err = copy_bundle(&src, &dest);
    assert!(err.is_err(), "symlink in bundle must be rejected");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$ENV cargo nextest run -p mur-core copy_bundle`
Expected: FAIL — `cannot find function copy_bundle`.

- [ ] **Step 3: Implement `copy_bundle`**

Add near the other free functions in `import.rs` (after `safe_member_name`):

```rust
/// Recursively copy every entry of `src_dir` into `dest_dir`, skipping a
/// top-level `SKILL.md` (the manifest is written separately). Rejects any
/// symlink and any entry whose name is unsafe, so a bundle can never write
/// outside its own install directory.
fn copy_bundle(src_dir: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    copy_bundle_inner(src_dir, dest_dir, true)
}

fn copy_bundle_inner(src: &Path, dest: &Path, top: bool) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_str().context("non-UTF8 bundle filename")?;
        if top && name_str == "SKILL.md" {
            continue;
        }
        // Reject `.`, `..`, path separators, absolute-ish names.
        if name_str == "." || name_str == ".." || name_str.contains('/') {
            anyhow::bail!("unsafe bundle entry name: {name_str}");
        }
        // Reject symlinks outright (escape vector).
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            anyhow::bail!("bundle contains a symlink ({name_str}); refusing import");
        }
        let src_path = entry.path();
        let dest_path = dest.join(name_str);
        if ft.is_dir() {
            fs::create_dir_all(&dest_path)?;
            copy_bundle_inner(&src_path, &dest_path, false)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}
```

Ensure `use anyhow::Context;` and `use std::path::Path;` are present (add if missing).

- [ ] **Step 4: Run tests to verify they pass**

Run: `$ENV cargo nextest run -p mur-core copy_bundle`
Expected: PASS (2 tests).

- [ ] **Step 5: Carry the source dir into phase 2 and call `copy_bundle`**

Change the pending vec to carry the source skill dir. At `import.rs:110`:

```rust
    let mut pending_skills: Vec<(PathBuf, SkillManifest, PathBuf)> = Vec::new();
```

In the phase-1 skills loop (~135), push the source dir `d`:

```rust
            pending_skills.push((dest, manifest, d.clone()));
```

In the phase-2 write loop (~205), copy the bundle after writing the manifest:

```rust
    for (dest, manifest, src_dir) in pending_skills {
        write_to_dir(&dest, &manifest)
            .with_context(|| format!("writing skill '{}'", manifest.name))?;
        copy_bundle(&src_dir, &dest)
            .with_context(|| format!("copying bundle for skill '{}'", manifest.name))?;
    }
```

(Leave the `pending_cmds` loop unchanged — command skills carry no bundle in this first cut.)

- [ ] **Step 6: Write the end-to-end import test**

Add a test that drives the real import entry point with a skill dir containing `scripts/`. Follow the existing import-test pattern in this file (the one using `fs::create_dir_all(root.join("skills/brainstorm"))` at ~459). Assert the installed dest dir contains the script:

```rust
#[test]
fn import_installs_skill_bundle_scripts() {
    // Arrange a minimal plugin root with one skill that ships a script,
    // mirroring the existing import test setup in this module, then invoke
    // the same import function those tests call.
    // After import, assert:
    //   agents/<agent>/skills/<name>/scripts/run.sh exists.
    // (Reuse the harness helper the sibling import tests use; do not
    //  hand-roll a second harness.)
}
```

Fill the body by copying the arrange/act lines from the nearest existing import test in the file and adding a `scripts/run.sh` to the source skill dir. Assert the file lands under the installed dir.

- [ ] **Step 7: Run the full import test module**

Run: `$ENV cargo nextest run -p mur-core addon::import`
Expected: PASS (existing tests + the 3 new ones).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/agent/addon/import.rs
git commit -m "feat(skill): preserve skill bundle siblings on import"
```

---

### Task 2: `LoadedSkill` carries its on-disk directory

**Files:**
- Modify: `mur-common/src/skill/loader.rs` (`LoadedSkill` struct ~35; `load_all` ~43-76; test helper `make`/callers)
- Modify: `mur-agent-runtime/src/skills/injector.rs:140` (`loaded` test helper)
- Modify: `mur-agent-runtime/src/skills/trigger_matcher.rs:139` (`sample` test helper)

**Interfaces:**
- Consumes: `agent_skills_dir(mur_home, agent) -> PathBuf`, `global_skill_dir(mur_home, name) -> PathBuf` (both in `mur_common::skill::store`).
- Produces: `LoadedSkill.dir: PathBuf` — absolute path of the skill's install directory. Every construction site sets it.

- [ ] **Step 1: Write the failing test**

Add to `loader.rs` tests:

```rust
#[test]
fn load_all_sets_agent_skill_dir() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let sdir = home.join("agents").join("a1").join("skills").join("demo");
    write_to_dir(&sdir, &make("demo")).unwrap();

    let loaded = load_all(home, "a1");
    let demo = loaded.iter().find(|s| s.name == "demo").unwrap();
    assert_eq!(demo.dir, sdir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `$ENV cargo nextest run -p mur-common load_all_sets_agent_skill_dir`
Expected: FAIL — `no field dir on LoadedSkill`.

- [ ] **Step 3: Add the field**

In `LoadedSkill` (loader.rs:35):

```rust
pub struct LoadedSkill {
    pub name: String,
    pub manifest: SkillManifest,
    pub trust: TrustLevel,
    pub scope: SkillScope,
    pub content_hash: String,
    /// Absolute install directory of this skill (holds skill.yaml + any bundle).
    pub dir: std::path::PathBuf,
}
```

- [ ] **Step 4: Populate `dir` in `load_all`**

`load_one` doesn't know the agent name, so set `dir` in `load_all` after each `load_one` returns. In the agent loop (~51-56):

```rust
            if let Some(mut loaded) = load_one(mur_home, &name, SkillScope::Agent, &trust, |m, n| {
                local::load_installed_agent(m, agent_name, n)
            }) {
                loaded.dir = crate::skill::store::agent_skills_dir(mur_home, agent_name).join(&name);
                seen_names.insert(loaded.name.clone());
                out.push(loaded);
            }
```

In the global loop (~64-71):

```rust
            if let Some(mut loaded) = load_one(
                mur_home, &name, SkillScope::Global, &trust, local::load_installed,
            ) {
                loaded.dir = crate::skill::store::global_skill_dir(mur_home, &name);
                out.push(loaded);
            }
```

In `load_one`, set a placeholder so both construction sites compile:

```rust
        Some(LoadedSkill {
            name: name.into(),
            manifest,
            trust: pinned.level,
            scope,
            content_hash: hash,
            dir: std::path::PathBuf::new(), // overwritten by load_all
        })
```

Do the same for the unpinned `LoadedSkill { .. }` branch.

- [ ] **Step 5: Fix the two runtime test helpers**

`injector.rs:140` `loaded(...)` and `trigger_matcher.rs:139` `sample()` build `LoadedSkill` literals — add `dir: std::path::PathBuf::new(),` to each so they compile.

- [ ] **Step 6: Run tests to verify they pass**

Run: `$ENV cargo nextest run -p mur-common -p mur-agent-runtime skill`
Expected: PASS (new test + existing skill tests compile and pass).

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/skill/loader.rs \
        mur-agent-runtime/src/skills/injector.rs \
        mur-agent-runtime/src/skills/trigger_matcher.rs
git commit -m "feat(skill): LoadedSkill carries its install directory"
```

---

### Task 3: Layer-3 injection appends the bundle path hint

**Files:**
- Modify: `mur-agent-runtime/src/skills/trigger_matcher.rs` (add `bundle_hint` helper + test)
- Modify: `mur-agent-runtime/src/task_runner.rs:546-550` (append hint to body)

**Interfaces:**
- Consumes: `LoadedSkill.dir` (Task 2), `layer3_body`, `format_layer3`.
- Produces: `fn bundle_hint(dir: &Path) -> Option<String>` — `Some(line)` if `dir` contains any entry besides `skill.yaml`; else `None`.

- [ ] **Step 1: Write the failing test**

Add to `trigger_matcher.rs` tests:

```rust
#[test]
fn bundle_hint_present_when_extra_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("skill.yaml"), "x").unwrap();
    std::fs::create_dir_all(tmp.path().join("scripts")).unwrap();
    let hint = bundle_hint(tmp.path()).unwrap();
    assert!(hint.contains(&tmp.path().display().to_string()));
    assert!(hint.contains("scripts"));
}

#[test]
fn bundle_hint_absent_when_only_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("skill.yaml"), "x").unwrap();
    assert!(bundle_hint(tmp.path()).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$ENV cargo nextest run -p mur-agent-runtime bundle_hint`
Expected: FAIL — `cannot find function bundle_hint`.

- [ ] **Step 3: Implement `bundle_hint`**

Add to `trigger_matcher.rs` (with `use std::path::Path;` present):

```rust
/// If the skill's install directory holds anything besides `skill.yaml`,
/// return a one-line hint telling the agent where the bundle lives so paths
/// like `scripts/start-server.sh` resolve. Returns None for asset-free skills.
pub fn bundle_hint(dir: &Path) -> Option<String> {
    let mut has_bundle = false;
    for entry in std::fs::read_dir(dir).ok()? {
        let name = entry.ok()?.file_name();
        if name != "skill.yaml" {
            has_bundle = true;
            break;
        }
    }
    if !has_bundle {
        return None;
    }
    Some(format!(
        "\n\nBundled files for this skill are on disk at: {0}\n\
         (e.g. run a script with the bash tool: `bash {0}/scripts/<file>`)",
        dir.display()
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `$ENV cargo nextest run -p mur-agent-runtime bundle_hint`
Expected: PASS (2 tests).

- [ ] **Step 5: Wire the hint into injection**

In `task_runner.rs`, at the layer-3 build (~546-550), append the hint to `body` before formatting:

```rust
            let Some(mut body) = layer3_body(&loaded.manifest, &inventory) else {
                continue;
            };
            if let Some(hint) = crate::skills::trigger_matcher::bundle_hint(&loaded.dir) {
                body.push_str(&hint);
            }
            layer3.push('\n');
            layer3.push_str(&format_layer3(&loaded.name, loaded.trust, &body));
```

- [ ] **Step 6: Run the runtime skill tests**

Run: `$ENV cargo nextest run -p mur-agent-runtime skills`
Expected: PASS.

- [ ] **Step 7: Full check + clippy**

Run: `$ENV cargo nextest run -p mur-core -p mur-common -p mur-agent-runtime`
Run: `$ENV cargo clippy -p mur-core -p mur-common -p mur-agent-runtime -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/skills/trigger_matcher.rs \
        mur-agent-runtime/src/task_runner.rs
git commit -m "feat(skill): inject bundle path hint for skills that ship assets"
```

---

### Task 4: Manual end-to-end verification (concierge `mur`)

**Files:** none (verification only).

- [ ] **Step 1: Re-import a bundled skill**

```bash
mur agent skill remove mur brainstorming 2>/dev/null || true
mur agent skill import mur <path-to-superpowers/skills/brainstorming-plugin-root>
```
(Use whatever the existing import subcommand is — check `mur agent skill --help`.)

- [ ] **Step 2: Confirm the bundle landed on disk**

```bash
ls -R ~/.mur/agents/mur/skills/brainstorming
```
Expected: `skill.yaml` **plus** `scripts/start-server.sh`, `helper.js`, etc.

- [ ] **Step 3: Confirm the path hint reaches the agent**

Trigger the skill in a `mur agent cli mur` session and confirm the injected skill text contains the `Bundled files for this skill are on disk at: …` line. If the agent can `bash <dir>/scripts/start-server.sh`, parity is proven for the concierge.

- [ ] **Step 4: Note the documented limitation**

No action — confirm the spec's non-goal holds: a headless agent runs the script but shows no browser. Nothing to fix.

---

## Self-Review

**Spec coverage:**
- §1 Storage (assets beside skill.yaml) → falls out of Task 1 (copy into dest dir).
- §2 Import preserve + path-safety → Task 1 (`copy_bundle`, symlink/escape rejection, source-dir plumbing).
- §2 shallow scan → Task 1: implemented as symlink + unsafe-name rejection (the honest form; no extension blocklist, per Global Constraints).
- §3 Injection resolved line → Task 3 (`bundle_hint` + wiring); depends on Task 2 (`LoadedSkill.dir`).
- §4 Security (reuse trust/HITL, path-safety new check, no auto-run) → Task 1 path-safety; no execution added anywhere.
- §5 Testing (import-preserve, import-reject, inject-present, inject-absent) → Task 1 steps 1/6, Task 3 step 1.
- §6 Non-goals → nothing in the plan adds a CLI verb, sandbox, or browser-forwarding.

**Placeholder scan:** Task 1 Step 6 and Task 4 reference "the existing import-test harness / import subcommand" rather than inlining — deliberate, because the exact harness helper and subcommand name must be read from the file at execution time; the step says which sibling to copy. All code-bearing steps show real code.

**Type consistency:** `copy_bundle(&Path, &Path) -> Result<()>`, `LoadedSkill.dir: PathBuf`, `bundle_hint(&Path) -> Option<String>` used identically across definition and call sites. `pending_skills` tuple widened to 3-arity consistently in push and consume.
