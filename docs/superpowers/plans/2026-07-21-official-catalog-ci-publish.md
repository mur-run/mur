# Official Catalog CI Publish Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Turn the `official-sign` crate into a working CI publish pipeline in the private `mur-run/official-catalog` repo: reviewed agent source on a PR → merge-to-main → CI builds + signs with the real official key → the signed `.muragent` is attached to a GitHub Release and `index.json` is committed, with SLSA provenance.

**Architecture:** Storage = **GitHub Releases on the private `mur-run/official-catalog` repo** (chosen because mur-server has no reusable private object store — R2 is a public download CDN only; private-repo release assets are auth-gated, so pro bytes stay non-public, and they are immutable/versioned by construction). The future mur-server `/download` fetches the release asset via the GitHub API with a token and returns it inline as `bundle_base64` (that server work is a separate plan). CI signing key lives in a protected GitHub Environment; PR runs use a throwaway test key and never touch the real secret.

**Tech Stack:** GitHub Actions, the `official-sign` Rust crate (already built), `gh` CLI (release upload), `actions/attest-build-provenance`, Cargo git dependency on `mur-common`.

## Global Constraints

- The real official signing key is used ONLY on merge-to-main, gated by a protected GitHub Environment (required reviewers, pinned action SHAs, least-privilege token). PR-triggered runs use a throwaway test key and MUST NOT be able to read the real secret. No fake key ever produces a published artifact.
- The signed bundle's fingerprint MUST equal `MUR_OFFICIAL_PUBLISHER_KEY_FP` = `ed25519-861d2acb`. `official-sign` already asserts this via `--expect-fp` defaulting to that constant.
- Published versions are immutable: a Release tag `official/<kind>/<name>/<version>` is created once; re-publishing an existing version must fail, not overwrite.
- Repo layout target (per design spec §3): `agents/<name>/` (reviewable source), `catalog.yaml` (metadata source of truth), `tools/official-sign/` (the crate), `.github/workflows/publish.yml`.
- Work happens in the private repo checkout at `/Volumes/Firecuda4tb/Projects/official-sign` (remote `origin` = `mur-run/official-catalog`). Build env for the crate: `ORT_STRATEGY=download`.

---

### Task 1: Restructure the repo + switch `mur-common` to a git dependency

**Files:**
- Move: repo-root crate (`Cargo.toml`, `src/`, `tests/`, `README.md`, `Cargo.lock`) → `tools/official-sign/`
- Modify: `tools/official-sign/Cargo.toml` — swap the `mur-common` path dep for a git dep
- Create: `catalog.yaml` (repo root), `agents/researcher/` sample source, top-level `README.md`

**Interfaces:**
- Produces: a repo whose `tools/official-sign` crate builds WITHOUT a local mur checkout (git dep resolves `mur-common` from `mur-run/mur`), and a `catalog.yaml` + one real `agents/<name>/` source dir the workflow can build.

- [ ] **Step 1: Move the crate under `tools/official-sign/`**

```bash
cd /Volumes/Firecuda4tb/Projects/official-sign
mkdir -p tools/official-sign
git mv Cargo.toml Cargo.lock src tests README.md tools/official-sign/
```

- [ ] **Step 2: Switch the dependency to git**

In `tools/official-sign/Cargo.toml`, replace the `mur-common` path line with:

```toml
mur-common = { git = "https://github.com/mur-run/mur", branch = "main" }
```

- [ ] **Step 3: Verify it still builds + tests via the git dep**

Run: `cd tools/official-sign && ORT_STRATEGY=download cargo test`
Expected: cargo fetches `mur-common` from git; all 8 tests pass. (First fetch is slow.)
If the git dep fails to resolve `mur-common` as a workspace member, fall back to pinning a tag: `tag = "v<latest-released>"` (check `git -C ../../../mur tag | tail -1`), and note it in the report.

- [ ] **Step 4: Add `catalog.yaml` and one sample source dir**

`catalog.yaml` (repo root):
```yaml
items:
  - id: agents/researcher
    kind: agent
    name: researcher
    version: 1.0.0
    tier: free
    description: "Example official research agent."
```

`agents/researcher/profile.yaml`: a real sanitized profile (schema/version/persona/sys_prompt_file/model required — copy the shape from `mur-common/tests/fixtures/minimal_profile.yaml` in the mur checkout, set `name: researcher`, `display_name: Researcher`). Add `agents/researcher/prompt.md` with a one-line prompt.

- [ ] **Step 5: Top-level README + commit**

Write a repo `README.md`: what this private repo is (official catalog source + CI signing), the layout (`agents/`, `catalog.yaml`, `tools/official-sign/`, `.github/`), and that publishing is PR → merge-to-main (no local publish tool).

```bash
git add -A
git commit -m "chore: repo layout (tools/official-sign, catalog.yaml, sample source) + git dep"
git push
```

---

### Task 2: PR dry-run workflow job

**Files:**
- Create: `.github/workflows/publish.yml` (the `dry-run` job only in this task)

**Interfaces:**
- Produces: on every PR touching `agents/**` or `catalog.yaml`, a job that builds each changed item with a THROWAWAY test key, runs `official-sign`'s verify (structure only), and posts the built manifest + file list as a step summary. Never reads the real secret.

- [ ] **Step 1: Write the dry-run job**

```yaml
name: publish
on:
  pull_request:
    paths: ["agents/**", "catalog.yaml", "tools/official-sign/**"]
  push:
    branches: [main]
    paths: ["agents/**", "catalog.yaml"]

jobs:
  dry-run:
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<pinned-sha>  # v4
      - uses: dtolnay/rust-toolchain@<pinned-sha>  # stable
      - name: Generate throwaway test key
        run: |
          # official-sign can generate one via a tiny helper, OR use openssl to
          # emit 32 random bytes as identity.key (matches AgentIdentity format).
          head -c 32 /dev/urandom > /tmp/test.key
      - name: Build + verify each item (test key, no upload)
        env: { ORT_STRATEGY: download }
        run: |
          # derive the test key's fp so --expect-fp matches (see Task 4 note on a
          # `--print-fp` helper, or compute via a one-off official-sign subcommand)
          cargo run --manifest-path tools/official-sign/Cargo.toml -- \
            --id agents/researcher --source-dir agents/researcher \
            --catalog catalog.yaml --out-dir /tmp/out --key /tmp/test.key \
            --expect-fp "$(cat /tmp/test.fp)"
      - name: Post built contents summary
        run: |
          echo "### Built bundles" >> "$GITHUB_STEP_SUMMARY"
          ls -la /tmp/out >> "$GITHUB_STEP_SUMMARY"
```

> NOTE: the dry-run needs the test key's fingerprint to pass `--expect-fp`. Task 4 adds a `--print-fp <key>` mode to `official-sign` (or a `keygen` subcommand that emits key + fp). Until then, the dry-run can pass the real pinned fp with the real key blocked — but the cleanest is the helper. Implement the `--print-fp` helper as part of THIS task if the dry-run needs it, in `tools/official-sign/src/main.rs`, with a unit test.

- [ ] **Step 2: Verify the workflow parses**

Run: `gh workflow view publish.yml` after pushing to a branch, or lint locally with `actionlint` if available. Expected: no syntax errors.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/publish.yml tools/official-sign
git commit -m "ci: PR dry-run build+verify with throwaway key"
git push
```

---

### Task 3: Merge-to-main protected publish job

**Files:**
- Modify: `.github/workflows/publish.yml` (add the `publish` job)

**Interfaces:**
- Consumes: `official-sign` (builds + signs + verifies + emits index.json), a GitHub Environment `official-signing` holding secret `OFFICIAL_SIGNING_KEY` (the real 32-byte identity.key, base64).
- Produces: on merge-to-main, for each item in `catalog.yaml`: build+sign with the real key, verify fp == pinned, create/upload a Release `official/<kind>/<name>/<version>` with the bundle asset (fails if the tag already exists = immutable), commit the regenerated `index.json`, emit provenance.

- [ ] **Step 1: Write the publish job**

```yaml
  publish:
    if: github.event_name == 'push'
    runs-on: ubuntu-latest
    environment: official-signing        # required-reviewers gate holds the real key
    permissions: { contents: write, id-token: write, attestations: write }
    steps:
      - uses: actions/checkout@<pinned-sha>
      - uses: dtolnay/rust-toolchain@<pinned-sha>
      - name: Stage real signing key
        run: printf '%s' "${{ secrets.OFFICIAL_SIGNING_KEY }}" | base64 -d > /tmp/official.key
      - name: Build + sign + verify + index
        env: { ORT_STRATEGY: download }
        run: |
          cargo run --manifest-path tools/official-sign/Cargo.toml -- \
            --id agents/researcher --source-dir agents/researcher \
            --catalog catalog.yaml --out-dir out --key /tmp/official.key
          # no --expect-fp → defaults to the pinned MUR_OFFICIAL_PUBLISHER_KEY_FP;
          # a wrong key fails the build here.
      - name: Publish immutable release (fails if version exists)
        env: { GH_TOKEN: ${{ github.token }} }
        run: |
          TAG="official/agents/researcher/1.0.0"
          if gh release view "$TAG" >/dev/null 2>&1; then
            echo "::error::$TAG already published (immutable)"; exit 1
          fi
          gh release create "$TAG" out/researcher-1.0.0.muragent --notes "official researcher 1.0.0"
      - name: Attest provenance
        uses: actions/attest-build-provenance@<pinned-sha>
        with: { subject-path: "out/researcher-1.0.0.muragent" }
      - name: Commit index.json
        run: |
          cp out/index.json index.json
          git config user.name "official-catalog-ci"
          git config user.email "ci@mur.run"
          git add index.json && git commit -m "publish: researcher 1.0.0" && git push
```

- [ ] **Step 2: Verify job structure**

Run: `actionlint .github/workflows/publish.yml` (or push to a branch and `gh workflow view`). Expected: no errors. Do NOT trigger the real publish yet (needs the secret — Task 4).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/publish.yml
git commit -m "ci: merge-to-main protected publish (real key, GitHub Release, provenance)"
git push
```

---

### Task 4: `--print-fp` helper + first-publish runbook

**Files:**
- Modify: `tools/official-sign/src/main.rs` (add `--print-fp` mode if not added in Task 2)
- Create: `docs/FIRST_PUBLISH.md`

- [ ] **Step 1: Add `--print-fp` (if not already present)**

A mode that loads `--key` and prints `keyid_from_pubkey(verifying_key_bytes())` — used by the PR dry-run to match `--expect-fp`. Add a unit test (generate key, print fp, assert it equals `keyid_from_pubkey`).

Run: `cd tools/official-sign && ORT_STRATEGY=download cargo test` — all pass.

- [ ] **Step 2: Write `docs/FIRST_PUBLISH.md`**

Document the operator steps (cannot be automated — they need the real key + org admin):
1. **Key custody:** the `OFFICIAL_SIGNING_KEY` secret must be the 32-byte Ed25519 private key (base64) whose fingerprint equals `MUR_OFFICIAL_PUBLISHER_KEY_FP` (`ed25519-861d2acb`). If the keypair does not yet exist, generate it, set the secret, AND ensure the client's pinned constant matches (coordinate a `mur` client release) — otherwise installs will reject the bundles.
2. **Environment:** create GitHub Environment `official-signing` with required reviewers + the secret.
3. **First publish:** open a PR adding `agents/researcher/`, confirm the dry-run is green, merge → the publish job runs.
4. **Verify end-to-end:** download the release asset, and locally run the shipped client gate — `mur agent install researcher.muragent` with NO license must be REFUSED (proves the anti-sharing gate sees it as official); with a matching test license it installs. (The server that issues real licenses is a separate plan.)

- [ ] **Step 3: Commit**

```bash
git add tools/official-sign docs/FIRST_PUBLISH.md
git commit -m "feat: --print-fp helper + first-publish runbook"
git push
```

---

## Out of scope (separate plans)

- **mur-server `/download` + `/catalog` endpoints (Go):** read the GitHub Release asset via the GitHub API with a server token, sign an `OfficialLicense` (entitlement-gated for pro), return `{license, bundle_base64}`; serve `catalog` from the committed `index.json`.
- **Fleet support:** lift `build_bundle_bytes` into `mur-common` + release it, add `build_official_fleet`, and fix `official-sign`'s `index.rs upsert` to key on `(kind, name, version)` (the flagged hard-blocker).
- **Client key-rotation:** evolve the single pinned `MUR_OFFICIAL_PUBLISHER_KEY_FP` into a set.
