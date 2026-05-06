# mur init — Dynamic LLM Model Selection & Config Default Fix

**Date:** 2026-05-06
**Status:** Approved

## Problem

Two issues in `mur init`'s model setup flow:

1. **Wrong default model.** `mur-common/src/config.rs` hardcodes `qwen3:14b` as the default LLM model in three places. This model tag does not exist in the Ollama registry. Users who skip `mur init` or use conversations features without going through full setup get a broken config.

2. **Static model menu in "All local" path.** Init mode 3 ("All local") calls `select_model(OLLAMA_RECS)` — a static curated list. It ignores models the user already has installed and cannot reflect new model releases without a code update. The embedding selection path already solves this correctly via `discover_blocking()` → `build_embedding_menu()`; the LLM path should do the same.

3. **macOS priority already implemented.** `init_local.rs::prompt_backend()` already prioritizes oMLX > mlx-lm > Ollama on Apple Silicon. No change needed.

## Design

### Change 1 — Fix config defaults (`qwen3:14b` → `qwen3:4b`)

**File:** `mur-common/src/config.rs`

Three default functions return `"qwen3:14b"`. Change all to `"qwen3:4b"`:

| Location | Function |
|----------|----------|
| line ~208 | `BackendConfig::default()` |
| line ~500 | `ask_default_model()` |
| line ~686 | `compact_default_model()` |

Update the 4 test assertions in `config.rs` that assert the default equals `"qwen3:14b"` to assert `"qwen3:4b"`.

Test fixtures in `conversations_cmd.rs` and `conversations_cost_report.rs` that use `"qwen3:14b"` as explicit test data are left unchanged — they test model-selection logic, not the default value.

### Change 2 — Wire `build_llm_menu` into init mode 3

The LLM discovery infrastructure is already complete:
- `aggregate::build_llm_menu()` exists and mirrors `build_embedding_menu()`
- `preference::LLM_PREFERENCE` ranks `qwen3.5:4b`=90, `qwen3.5:9b`=95, etc.
- Ollama discovery correctly classifies chat models as `ModelKind::Llm`

What is missing: a `select_local_llm()` function and its wiring.

#### New function: `select_local_llm` in `mur-core/src/cmd/init_local.rs`

Signature:
```rust
pub fn select_local_llm(
    config: &mut mur_common::config::Config,
    available: &[crate::discovery::DiscoveredModel],
) -> Result<bool>
```

Returns `Ok(true)` if config was written, `Ok(false)` if the user picked Skip or a [pull] row.

Behaviour mirrors `select_local_embedding()`:
1. Calls `build_llm_menu(available)` → ranked rows: `[auto]`, installed, `[pull]`, Skip
2. Prints numbered menu
3. Reads user choice, defaults to row 1
4. **Auto/Pulled rows** — writes `config.llm` based on `m.backend`:
   - `Backend::Ollama` → `provider="ollama"`, `model=m.id`, `api_key_env=None`, `openai_url=None`
   - `Backend::OMlx` → `provider="openai"`, `model=m.id`, `api_key_env=Some("OMLX_API_KEY")`, `openai_url=Some("http://localhost:8000/v1")`
   - Prints an `OMLX_API_KEY` hint if the env var is absent
5. **Pull rows** — Ollama-style ids (`name:tag` without `/`): calls `ollama pull <id>`. HF-style ids (contain `/`): prints oMLX dashboard instructions. Returns `Ok(false)` in both cases.
6. **Skip row** — prints "Keeping current LLM config." Returns `Ok(false)`.

#### Wiring change in `mur-core/src/cmd/init.rs` mode 3

Replace the existing `prompt_backend()` → `select_model()` chain:

**Before:**
```rust
let runtimes = detect_local_runtimes();
match prompt_backend(&runtimes)? {
    Some(LocalBackend::Ollama) => {
        let m = select_model(OLLAMA_RECS)?;
        config.llm.provider = "ollama"...
        ...
    }
    Some(LocalBackend::OMlx) => { ... }
    Some(LocalBackend::MlxLm) => { ... }
    None => { /* no runtime */ }
}
```

**After:**
```rust
let runtimes = detect_local_runtimes();
print_runtime_summary(&runtimes);          // keep diagnostic output
if !runtimes.ollama_installed && !runtimes.omlx_installed && !runtimes.mlx_lm_installed {
    print_install_help(runtimes.apple_silicon);
} else {
    let available = discover_blocking(refresh_discovery)?;
    let wrote = select_local_llm(&mut config, &available)?;
    if wrote {
        let available_embed = discover_blocking(false)?;
        select_local_embedding(&mut config, &available_embed)?;
        crate::store::config::save_config(&config)?;
        println!("  ✓ Config: {}/{} (LLM) + {}/{} (search)",
            config.llm.provider, config.llm.model,
            config.embedding.provider, config.embedding.model);
    }
}
```

Note: `discover_blocking` is called once for LLM selection. A second call for embedding selection is acceptable (the cache TTL covers it). Alternatively a single call can be passed to both if the same `available` list is filtered by kind in each menu builder — both `build_llm_menu` and `build_embedding_menu` already filter internally, so passing the full list to both is correct.

#### Remove `prompt_backend` auto-selection

`prompt_backend()` previously auto-selected a backend without user input. Its diagnostic output (which runtimes are detected) is preserved via `print_runtime_summary()`. The auto-selection logic is no longer needed because the dynamic menu lets the user pick any installed model from any backend. `prompt_backend()` can be removed or kept as dead code; removing it is cleaner.

`MLX_RECS` and `OLLAMA_RECS` remain as pull suggestion sources: `LLM_PREFERENCE` (which backs `build_llm_menu`'s pull rows) already encodes the same model ids. `OLLAMA_RECS` and `MLX_RECS` can be removed once the dynamic path is fully in place, but this is optional cleanup.

## Invariants

- `select_local_llm` must never write `config.llm` for a Pull or Skip row.
- Pull for Ollama-style ids calls `ollama pull`; the user is told to re-run `mur init` after pulling (same as embedding path).
- HF-style ids (oMLX) show the dashboard instruction; no CLI pull available.
- If `available` is empty (no runtimes running), `build_llm_menu` returns 2 pull suggestions + Skip. The user can still pull a model.
- The config default fix is independent of the dynamic selection fix; either can ship alone.

## Files Changed

| File | Change |
|------|--------|
| `mur-common/src/config.rs` | 3 default strings + 4 test assertions |
| `mur-core/src/cmd/init_local.rs` | Add `select_local_llm()` |
| `mur-core/src/cmd/init.rs` | Replace mode-3 chain; wire `select_local_llm` |

## Non-goals

- Dynamic registry query (hitting registry.ollama.ai at runtime) — not needed; `LLM_PREFERENCE` serves as the curated recommendation table and is updated with code.
- Changing the embedding selection path — already correct.
- `mlx-lm` backend: not returned by `discover_blocking` (no discovery impl); continues to be handled by the static `print_install_help` / `print_runtime_summary` diagnostic. If `Backend::MlxLm` is never present in `available`, the menu simply won't show mlx-lm models — acceptable.
