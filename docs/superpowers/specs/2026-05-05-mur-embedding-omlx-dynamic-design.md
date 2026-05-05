# Dynamic Local Embedding Discovery (incl. oMLX) — Design

**Status**: proposed
**Author**: david + Claude Opus 4.7 (1M context)
**Date**: 2026-05-05
**Related**: P1.4 sources integration, M3.5 model registry & secret refs

## Problem

`mur init` (with or without `--hooks`) treats local embedding setup as a hardcoded waterfall:

1. **`select_ollama_embedding`** (`mur-core/src/cmd/init.rs:769-794`) hardwires four model options regardless of what is actually pulled.
2. **`select_cloud_embedding`** sends `provider: "openai" | "gemini" | "anthropic"` but the OpenAI embedder at `mur-core/src/store/embedding.rs:137` ignores `cfg.embedding.openai_url` and posts to `https://api.openai.com/v1/embeddings`. Gemini / Voyage / oMLX / mlx-omni-server are silently broken.
3. **oMLX is not offered as an embedding backend**, only as an LLM backend (`init.rs:923-946`, Mode 3 only). Even when oMLX has Qwen3-Embedding loaded, Mode 1 (the default recommended mode) writes `embedding.provider: ollama` unconditionally.
4. **No availability check** — the menu offers `qwen3-embedding:8b` even when the user has only `qwen3-embedding:0.6b` pulled, so the first retrieval call 404s on the model name.

End-user symptom (the report that triggered this work): user installed oMLX with `mlx-community/Qwen3-Embedding-0.6B-8bit`, ran `mur init --hooks`, picked Mode 1, and ended up with an Ollama-pinned config they did not consent to.

## Research findings

Three deep-research passes. URLs in §8.

### Q1 — Does oMLX expose embeddings? **Yes, confirmed-runs.**

- oMLX serves `POST /v1/embeddings` on `localhost:8000`.
- Backing engine is **`Blaizzy/mlx-embeddings`**, whose registry explicitly includes Qwen3 embedding family alongside BERT, BGE-M3, ModernBERT, XLM-RoBERTa. The README's "BERT/BGE-M3/ModernBERT" line is incomplete documentation, not an authoritative whitelist.
- Two production user reports (oMLX issues #266 and #684) confirm Qwen3-Embedding-0.6B / 0.6B-4bit-DWQ / 8B all load and serve through `/v1/embeddings`.
- oMLX v0.3.0 release notes ship dedicated processor hooks for Qwen3-VL-Embedding, demonstrating active maintenance for the Qwen3 embedding family.
- **mlx-lm CLI (`mlx_lm.server`)** has **no** embeddings endpoint — generation only. So if a user has only mlx-lm installed (no oMLX), embedding must fall back to Ollama or a third-party shim (e.g. `cubist38/mlx-openai-server`).
- **oMLX caveat #266**: graph recompiles on first call after >3s idle (~3s extra latency on first probe). Behaviour, not bug.

### Q2 — Qwen3.5-Embedding state-of-the-art? **Does not exist (yet).**

- Alibaba shipped Qwen3.5 (chat/base, 0.8B-9B, March 2026) and Qwen3.6 (April 2026). Neither has an embedding variant.
- Current SOTA open-weight multilingual embedder is still **Qwen3-Embedding** (June 2025): 8B → MTEB-multilingual 70.58, 4B → 69.45, 0.6B → 64.33. Apache-2.0, Matryoshka-rep, 100+ languages.
- Ollama tags: `qwen3-embedding:{0.6b|4b|8b}` plus `-q4_K_M / -q8_0 / -fp16` quants. `qwen3-embedding:latest` aliases to `:8b` (4.7 GB Q4).
- HF MLX: `mlx-community/Qwen3-Embedding-{0.6B|4B|8B}-{4bit-DWQ|8bit}`.
- Credible alternatives (May 2026 multilingual leaderboard): BGE-M3 (568M, dense+sparse+multivector), jina-embeddings-v3 (570M, 8K ctx), EmbeddingGemma (308M, ultra-light).
- For Chinese-heavy corpora, Qwen3-Embedding still wins C-MTEB.

### Q3 — Dynamic detection prior art

- **Ollama**: `POST /api/show` → `capabilities[]` array contains `"embedding"` when GGUF metadata has `pooling_type`. Authoritative but doc-example-not-yet-updated; verify on target Ollama version. Fallback heuristic: `/api/tags` → `details.family ∈ {bert, nomic-bert, jina-bert, qwen3}`.
- **oMLX / mlx_lm.server**: `/v1/models` is flat OpenAI-shape, no `type` field. Disambiguation requires either probing `/v1/embeddings` with a 1-token request, or matching against a known-embedder allowlist by HF id.
- **LM Studio** (the gold standard): `GET /api/v0/models` returns each model with `"type": "llm" | "embedding"` plus `state: "loaded" | "unloaded"`. Drop-in filter.
- **Continue.dev pattern**: `model: AUTODETECT` plus `roles: [embed, chat, ...]` per-model override, capabilities autodetect with explicit `capabilities: []` array override. **Always allow user override** — heuristics misclassify edge cases (fine-tuned classifiers showing up as "embedding").

## Decisions

1. **Bundle four changes into one feature** — bug fix (`openai_url` honored), discovery module, init UX rewrite, LLM-side dynamic discovery in Mode 3. Single Brainstorming → Plan → cascade-merge cycle.
2. **Mode 1 default = `[auto]`** — Enter on the first menu entry picks the top-of-preference model that is actually pulled. Power users see the detected list and can override; casual users hit Enter twice and get the right thing. (Choice B in the brainstorming round.)
3. **Pull behaviour is mixed** — Ollama auto-pulls via `Command::new("ollama").arg("pull")...status()` with stdout passthrough; oMLX prints a GUI hint (oMLX has no CLI pull mechanism). Same logic applies to LLM Mode 3. (Choice B in the brainstorming round.)
4. **Probe-based dimension detection** — once user picks a model, mur issues a 1-token `POST /v1/embeddings` (or `POST /api/embed`) to learn the actual dim and write it to config. This is the only Matryoshka-safe approach.
5. **Static preference table with prefix matching** — future-proofs against `qwen3.5-embedding` and similar future tags without code changes. Hardcoded dim/family hints serve as cache-warming fallback when probe fails.
6. **Discovery cache** at `~/.mur/cache/embedding-discovery.json`, TTL 24h, schema-versioned. `mur init --refresh-discovery` busts it. Cache is per-runtime-endpoint to support multi-instance setups.
7. **Model registry tie-in deferred** — `EmbeddingConfig` is designed to grow an optional `model_ref:` field later (mirroring LLM `model_ref` from M3.5), but this design does not extend `models.yaml` or touch `mur model add`. Marked future-work in §7.
8. **No `embedding.enabled: false` flag** — pure-BM25 retrieval is not a supported mode in mur today. Not adding it as a side effect.
9. **No mixed-workload guard** for oMLX issue #266 — current evidence is latency-only, not correctness. Document the first-call slowness; do not refuse setup.

## §1 Architecture

```
mur-core/src/
├── discovery/                                ← new module
│   ├── mod.rs                                ← Discovery trait + Backend + DiscoveredModel + cache I/O
│   ├── ollama.rs                             ← OllamaDiscovery
│   ├── omlx.rs                               ← OMlxDiscovery
│   └── preference.rs                         ← prefix-matched preference table (LLM + embedding)
├── store/embedding.rs                        ← bug fix: EmbeddingProvider::OpenAI honors base_url
└── cmd/
    ├── init.rs                               ← select_ollama_embedding → select_local_embedding
    └── init_local.rs                         ← runtime detection retained; OLLAMA_RECS / MLX_RECS
                                                 reference preference.rs as fallback list

~/.mur/cache/discovery.json                   ← runtime cache (LLM + embedding), TTL 24h, schema_version: 1
```

`Discovery` is one trait, two impls. LLM-side init (`init_local.rs::select_model`) and embedding-side init (`init.rs::select_local_embedding`) both consume the same `Vec<DiscoveredModel>`, filtering by `kind`.

## §2 Components

### 2.1 `Discovery` trait

```rust
// mur-core/src/discovery/mod.rs
#[async_trait::async_trait]
pub trait Discovery: Send + Sync {
    fn backend(&self) -> Backend;
    async fn list_models(&self) -> anyhow::Result<Vec<DiscoveredModel>>;
    /// 1-token probe to determine kind + dim. Used after list_models when
    /// kind is Unknown, or when user picks a model whose dims we lack.
    async fn probe_embedding(&self, model_id: &str) -> anyhow::Result<EmbeddingProbe>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend { Ollama, OMlx }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,                  // "qwen3-embedding:0.6b" / "mlx-community/Qwen3-Embedding-0.6B-8bit"
    pub backend: Backend,
    pub kind: ModelKind,             // Llm | Embedding | Unknown
    pub dims: Option<usize>,         // populated for Embedding kind
    pub family: Option<String>,      // "qwen3", "bge", "modernbert", "bert"
    pub size_bytes: Option<u64>,     // /api/tags has it; /v1/models does not
    pub probed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelKind { Llm, Embedding, Unknown }

#[derive(Debug, Clone)]
pub struct EmbeddingProbe { pub dims: usize, pub latency_ms: u64 }
```

### 2.2 `OllamaDiscovery`

- `GET {endpoint}/api/tags` → enumerate models with `details.family`, `size`.
- For each, `POST {endpoint}/api/show {name}` → extract `capabilities` array.
  - `capabilities.contains("embedding")` → `kind = Embedding` (authoritative).
  - else if `capabilities.contains("completion")` → `kind = Llm`.
  - else (older Ollama versions without `capabilities`) → fallback heuristic: `family ∈ {bert, nomic-bert, jina-bert}` → `Embedding`; `family ∈ {qwen3, llama, gemma}` → `Llm`; else `Unknown`.
  - **Disambiguation note**: when `capabilities` is absent and family alone is the signal, `family = "qwen3"` is ambiguous (Qwen3 chat models share the family string with anyone who packages a Qwen3-derived embedder). In practice Ollama's `qwen3-embedding:*` GGUFs report `family = "bert"` (BERT-style pooling), so the heuristic generally works without name-string sniffing. If a future Ollama version reports `family = "qwen3"` for the embedder, fall back to `name.contains("embedding")` as a tiebreaker.
- Probe path: `POST {endpoint}/api/embed {model, input: "."}` with 5s timeout; success ⇒ `embeddings[0].len()` = dims.

### 2.3 `OMlxDiscovery`

- `GET {base_url}/v1/models` → flat list (`{data: [{id, ...}]}`). All entries are loaded models.
- Probe each candidate via `POST {base_url}/v1/embeddings {model, input: "."}` with 10s timeout (oMLX issue #266 recompile budget).
- Result mapping:
  - 200 + `data[0].embedding` array → `kind = Embedding`, `dims = embedding.len()`.
  - 4xx with `"does not support embeddings"` body → `kind = Llm`.
  - timeout / 5xx → `kind = Unknown`, log warn.
- Family inference from id: `Qwen3-Embedding-*` → `qwen3`; `bge-*` → `bge`; `*-modernbert-*` → `modernbert`.

### 2.4 `PreferenceTable`

```rust
// mur-core/src/discovery/preference.rs
const EMBEDDING_PREFERENCE: &[(&str, u32)] = &[
    ("qwen3.5-embedding",        105),  // future-proof; matches both Ollama and HF forms
    ("Qwen3-Embedding-8B",       100),  // oMLX HF form
    ("qwen3-embedding:8b",       100),  // Ollama tag form
    ("Qwen3-Embedding-4B",        90),
    ("qwen3-embedding:4b",        90),
    ("bge-m3",                    80),
    ("jina-embeddings-v3",        75),
    ("Qwen3-Embedding-0.6B",      70),
    ("qwen3-embedding:0.6b",      70),
    ("embeddinggemma",            55),
    ("nomic-embed-text",          40),
    ("all-minilm",                20),
];

// Aligned with the curated picks in init_local.rs (OLLAMA_RECS / MLX_RECS).
// Multilingual-first ordering — leading entry must handle Chinese well.
const LLM_PREFERENCE: &[(&str, u32)] = &[
    ("Qwen3.5-9B",                95),  // mlx-community HF id form
    ("qwen3.5:9b",                95),  // Ollama tag form
    ("Qwen3.5-4B",                90),
    ("qwen3.5:4b",                90),
    ("Gemma4-E2B",                85),
    ("gemma4:e2b",                85),
    ("Qwen3-9B",                  70),
    ("qwen3:9b",                  70),
    ("Qwen3-4B",                  65),
    ("qwen3:4b",                  65),
    ("llama3.3",                  60),
];

pub fn rank(id: &str, table: &[(&str, u32)]) -> u32 {
    table.iter()
        .filter(|(prefix, _)| id.contains(prefix))
        .map(|(_, score)| *score)
        .max()
        .unwrap_or(0)
}
```

`contains` not `starts_with` so both `Qwen3-Embedding-0.6B` and `mlx-community/Qwen3-Embedding-0.6B-8bit` match the same rule.

### 2.5 `EmbeddingProvider` bug fix

```rust
// mur-core/src/store/embedding.rs
pub enum EmbeddingProvider {
    Ollama { base_url: String },
    OpenAI { api_key: String, base_url: String },  // ← new field
}

impl EmbeddingProvider {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        match cfg.embedding.provider.as_str() {
            "openai" | "gemini" | "anthropic" | "voyage" | "omlx" | "mlx" => {
                let api_key_env = cfg.embedding.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
                let api_key = std::env::var(api_key_env).unwrap_or_default();
                let base_url = cfg.embedding.openai_url.clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                Ok(EmbeddingProvider::OpenAI { api_key, base_url })
            }
            _ => Ok(EmbeddingProvider::Ollama {
                base_url: cfg.embedding.ollama_endpoint.clone(),
            }),
        }
    }
}

async fn embed_openai(client: &Client, base_url: &str, api_key: &str, model: &str, input: &str)
    -> Result<Vec<f32>> {
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    // ... POST as before
}
```

Note: oMLX accepts any non-empty token on localhost. The init flow sets `api_key_env: OMLX_API_KEY` and prints a hint to `export OMLX_API_KEY=local`. If the env var is unset at request time, `embed_openai` will get an empty bearer header — oMLX's localhost path tolerates this in current versions, but the hint is the documented convention.

### 2.6 Cache

```rust
// ~/.mur/cache/discovery.json
{
  "schema_version": 1,
  "entries": [
    {
      "endpoint": "http://localhost:11434",
      "backend": "Ollama",
      "captured_at": "2026-05-05T03:30:00Z",
      "models": [ { "id": "qwen3-embedding:0.6b", "kind": "Embedding", "dims": 1024, ... } ]
    }
  ]
}
```

TTL 24h. `--refresh-discovery` flag on `mur init` or stale entry triggers fresh probe. Schema mismatch → log warn, treat as empty, repopulate.

## §3 Data flow — `mur init` Mode 1

```
1. detect_local_runtimes()                     # existing, returns LocalRuntimes
2. spawn parallel discovery tasks for each present runtime:
     · OllamaDiscovery::list_models  →  cache.entries
     · OMlxDiscovery::list_models    →  cache.entries
3. merge → Vec<DiscoveredModel>; filter kind ∈ {Embedding, Unknown}
4. preference::rank(id, EMBEDDING_PREFERENCE) → sort desc
5. render menu (4 segments, in order):
     entry 1            : "[auto] <top-of-rank pulled model>"   ← always single entry, the default
     entries 2..N       : remaining pulled models with kind ∈ {Embedding, Unknown}
     entries N+1..N+2   : "[pull] <id>" — top 1-2 preference-table entries NOT yet pulled
     last entry         : "Skip — configure later"
6. prompt user; pressing Enter selects entry 1 ([auto])
7. dispatch on choice:
     · entry 1 ([auto]) or 2..N (already-pulled)
                    → probe_embedding(id) for dims if cache miss → write config
                    → if probe says kind=Llm or 4xx, abort with error and re-prompt
     · [pull] Ollama → spawn `ollama pull <id>` (stdout passthrough, inherits stdin for Ctrl-C)
                       on success: re-discover, probe dims, write config
                       on failure: print stderr; leave embedding unconfigured
     · [pull] oMLX  → print "Open oMLX.app → Models → search '<id>' → Pull"
                       leave embedding unconfigured
     · [skip]       → leave embedding unconfigured (current config preserved)
```

Mode 3 adds a parallel pass for LLM (filter `kind ∈ {Llm, Unknown}`, use `LLM_PREFERENCE`). Same data flow, different filter and table.

## §4 Error handling

| Scenario | Behaviour |
|---|---|
| Runtime detected, daemon not running (Ollama / oMLX server) | Print runtime-specific start hint; `list_models` returns empty; menu skips that backend |
| Probe timeout — Ollama 5s | Mark `kind = Unknown`; keep in menu with `[?]` tag; full probe on user select |
| Probe timeout — oMLX 10s (issue #266 budget) | Same as above |
| Probe returns 4xx "model does not support embeddings" | `kind = Llm`; excluded from embedding menu but kept for LLM menu |
| `ollama pull` exit ≠ 0 | Print stderr; embedding stays unconfigured; init continues to community / sync setup |
| `ollama pull` interrupted (Ctrl-C) | Subprocess dies via SIGINT propagation; init exits with the same signal |
| Cache file corrupt JSON | log::warn; ignore; repopulate from fresh discovery |
| Cache schema_version mismatch | Same as corrupt: discard, repopulate |
| User selects `[skip]` and current `config.yaml` already has embedding set | Preserve existing config; print "Keeping current embedding config: <provider>/<model>" |
| Both backends present, both have no embedding model | Default `[pull]` row points to Ollama `qwen3-embedding:0.6b` (700MB, lowest barrier); oMLX hint appears as second `[pull]` option |
| `OMLX_API_KEY` env not set when oMLX selected | Set inline `OMLX_API_KEY=local` in shell hint printed at end of init |
| HF id contains `/` (e.g. `mlx-community/Qwen3-...`) | Pass through verbatim — oMLX accepts the full HF id as `model` parameter |

## §5 Testing

### Unit (no network)

- `discovery::preference::rank` — table-driven against both `EMBEDDING_PREFERENCE` and `LLM_PREFERENCE`, exhaustive over: HF-id form (`mlx-community/X`), Ollama-tag form (`x:N`), unknown id (returns 0), case sensitivity expectations.
- `discovery::cache` — TTL expiry, schema-version mismatch, corrupt JSON, atomic write.
- `EmbeddingProvider::from_config` — six providers (`openai`, `gemini`, `anthropic`, `voyage`, `omlx`, `mlx`) all route to OpenAI variant with correct `base_url`.
- `embed_openai` URL construction — trailing slash on `openai_url`, default fallback.

### Mocked HTTP (`wiremock` crate)

- `OllamaDiscovery::list_models`:
  - `/api/tags` returns mixed embedding + chat models.
  - `/api/show` with `capabilities: ["embedding"]` → kind=Embedding.
  - `/api/show` without `capabilities` field, family=`qwen3`, name contains `embedding` → kind=Embedding (heuristic path).
  - `/api/show` 500 error → entry kept with kind=Unknown.
- `OMlxDiscovery::list_models`:
  - `/v1/models` returns 3 candidates.
  - `/v1/embeddings` 200 with 1024-dim → kind=Embedding, dims=1024.
  - `/v1/embeddings` 400 "does not support embeddings" → kind=Llm.
  - `/v1/embeddings` timeout → kind=Unknown.

### Integration (opt-in, env-gated)

- `OLLAMA_E2E=1` — requires real Ollama on `localhost:11434` with `qwen3-embedding:0.6b` pulled. Asserts probe returns dim=1024.
- `OMLX_E2E=1` — requires real oMLX on `localhost:8000` with `mlx-community/Qwen3-Embedding-0.6B-8bit` pulled. Asserts probe returns dim=1024 within 10s.
- Both — full `mur init` Mode 1 dry-run path: discovery → menu render (capture stdout) → assert top entry is oMLX (since oMLX > Ollama at same rank).

### Manual smoke (release gate)

Spec-shipped checklist, four cases:

1. oMLX-only (no Ollama) — picks oMLX/Qwen3-Embedding.
2. Ollama-only (no oMLX) — picks Ollama/qwen3-embedding.
3. Both backends, both have models — picks oMLX (priority).
4. Both backends, neither has embedding model — `[pull]` row offers Ollama default.

## §6 Migration & rollout

Phased into 5 small PRs (each ≤ 300 LOC, mergeable independently).

| PR | Scope | LOC | Blocking |
|----|---|---|---|
| **M1** | Bug fix: `EmbeddingProvider::OpenAI { base_url }` honored. New `omlx` / `mlx` provider aliases. Tests for from_config. | ~150 | none |
| **M2** | `discovery::mod` + `discovery::preference` + cache scaffold. Pure logic, no HTTP. | ~250 | M1 |
| **M3** | `discovery::ollama` + wiremock tests. | ~250 | M2 |
| **M4** | `discovery::omlx` + wiremock tests. | ~250 | M2 (parallel with M3) |
| **M5** | `init.rs::select_local_embedding` rewrite + LLM-side wiring in `init_local.rs`. Manual smoke checklist. | ~300 | M1, M3, M4 |

`M1` ships value standalone (users can hand-edit `config.yaml` to point at oMLX). `M2-M4` ship value to library consumers but no UX change. `M5` flips the user-facing flow.

Existing config files are untouched by `mur init` re-runs unless the user chooses to re-configure.

## §7 Open questions / non-goals

- **`mur model add --embedding`**: deferred to a follow-up. `EmbeddingConfig` reserves space for `model_ref: Option<String>` but the field is not added in this design. Spec at `2026-04-29-model-registry-and-secret-refs-design.md` § future work.
- **mlx-lm CLI for embeddings**: not supported. mlx-lm has no `/v1/embeddings`. Init prints hint pointing user to oMLX or third-party shim if mlx-lm is the only MLX runtime.
- **Pure-BM25 fallback**: out of scope. mur's retrieval pipeline assumes vector search.
- **Embedding model auto-update** (Ollama notifying mur of new versions): out of scope.
- **Reranker discovery**: out of scope; this design covers embedders only. Rerankers (BGE-reranker-v2-m3 etc.) are a future concern.
- **Per-collection embedding model** (sources A uses Qwen3-Embedding, sources B uses BGE-M3): out of scope. One global embedding model per mur install.

## §8 Citations

- [jundot/omlx (GitHub)](https://github.com/jundot/omlx)
- [oMLX issue #266 — Qwen3-Embedding-0.6B / 8B confirmed working](https://github.com/jundot/omlx/issues/266)
- [oMLX issue #684 — Qwen3-Embedding-0.6B-4bit-DWQ in production](https://github.com/jundot/omlx/issues/684)
- [oMLX v0.3.0 release — Qwen3-VL-Embedding processor hooks](https://github.com/jundot/omlx/releases/tag/v0.3.0)
- [Blaizzy/mlx-embeddings — Qwen3 in registry](https://github.com/Blaizzy/mlx-embeddings)
- [mlx-lm SERVER.md — no /v1/embeddings](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md)
- [mlx-community/Qwen3-Embedding-0.6B-8bit on HF](https://huggingface.co/mlx-community/Qwen3-Embedding-0.6B-8bit)
- [QwenLM/Qwen3-Embedding paper (arXiv 2506.05176)](https://arxiv.org/abs/2506.05176)
- [Ollama qwen3-embedding tags](https://ollama.com/library/qwen3-embedding/tags)
- [Ollama API reference (capabilities array)](https://github.com/ollama/ollama/blob/main/docs/api.md)
- [LM Studio /api/v0/models reference](https://lmstudio.ai/docs/developer/rest/list)
- [Continue.dev model capabilities + roles](https://docs.continue.dev/customize/deep-dives/model-capabilities)
- [BentoML 2026 open-source embeddings guide](https://www.bentoml.com/blog/a-guide-to-open-source-embedding-models)
