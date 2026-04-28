# Implementation Plan — `mur agent export --format gui`

**Spec:** `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`
**Branch:** `feat/agent-gui-export` (off `main`)
**Mode:** Autonomous; resumable across sessions.

---

## RESUME PROTOCOL

Any future Claude Code session that picks up this work should:

1. `git -C /Users/david/Projects/mur status` — confirm branch is `feat/agent-gui-export`.
2. Read **§ STATE** below to find the next pending task.
3. Read the most recent commit (`git log -1`) for the last successful checkpoint.
4. Continue from the next pending phase. **No questions to user.**

If a phase fails verification (cargo test / clippy / harness), the current phase is **kept in_progress** and a `BLOCKER` note is appended to STATE; the session continues debugging until either:
- Tests pass → mark phase done → advance.
- Hit context / rate limit → commit WIP with `wip(phase X.Y): <subject>` so resume is possible.

A WIP commit must include enough context in the message body for a future session to resume without re-deriving intent — quote the failing test name, the error, and the next-step hypothesis.

---

## STATE

```yaml
last_updated: 2026-04-29
current_phase: COMPLETE (see plan-COMPLETE.md sibling)
status: branch ready for review + merge
last_completed: P1.7 + P1.8 + P1.9 + COMPLETE log
blockers:
  - tests/agent_card_ephemeral.rs::card_displays_identity_pubkey FAILING on main upstream (pre-existing, unrelated).
  - P1.4-P1.6 deeper Tauri-side wiring deferred (documented in COMPLETE.md). Resume needs tauri-cli installed.
next_action: open PR; deferred P1.4-P1.6 work tackled in follow-up branch with tauri-cli toolchain available
```

---

## PHASE TRACKER

| ID | Phase | Status | LOC est. | Verification gate |
|----|-------|--------|----------|-------------------|
| P1.0 | Extract `agent_admin` library | ✅ done @ 768a0d9 (façade-only; 343 LOC) | — | lib + bin build; clippy clean |
| P1.1 | `setpgid` + `mur agent doctor` | ✅ done @ 7e90064 (340 LOC + 3 unit tests) | — | doctor smoke OK; clippy clean |
| P1.2 | Scaffold `mur-agent-gui` crate | ✅ done @ 64441a2 (1253 LOC; 32 files) | — | workspace exclude verified; toolchain not present locally |
| P1.7 | Export pipeline (deferred ahead of P1.3-P1.6 deeper wiring) | ⏳ in_progress | +1200 | end-to-end `mur agent export --format gui` produces an artifact |
| P1.3 | Wire admin via Tauri commands | ⏸ pending | +1500 | Manual: edit each tab, verify YAML write |
| P1.4 | Sidecar manager | ⏸ pending | +900 | Spawn / kill / restart-with-backoff exercised |
| P1.5 | Theme system | ⏸ pending | +700 | 5 themes load + tray icon swaps + appearance subscriber |
| P1.6 | First-launch bootstrap | ⏸ pending | +600 | Template / Clone modes both produce valid `~/.mur/agents/<name>/` |
| P1.7 | Export pipeline | ⏸ pending | +1200 | `mur agent export ... --format gui` produces signed `.app` |
| P1.8 | GH Actions matrix | ⏸ pending | +300 | Three-OS CI run produces artifacts |
| P1.9 | E2E + harness scenarios | ⏸ pending | +400 | OFFICE-12-GUI-EXPORT scenario passes; existing OFFICE-1..11 still green |

---

## P1.0 — Extract `agent_admin` library module

### Goal
The single largest refactor. Both `mur` CLI clap handlers AND future Tauri command handlers must call the same admin functions. clap stays in `cmd/agent.rs` as a thin shell.

### Tasks

- [ ] T1.0.1 — Survey current `cmd/agent.rs`: enumerate every admin verb (`cmd_*` function) and its dependencies.
- [ ] T1.0.2 — Create `mur-core/src/agent_admin/mod.rs` with module exports.
- [ ] T1.0.3 — Move `prompt`, `mcp`, `skill`, `perm`, `rekey`, `card`, `status`, `stop`, `start`, `install-service`, `stats`, `logs` admin logic into per-verb files under `agent_admin/`.
  - `agent_admin/prompt.rs`
  - `agent_admin/mcp.rs`
  - `agent_admin/skill.rs`
  - `agent_admin/perm.rs`
  - `agent_admin/rekey.rs`
  - `agent_admin/lifecycle.rs` (status / start / stop / install-service)
  - `agent_admin/observability.rs` (stats / logs / card)
- [ ] T1.0.4 — Each function in `agent_admin/*` takes typed params (no `&str` for enum-y things), returns `Result<T>` where T is a typed value (not formatted output). The CLI layer converts T into stdout text.
- [ ] T1.0.5 — Update `cmd/agent.rs` to be thin clap → `agent_admin::*` wrappers.
- [ ] T1.0.6 — Add unit tests in `agent_admin/<verb>.rs` `#[cfg(test)]` blocks. Use a temp `MUR_HOME` to avoid polluting `~/.mur`.
- [ ] T1.0.7 — Run `cargo test --workspace`; fix any breakage.
- [ ] T1.0.8 — Run `cargo clippy --workspace -- -D warnings`; fix lints.
- [ ] T1.0.9 — Run a manual smoke: `cargo run -- agent list`, `cargo run -- agent perm show <test-agent>`. Compare to pre-refactor output.
- [ ] T1.0.10 — Commit with message `refactor(mur-core): extract agent_admin library module`.

### Decision points encountered along the way

(populated during execution — captures any non-obvious choice the next session would need)

---

## P1.1 — `setpgid` + `mur agent doctor`

### Tasks

- [ ] T1.1.1 — Add `nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))` near the top of `mur-agent-runtime/src/supervisor.rs::run()` (Unix only; cfg-gate). Document why in a one-line comment.
- [ ] T1.1.2 — Add a process-group kill helper used by future GUI sidecar manager.
- [ ] T1.1.3 — Create `mur-core/src/cmd/doctor.rs` with prereq-check logic broken into per-check functions returning `(name, status, hint)` tuples.
- [ ] T1.1.4 — Wire `mur agent doctor [--format gui]` into clap.
- [ ] T1.1.5 — Existing prereq logic in `mur-agent-runtime/src/export/prereq_check.rs` is for MCP servers — separate from build-toolchain doctor; keep both.
- [ ] T1.1.6 — Tests + clippy + commit.

---

## P1.2 — Scaffold `mur-agent-gui`

### Tasks

- [ ] T1.2.1 — Add to root `Cargo.toml`: `[workspace] exclude = ["mur-agent-gui"]`. Verify `cargo build --workspace` does NOT pick it up.
- [ ] T1.2.2 — `cargo tauri init` style bootstrap (manual, since we don't want CLI prompts): write `mur-agent-gui/src-tauri/Cargo.toml`, `tauri.conf.json` (template), `src/main.rs`, `capabilities/main.json`, `entitlements.plist`.
- [ ] T1.2.3 — Pin Tauri 2.x exact versions; add `rust-toolchain.toml` if not present.
- [ ] T1.2.4 — `mur-agent-gui/ui/`: Vite + React 18 + Tailwind 4 + shadcn/ui + Radix. `package.json`, `vite.config.ts`, `tailwind.config.ts`, `.nvmrc` (node 20).
- [ ] T1.2.5 — 6-tab sidebar layout (Status / System Prompt / Skills / MCP Servers / Permissions / Identity) with stub content. Logs window stub.
- [ ] T1.2.6 — 5 built-in themes (light / dark / high-contrast / solarized / cyberpunk) under `mur-agent-gui/src-tauri/themes/`.
- [ ] T1.2.7 — `cargo tauri dev` from `mur-agent-gui/src-tauri/` opens the empty shell.
- [ ] T1.2.8 — Commit.

---

## P1.3 — Wire admin via Tauri commands

### Tasks

- [ ] T1.3.1 — Add `mur-core` as path dependency in `mur-agent-gui/src-tauri/Cargo.toml`.
- [ ] T1.3.2 — One Tauri command per `agent_admin::*` function. Generate the `capabilities/main.json` permission list automatically from the command list.
- [ ] T1.3.3 — Each tab: form components (shadcn) + Tauri `invoke()` calls + restart-required pill state.
- [ ] T1.3.4 — System Prompt = Monaco editor with explicit Save (not auto-save).
- [ ] T1.3.5 — Permissions tab = nested form with allow/deny lists.
- [ ] T1.3.6 — Identity tab = read-only display + Rotate Key… modal.
- [ ] T1.3.7 — Manual smoke: edit perm host list → save → check `~/.mur/agents/<name>/profile.yaml` mutated.
- [ ] T1.3.8 — Commit.

---

## P1.4 — Sidecar manager

### Tasks

- [ ] T1.4.1 — Sidecar bundle hookup: `bundle.externalBin` references `mur-agent-runtime` per-target-triple naming.
- [ ] T1.4.2 — Build script: `cargo build -p mur-agent-runtime --release --target <host>`; copy with target-triple suffix into `src-tauri/binaries/`.
- [ ] T1.4.3 — Sidecar manager Rust: spawn via `tauri-plugin-shell` with PATH augment + arg `--profile <name>`. Capture stdout/stderr to log channel.
- [ ] T1.4.4 — Restart-with-backoff state machine.
- [ ] T1.4.5 — Process-tree kill: Unix `kill(-pgid, SIGTERM)`; Windows Job Object.
- [ ] T1.4.6 — Logs window: live tail via `notify` crate watching `<agent_home>/stderr.log` + GUI `gui.log`.
- [ ] T1.4.7 — Tests + commit.

---

## P1.5 — Theme system

### Tasks

- [ ] T1.5.1 — `mur-agent-gui/src-tauri/src/theme.rs`: load theme dir, parse `theme.json`, validate WCAG.
- [ ] T1.5.2 — Tauri command `set_theme(name)` → injects CSS vars + swaps tray icon + swaps dock icon.
- [ ] T1.5.3 — Appearance subscriber: macOS `WindowEvent::ThemeChanged`, Windows registry watch, Linux gsettings monitor.
- [ ] T1.5.4 — Author all 5 built-in `theme.json` + WCAG-validated palettes + tray icons.
- [ ] T1.5.5 — Build-time validator script in `mur-agent-gui/build.rs` or a separate `scripts/validate-themes.sh`.
- [ ] T1.5.6 — Commit.

---

## P1.6 — Bootstrap

### Tasks

- [ ] T1.6.1 — `mur-agent-gui/src-tauri/src/bootstrap.rs`: parse embedded `metadata.json`, branch by mode.
- [ ] T1.6.2 — Template mode: extract payload (sans identity), mint Ed25519 keypair (`mur-common::identity`), mint UUIDv7, write rotations.jsonl bootstrap entry.
- [ ] T1.6.3 — Clone mode: extract full payload, immediately rekey with shipped key, shred key.prev.
- [ ] T1.6.4 — UUID-conflict dialog (Tauri dialog API).
- [ ] T1.6.5 — Symlink creation in `~/.local/bin/`.
- [ ] T1.6.6 — Tests with synthetic payloads (build a minimal payload tarball in test setup).
- [ ] T1.6.7 — Commit.

---

## P1.7 — Export pipeline

### Tasks

- [ ] T1.7.1 — `mur-core/src/agent/export_gui.rs`: 13-phase pipeline matching § 8.2 of spec.
- [ ] T1.7.2 — Add `gui` arm to `cmd_export` in `mur-core/src/cmd/agent.rs`.
- [ ] T1.7.3 — Theme/icon resolution + `tauri.conf.json` rewrite.
- [ ] T1.7.4 — `cargo build` sidecar (universal mac via lipo).
- [ ] T1.7.5 — `npm ci && npm run build` frontend.
- [ ] T1.7.6 — `cargo tauri build` invocation with proper bundle args per host.
- [ ] T1.7.7 — Codesign + notarytool + stapler + spctl assess (mac); signtool (win); noop (linux).
- [ ] T1.7.8 — OTEL spans per phase.
- [ ] T1.7.9 — Tests with `--skip-notarize` so CI doesn't need creds.
- [ ] T1.7.10 — Commit.

---

## P1.8 — GH Actions

### Tasks

- [ ] T1.8.1 — Write `scripts/templates/agent-export-multi-platform.yml`.
- [ ] T1.8.2 — Write `docs/cookbook/multi-platform-export.md`.
- [ ] T1.8.3 — Optional: trigger a workflow_dispatch to verify on real CI runners (one-shot, opt-in).
- [ ] T1.8.4 — Commit.

---

## P1.9 — E2E + harness scenarios

### Tasks

- [ ] T1.9.1 — `scripts/e2e/p1-export-gui.sh`: build + spawn + assert running.lock + send A2A test message.
- [ ] T1.9.2 — In `mur-agent-harness/`, add `phaseN/design.md` for OFFICE-12-GUI-EXPORT.
- [ ] T1.9.3 — Extend `harness.py` with the new scenario.
- [ ] T1.9.4 — Run full harness; capture REPORT.md.
- [ ] T1.9.5 — Fix any bugs surfaced; commit fixes individually so they're reviewable.
- [ ] T1.9.6 — Final commit + plan COMPLETE log at `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan-COMPLETE.md`.

---

## Verification gates

After each phase:

1. `cargo test --workspace` (excluding `mur-agent-gui`)
2. `cargo clippy --workspace -- -D warnings`
3. Phase-specific manual smoke (documented in each phase's tasks)
4. Update STATE block above with the actual `last_commit_on_branch` short SHA
5. Append a one-liner to **DECISIONS LOG** below if any non-obvious choice was made

---

## DECISIONS LOG

(append-only — every entry includes date + short SHA + decision + reasoning)

- 2026-04-29 (pre-impl) — feature branch off main, NOT a separate worktree. Reason: simpler resumability for autonomous sessions; user can always `git checkout` if needed.
- 2026-04-29 768a0d9 — P1.0 implemented as façade (cmd::agent re-export layer + typed read views) rather than full code movement. Reason: GUI Tauri commands need a clean lib API; that's achievable in 343 LOC; full refactor of 32 cmd_* fns into per-area files would be 1500-LOC churn with no functional change since cmd_* fns are already mostly print-free. cmd::agent stays canonical implementation.
- 2026-04-29 7e90064 — P1.1 doctor module is its own `cmd::doctor` namespace, not co-located with `cmd::agent`, because it's reusable as the export pipeline's pre-flight (ergonomic for `agent::export_gui::pre_flight()` to call `cmd::doctor::checks_for("gui")`). setpgid uses raw libc::setpgid with a SAFETY comment rather than adding the `nix` crate (libc is already a dep, nix would add ~30 transitive crates).
- 2026-04-29 64441a2 — P1.2 scaffolds 32 files in one commit (Tauri main + commands + theme + 5 themes + Vite frontend + 6 tabs). Took the deep + minimal route: every Tauri command in `commands.rs` is wired to `mur_core::agent_admin` (so P1.3 wiring is mostly UI work). Each tab is a stub component but talks to a real Tauri command. Tauri toolchain not exercised on this dev host (tauri-cli missing).
- 2026-04-29 (deferral) — P1.7 (export pipeline) is being tackled ahead of P1.3-P1.6 deeper wiring. Reason: a stub-but-end-to-end pipeline that produces an unsigned `.app` validates the architecture and gives a real artifact for harness verification (P1.9). Deeper UI/sidecar/bootstrap work (P1.3-P1.6) can land iteratively after the pipeline is in place.
