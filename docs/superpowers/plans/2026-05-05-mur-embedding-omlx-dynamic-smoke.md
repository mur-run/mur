# M5 Manual Smoke Checklist

Run after `cargo build --release` against the merged M1+M2+M3+M4+M5 branch.

## Case 1 — oMLX-only (no Ollama)

**Setup:** Stop Ollama daemon. Ensure oMLX.app server is running with
`mlx-community/Qwen3-Embedding-0.6B-8bit` pulled.

```bash
rm -f ~/.mur/cache/discovery.json   # clean slate
./target/release/mur init --hooks --refresh-discovery
```

**Pick Mode 1.** At embedding prompt, expect:
- Row 1: `[auto] oMLX/mlx-community/Qwen3-Embedding-0.6B-8bit (1024d)`
- Row 2-N: any other oMLX models pulled
- `[pull] qwen3-embedding:0.6b` row
- `Skip — configure later` row

Press Enter. Verify `~/.mur/config.yaml`:
```yaml
embedding:
  provider: omlx
  model: mlx-community/Qwen3-Embedding-0.6B-8bit
  dimensions: 1024
  api_key_env: OMLX_API_KEY
  openai_url: http://localhost:8000/v1
```

Verify `OMLX_API_KEY` hint printed.

## Case 2 — Ollama-only (no oMLX)

**Setup:** Quit oMLX.app. Ensure `qwen3-embedding:0.6b` is pulled in Ollama.

```bash
rm -f ~/.mur/cache/discovery.json
./target/release/mur init --hooks --refresh-discovery
```

Pick Mode 1, press Enter at embedding prompt. Expect:
- Row 1: `[auto] Ollama/qwen3-embedding:0.6b (1024d)`

Verify `~/.mur/config.yaml`:
```yaml
embedding:
  provider: ollama
  model: qwen3-embedding:0.6b
  dimensions: 1024
```

## Case 3 — Both backends with embedding models

**Setup:** Both Ollama and oMLX running, both have an embedding model pulled.

Expect one of them as `[auto]` (the higher-ranked one), the other as a
`Pulled` row. Either backend as `[auto]` is acceptable as long as the
selected kind is Embedding.

## Case 4 — Both backends, no embedding models

**Setup:** Both daemons running, neither has any embedding model pulled.

Expect:
- Row 1: `[pull] qwen3.5-embedding` (highest score in preference table)
- Row 2: `[pull] Qwen3-Embedding-8B`
- Row 3: `Skip — configure later`

Pick row 1 (the Ollama-style tag). Verify `ollama pull qwen3.5-embedding` runs
(progress streams to stdout). On success, verify init prints:
```
✓ Pulled. Re-run `mur init` to select it.
```

If the tag is an HF-id form (`mlx-community/...`), verify the oMLX GUI hint
is printed instead of an ollama pull.

## Case 5 — `--refresh-discovery` busts cache

**Setup:** After Case 1, ensure `~/.mur/cache/discovery.json` exists.

```bash
./target/release/mur init --hooks --refresh-discovery
```

Verify the cache file mtime is updated after this run. To observe the probe
latency in trace logs:

```bash
RUST_LOG=debug ./target/release/mur init --refresh-discovery
```

Expect `[DEBUG mur_core::discovery::ollama]` or `[DEBUG mur_core::discovery::omlx]`
lines confirming fresh discovery.

## Case 6 — Mode 3 (all local) with oMLX LLM + oMLX embedding

**Setup:** oMLX running with both a chat model (e.g. `Qwen3-4B`) and an
embedding model (e.g. `Qwen3-Embedding-0.6B-8bit`) pulled.

```bash
./target/release/mur init
```

Pick Mode 3 → oMLX as backend → select the LLM model. At the embedding prompt,
expect oMLX embedding model in the discovery menu. Press Enter.

Verify `~/.mur/config.yaml` has:
- `llm.provider: openai`, `llm.openai_url: http://localhost:8000/v1`
- `embedding.provider: omlx`, `embedding.openai_url: http://localhost:8000/v1`
