# `mur browser setup` — implementation plan

> Execute with `mur-executing-plans` (in-context, sequential). Tick boxes on disk.

**Goal**: `mur browser setup` installs replay's Chromium on a literal `yes`,
then proves it renders.

**Architecture**: steps 1–5 of the spec's flow live in a sync, pure
`setup::prepare` (TTY flag, input, output, PATH, browsers dir, probe,
installer all injected). `Ok(())` means "go to the live test". Dispatch then
calls the existing async `doctor::live_check`. Consent is one shared
`consent::literal_yes`, also used by deep research.

**Deviation from spec (Code shape)**: the spec sketches one
`setup(..., live)` function. `live_check` is async and borrows `output`, so
it is called by dispatch after `prepare` instead of being injected. The
spec's "live never called" cases become "`prepare` returns `Err`", which
dispatch propagates with `?` before `live_check`.

**Tech**: Rust 2024, `mur-core`, anyhow, tempfile in tests.

## Global Constraints (from spec)

- Nothing downloads without a human typing `yes`. No `--yes` flag.
- Only a trimmed literal `yes` is consent; `y`, `Y`, empty, EOF are not.
- The printed plan appears before the prompt and contains the full argv and dir.
- Install argv comes from `doctor::install_argv()`; `install_hint() == install_argv().join(" ")`.
- Install child uses the unmodified `PATH` and inherited stdio.
- `PLAYWRIGHT_BROWSERS_PATH=0` never installs; goes straight to live.
- A declined install exits 1.
- non-TTY bails before any check, printing both script commands.
- `mur browser doctor` output and exit codes byte-identical, except one new
  `or run: mur browser setup` line under the Chromium install hint.
- Deep-research consent tests pass unchanged.
- No new `cfg!` / `target_os` (FreeBSD audit).

## Files

| File | Responsibility |
|---|---|
| `mur-core/src/cmd/consent.rs` (new) | `literal_yes` prompt + rule |
| `mur-core/src/cmd/mod.rs` | `pub mod consent;` |
| `mur-core/src/cmd/deep_research/browser.rs` | call `literal_yes` instead of inline prompt |
| `mur-core/src/cmd/browser/doctor.rs` | `install_argv`, `L1Report`, `l1_check`, setup hint line |
| `mur-core/src/cmd/browser/setup.rs` (new) | `prepare`, `system_installer`, `SCRIPT_HINT` |
| `mur-core/src/cmd/browser/mod.rs` | `pub mod setup;` |
| `mur-core/src/cli/actions.rs` | `BrowserAction::Setup` |
| `mur-core/src/dispatch.rs` | wire `Setup` |

## Task 1 — shared consent

Produces: `pub fn literal_yes(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<bool>`
in `crate::cmd::consent`. Writes `Type 'yes' to do this now (anything else = skip): `,
flushes, reads one line, returns `line.trim() == "yes"` (EOF → empty → false).

- [x] RED: table test `yes`, `yes\n`, `  yes  \n` → true; `y\n`, `Y\n`, `YES\n`, `\n`, `` (EOF), `yess\n` → false; prompt text written.
- [x] GREEN: implement; `pub mod consent;`.
- [x] Refactor `ensure_render_browser` to `if !crate::cmd::consent::literal_yes(input, output)? {` — deep-research tests green unchanged.
- [x] Commit.

## Task 2 — doctor seams

Produces:
```rust
pub fn install_argv() -> Vec<String>          // ["npx","-y",PKG,"install-browser","chromium"]
pub enum Chromium { Found(String), Missing, Unknown }   // Found carries build name
pub struct L1Report { pub npx_ok: bool, pub chromium: Chromium }
impl L1Report { pub fn ready(&self) -> bool }
pub fn l1_check(output, path_var, browsers, run) -> Result<L1Report> // writes, never bails on problems
pub fn doctor(...) -> Result<()>  // = l1_check, then bail if !ready (same message)
```
`npx_ok` = npx resolved AND `node`/`npx --version` both passed (setup cannot install otherwise).

- [x] RED: `install_hint() == install_argv().join(" ")`; missing-Chromium output contains `  or run: mur browser setup`; `l1_check` report cases (Found / Missing / Unknown, npx_ok false on missing npx and on failing node).
- [x] GREEN; existing 11+ doctor tests unchanged.
- [x] Commit.

## Task 3 — `setup::prepare`

Consumes Task 1 + 2. Produces:
```rust
pub type Installer<'a> = &'a mut dyn FnMut(&[String]) -> Result<bool>; // Ok(true) = exit 0
pub const SCRIPT_HINT_HEAD: &str = "`setup` is interactive; in scripts run:";
pub fn prepare(interactive: bool, input: &mut dyn BufRead, output: &mut dyn Write,
               path_var: &OsStr, browsers: Option<&Path>,
               probe: Probe<'_>, install: Installer<'_>) -> Result<()>
pub fn system_installer(argv: &[String]) -> Result<bool>
```
Flow: !interactive → bail with head + `  <install_hint>` + `  mur browser doctor --live`
(nothing probed); `l1_check`; !npx_ok → bail; Found/Unknown → Ok (Unknown prints why);
Missing → plan (`Installing Chromium for replay does:` / `    run       <hint>` /
`    into      <dir>` / `    (downloads the Chromium revision this pinned package launches)`),
`literal_yes`, false → `  skipped — nothing was downloaded.` + bail; install Err/false → `  ✗ install command failed` + bail;
rescan `installed_builds` (headless shell, then full) → none: `  ✗ still no completed Chromium build in <dir>` + bail; else `  ✓ <name> in <dir>` + Ok.

- [ ] RED/GREEN one case at a time, the 9 non-live cases from the spec.
- [ ] Commit.

## Task 4 — CLI + dispatch

- [ ] RED: `mur browser setup` parses to `BrowserAction::Setup`.
- [ ] GREEN: variant with doc comment; dispatch: `prepare(stdin.is_terminal(), &mut stdin.lock(), ...)?; live_check(&mut out).await?; println!("mur browser is ready.");`
- [ ] Commit.

## Task 5 — verify & ship

- [ ] `cargo fmt --check`, clippy `-D warnings` on mur-core, `cargo test -p mur-core --lib` touched modules, `python3 scripts/check-freebsd-audit.py`.
- [ ] Manual (user, macOS): empty `PLAYWRIGHT_BROWSERS_PATH` → decline / accept / re-run.
- [ ] PR with MUR signature.
