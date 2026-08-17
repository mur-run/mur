# murmur `/model` Slash Command — Design

**Status**: approved (conversation 2026-08-17); implementation in progress
**Author**: david + Claude Fable 5

## Problem

Switching an agent's model today means editing `profile.yaml` and restarting
the runtime (the profile is loaded once at boot). Pi/Claude Code users expect
an in-session `/model` picker. murmur already has a slash-command surface
(`/channels`, `/sessions`, …) but no model command, and the runtime has no way
to change its LLM client after boot.

## Decisions (grilled + approved)

1. **Hot switch** — new A2A method `model/set`; the running supervisor swaps
   its live LLM client. No restart (auto-restart stays forbidden).
2. **Registry-only listing** — `/model` lists `~/.mur/models.yaml` entries.
   No key entry, no unregistered catalog models in the TUI (that flow is the
   CLI `mur model connect` project).
3. **Persist to profile** — a successful switch rewrites `model_ref` in the
   agent's `profile.yaml` so disk and process never disagree. The registry is
   never modified.

## UX (murmur TUI)

- `/model` — numbered list of registry entries grouped by provider, current
  model marked `●` (from `profile.yaml` `model_ref`), per-1k prices shown
  when the entry carries them. Pure read; cancelling (not picking) leaves no
  state anywhere.
- `/model <n|name>` — switch by list number or registry alias
  (case-insensitive alias match, same convention as agent names).
- Success: `model → <ref> (effective next turn; saved to profile)`.
- Old runtime without the method (JSON-RPC method-not-found): the TUI writes
  `model_ref` to `profile.yaml` itself and prints a restart hint — graceful
  degradation to the write-and-restart behaviour.
- Agent not running: same profile write + restart hint (it had to restart
  anyway).
- Multiplexer panes are independent TUI processes; `/model` naturally targets
  the focused pane's agent.

## Protocol

`model/set` params `{"model_ref": "<registry alias>"}` → result
`{"model_ref": "...", "effective": "next-turn"}`.

Trust surface = `message/send` (whoever can chat can already spend tokens);
no HITL gate.

## Runtime mechanics

- `SwitchableLlmClient` — wrapper implementing `LlmClient`, holding
  `std::sync::RwLock<Arc<dyn LlmClient>>`; each call clones the current inner
  client (lock never held across await). `model_name()` returns the boot-time
  name — same static-label precedent as `FallbackLlmClient::model_name`.
- Boot (single-model path in `supervisor_runner`) wraps the built client in
  `SwitchableLlmClient` and hands the handle + a client-builder closure
  (registry ref → client, the same `build_client_from_entry` path) to the
  dispatcher, which registers `ModelSetHandler`.
- Switch order is strict: **validate ref → build new client → persist
  profile.yaml → swap**. Any failure aborts with the old state fully intact —
  disk and process can never disagree (the exact failure mode the
  profile-staleness gotcha documents).
- In-flight turns hold their cloned `Arc` and finish on the old model;
  the swap is visible from the next turn.
- Persistence edits `profile.yaml` as a YAML value tree (only the
  `model_ref` key changes; the legacy `model:` block and unknown fields are
  preserved byte-for-byte elsewhere) and writes temp+rename.

## v1 limits (explicit)

- **Fallback-chain / routing agents refuse the hot switch** with a clear
  error (edit profile + restart instead). Rebuilding the routed
  `FallbackLlmClient` stack live means extracting the whole boot construction
  block; that is v2. The handler is only registered on the single-model path,
  so chain/routing (and echo/misconfigured) agents surface method-not-found
  and the TUI degrades to profile-write + restart hint.
- The stale legacy `model:` block is left untouched; `mur model doctor` may
  flag the disagreement until the block is refreshed by its own tooling.
- The in-memory `Arc<Profile>` keeps the boot `model_ref` (used only for
  boot resolution and card display); known, accepted staleness.

## Testing

- Runtime: `SwitchableLlmClient` delegates and a swap is visible to the next
  request; `model/set` rejects an unknown ref (state unchanged); a builder
  failure aborts without swapping (injected stub builder seam); persisted
  profile parses back with the new `model_ref` and preserved fields.
- TUI: `parse_slash` for `/model`, `/model 2`, `/model claude_opus`; list
  formatting is a pure function with a unit test; dial-failure path falls
  back to profile write + hint.

## Non-goals

- Provider connect / key entry in the TUI (CLI `mur model connect`, separate
  design).
- Editing fallback chains from the TUI.
- Auto-restart of any kind.
- `/login` naming — the command is `/model`, matching Pi and Claude Code.

## Related

- Discovery catalog-first fix (PR #950): known cloud vendors list models from
  the models.dev catalog; the `openai` slug stays live because it is a wire
  protocol, not a vendor.
