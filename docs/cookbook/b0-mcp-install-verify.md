# B0 MCP install verifier (rule 6)

Closes the supply-chain attack vector for MCP servers: even if the
publisher's signature is still valid (rule 11), this catches the
"signed-but-evolved" rug-pull where a benign MCP rolls out an update
that adds a tool whose description hijacks the LLM.

## What gets pinned

When you run `mur agent mcp add`, the install path captures three
artefacts and writes them into `~/.mur/agents/<name>/profile.yaml`:

1. **Binary SHA-256** of the resolved `command` path. Detects "the
   bytes on disk changed" — even between two signed versions from the
   same publisher.
2. **Description hash** (M9.3.5) — SHA-256 over the canonical-JSON of
   the MCP's `tools/list` response. Captured at install time by the
   install probe (below) and on every `mur agent mcp pin`; verified on
   demand via `mur agent mcp inspect --probe`. Catches the pure-prompt-injection
   update where the binary doesn't change but a
   tool's description gains a "IGNORE PREVIOUS INSTRUCTIONS …"
   prefix.
3. **Publisher metadata** (display-only) — name + optional homepage +
   optional registry coordinate. Stored so the user can recall who
   they trusted at install time; never validated against any external
   authority.

## Install flow

```bash
mur agent mcp add my-agent weather \
    --command /opt/mcp/weather-server \
    --publisher-name "@anthropic-mcp/weather" \
    --publisher-homepage https://github.com/anthropic-mcp/weather \
    --publisher-registry-id "@anthropic-mcp/weather@1.2.3"
```

Output:

```
About to install MCP server "weather":
  command:        /opt/mcp/weather-server
  publisher:      @anthropic-mcp/weather
                  https://github.com/anthropic-mcp/weather
                  @anthropic-mcp/weather@1.2.3
  binary sha256:  3f4abca8b0e6e2c1…  (full: 3f4abca8…b81c)
  description hash: <pinned by the install probe, below>

Approve? [y/N]
```

`y` runs the probe and writes the entry. Anything else aborts without
modifying the profile.

For scripted installs (CI / cookbook examples), pass `--force` to
skip the prompt — the hashes are still captured and the probe still
runs.

## The install probe

Approval is not the last gate. The install spawns the staged entry,
runs `initialize` + `tools/list` and tears it down — under the agent's
**real** sandbox policy, built from the profile as it stands after the
entry and its `--state-path` grants are added:

```
  probing:        spawning 'weather' under this agent's sandbox…
  probe:          ok — 4 tools listed, description hash pinned
```

A permissive probe would be worse than none: it reports fine for
exactly the servers that die on startup under the policy the
supervisor actually seals (#1161).

**A failed probe writes nothing.** A server that cannot start is not
installed, and an entry left behind is how a command that looked like
it succeeded produces an agent that will not boot at the next restart.
The failure names what to fix, chosen from the failure shape — a
timeout points at `MUR_MCP_PROBE_TIMEOUT_S`, a sandbox denial points at
`--state-path`, anything else points at the command itself — and every
one of them names `--no-probe`, which installs unchecked for servers
that legitimately cannot be probed (side-effecting init, hardware, a
login flow).

`--force` does not imply `--no-probe`: one skips the approval prompt,
the other skips the check that the thing being approved works.

## Startup enforcement

On every supervisor start, `B0SafetyHook::on_startup` re-hashes each
pinned binary and compares with the recorded value. Three outcomes:

| Outcome | Behaviour |
|---|---|
| **Match** | Logged at debug; supervisor proceeds. |
| **Drift** | Hard fail. Supervisor refuses to start with: `B0 rule 6: MCP \`weather\` changed since install — run \`mur agent mcp inspect weather\` to review …` |
| **Missing binary / unreadable** | Soft fail. Logged at warn; supervisor proceeds. (Rule 11 above already caught real tampering; this is more likely "user uninstalled the MCP without removing the profile entry".) |

Description-hash verification fires lazily at first MCP call (M9.3.5)
rather than at startup so boot time stays fast.

## Recovering from drift

When you see a rule-6 refusal at startup, the recovery flow is:

```bash
# 1. Diff pinned vs current state.
mur agent mcp inspect my-agent --server weather

# Output:
#   MCP server: weather
#     command:        /opt/mcp/weather-server
#     pinned sha256:  3f4abca8b0e6e2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b81c
#     current sha256: 9a01b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9c7e2
#     status:         BINARY DRIFT
#     hint:           `mur agent mcp pin my-agent weather` to re-approve, …

# 2. Either re-approve (you reviewed the upstream change and trust it):
mur agent mcp pin my-agent weather

# …or remove (you don't):
mur agent mcp remove my-agent weather
```

`mur agent mcp pin` shows the same prompt as install with the new
hash visible alongside the old, then persists with `installed_at`
updated to "now". Add `--force` for scripted re-approvals.

## Inspect exit codes

`mur agent mcp inspect` returns a stable exit code for scripted
branching:

| Code | Meaning |
|---|---|
| 0 | Clean — pin matches current state |
| 1 | Binary drift (the binary on disk has changed) |
| 2 | Description drift — `--probe` only; live `tools/list` differs from pinned hash |
| 3 | Both drifted — `--probe` only; binary AND descriptions changed |
| 4 | Missing pin — pre-M9 entry, run `mur agent mcp pin` to start enforcing |
| 5 | Binary missing — pinned binary not on disk anymore |

Codes 2 + 3 only fire when you pass `--probe` (default `inspect` is
binary-only and fast). Without `--server`, inspect reports the
**worst** status across all configured MCPs so a script can
`if mur agent mcp inspect my-agent; then …` and bail on any drift.

## Periodic drift checks (`--probe`)

Default `mur agent mcp inspect` only re-hashes the binary on disk —
fast, no MCP spawn. To catch description rug-pulls (the binary
didn't change but a tool description did), pass `--probe`:

```bash
mur agent mcp inspect my-agent --server weather --probe
```

This spawns the MCP, sends `initialize` + `tools/list`, computes the
canonical-JSON SHA-256, and compares against the pinned
`description_hash`. Output adds:

```
  pinned descr:   9a01b2c3…c7e2
  current descr:  3f4abca8…b81c
  description status: DESCRIPTION DRIFT
  hint:           the MCP's tools/list changed since install; review
                  the new tool descriptions then `mur agent mcp pin
                  my-agent weather` to re-approve, …
```

The probe budget defaults to 10 s; raise it via env var for
slow-startup MCPs (model warm-up, network discovery during
initialization):

```bash
MUR_MCP_PROBE_TIMEOUT_S=30 mur agent mcp inspect my-agent --probe
```

Probe failure (timeout, spawn error) is non-fatal — inspect prints
`<probe failed: …>` and falls back to the binary-only status. Pass
`--no-probe` (on `pin`, not `inspect`) to skip the spawn entirely.

A reasonable cadence is: run `--probe` after every MCP-publisher
update + any time `mur agent doctor` flags an unpinned entry. CI
pipelines can wire `mur agent mcp inspect <agent> --probe` into the
post-deploy sanity check; non-zero exit → drift → manual review.

## Pre-M9 profile migration

Profile entries written before M9.1 don't have `binary_sha256` and
are exempt from rule 6 enforcement (warned at startup, then skipped).
Migrate them with:

```bash
mur agent mcp pin <agent> <server_id>
```

This computes the binary hash of the current on-disk binary and
records it as the pinned value. Subsequent updates to the binary
will then trigger drift detection.

## What rule 6 does NOT defend against

- **Same-version sleeper attacks** — an MCP that ships malicious
  behaviour from day one. The hash + description match what was at
  install, so rule 6 is silent. Defence: review tool descriptions
  yourself (or have someone you trust review them) before approving.
- **Live runtime behaviour changes** — an MCP whose tools take a
  sleeper code path on a specific input. Rule 6 only checks the
  declared interface. B1 (runtime intrusion detection) is the long-
  term defence.
- **Centralised registry attestation** — `publisher` metadata is
  display-only. We don't validate against any registry. Use rule 11
  (codesign) for cryptographic publisher verification.

## See also

- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 rule 6
- `docs/superpowers/specs/2026-05-06-b0-m9-mcp-install-verifier-design.md` (M9 design)
- `mur-core/src/cmd/agent_mcp_pin.rs` (helpers + inspect/pin verbs)
- `mur-agent-runtime/src/hooks/b0_helpers.rs::verify_mcp_binary_hash` (startup verifier)
- `mur-agent-runtime/src/hooks/b0.rs::on_startup` (rule-6 enforcement)
- `scripts/e2e/b0-m9-mcp-install-verifier.sh` (acceptance gate)
