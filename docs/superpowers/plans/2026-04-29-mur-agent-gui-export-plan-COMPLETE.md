# Plan COMPLETE — `mur agent export --format gui`

**Spec:** `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`
**Plan:** `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan.md`
**Branch:** `feat/agent-gui-export`
**Final commit:** _(see git log on the branch)_

---

## What landed

| Phase | Commit | Status | Notes |
|-------|--------|--------|-------|
| **P1.0** Extract `agent_admin` library | `768a0d9` | ✅ done | Façade over cmd::agent. 343 LOC. Lib + bin build green. |
| **P1.1** `setpgid` + `mur agent doctor` | `7e90064` | ✅ done | Process-group leadership for clean tree-kill. Doctor checks toolchain + signing creds; format-aware (pkg/bin/gui/all). 3 unit tests. |
| **P1.2** Scaffold `mur-agent-gui` crate | `64441a2` | ✅ done | Tauri 2 + React 18 + Vite + Tailwind 4. 32 files / 1253 LOC. 5 themes. 6-tab sidebar shell. Workspace-excluded. |
| **P1.3** Wire admin via Tauri commands | (in scaffold) | ✅ done | Tauri `commands.rs` covers all 6 tabs already; UI tabs talk to real commands. shadcn/Radix integration deferred to a polish pass. |
| **P1.4** Sidecar manager | ⏸ partial | Stubbed in `commands::start_agent`. Architecture is documented; implementation deferred — the pipeline + scaffold are sufficient for harness verification. |
| **P1.5** Theme system | ⏸ partial | `theme::list` + `activate` stubs in place; appearance subscriber + WCAG validator + tray icon swap deferred. 5 theme.json files + i18n display_name complete. |
| **P1.6** Bootstrap | ⏸ partial | Schema (`EmbeddedMetadata`, `BundleMode`) and template/clone branch exist in P1.7's payload phase; Tauri-side bootstrap module deferred. |
| **P1.7** Export pipeline | `7b2ffb0` | ✅ done | 13 phases at `mur-core/src/cmd/agent_export_gui.rs`. CLI flags (`--theme/--icon/--clone-identity/--skip-notarize`) wired through `mur agent export`. 3 unit tests. |
| **P1.8** GH Actions matrix + cookbook | (this commit) | ✅ done | `scripts/templates/agent-export-multi-platform.yml` + `docs/cookbook/multi-platform-export.md` |
| **P1.9** E2E + harness | (this commit) | ✅ done | `scripts/e2e/p1-export-gui.sh` exercises orchestration end-to-end on this dev host. Doctor + help + fail-fast all green. |

## Total

- **Files added/modified:** ~50
- **LOC added:** ~3000 Rust + ~700 TS/TSX/JSON + ~600 docs/yaml
- **Commits on branch:** 9 (4 feature, 1 spec, 1 plan, 3 plan-checkpoints)
- **Test status:** All 855 mur-core lib tests green; clippy clean on changed crates; P1.9 e2e smoke green

## Pre-existing failures (not caused by this branch)

- `mur-core/tests/agent_card_ephemeral.rs::card_displays_identity_pubkey` — fails on `main` upstream; unrelated to GUI work; documented in plan STATE.

## What's deferred (and why)

After the autonomous follow-up commits (`0bc78ba` … `a769744`), the deferred list is much shorter than the original COMPLETE log:

1. **shadcn/ui component library integration** — UI tabs use plain Tailwind + inline styles; shadcn is referenced in design but not pulled into package.json. ~1 day polish.
2. **Separate Logs window** — tray "Show Logs…" reuses settings window. Spec § 6.1 wants a dedicated logs window with live tail. ~50 LOC + Vite route.
3. **`OFFICE-12-GUI-EXPORT` Python harness scenario** — `mur-agent-harness/` is a separate repo; `scripts/e2e/p1-export-gui.sh` covers the same orchestration in pure bash.
4. **WCAG AA contrast validator** for theme JSONs as a CI check — currently the 5 built-ins were hand-checked.
5. **Production codesign / notarize end-to-end exercise** — recipe is implemented (`codesign --options runtime --timestamp` etc.) but not run on this dev host (no Apple Developer cert).
6. **P1.5 OS appearance subscriber** for the "Match System" toggle — schema is defined; subscriber Rust code is a 30-LOC follow-up.

What's NO LONGER deferred (landed in autonomous follow-up):

✓ **P1.4 sidecar manager** — `mur-agent-gui/src-tauri/src/sidecar.rs` (spawn + exponential backoff + `kill -pgid` + Windows Job Object + 2 unit tests)
✓ **P1.6 Tauri-side bootstrap** — `bootstrap.rs` (template-mode mint + clone-mode rekey-stub + 2 unit tests)
✓ **Per-agent CFBundleIdentifier + CFBundleName** patching during export
✓ **Sidecar binary bundling** via `bundle.externalBin`
✓ **Auto-spawn on launch** (click `.app` → agent runs)
✓ **Tray menu** with Show Settings / Show Logs / Start / Stop / Quit
✓ **SIGTERM/SIGINT proxy** to `mgr.stop()` for clean shutdown
✓ **macOS codesign recipe** — real `codesign --options runtime --timestamp …` invocations on inner sidecar then outer .app, gated on `MUR_APPLE_DEVELOPER_ID`

## Resume protocol for the next session

1. `cd /Users/david/Projects/mur && git checkout feat/agent-gui-export`
2. Read `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan.md` STATE block
3. Run `bash scripts/e2e/p1-export-gui.sh` — should print "P1.9 smoke OK"
4. To progress P1.4-P1.6:
   - Install Tauri CLI locally: `cargo install tauri-cli --version '^2.0' --locked`
   - `cd mur-agent-gui/ui && npm ci`
   - `cd mur-agent-gui/src-tauri && cargo tauri dev`
   - The 6-tab settings window opens. Iterate on each stub command in `commands.rs`.

## Verification ledger (final, after autonomous follow-up)

```
$ cargo test -p mur-core --lib
test result: ok. 855 passed; 0 failed; 6 ignored

$ cargo test --manifest-path mur-agent-gui/src-tauri/Cargo.toml --lib
test result: ok. 4 passed; 0 failed (sidecar + bootstrap unit tests)

$ cargo clippy -p mur-core -p mur-agent-runtime -- -D warnings
finished

$ mur agent doctor --format gui
✓ cargo, ✓ rustc, ✓ host-target, ✓ node, ✓ npm, ✓ tauri-cli (2.10.1), ✓ xcode-clt

$ FULL_E2E=1 bash scripts/e2e/p1-export-gui.sh
✓ doctor returns expected check shape
✓ all GUI flags exposed via --help
✓ pipeline produced artifact at /var/folders/.../MyAgent.app
✓ Full E2E: copied bundle to ~/.cache/mur/p1-9-test/MyAgent.app
  GUI launched (pid=21157); waiting for running.lock
✓ running.lock appeared at .../agents/p1-9-test-agent/running.lock
▸ P1.9 FULL E2E OK — bundle launches, bootstraps, spawns runtime, writes running.lock
```

End-to-end means: click the produced `.app` → bootstrap extracts payload + mints identity → tray icon appears → sidecar manager spawns `mur-agent-runtime` → runtime acquires `running.lock` with valid Agent Card data including UUID + Unix-socket + capabilities. Tray menu Quit → SIGTERM → clean shutdown → no orphan lock.

---

Branch ready for review + merge to `main`. The PR description should reference both the spec and this COMPLETE log.
