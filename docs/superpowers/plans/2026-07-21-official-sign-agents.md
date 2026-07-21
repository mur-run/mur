# Official Catalog `official-sign` (Agents) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone CI-only Rust binary `official-sign` that builds a `distribution: official`, official-key-signed `.muragent` package from reviewable plaintext source, verifies it, and emits a catalog `index.json` entry — the core of the publish pipeline, with no publish capability shipped in the `mur` binary.

**Architecture:** New crate `official-sign` (destined for the private `mur-run/official-catalog` repo) depending on `mur-common`. It reads an agent source directory (`profile.yaml`, `prompt.md`, `skills/`, optional icon) plus a `catalog.yaml` entry, builds the package via `mur_common::muragent::writer::MuragentWriter` using the official Ed25519 key as the signing identity, stamps `distribution: "official"` into the manifest before signing, then re-verifies with `mur_common::muragent::validator` and asserts the signer fingerprint equals an expected value. The expected fingerprint is a parameter (real pinned constant in production; the test key's fingerprint in tests) — the same injectable-fp seam used by the shipped import gates.

**Tech Stack:** Rust 2024, `mur-common` (path dep during dev, crates.io/git for the private repo), `clap` (CLI), `serde`/`serde_yaml_ng` (catalog.yaml), `sha2`/`hex` (index integrity), `tempfile` (test fixtures + key loading), `anyhow` (errors).

## Global Constraints

- Rust edition 2024. No hardcoded values — the expected official fingerprint is `mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP` in production, injectable in tests.
- Brand name user-facing is uppercase **MUR**; internal identifiers/paths stay lowercase.
- The `mur` binary gains NO publish capability — all publish logic lives only in this crate.
- The trust root is real: no fake key ever appears in a published artifact. Tests use a throwaway generated key and assert against *that key's* fingerprint via the injectable parameter.
- TDD: every task writes a failing test first, verifies red, implements minimally, verifies green, commits.
- Build/test env for the `mur-common` path dependency during dev: `ORT_STRATEGY=download` (mur-common links onnxruntime transitively). Use `cargo test` (this crate is small and standalone — not the mur workspace's nextest gotchas).
- Scope: **agents (`.muragent`) only.** Fleets require lifting `build_bundle_bytes` from `mur-core/src/cmd/fleet/export.rs` into `mur-common` first (out of scope — see Follow-ups).

---

### Task 1: Scaffold the `official-sign` crate

**Files:**
- Create: `official-sign/Cargo.toml`
- Create: `official-sign/src/main.rs`
- Create: `official-sign/src/lib.rs`

**Interfaces:**
- Produces: a compiling binary crate with a `lib.rs` exposing an (initially empty) `pub mod catalog; pub mod sign; pub mod index;` module tree that later tasks fill in; `main.rs` parses CLI args into a `Cli` struct.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "official-sign"
version = "0.1.0"
edition = "2024"

[dependencies]
# During development, path dep to the local mur checkout. For the private
# repo, replace with: mur-common = { git = "https://github.com/mur-run/mur", tag = "vX.Y.Z" }
mur-common = { path = "../mur/mur-common" }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_yaml_ng = "0.10"
serde_json = "1"
sha2 = "0.10"
hex = "0.4"
anyhow = "1"
tempfile = "3"
```

- [ ] **Step 2: Write `src/lib.rs`**

```rust
pub mod catalog;
pub mod index;
pub mod sign;
```

- [ ] **Step 3: Write `src/main.rs` with the CLI skeleton**

```rust
use clap::Parser;

/// CI-only publisher: build + sign an official .muragent from source.
#[derive(Parser, Debug)]
#[command(name = "official-sign")]
struct Cli {
    /// Catalog id to build, e.g. `agents/researcher`.
    #[arg(long)]
    id: String,
    /// Directory of reviewable agent source (profile.yaml, prompt.md, skills/, icon).
    #[arg(long)]
    source_dir: std::path::PathBuf,
    /// catalog.yaml describing this item's metadata.
    #[arg(long)]
    catalog: std::path::PathBuf,
    /// Output directory for the signed bundle + index entry.
    #[arg(long)]
    out_dir: std::path::PathBuf,
    /// Path to the official identity.key (32-byte Ed25519 secret).
    #[arg(long)]
    key: std::path::PathBuf,
    /// Expected signer fingerprint. Omit to use the pinned official fp.
    #[arg(long)]
    expect_fp: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    println!("official-sign: not yet implemented");
    Ok(())
}
```

- [ ] **Step 4: Create empty module files**

Create `official-sign/src/catalog.rs`, `official-sign/src/index.rs`, `official-sign/src/sign.rs` each containing only a doc comment line (e.g. `//! catalog.yaml parsing.`).

- [ ] **Step 5: Verify it compiles**

Run: `cd official-sign && ORT_STRATEGY=download cargo build`
Expected: compiles; `cargo run -- --help` prints the arg list.

- [ ] **Step 6: Commit**

```bash
git add official-sign/Cargo.toml official-sign/src
git commit -m "chore(official-sign): scaffold CI-only publisher crate"
```

---

### Task 2: Parse `catalog.yaml`

**Files:**
- Modify: `official-sign/src/catalog.rs`

**Interfaces:**
- Produces:
  - `pub struct CatalogEntry { pub id: String, pub kind: String, pub name: String, pub version: String, pub tier: String, pub description: String }` (all `Deserialize`)
  - `pub fn load_entry(catalog_path: &Path, id: &str) -> anyhow::Result<CatalogEntry>` — parses `{items: [CatalogEntry...]}` and returns the entry whose `id` matches, erroring if absent.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_matching_entry() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "items:\n  - id: agents/researcher\n    kind: agent\n    name: researcher\n    version: 1.0.0\n    tier: pro\n    description: d\n").unwrap();
        let e = load_entry(f.path(), "agents/researcher").unwrap();
        assert_eq!(e.name, "researcher");
        assert_eq!(e.tier, "pro");
    }

    #[test]
    fn errors_on_missing_id() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "items: []\n").unwrap();
        assert!(load_entry(f.path(), "agents/nope").is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test catalog::`
Expected: FAIL (compile error — `CatalogEntry`/`load_entry` missing).

- [ ] **Step 3: Implement**

```rust
//! catalog.yaml parsing.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub version: String,
    pub tier: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
struct Catalog {
    items: Vec<CatalogEntry>,
}

pub fn load_entry(catalog_path: &Path, id: &str) -> Result<CatalogEntry> {
    let text = std::fs::read_to_string(catalog_path)
        .with_context(|| format!("read {}", catalog_path.display()))?;
    let cat: Catalog = serde_yaml_ng::from_str(&text).context("parse catalog.yaml")?;
    cat.items
        .into_iter()
        .find(|e| e.id == id)
        .with_context(|| format!("no catalog entry with id '{id}'"))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test catalog::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/catalog.rs
git commit -m "feat(official-sign): parse catalog.yaml entries"
```

---

### Task 3: Load the official signing identity from a key file

**Files:**
- Modify: `official-sign/src/sign.rs`

**Interfaces:**
- Consumes: `mur_common::identity::AgentIdentity` (`load(dir)` expects a directory containing `identity.key` = 32 raw Ed25519 secret bytes).
- Produces: `pub fn load_identity(key_path: &Path) -> anyhow::Result<mur_common::identity::AgentIdentity>` — copies the 32-byte key into a temp dir as `identity.key` and calls `AgentIdentity::load`, so the exact on-disk format is reused rather than reconstructed.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_identity_from_raw_key_bytes() {
        let gen = mur_common::identity::AgentIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        gen.save(dir.path()).unwrap();
        // load_identity takes the identity.key file path directly
        let loaded = load_identity(&dir.path().join("identity.key")).unwrap();
        assert_eq!(loaded.verifying_key_bytes(), gen.verifying_key_bytes());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::loads_identity`
Expected: FAIL (`load_identity` missing).

- [ ] **Step 3: Implement**

```rust
//! Build + sign an official .muragent from source.
use anyhow::{Context, Result};
use mur_common::identity::AgentIdentity;
use std::path::Path;

pub fn load_identity(key_path: &Path) -> Result<AgentIdentity> {
    let bytes = std::fs::read(key_path)
        .with_context(|| format!("read key {}", key_path.display()))?;
    let dir = tempfile::tempdir().context("temp dir for identity")?;
    std::fs::write(dir.path().join("identity.key"), &bytes).context("stage identity.key")?;
    AgentIdentity::load(dir.path()).context("load official identity")
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::loads_identity`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/sign.rs
git commit -m "feat(official-sign): load official identity from key file"
```

---

### Task 4: Build + sign an official `.muragent` from source

**Files:**
- Modify: `official-sign/src/sign.rs`

**Interfaces:**
- Consumes: `mur_common::agent::AgentProfile`, `mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile}`, `mur_common::official::DISTRIBUTION_OFFICIAL`, the `load_identity` from Task 3.
- Produces: `pub fn build_official_muragent(source_dir: &Path, out_path: &Path, identity: &AgentIdentity, mur_version: &str) -> anyhow::Result<()>` — reads `profile.yaml` (+ `prompt.md`, `skills/`, optional `icon.png`), builds a manifest with `distribution = Some("official")`, and writes the signed package to `out_path`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn builds_and_signs_official_muragent() {
    // minimal source dir: profile.yaml only
    let src = tempfile::tempdir().unwrap();
    std::fs::write(
        src.path().join("profile.yaml"),
        "id: u1\nname: researcher\ndisplay_name: Researcher\n",
    ).unwrap();
    let id = mur_common::identity::AgentIdentity::generate();
    let out = src.path().join("researcher.muragent");
    build_official_muragent(src.path(), &out, &id, "1.0.0").unwrap();
    assert!(out.exists());
    // the package validates and carries the official marker
    let archive = mur_common::muragent::reader::MuragentArchive::read(&out).unwrap();
    let v = mur_common::muragent::validator::validate(&archive).unwrap();
    assert_eq!(v.manifest.distribution.as_deref(), Some("official"));
    assert_eq!(v.author_pubkey, id.verifying_key_bytes());
}
```

> NOTE for the implementer: read `mur-common/src/agent.rs` for `AgentProfile`'s exact required fields and adjust the fixture `profile.yaml` minimally so it deserializes; read `mur-core/src/cmd/agent/export.rs:99-145` for the exact `MuragentWriter` call sequence (add_icon/set_sys_prompt/add_skill) and mirror only the parts your source dir provides.

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::builds_and_signs`
Expected: FAIL (`build_official_muragent` missing).

- [ ] **Step 3: Implement**

```rust
use mur_common::agent::AgentProfile;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
use mur_common::official::DISTRIBUTION_OFFICIAL;

pub fn build_official_muragent(
    source_dir: &Path,
    out_path: &Path,
    identity: &AgentIdentity,
    mur_version: &str,
) -> Result<()> {
    let profile_yaml = std::fs::read_to_string(source_dir.join("profile.yaml"))
        .context("read profile.yaml")?;
    let profile: AgentProfile =
        serde_yaml_ng::from_str(&profile_yaml).context("parse profile.yaml")?;

    let mut manifest = build_manifest_from_profile(&profile, mur_version);
    manifest.distribution = Some(DISTRIBUTION_OFFICIAL.to_string());

    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity.clone());

    let prompt = source_dir.join("prompt.md");
    if prompt.exists() {
        writer.set_sys_prompt(std::fs::read_to_string(&prompt).context("read prompt.md")?);
    }
    let icon = source_dir.join("icon.png");
    if icon.exists() {
        writer.add_icon("icon.png", std::fs::read(&icon).context("read icon.png")?);
    }
    let skills = source_dir.join("skills");
    if skills.is_dir() {
        for entry in std::fs::read_dir(&skills).context("read skills/")? {
            let p = entry?.path();
            if p.is_file() {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                writer.add_skill(&name, std::fs::read(&p)?);
            }
        }
    }
    writer.write(out_path).map_err(|e| anyhow::anyhow!("write .muragent: {e}"))?;
    Ok(())
}
```

> NOTE: `MuragentWriter::new` takes `identity` by value; `AgentIdentity` derives `Clone` (verify in `mur-common/src/identity.rs`) so `identity.clone()` is correct. If it does not, change `load_identity` to return an owned identity moved in per call.

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::builds_and_signs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/sign.rs
git commit -m "feat(official-sign): build + sign official .muragent from source"
```

---

### Task 5: Post-build verification assertions

**Files:**
- Modify: `official-sign/src/sign.rs`

**Interfaces:**
- Consumes: `mur_common::muragent::{reader::MuragentArchive, validator}`, `mur_common::muragent::dsse::keyid_from_pubkey`, the Task 2 `CatalogEntry`.
- Produces: `pub fn verify_official(bundle_path: &Path, entry: &crate::catalog::CatalogEntry, expect_fp: &str) -> anyhow::Result<()>` — fails closed unless the package validates, its signer fingerprint equals `expect_fp`, its marker is `official`, and its manifest `name`/`version` match the catalog entry.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn verify_rejects_wrong_fingerprint() {
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("profile.yaml"),
        "id: u1\nname: researcher\ndisplay_name: Researcher\n").unwrap();
    let id = mur_common::identity::AgentIdentity::generate();
    let out = src.path().join("r.muragent");
    build_official_muragent(src.path(), &out, &id, "1.0.0").unwrap();
    let entry = crate::catalog::CatalogEntry {
        id: "agents/researcher".into(), kind: "agent".into(), name: "researcher".into(),
        version: "1.0.0".into(), tier: "pro".into(), description: "d".into(),
    };
    // correct fp passes
    let good_fp = mur_common::muragent::dsse::keyid_from_pubkey(&id.verifying_key_bytes());
    verify_official(&out, &entry, &good_fp).unwrap();
    // wrong fp fails closed
    assert!(verify_official(&out, &entry, "ed25519-deadbeef").is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::verify_rejects`
Expected: FAIL (`verify_official` missing).

- [ ] **Step 3: Implement**

```rust
use mur_common::muragent::dsse::keyid_from_pubkey;
use mur_common::muragent::{reader::MuragentArchive, validator};

pub fn verify_official(
    bundle_path: &Path,
    entry: &crate::catalog::CatalogEntry,
    expect_fp: &str,
) -> Result<()> {
    let archive = MuragentArchive::read(bundle_path).context("read built .muragent")?;
    let v = validator::validate(&archive).map_err(|e| anyhow::anyhow!("validate: {e}"))?;
    let fp = keyid_from_pubkey(&v.author_pubkey);
    if fp != expect_fp {
        anyhow::bail!("signer fingerprint {fp} != expected {expect_fp}");
    }
    if v.manifest.distribution.as_deref() != Some("official") {
        anyhow::bail!("built package is missing the official distribution marker");
    }
    if v.manifest.agent.slug != entry.name {
        anyhow::bail!("manifest slug '{}' != catalog name '{}'", v.manifest.agent.slug, entry.name);
    }
    Ok(())
}
```

> NOTE: the manifest carries no `version` field of its own in the current schema; the catalog `version` governs the storage path (Task 6). If a version field is later added to the manifest, extend this check. Confirm `v.manifest.agent.slug` is the field name by reading `mur-common/src/muragent/manifest.rs` (`AgentRef.slug`).

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test sign::tests::verify_rejects`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/sign.rs
git commit -m "feat(official-sign): fail-closed post-build verification"
```

---

### Task 6: Emit the `index.json` entry (with immutable-version guard)

**Files:**
- Modify: `official-sign/src/index.rs`

**Interfaces:**
- Consumes: the Task 2 `CatalogEntry`.
- Produces:
  - `pub struct IndexItem { pub id, kind, name, version, tier, description, storage_key, sha256: String, pub size: u64 }` (`Serialize`, `Deserialize`, `Clone`).
  - `pub fn make_item(entry: &CatalogEntry, bundle_path: &Path) -> anyhow::Result<IndexItem>` — computes `sha256`/`size`, sets `storage_key = official/<kind>s/<name>/<version>/bundle.muragent`.
  - `pub fn upsert(existing: &[IndexItem], new: IndexItem) -> anyhow::Result<Vec<IndexItem>>` — appends `new`, but errors if an item with the same `(name, version)` already exists with a *different* `sha256` (immutable-version guard).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, ver: &str, sha: &str) -> IndexItem {
        IndexItem { id: format!("agents/{name}"), kind: "agent".into(), name: name.into(),
            version: ver.into(), tier: "pro".into(), description: "d".into(),
            storage_key: format!("official/agents/{name}/{ver}/bundle.muragent"),
            sha256: sha.into(), size: 1 }
    }

    #[test]
    fn upsert_appends_new_and_blocks_version_overwrite() {
        let base = vec![item("researcher", "1.0.0", "aaa")];
        // new version → ok
        let ok = upsert(&base, item("researcher", "1.1.0", "bbb")).unwrap();
        assert_eq!(ok.len(), 2);
        // same version, different bytes → refused (immutable)
        assert!(upsert(&base, item("researcher", "1.0.0", "ccc")).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test index::`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! index.json generation.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;

use crate::catalog::CatalogEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub version: String,
    pub tier: String,
    pub description: String,
    pub storage_key: String,
    pub sha256: String,
    pub size: u64,
}

pub fn make_item(entry: &CatalogEntry, bundle_path: &Path) -> Result<IndexItem> {
    let bytes = std::fs::read(bundle_path)?;
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    Ok(IndexItem {
        id: entry.id.clone(),
        kind: entry.kind.clone(),
        name: entry.name.clone(),
        version: entry.version.clone(),
        tier: entry.tier.clone(),
        description: entry.description.clone(),
        storage_key: format!(
            "official/{}s/{}/{}/bundle.muragent",
            entry.kind, entry.name, entry.version
        ),
        sha256,
        size: bytes.len() as u64,
    })
}

pub fn upsert(existing: &[IndexItem], new: IndexItem) -> Result<Vec<IndexItem>> {
    let mut out: Vec<IndexItem> = Vec::with_capacity(existing.len() + 1);
    for it in existing {
        if it.name == new.name && it.version == new.version && it.sha256 != new.sha256 {
            anyhow::bail!(
                "immutable version violation: {} {} already published with different bytes",
                new.name, new.version
            );
        }
        // drop an identical re-publish (same name+version+sha) to avoid dupes
        if !(it.name == new.name && it.version == new.version) {
            out.push(it.clone());
        }
    }
    out.push(new);
    Ok(out)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test index::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/index.rs
git commit -m "feat(official-sign): index.json items with immutable-version guard"
```

---

### Task 7: Wire the CLI end-to-end + integration test

**Files:**
- Modify: `official-sign/src/main.rs`
- Create: `official-sign/tests/end_to_end.rs`

**Interfaces:**
- Consumes: `catalog::load_entry`, `sign::{load_identity, build_official_muragent, verify_official}`, `index::{make_item, upsert}`.
- Produces: a `main` flow: load entry → load identity → build → verify (with `--expect-fp` or the pinned constant) → write bundle to `out_dir/<name>-<version>.muragent` → merge into `out_dir/index.json`.

- [ ] **Step 1: Write the failing integration test**

```rust
// official-sign/tests/end_to_end.rs
use std::process::Command;

#[test]
fn cli_builds_verifies_and_indexes_with_test_key() {
    let tmp = tempfile::tempdir().unwrap();
    // source
    let src = tmp.path().join("agents/researcher");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("profile.yaml"),
        "id: u1\nname: researcher\ndisplay_name: Researcher\n").unwrap();
    // catalog.yaml
    let cat = tmp.path().join("catalog.yaml");
    std::fs::write(&cat,
        "items:\n  - id: agents/researcher\n    kind: agent\n    name: researcher\n    version: 1.0.0\n    tier: pro\n    description: d\n").unwrap();
    // test key + its fp
    let id = mur_common::identity::AgentIdentity::generate();
    let keydir = tmp.path().join("k");
    id.save(&keydir).unwrap();
    let fp = mur_common::muragent::dsse::keyid_from_pubkey(&id.verifying_key_bytes());
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_official-sign"))
        .args(["--id", "agents/researcher",
               "--source-dir", src.to_str().unwrap(),
               "--catalog", cat.to_str().unwrap(),
               "--out-dir", out.to_str().unwrap(),
               "--key", keydir.join("identity.key").to_str().unwrap(),
               "--expect-fp", &fp])
        .env("ORT_STRATEGY", "download")
        .status().unwrap();
    assert!(status.success());
    assert!(out.join("researcher-1.0.0.muragent").exists());
    let idx = std::fs::read_to_string(out.join("index.json")).unwrap();
    assert!(idx.contains("agents/researcher"));
}
```

> NOTE: this test needs `mur-common` + `tempfile` as `dev-dependencies` of `official-sign` (add them to `Cargo.toml`).

- [ ] **Step 2: Run to verify failure**

Run: `cd official-sign && ORT_STRATEGY=download cargo test --test end_to_end`
Expected: FAIL (main still prints "not yet implemented").

- [ ] **Step 3: Implement the main flow**

`main.rs` depends on the lib crate (`official_sign`, created in Task 1). Also update the Task 1 note about `--id` — it is already in the `Cli` struct above. Then:

```rust
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let entry = official_sign::catalog::load_entry(&cli.catalog, &cli.id)?;
    let identity = official_sign::sign::load_identity(&cli.key)?;
    let expect_fp = cli.expect_fp.clone().unwrap_or_else(||
        mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP.to_string());

    std::fs::create_dir_all(&cli.out_dir)?;
    let bundle = cli.out_dir.join(format!("{}-{}.muragent", entry.name, entry.version));
    official_sign::sign::build_official_muragent(&cli.source_dir, &bundle, &identity, env!("CARGO_PKG_VERSION"))?;
    official_sign::sign::verify_official(&bundle, &entry, &expect_fp)?;

    let item = official_sign::index::make_item(&entry, &bundle)?;
    let index_path = cli.out_dir.join("index.json");
    let existing: Vec<official_sign::index::IndexItem> = if index_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&index_path)?)?
    } else { Vec::new() };
    let merged = official_sign::index::upsert(&existing, item)?;
    std::fs::write(&index_path, serde_json::to_string_pretty(&merged)?)?;
    println!("✅ signed + indexed {} {}", entry.name, entry.version);
    Ok(())
}
```
> The crate name `official-sign` produces lib crate `official_sign`; ensure `main.rs` uses that lib crate. `--id` is already defined on `Cli` (Task 1).

- [ ] **Step 4: Run to verify pass**

Run: `cd official-sign && ORT_STRATEGY=download cargo test --test end_to_end`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add official-sign/src/main.rs official-sign/tests/end_to_end.rs official-sign/Cargo.toml
git commit -m "feat(official-sign): end-to-end build+verify+index CLI"
```

---

### Task 8: README + fmt/clippy + follow-up notes

**Files:**
- Create: `official-sign/README.md`

- [ ] **Step 1: Write the README**

Document: purpose (CI-only, never shipped in `mur`), usage (`--id --source-dir --catalog --out-dir --key [--expect-fp]`), the real-key requirement (the `--key` in production must be the private key whose fingerprint equals `MUR_OFFICIAL_PUBLISHER_KEY_FP`; if that keypair must be generated, the client pin has to match — coordinate a client release), and that output is a local `out-dir` (upload + provenance are the CI workflow's job, a separate plan).

- [ ] **Step 2: fmt + clippy**

Run: `cd official-sign && ORT_STRATEGY=download cargo fmt && cargo clippy -- -D warnings`
Expected: clean.

- [ ] **Step 3: Full test run**

Run: `cd official-sign && ORT_STRATEGY=download cargo test`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add official-sign/README.md
git commit -m "docs(official-sign): usage + real-key/follow-up notes"
```

---

## Follow-ups (out of scope for this plan)

1. **Fleet support:** lift `build_bundle_bytes` (currently private in `mur-core/src/cmd/fleet/export.rs`) into `mur-common` as a `pub fn`, release `mur-common`, then add a `build_official_fleet` mirroring Task 4/5 for `.fleet` bundles.
2. **CI workflow** (`.github/workflows/publish.yml`): PR dry-run with a throwaway test key + structure check + expanded-contents summary; merge-to-main protected-Environment publish with the real key; storage upload; `actions/attest-build-provenance`. Needs the **storage backend pinned** first (S3/R2/GCS + bucket + credentials) — the one open decision from the spec.
3. **Repo creation + first real publish:** `gh repo create mur-run/official-catalog --private`, wire the `mur-common` git/crates.io dependency, add the `OFFICIAL_SIGNING_KEY` Environment secret, publish the first agent, and verify locally with the PR #738 import gate (no license ⇒ refused; test license ⇒ installs).
4. **Client key-rotation:** evolve the client's single pinned `MUR_OFFICIAL_PUBLISHER_KEY_FP` into a set to allow rotation without orphaning installed content.
