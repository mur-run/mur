# Channel Consult (`Ask` / `AskReply`) — Implementation Plan

Design: `docs/superpowers/specs/2026-09-26-channel-consult-design.md`
Status: ready for implementation. Nothing here has been built or run yet.

Section numbers (§D3, P1, R4…) refer to the design.

## Shipping shape

Three rounds, in this order. Each prerequisite is its own PR.

1. **Prerequisites (A)** — merged before anything else.
2. **Release N — readers only (B, C, D).** New kinds parse, render and are
   filtered; the `ask` tool exists but is gated off. Nothing writes `Ask` yet.
3. **Release N+1 — writers (E).** The `ask` tool is enabled.

Readers ship one release before writers because an older binary that cannot
parse the last line of a log reuses its `seq` and silently loses its own next
event (§D7).

## Work items

### A. Prerequisites (each its own PR)

- [ ] **A1 · P0 — move verification.** Move `actor_pubkey` + `verify_event`
  from `mur-core/src/channel_verify.rs` to `mur-channel`; re-export from
  `mur-core`; no behaviour change. Correct the stale Linux comment
  (`mur-core/src/channel_verify.rs:29-30`) in the same PR.
- [ ] **A2 · P1 — validate actor ids.** In the moved `actor_pubkey`, return
  `None` for an `Agent{id}` that fails `validate_agent_name`. Test with `../`-
  and `/`-bearing ids.
- [ ] **A3 · P2 — Linux `profile.yaml`.** Reproduce on Linux whether `bash`
  can rewrite the agent's own `profile.yaml`. If yes, fix it (#712 on Linux)
  before E4's platform guard is lifted there.
- [ ] **A4 · seq from raw lines.** Derive the next `seq` from a minimal
  `{seq}` parse of every non-empty line (`mur-channel/src/store.rs:214`). Test
  with a trailing line of unknown kind (R7).
- [ ] **A5 · HITL actor finding.** File the design's "Related finding" as its
  own issue. Not fixed here.

### B. Event model (release N, `mur-common` / `mur-channel` / `mur-core`)

- [ ] **B1.** Add `Ask`, `AskReply` to `EventKind`
  (`mur-common/src/channel.rs`); payload structs carrying `to`, `question`,
  `expires_at_ms`, `nonce`.
- [ ] **B2.** Update every exhaustive `match` on `EventKind`
  (`mur-core/src/cmd/agent/cli/follow.rs`, `mur-channel/src/index.rs`, and
  whatever else the compiler finds).
- [ ] **B3.** Kind-filter the actor-only readers (§D8):
  `mur-core/src/cmd/fleet/loop_run.rs:1064-1066`,
  `mur-core/src/cmd/fleet/run.rs:601-610`,
  `mur-core/src/cmd/deep_research/ask.rs:282-284`.
- [ ] **B4.** MUR Hub renders `Ask` / `AskReply`.

### C. Runtime plumbing (release N, `mur-agent-runtime`)

- [ ] **C1.** `TaskSpec` gains a tool allowlist
  (`mur-agent-runtime/src/task_runner/mod.rs:35`); the loop both **offers** and
  **dispatches** from it (`task_runner/agentic_loop.rs:93-99`, `:466`). Offering alone
  is not enough — a model can emit a call for a tool it was not shown.
- [ ] **C2.** `decide_read` (`mur-agent-runtime/src/tools/fs_policy/mod.rs:418`)
  gains an optional second `FilesystemEntitlement`; a path passes only when
  both allow it.
- [ ] **C3.** The `Ask` expiry window comes from config, not a constant.

### D. `ask` tool, asker side (built in release N, gated off)

- [ ] **D1.** Tool appends a self-signed `Ask` through the `append_self_reply`
  path (`mur-agent-runtime/src/protocol/methods/channel_delegate.rs:57`) with a
  fresh random nonce.
- [ ] **D2.** Dials the answerer's `channel/consult(channel_id, nonce)` and
  returns the `AskReply` text.
- [ ] **D3.** Gate: off in release N.

### E. `channel/consult`, answerer side

- [ ] **E1.** Register the method next to `channel/delegate`
  (`mur-agent-runtime/src/supervisor/dispatch.rs:140`).
- [ ] **E2.** Run the checks in §D3 order; every failure refuses with a named
  reason, nothing falls back to a default.
- [ ] **E3.** Single-use ledger of `(asker, nonce)` in agent-owned state.
- [ ] **E4.** Platform guard: refuse on Linux until A3 lands, and on Windows
  until someone verifies it there (R6).
- [ ] **E5.** Run the turn with allowlist `[read_file]` (C1) and the
  intersected read policy (C2).
- [ ] **E6.** Append a self-signed `AskReply` referencing the `Ask` nonce.
- [ ] **E7.** Release N+1: lift D3's gate.

### F. Tests

One test per design scenario, named after it:

| Requirement | Scenario | Lands with |
|---|---|---|
| R1 | Asker's runtime writes the Ask | D1 |
| R1 | The kind cannot be forged from a Message | B1 |
| R2 | Unsigned Ask with enforcement off | E2 |
| R2 | Traversal in the actor id | A2, E2 |
| R2 | Two Asks share a nonce | E2 |
| R3 | Ask addressed to someone else | E2 |
| R3 | Second consult for the same Ask | E3 |
| R4 | Asker lacks a path the answerer has | C2 |
| R4 | Asker has a path the answerer lacks | C2 |
| R5 | Model calls bash during consult | C1, E5 |
| R6 | Linux before P2 | E4 |
| R7 | Log ends in a kind this binary cannot parse | A4 |
| R8 | Fleet stuck detection | B3 |

Plus: serde round-trip for both new kinds; an old-shaped log (no `Ask`) still
folds unchanged.

### G. Docs

- [ ] **G1.** `docs/architecture/runtime-overview.md`: event kinds, the consult
  trust model, the memory caveat (§D6), platform limits.
- [ ] **G2.** README + docs site + product page, via the `update-docs` skill.
- [ ] **G3.** Release notes for N: mixed-version guidance (§D7) — anyone who
  skips release N is still exposed to the `seq` reuse.

## Order of execution

1. A1 → A2 (A2 edits the code A1 moves). A4 and A5 in parallel with them.
   A3 whenever a Linux box is available; it only blocks E4's Linux path.
2. B1 → B2 → B3/B4.
3. C1, C2, C3 (independent of each other; after B1).
4. D1–D3 and E1–E6 (after A1, A2, C1, C2).
5. G1–G3, then cut release N.
6. E7 in release N+1.

## Verification

Per CLAUDE.md, with the CI invocation:

```bash
cargo clippy --all --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
cargo test -p mur-channel -p mur-common -p mur-agent-runtime
```

Manual, on macOS, after E7:

- Two agents with different `fs_read` grants in one channel; `pm` asks
  `tech-writer` about a file only `tech-writer` can read → refused, and the
  refusal names the scope.
- Same pair, a file both can read → answered, `AskReply` in the channel signed
  by `tech-writer`.
- Rewrite the `Ask` line's `to` by hand → consult refuses on signature.
- Run a release-N binary against a log whose last line is an `Ask` written by
  N+1 → its next event gets a fresh `seq` and appears in `mur agent cli`
  follow. (An N−1 binary still loses it; that is the exposure G3 documents.)
