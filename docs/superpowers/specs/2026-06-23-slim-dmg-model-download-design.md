# Slim DMG + First-Run Model Setup — Design

**Date:** 2026-06-23
**Status:** Approved
**Author:** david + Claude Opus 4.8 (1M context)
**Related:** self-contained Hub install (the offline-first pillar this revises); Hub autoupdate 1.6GB re-ship pain

## Problem

The MUR Hub `.dmg` bakes the ~1.6GB `Qwen3.5-2B-MLX-4bit` MLX model into the bundle
(`scripts/fetch-bundle-model.sh` → `resources/models/default/` → tauri.conf resources glob →
`.app`/`.dmg`). Two costs:

1. The download is huge for every user, including those who will use a cloud LLM or an existing
   local runtime (ollama / oMLX / LM Studio) and never touch the bundled model.
2. Autoupdate re-ships the full 1.6GB on every update (the model rides inside the bundle).

## Goal

Ship a **slim** `.dmg`. On first run, if no usable model is found, let the user **either download
the local MLX model or connect an LLM** (cloud or local). The downloaded model lives outside the
`.app` so updates and reinstalls never re-download it.

## Decisions (from brainstorming)

- **Q1 — First-run flow:** auto-detect existing local runtimes first; show a picker only when nothing
  is found.
- **Q2 — Download source:** HuggingFace directly (`mlx-community/Qwen3.5-2B-MLX-4bit`), the same repo
  the build script trusts today. No MUR-hosted mirror (add only if HF actually bites).
- **Q3 — Skippable?** No. The picker is **mandatory** — chat *is* the Hub, so a no-model state is not
  worth building or maintaining. Auto-detect already spares anyone with a local runtime from ever
  seeing it.
- **Q4 — Offline-first:** Slim only; first run requires a network. The model is **cached** in a
  writable location so reinstalls/updates reuse it (also fixes the autoupdate re-ship). No second
  "offline" artifact unless a real air-gapped customer asks.

## Design

### 1. Bundling

- Remove `"resources/models/**/*"` from `mur-hub-gui/src-tauri/tauri.conf.json` bundle resources.
  This is the change that shrinks the `.dmg`.
- **Keep** the `mlx-server` external binary (`binaries/mlx-server`) — it is small and is what runs the
  downloaded weights.
- `scripts/fetch-bundle-model.sh` is no longer invoked by the default build. Keep it as a
  dev/offline helper (it can still populate `~/.mur/models/<name>/` for local testing). Not deleted.

### 2. Model location (writable, update-surviving)

- Canonical model dir: `~/.mur/models/Qwen3.5-2B-MLX-4bit/`.
  - Outside the `.app` bundle → survives reinstall and autoupdate → the cache from Q4.
- Path constant lives in `mur-common` (a `pub fn local_model_dir(mur_home, name) -> PathBuf`), so the
  Hub sidecar, the downloader, and the CLI agree on one location (no hardcoded path duplicated).
- `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`:
  - `model_available()` checks `~/.mur/models/<name>/` for a completed model (see `.complete` marker
    in §5), falling back to the legacy bundled `resources/models/default` only if it still exists
    (dev/offline builds). Live slim builds use the writable dir.
  - `start()` spawns `mlx-server --model <resolved-dir>` unchanged otherwise.

### 3. First-run decision (Hub launch)

In `mur-hub-gui/src-tauri/src/lib.rs`, where `mlx_sidecar::start` / `ensure_concierge_model` are
called today, the decision becomes:

```
1. completed model in ~/.mur/models/<name>/        → start mlx sidecar           (existing path)
2. else local runtime detected (probe_local: ollama/oMLX/LM Studio)
                                                    → ensure_concierge_model      (existing fallback)
3. else concierge profile already has a model_ref  → use it (user-configured cloud)
4. else                                             → emit "need-model" event → mandatory picker
```

Steps 1–3 are existing behavior re-ordered into an explicit chain. Step 4 is new: a Tauri event the
frontend listens for to open the picker. No degraded/no-model runtime state exists — the picker
blocks until 1, 2, or 3 becomes true.

### 4. The picker = existing Model Library + one tile

No bespoke screen. The frontend opens the **existing Model Library UI** in a mandatory
(non-dismissible) mode on the `need-model` event, with one tile added at the top:

- **"Use MUR's local model — download ~1.6GB"** → invokes the `download_local_model` Tauri command
  (§5), shows a progress bar from streamed progress events. On success: start the sidecar, close the
  picker.
- **Connect tiles** (existing): connect cloud / local provider via the existing `add_models` registry
  write, **then** set the concierge profile's `model_ref` to the chosen alias (§6), then restart the
  concierge agent.
- **Offline + chose download** → show the download error with a Retry; the connect tiles remain
  available so the user can pick a cloud/local LLM instead.

"Mandatory" = the modal cannot be dismissed and the rest of the Hub is not interactable until a model
is configured. (Auto-detect in §3 means most users never see it.)

### 5. Download mechanism (the one new component)

The HF repo is a *folder* of files (`model.safetensors`, `config.json`, `tokenizer.json`, …), so the
existing single-file voice downloader (`mur-agent-runtime/src/voice/download.rs`) is not a drop-in.

New module: `mur-core/src/model_download.rs` (in mur-core so it is unit-testable under nextest and the
`mur` CLI can reuse it later):

```rust
/// Download an MLX model repo from HuggingFace into `dest`, reporting aggregate progress.
pub async fn download_hf_model(
    repo: &str,                 // e.g. "mlx-community/Qwen3.5-2B-MLX-4bit"
    dest: &Path,                // ~/.mur/models/<name>/
    on_progress: impl Fn(u64 /*done*/, u64 /*total*/),
) -> Result<()>;
```

Algorithm:
1. **Cache check.** If `dest/.complete` exists → return `Ok(())` immediately (already downloaded).
2. **List files.** `GET https://huggingface.co/api/models/<repo>` → parse `siblings[].rfilename`. No
   hardcoded filename list (rule #1). (Optionally read each file's size for accurate total progress;
   if the API omits sizes, fall back to per-file byte progress without a grand total.)
3. **Download each file** from `https://huggingface.co/<repo>/resolve/main/<rfilename>` using the
   existing idiom: stream to a temp file, then atomic rename into place. Sum bytes for aggregate
   progress.
4. **Mark complete.** After every file is renamed in, write `dest/.complete` (atomically). A partial
   download therefore never looks cached, because the marker is only written on full success.

Tauri command `download_local_model` in the Hub wraps `download_hf_model`, translating the
`on_progress` callback into Tauri progress events the picker subscribes to. It resolves `dest` via the
`mur-common` path helper and the model name from the concierge template.

**Verification:** HF `resolve` URLs are content-addressed by commit; an interrupted file is left as a
temp file and discarded (never renamed in), so a half-file can't be mistaken for complete. SHA-256 of
each file against the HF API's `lfs.oid` is a cheap add if we want it, but the temp+rename+marker
sequence already prevents partial-state corruption. (Add per-file SHA only if a corruption report
appears.)

### 6. Concierge profile wiring

- **Download path:** the concierge template profile already says `provider: local`,
  `name: Qwen3.5-2B-MLX-4bit`. No rewrite needed — the sidecar publishes
  `~/.mur/runtime/local_llm.url` and the runtime reads it (existing).
- **Connect path:** we must point the concierge at the chosen registry alias. `ensure_concierge_model`
  today *respects* an existing `model_ref:` (it bails, leaving the user's choice alone) but does not
  *write* one. Add a small helper in `seed_mur.rs`:
  `set_concierge_model_ref(mur_home, alias)` that rewrites the profile's `model:` block to
  `model_ref: <alias>` (reusing the existing `rewrite_model_block` machinery). Called by the connect
  tile after `add_models`.

## Files touched

| File | Change |
|------|--------|
| `mur-hub-gui/src-tauri/tauri.conf.json` | drop `resources/models/**/*` glob |
| `mur-common/src/…` | `local_model_dir()` path helper |
| `mur-core/src/model_download.rs` (new) | `download_hf_model()` + HF file-list + cache marker |
| `mur-hub-gui/src-tauri/src/mlx_sidecar.rs` | resolve/`model_available` from `~/.mur/models/<name>/` |
| `mur-hub-gui/src-tauri/src/lib.rs` | first-run decision chain + `need-model` event |
| `mur-hub-gui/src-tauri/src/model_download.rs` (new) | `download_local_model` Tauri command + progress events |
| `mur-hub-gui/src-tauri/src/seed_mur.rs` | `set_concierge_model_ref()` helper |
| Hub frontend (mur-web) | download tile, progress bar, mandatory-on-`need-model` modal |
| `scripts/fetch-bundle-model.sh` | de-wire from default build; keep as dev/offline helper |

## Testing

- **mur-core nextest** for `model_download`: parse a fixture HF `siblings` JSON into a file list;
  assert the `.complete` cache short-circuit (present → skip) and that a missing marker forces a
  re-download. No network in the test — inject the file-listing + fetch via a small fn boundary, or
  point at a `file://`/local fixture.
- Manual/operator: slim build → fresh `~/.mur` → first run shows picker → download completes →
  concierge chats; second run (model cached) skips the picker; connect-cloud path sets `model_ref`
  and chats.

## Out of scope (YAGNI)

- MUR-hosted model mirror / CDN (Q2 — add only if HF fails in practice).
- A second "offline" full `.dmg` artifact (Q4 — add only for a real air-gapped customer).
- No-model degraded Hub state (Q3 — picker is mandatory).
- Per-file SHA-256 verification (temp+rename+marker already prevents partial-state; add on first
  corruption report).
