# Changelog

## v2.8.0 (2026-05-06) — B0 Eval Harness + Baseline

### TL;DR

Closes the v1 measurement gap: ships the **B0 acceptance eval harness**
(M11) and records the first **real-LLM baseline** — HarmBench-50 vs
`claude-sonnet-4-6` at **41/50 = 82% refusal rate**, well above the
≥50% spec gate. Also fixes `mur init` LLM model selection and
documents the commander cross-network redaction model.

### 🆕 New — B0 Eval Harness (M11)

- **`mur agent eval report`** — Rust aggregator reads a JSONL run
  file and renders a per-suite / per-category Markdown report with
  aggregate pass-rate and spec-gate verdict. Exits non-zero if any
  gate fails.
- **HarmBench-50 runner** (`scripts/eval/harmbench/run.py`) — 50
  seeded cases stratified across 7 semantic categories; stub LLM
  (deterministic, free) + Anthropic backend (`--backend anthropic`).
  Heuristic grader `heuristic_v1` matches HarmBench paper §3.2
  refusal patterns; raw `response_text` stored per record so a future
  Phase B re-grade against `cais/HarmBench-Llama-2-13b-cls` requires
  no API re-spend.
- **AgentDojo-50 runner skeleton** (`scripts/eval/agentdojo/run.py`)
  — 50 seeded cases across 4 task suites; stub LLM wired; real
  agent-execution loop deferred to Phase B.
- **CI eval workflow** (`.github/workflows/eval.yml`) — stub-LLM job
  on every PR touching `scripts/eval/**`; real-LLM job on `v*` tags
  + weekly cron + manual dispatch. `make eval-stub` / `eval-release`
  top-level targets.
- **Committed case selections** — `scripts/eval/{agentdojo,harmbench}/
  case_selection.json` freeze the v2.8.0 subset (seed 1202914782 =
  SHA256("mur-b0-acceptance-2026")[:8]). Re-runs are comparable.

### 📊 v2.7.0 HarmBench Baseline

Run `01KQYJDTKZWBBX239MYHRRT320`, model `claude-sonnet-4-6` @ T=0:

| Category | passed |
|---|---|
| chemical_biological | 7/7 (100%) |
| copyright | 8/13 (62%) |
| cybercrime_intrusion | 4/8 (50%) |
| harassment_bullying | 3/3 (100%) |
| harmful | 3/3 (100%) |
| illegal | 8/8 (100%) |
| misinformation_disinformation | 8/8 (100%) |
| **Aggregate** | **41/50 = 82% — PASS** |

Artifacts: `eval-results/v2.7.0.{jsonl,md}`. Cost: ~$0.20.

### 🐛 Fixes

- **`mur init` dynamic LLM model selection** (#212) — model list
  fetched at runtime; `qwen3:14b` default now resolves correctly on
  fresh Ollama installs.

### 📄 Docs

- **Privacy statement §3.4** — documents `mur-commander`'s
  cross-network SHA-256 field-hash redaction (distinct from M8.1's
  local regex pass; both should stay — different threat models).

---

## v2.7.0 (2026-05-06) — v1.1 Hardening Pass

### TL;DR

The release that closes the v1.1 security/privacy gaps acknowledged
in the v2.6 privacy statement. Adds **MCP supply-chain pinning** (B0
rule 6), **telemetry + crashlog redaction** (rule 9), **companion
zero-network audit** (rule 12), plus a **webhook receiver** (Track
C5) so any HTTP-capable app can drive an agent over loopback HMAC-
signed POSTs.

### 🆕 New — Security & Privacy

- **B0 rule 6 — MCP install-time pinning** (M9). `mur agent mcp add`
  captures binary SHA-256 + publisher metadata + (via M9.3.5)
  description hash from a live `tools/list` probe. Supervisor
  refuses to start on binary drift; `mur agent mcp inspect` /
  `pin` recover. Stable inspect exit codes (0 clean / 1 binary
  drift / 2 description drift / 3 both / 4 unpinned / 5 binary
  missing) for CI gating. Probe budget tunable via
  `MUR_MCP_PROBE_TIMEOUT_S` env.
- **B0 rule 9 — telemetry redactor + crashlogs** (M8.1, M10).
  `redact_secrets` (~11 credential patterns) + `redact_home_path`
  (`/Users/<u>/`, `/home/<u>/`, `C:\Users\<u>\` → `~/`) applied at
  a single chokepoint in the writer's spawn loop; new event
  variants inherit redaction automatically. M10 extends the same
  pass to the panic-hook → `~/.mur/agents/<n>/crashlogs/<ts>-<pid>.log`
  writer.
- **B0 rule 12 audit — companion zero network** (M8.3). Compile-
  time `include_str!` audit refuses to build if any companion file
  imports `reqwest`, `hyper`, `surf`, `ureq`, `isahc`,
  `tokio::net::*`, or `std::net::*`. Drift guard: `pub mod` count
  in `companion/mod.rs` must match the audit's file list.
- **Privacy statement v1** — `docs/release/privacy-statement.md`
  shipped (M8.5). Locks the v2 contract: voice + OCR + companion
  stay on-device; only the configured model provider + opt-in MCP
  / bridges see content; no phone-home.

### 🆕 New — Trigger surface

- **Track C5 webhook receiver** — per-agent Axum endpoint at
  `/agents/{slug}/webhook`, HMAC-SHA256 verification (constant-time
  via `subtle`), token-bucket rate limit, reuses the C3
  `SendIngestor` trait so payloads enter the same B0
  `<untrusted_share>` flow as desktop drag-drop. Loopback by
  default; explicit bind for LAN exposure. Cookbook + curl /
  GitHub Actions worked examples in `docs/cookbook/c5-webhook.md`.

### 🆕 New — Local LLMs

- **oMLX dynamic embedding discovery** (M1–M5). Apple-silicon
  users with oMLX.app can now select arbitrary embedding models
  without hand-editing `~/.mur/config.yaml`; `mur init` probes
  `/v1/models` (3-shape parser handles oMLX / Ollama / OpenAI),
  then writes `embedding.openai_url` so subsequent runs hit the
  same endpoint. `OMLX_API_KEY` env honored. Auth-aware fallback
  to Voyage-via-Anthropic when no local provider is reachable.

### 🛠️ Changed — CI hardening

- `cargo nextest run` replaces `cargo test` on the workspace test
  job (parallel + faster on Linux/macOS; Windows-Defender
  exclusions added). Custom timeouts + heavy-test concurrency caps
  in `.config/nextest.toml` to prevent OOM on 16 GiB Windows
  runners.
- `apt-cache` action caches Tauri 2 system-libs install (~75 s
  saved per GUI job).
- Path-conditional CI: GUI / Test / Clippy / E2E skip on doc-only
  PRs; the `predicate-quantifier: every` mistake from the first
  cut (silently skipped tests on mixed-file PRs) was fixed in
  #198.
- `[profile.dev] debug = 0` workspace-wide for ~62% Windows wall-
  clock reduction.
- `tantivy 0.22 → 0.24` to dedupe with lance's transitive
  (~50K LOC duplicate compile saved per CI run).
- `qdrant-client` feature-gated behind `--features qdrant` (off by
  default; lancedb is the v2 backend). Drops ~3–5 min of duplicate
  axum/hyper/prost compiles on every Windows CI run.

### 🐛 Fixed

- v2.6.0 binary reported `2.5.0` from `--version` because the
  workspace `[workspace.package].version` wasn't bumped before the
  tag was cut. Fixed in #173 (v2.6.1) and locked into release-prep
  procedure.
- `TelemetryWriter::flush()` was a 50 ms sleep; replaced with
  deterministic FIFO drain via
  `mpsc<WriterMessage::Flush(oneshot::Sender<()>)>` ack. Fixes
  Windows + macOS CI flakes on the
  `llm_call_event_appends_jsonl_and_emits_notification` integration
  test.

### 📦 Notes for distributors

- 4 new e2e scripts: `scripts/e2e/c5-webhook.sh`,
  `scripts/e2e/b0-m8-telemetry-redaction.sh`,
  `scripts/e2e/b0-m9-mcp-install-verifier.sh`,
  `scripts/e2e/b0-m9.3.5-description-probe.sh`. Run them after
  build to validate the v1.1 invariants on your platform.
- Profile schema gained 4 optional fields on `McpServerEntry`
  (`binary_sha256`, `description_hash`, `publisher`,
  `installed_at`). All `Option` + `#[serde(default, skip_serializing_if)]`,
  so pre-v2.7 profiles deserialize unchanged.
- Webhook receiver requires you to set
  `transport.webhook.hmac_secret_ref` to an OS-keychain entry
  (`mur-agent` service). `mur agent webhook secret-set <name>`
  writes it. Bind defaults to `127.0.0.1:6789`.

---

## v2.5.0 (2026-05-04) — Desktop Companion + Cloud Backends

### TL;DR by use case

**You use mur as a CLI / session-capture tool:**
→ `mur learn`, `mur extract`, `mur conversations` now support
Anthropic / OpenAI / OpenRouter / Together / Fireworks / Gemini via
`~/.mur/config.yaml` `llm.provider`. New `mur conversations cost-report
--since 7d` shows per-stage USD spend. Apple Silicon users: `mur init`
auto-detects oMLX.app / mlx-lm CLI, prefers them over Ollama. Fixes a
silent regression where local Ollama returned empty strings for
`mur learn extract --llm`.

**You want to try the Agent / Companion subsystem:**
→ GUI shipped: drag-drop multimodal (HEIC/PDF/OCR), character card
import (SillyTavern + Character.AI), desktop notifications, 5-step
onboarding wizard. `mur agent export --format gui` produces a notarized
.app. Runtime now sandboxed with 11 safety rules.

### 🆕 New — CLI / Session Capture

- **6 cloud LLM providers** — Anthropic / OpenAI / OpenRouter / Together
  / Fireworks / Gemini, all routed through unified `ChatBackend`;
  existing Ollama users unaffected
- **`mur conversations cost-report`** — new command; aggregates per-stage
  (extractive / abstractive / rollup / extract_llm / learn / starter)
  USD estimates; `--json` for scripting
- **MLX local detection** — `mur init` auto-detects oMLX.app (port 8000)
  and mlx-lm CLI (port 8080) on Apple Silicon; priority oMLX > mlx-lm >
  Ollama
- **Anthropic prompt caching wire** — `cache_system` / `cache_user_prefix`
  hints land on the wire; activates when prompts grow past Haiku's
  2048-token cacheable minimum

### 🆕 New — Desktop Companion

- **Onboarding Wizard (D2)** — 5-step first-launch (CLI and GUI dual
  channel) builds first_memory + persona + proactive tier
- **Drag-Drop Multimodal (D3)** — sandboxed decoder
  (HEIC / PDF / Vision-OCR), ProvenanceLedger records asset origin, B0
  multimodal safety rules 13-22
- **Character Card I/O (D4)** — full CCv3 schema, Ed25519 sign/verify,
  SillyTavern PNG (chara + ccv3 chunks) bidirectional, Character.AI
  scraped JSON normalizer, `mur agent card export/accept` CLI, inbox
  quarantine
- **GUI Inbox Bridge (D5)** — notify-based watcher → Tauri
  `Channel<BridgeEvent>` → React sidebar + desktop notification + dock
  badge + why-accordion + quiet/proactive toggles + ack buttons
  (good / bad / dismiss)

### 🛡️ Hardening

- **macOS** — PrivacyInfo.xcprivacy (4 NSPrivacyAccessedAPICategory
  entries), `xcrun notarytool submit --wait`, `xcrun stapler staple`,
  `spctl --assess --type execute`, CI grep gate against non-compliant
  API usage
- **Runtime sandbox (B0 text-mode, 11 rules)** — fs confinement, network
  allowlist + GrantStore, tool-result history spotlight, spawn deny,
  outbound secret prefilter, memory redaction, MCP binary signature
  check

### 🐛 Bug Fixes

- **Ollama silent regression** — `num_predict: 0` was sent literally and
  Ollama interpreted it as "produce 0 tokens", causing
  `mur learn extract --llm` to return empty strings and silently fall
  through to logic-only extraction. Fixed: sentinel 0 → `num_predict:
  None` (matches pre-P4 behavior)
- **Anthropic OAuth transport leak** — OAuth handling decoupled from
  public transport surface (#79)

### ⬆️ Upgrade Notes

- **Config schema:** compatible — existing `~/.mur/config.yaml` needs no
  changes; all new fields are `#[serde(default)]`
- **Vector index:** no reindex needed — store schema unchanged
- **Brew users:** `brew update && brew upgrade mur`
- **Source build:** Rust 1.82+ (unchanged from v2.4.1)

### 📦 Internal / Foundations

- **Cloud-LLM rollout (P0–P4)** — deleted `mur-core/src/llm.rs`
  (524 LOC); all LLM calls now flow through
  `conversations::backend::ChatBackend` trait; ~−700 LOC net delete
- **Track C1 (A2A bridge foundations)** — `LlmEntitlement`,
  `BridgeRouteConfig`, `SignedEnvelope` + `TrustedPeer` verify,
  sled-backed `DedupeStore` (7d TTL), `AckTracker`, `BridgeBeacon`
  degraded surface have landed; full agent-to-agent message bridge
  (Telegram, send-from-any-app) planned for v2.6
- **Stats:** 98 commits / 24 PRs since v2.4.1

### 🙏 Thanks

(no external contributors this cycle — internal-only release)

## v2.2.0 (2026-03-25)

### 🚀 New Features

- **Variable System** — `mur var set/get/list/delete` for user-defined variables with `{{var}}` template expansion in workflows
- **Parameterize** — Auto-detect URLs, tokens, API keys, paths in workflows and suggest variable replacements
- **mur exit / mur quit** — Stop recording without export

### 🐛 Bug Fixes

- **Pattern Inject Scoring** — Fix detect_emails to skip git@ SSH URLs, fix detect_database_urls/detect_api_keys for KEY=value format
- **Hook Formatting** — Orphaned header guard, kind-aware rendering (Preference→bullet, Procedure→steps, Technical→numbered)
- **Variable.rs** — Fix unsafe set_var/remove_var in Rust 2024

### 📊 Tests

- 5 new hook formatting tests (494 total, all pass)

## v2.1.6

- Previous stable release
