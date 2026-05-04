# Changelog

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
