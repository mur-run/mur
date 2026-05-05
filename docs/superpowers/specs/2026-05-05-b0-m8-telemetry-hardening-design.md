# B0 M8 — Telemetry hardening design

**Status:** Draft (M8 cascade entry)
**Author:** david
**Date:** 2026-05-05
**Roadmap anchor:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 rules 9 + 12 (B0 v1).
**Predecessors on main:** M7.1–M7.8 (B0 text rules), M5.1–M5.6 (C5 webhook), all D-track.

---

## 1. Goal

Close the last two privacy-correctness gaps blocking v1's privacy statement:

- **B0 rule 9** — telemetry / crashlogs must redact tool-result bodies and user-file content.
- **B0 rule 12 audit** — verify companion subsystem cannot egress to non-model-provider hosts; document the residual model-provider egress.

These are the prerequisites for shipping a truthful `docs/release/privacy-statement.md` in v1.

## 2. Threat model — what leaks today

The runtime writes `~/.mur/agents/<name>/telemetry/<date>.jsonl`. Inspection of `mur-agent-runtime/src/telemetry_writer.rs` (events shipped 2026-04-22 in P0a) shows:

| Field | Source | Leak risk | Mitigation needed |
|---|---|---|---|
| `Event::LlmCall.{model, tokens, cost, latency, provider}` | M0 hook chain | metadata only | none |
| `Event::ToolCall.{mcp_server, tool, duration, ok}` | M0 hook chain | metadata only | none |
| `Event::Error.message` | `anyhow::Error` `Display` chain | **HIGH** — embeds file paths, tool error strings, contextual values | redact at write boundary |
| `Event::Warning.message` | hooks / supervisor | **MEDIUM** — free-form | redact |
| `Event::TaskProgress.message` | task runner | **MEDIUM** — free-form | redact |
| `Event::HookFired.attrs` (Value::Object merged into envelope) | arbitrary hook payload | **HIGH** — unstructured; hooks may insert tool args / outputs / file content | redact every string-valued leaf |
| `Event::BridgeAlive.bridge_id` | bridge spawn | metadata only | none |

The structural fields (token counts, durations, names) are intentional metadata. The free-form strings (`Error.message`, `Warning.message`, `TaskProgress.message`, every leaf of `HookFired.attrs`) are the leakage surface.

## 3. Architecture

**Redact at the write boundary, not at the source.** A single redactor pass inside `telemetry_writer::event_to_notification` (or its caller `f.write_all`) gives one chokepoint that cannot be bypassed by adding a new `Event` variant or a new hook.

```
hook / supervisor / bridge
    ↓ Event::* (untrusted free-form strings)
TelemetryWriter::event_to_notification()
    ↓ Value (envelope JSON)
[NEW] redact_envelope(&mut Value)              ← M8.1
    ↓ Value (string leaves redacted)
serde_json::to_string + f.write_all
    ↓ JSONL on disk
```

**Redactor reuse.** `crate::hooks::b0_helpers::scan_for_secrets(&str) -> Option<&str>` (M7.5) already enumerates the ~11 credential patterns. M8.1 adds a sister `redact_secrets(&str) -> Cow<'_, str>` that walks the same regex set and replaces matches with `[REDACTED:<label>]`. Single regex source of truth across rule 7 (drop) and rule 9 (redact).

**File-path content.** Tool errors of the form `Error: failed to read /home/alice/.ssh/id_rsa: …` should be redacted because the path is a privacy leak (home dir, user ID). M8.1 extends the redactor with a path heuristic: `/home/<user>/`, `/Users/<user>/`, `C:\Users\<user>\` → `~/`. This is a shallow pass; we don't try to detect "all privacy-sensitive content" since that's a research problem (B1).

## 4. Milestones

### M8.1 — `redact_secrets` + `redact_envelope` core (~120 LOC + tests)

- `mur-agent-runtime/src/hooks/b0_helpers.rs`: add `redact_secrets(&str) -> Cow<'_, str>` reusing M7.5's regex set; add `redact_home_path(&str) -> Cow<'_, str>` for the three home-dir patterns.
- `mur-agent-runtime/src/telemetry_writer.rs`: new `redact_envelope(value: &mut Value)` that recurses into `Value::Object`/`Value::Array`, applies both redactors to every `Value::String` leaf in-place.
- Wire `redact_envelope` between `event_to_notification` and `f.write_all`.
- Tests: 8 fixtures (each credential pattern + each path pattern + empty + nested object).

**Acceptance:** `cargo test -p mur-agent-runtime telemetry::redact` green; integration test injects `Event::HookFired { attrs: { "tool_input": { "key": "sk-ant-api03-…" } } }` and asserts the on-disk JSONL contains `[REDACTED:anthropic-key]`.

### M8.2 — apply redactor to commander observability collector (~40 LOC)

- `engine::observability::redaction` already has full / redacted / metadata-only modes. Verify the `redacted` mode delegates to a regex set equivalent to M8.1's; if not, refactor to import the same patterns.
- This is cross-repo (`~/Projects/mur-commander branch feat/b0-m8`). Same Tier 2 cascade discipline.

**Acceptance:** the commander's spool produces JSONL where credential patterns are `[REDACTED:…]`, identical labels to mur side.

### M8.3 — companion zero-network audit (~80 LOC)

- Inventory: companion modules import only `crate::llm::LlmClient` for network — no `reqwest` / `tokio::net` / `hyper` / `surf` / `ureq` / `isahc`. The audit confirms this.
- **Refine the rule-12 claim.** Spec currently says "companion subsystem has no network egress" — strictly false because `LlmClient` calls the configured model provider. Update roadmap §6.1 rule 12 to: "companion's only outbound is to the agent's configured model provider; no other hosts are reachable via the companion code path."
- Enforce: `cargo deny` config that forbids companion files from gaining a direct dep on any HTTP client crate (defense in depth — `LlmClient` is the only allowed indirection).
- Integration test: dispatch `outbox::tick` against a `MockLlmClient` and assert no real-network sockets opened (use `tokio::net::TcpListener::bind("127.0.0.1:0")` accept-loop as a poison-canary; if companion reaches network it'll hit the canary and fail).

**Acceptance:** `cargo test -p mur-agent-runtime companion::network_audit` green; `cargo deny check` green; roadmap §6.1 rule 12 wording updated.

### M8.4 — E2E + cookbook (~50 LOC + docs)

- `scripts/e2e/b0-m8-telemetry-redaction.sh`: spawns an agent, fires a hook with a fake credential in attrs, reads the on-disk JSONL, asserts redaction.
- `docs/cookbook/b0-telemetry-redaction.md`: explains what is redacted, what isn't (token counts, model names, durations), how to tail logs without leaking secrets, and the residual companion model-provider egress.
- Roadmap §6.1 footer: "B0 rule 9 + 12 shipped 2026-05-XX."

### M8.5 — privacy statement release doc (~150 LOC of docs)

- `docs/release/privacy-statement.md`: 6 sections — what's collected locally / what leaves the device / what's redacted / Telegram non-E2E disclosure / voice-stays-on-device / how to disable telemetry.
- Cross-link from `README.md` and Documents page (CLAUDE.md docs checklist item 2).

## 5. Non-goals

- **Not building** AgentDojo / HarmBench eval harnesses — those are v1.1 work (separate infra spec).
- **Not implementing** B0 rule 6 (MCP install verifier) — separate cascade after M8.
- **Not** redacting structural metadata (model name, token counts, tool names, durations) — those are operational telemetry and the v1 telemetry contract.
- **Not** trying to detect "all PII" via NER/classifier — that's B1 work (research problem).

## 6. Acceptance gates

- 8/8 redactor unit tests green (rule 9).
- Integration test confirms on-disk JSONL is redacted (rule 9).
- Companion network-audit test green + `cargo deny` green (rule 12).
- `bash scripts/e2e/b0-m8-telemetry-redaction.sh` green.
- `docs/release/privacy-statement.md` reviewed by user.
- Roadmap §6.1 rule 9 + 12 footers updated.

## 7. Open questions

- **Should `Event::Error.message` redaction be more aggressive?** anyhow's `Display` walks the entire context chain. A single error like "failed to load profile: failed to parse YAML at line 42: unexpected `secret_key: sk-…`" exposes the secret. M8.1's regex pass should catch the credential portion; the rest is acceptable.
- **Cross-repo timing.** M8.2 (commander side) can land in either order vs M8.1, since they share no compile-time dependency. Default: ship M8.1 first to lock the regex API, then M8.2 imports it.

## 8. Cascade plan

Same Tier 2 stacked-PR recipe as M5 / M7:

| PR | Branch | Base |
|---|---|---|
| M8.0 (this spec) | `feat/mur-agent-b0-m8.0-spec` | main |
| M8.1 redactor + writer wiring | `feat/mur-agent-b0-m8.1-redactor` | M8.0 |
| M8.2 commander observability | (cross-repo `mur-commander#feat/b0-m8`) | main of mur-commander |
| M8.3 companion audit | `feat/mur-agent-b0-m8.3-companion-audit` | M8.1 |
| M8.4 E2E + cookbook | `feat/mur-agent-b0-m8.4-e2e-cookbook` | M8.3 |
| M8.5 privacy statement | `feat/mur-agent-b0-m8.5-privacy-doc` | M8.4 |

5 mur-side PRs + 1 commander PR. Estimated ~3 dev-days.
