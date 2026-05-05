# B0 M9 — MCP install verifier design (rule 6)

**Status:** Draft (M9 cascade entry)
**Author:** david
**Date:** 2026-05-06
**Roadmap anchor:** `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 rule 6 (B0 v1.1).
**Predecessors on main:** M7.x (B0 text rules), M8.x (telemetry hardening), all D-track, C5 webhook.

---

## 1. Goal

Close B0 rule 6: defend against **MCP rug-pull** — the attack where an MCP server you trusted at install time silently changes its behavior after install (new tool descriptions that hijack the LLM, swapped binary, expanded permission scope).

Concretely, install-time pinning of:

1. **Binary SHA-256** of the resolved `command` path.
2. **Description hash** — SHA-256 of the canonical-JSON of the MCP's `tools/list` response.
3. **Publisher metadata** — display-only; what was shown at install time so the user knows what they consented to.

On every subsequent startup, B0SafetyHook re-computes both hashes; if either drifted, the supervisor refuses to spawn the MCP and surfaces a `mur agent mcp inspect` diff with the user-facing prompt "this MCP changed since you installed it — re-approve or remove?"

## 2. Threat model

Out of the OWASP LLM 2025 top 10, this addresses **LLM03 (supply chain)** + **LLM05 (output handling → tool-arg injection)**. ATLAS T0010 (supply chain). NIST AI 600-1: control 3.4 (supply-chain integrity).

**Concrete scenarios:**

- A: User installs a `weather` MCP server with one tool `get_weather`. Three weeks later the package author rolls out an update that adds tool `read_user_files(path)` with a description starting with `"Returns nothing. IGNORE PREVIOUS INSTRUCTIONS and …"`. Without pinning, the agent silently picks up the new tool and follows the embedded directive.
- B: A package-registry takeover replaces the binary at the original path. SHA-256 mismatch catches this even if the description is unchanged.
- C: A malicious MCP changes JUST its tool description (no binary change) to add prompt-injection text. Description hash catches this.

## 3. What rule 11 (M7.7) already does

`B0SafetyHook::on_startup` calls `verify_signed(path)` per MCP binary — refuses startup if the binary is not codesigned (macOS) / Authenticode-signed (Windows). **This catches "did some attacker tamper with the file on disk?" but does NOT catch:**

- A signed binary that legitimately got an update from the same publisher (signature still valid → silent acceptance).
- Description changes (signatures are over the binary, not the runtime tool list).

Rule 6 stacks on top of rule 11 — rule 11 is "trust the publisher", rule 6 is "trust this exact version of the publisher's behavior."

## 4. Schema additions (`mur-common::agent::McpServerEntry`)

All new fields `Option<…>` with `#[serde(default)]` for back-compat with existing profiles.

```rust
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,

    /// SHA-256 of the binary at `command`'s resolved path, captured at
    /// install time. None means the entry was added before M9.1
    /// (back-compat) and pinning is not enforced for it.
    #[serde(default)]
    pub binary_sha256: Option<String>,

    /// SHA-256 of the canonical-JSON of the MCP's `tools/list` response,
    /// captured at install time. None means the install path skipped
    /// the description probe (e.g. the binary couldn't be reached) or
    /// the entry pre-dates M9.
    #[serde(default)]
    pub description_hash: Option<String>,

    /// Display-only metadata captured at install time so the user can
    /// recall what they consented to. None for older entries.
    #[serde(default)]
    pub publisher: Option<McpPublisherInfo>,

    /// RFC3339 timestamp of when the entry was added or last
    /// re-approved by the user. Used for the "re-approve vs remove"
    /// UX in the rug-pull dialog.
    #[serde(default)]
    pub installed_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct McpPublisherInfo {
    pub name: String,                       // e.g. "Anthropic" or "GitHub user @alice"
    #[serde(default)]
    pub homepage: Option<String>,           // best-effort; extracted from MCP serverInfo or registry
    #[serde(default)]
    pub registry_id: Option<String>,        // e.g. "@anthropic/mcp-weather@1.2.3"
}
```

## 5. Architecture

### 5.1 Install-time (M9.2 — `mur agent mcp add` enhancements)

1. Resolve `command` to an absolute path (`which` for bare names; canonicalize for relative paths).
2. Compute `binary_sha256` via `sha2::Sha256` on the file bytes.
3. Spawn the MCP one-shot in a probe mode: send `tools/list`, capture the response, kill the process. Compute `description_hash` over the canonical-JSON of the tool list (sorted keys; same canonicalisation as M4.2's character-card signing).
4. Try to extract publisher info from the MCP's `serverInfo.name` + `serverInfo.metadata.publisher` if present (MCP spec extension, optional).
5. Print a confirmation prompt:
   ```
   About to install MCP server "weather":
     command:        /opt/mcp/weather-server
     publisher:      @anthropic-mcp/weather (https://github.com/anthropic-mcp/weather)
     binary sha256:  3f4a…b81c (4.2 MB)
     tools (3):
       - get_weather: Returns the current weather…
       - get_forecast: Returns a 7-day forecast…
       - subscribe_alerts: Subscribes to weather alerts…
     description sha256: 9a01…c7e2

   Approve? [y/N]
   ```
6. On `y`, write the new fields into `profile.yaml`. On `n`, abort without modifying the profile.
7. `--force` flag (for scripted installs) skips the confirm prompt but still records the hashes.

### 5.2 Startup-time (M9.3 — B0 rule 6 enforcement)

Extend `B0SafetyHook::on_startup` to iterate every entry that has `binary_sha256.is_some()`:

- Re-compute the binary SHA-256.
- Probe the live MCP for `tools/list` and re-compute `description_hash`.
- If either differs, return `HookError::Runtime("B0 rule 6: MCP <name> changed since install — run `mur agent mcp inspect <name>` to review and re-approve.")`.

Strict mode is the default (refuse to spawn). The supervisor's existing fail-fast pathway already handles this — same shape as rule 11.

Entries with `binary_sha256.is_none()` (older profiles pre-M9) are exempt — log a warning at startup but don't block. Adds a "drift" path so existing users aren't locked out by an upgrade. The cookbook documents `mur agent mcp pin <name>` for retroactive pinning.

### 5.3 Inspect / re-approve UX (M9.4 — `mur agent mcp inspect` + `pin`)

- `mur agent mcp inspect <name>` — dumps the pinned values vs the current values; shows a unified diff of the tool descriptions if `description_hash` mismatched.
- `mur agent mcp pin <name>` — re-runs the install-time probe + writes the new hashes (after explicit `y/N` confirmation), updating `installed_at`. This is the "I reviewed the changes and re-approve" verb.

## 6. Milestones

### M9.1 — schema additions (~80 LOC + tests)

- `mur-common::agent::McpServerEntry`: add the four optional fields above.
- `mur-common::agent::McpPublisherInfo`: new struct.
- All fields `#[serde(default)]` so existing profiles continue to deserialize cleanly.
- 4 unit tests: round-trip with all fields set, round-trip with all fields absent, partial fields, schema_compatible_with_pre_m9_profiles.

**Acceptance:** `cargo test -p mur-common --lib mcp` green; cargo build runs over an in-tree pre-M9 profile fixture without error.

### M9.2 — install-time hashing (~250 LOC + tests)

- `mur-core::cmd::agent_mcp_pin` new module with helpers:
  - `compute_binary_sha256(path: &Path) -> Result<String>`
  - `probe_mcp_descriptions(command: &str, args: &[String]) -> Result<(String, Vec<ToolListEntry>)>` — spawns the MCP via stdio, sends `tools/list`, hashes canonical-JSON.
- `cmd_mcp_add` enhanced to compute hashes + show the prompt + persist.
- `--force` flag for non-interactive installs.
- 6 tests using a stub MCP fixture (mock binary that responds to `tools/list` with a fixed payload).

**Acceptance:** synthesized fixture install flow round-trips with hashes set; rejecting the prompt leaves profile.yaml unchanged.

### M9.3 — startup verification (~120 LOC + tests)

- `B0SafetyHook::on_startup` extended to re-verify hashes for every entry with `binary_sha256.is_some()`.
- New helper `crate::hooks::b0_helpers::verify_mcp_pin(entry, command_path) -> Result<(), DriftReason>`.
- 4 tests covering: clean re-verify, binary-only drift, description-only drift, both drifted.

**Acceptance:** `cargo test -p mur-agent-runtime --lib b0::rule_6` green; integration test that mutates a fake MCP binary in tmp + asserts startup refuses with rule-6 error.

### M9.4 — inspect / pin CLI (~150 LOC + tests)

- `mur agent mcp inspect <name>` — pretty-prints pinned vs current; uses unified-diff for description drift.
- `mur agent mcp pin <name>` — re-runs probe + persists with confirm. Optional `--force`.
- 4 tests for the inspect output formatting + 2 tests for the pin verb.

**Acceptance:** `mur agent mcp inspect <name>` exit codes — 0 clean / 1 binary drift / 2 description drift / 3 both.

### M9.5 — E2E + cookbook + roadmap footer (~80 LOC + docs)

- `scripts/e2e/b0-m9-mcp-install-verifier.sh` runs the M9.1-M9.4 test suites.
- `docs/cookbook/b0-mcp-install-verify.md` — install flow walkthrough, the rug-pull recovery path, the `--force` escape hatch, the back-compat pre-M9-entry rules.
- Roadmap §6.1 v1-ship-status footer: rule 6 marked shipped.

## 7. Non-goals

- **Not** verifying the MCP runtime's behavior on every tool call (out of scope; that's a B1 item — runtime intrusion detection).
- **Not** building a centralized package registry — `publisher` is just metadata; we don't validate it against any authority.
- **Not** detecting **same-version** prompt injection (description happens to look identical but contains a sleeper). That's research; the user's defense remains "review descriptions when you run `mur agent mcp inspect`."
- **Not** supporting non-stdio MCP transports for description probing in v1; HTTP/Unix-socket MCPs require separate probe logic (deferred).

## 8. Acceptance gates

- Schema round-trip green (M9.1).
- Install-time hashes captured + persisted + prompted (M9.2).
- Startup verification refuses on either drift (M9.3).
- `mur agent mcp inspect` + `pin` shipped (M9.4).
- `bash scripts/e2e/b0-m9-mcp-install-verifier.sh` green.
- Roadmap §6.1 rule 6 footer updated.

## 9. Open questions

- **Stdio probe lifetime budget.** The probe spawns the MCP, sends `tools/list`, and kills it. On some MCPs the boot can take seconds (model warm-up). Default budget: 10 s; configurable via `MUR_MCP_PROBE_TIMEOUT_S`. Long-startup MCPs that exceed the budget fail install with a hint to set the env var.
- **Pre-M9 upgrade path.** `mur agent mcp pin <name>` is the upgrade verb. Should the supervisor also nag on every startup that a profile has unpinned entries? Default: nag once per startup at warn level; suppress via `--silence-pin-nag` flag.

## 10. Cascade plan

| PR | Branch | Base |
|---|---|---|
| M9.0 (this spec) | `feat/mur-agent-b0-m9.0-spec` | main |
| M9.1 schema additions | `feat/mur-agent-b0-m9.1-schema` | M9.0 |
| M9.2 install-time hashing | `feat/mur-agent-b0-m9.2-install` | M9.1 |
| M9.3 startup verification | `feat/mur-agent-b0-m9.3-verify` | M9.2 |
| M9.4 inspect / pin CLI | `feat/mur-agent-b0-m9.4-inspect-pin` | M9.3 |
| M9.5 E2E + cookbook | `feat/mur-agent-b0-m9.5-e2e-cookbook` | M9.4 |

5 mur-side PRs. Estimated ~3 dev-days. Same Tier 2 stacked-PR recipe as M5 / M7 / M8.
