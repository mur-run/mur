# Code navigation for agents (ast-grep + optional serena) — plan skeleton

> Skeleton only. Phase 0 must finish before Phases 1–3 are detailed into
> tasks; several Phase 0 results already changed the design (marked **[P0]**).
> Track progress with the checkboxes in this file, not in memory.

**Goal:** Give MUR agents structural code search (ast-grep, on by default) and
optional symbol navigation (serena + language servers, opt-in), without letting
a hostile repository execute code through either tool.

**Threat model in one line:** a read-only tool surface does not stop a repo from
running code *through* the tool (config discovery, build scripts, proc-macros,
build-tool imports). Every tool config MUR launches must come from MUR, never
from the repo.

## Global constraints (every phase)

- No repo-controlled config reaches a tool. ast-grep: `-c` + MUR-owned cwd.
  serena: MUR-owned config dir must exist or startup is refused.
- `--yes` confirms a printed plan; it never adds anything to the plan. Flags
  decide scope. Precedent: `mur-core/src/cmd/browser/setup.rs:11-14`,
  `grant_egress` in `mur-core/src/cli/actions.rs:961-965`.
- No TTY and no `--yes` ⇒ refuse.
- Plan construction (`prepare`) is pure: detection results + flags in, plan out.
- No hardcoded paths; everything derives from mur home / agent home.
- Source files ≤ 800 lines.

## Phase 0 — verification (blocks Phase 1+)

Binary under test: ast-grep 0.45.3. Raw notes:
`<mur_home>/artifacts/mur/astgrep-phase0/results.md`.

### ast-grep (items 1–12 + 2a) — done

Note: the original wording of items 1–12 was not persisted; the list below
was reconstructed from the conversation and then run. Re-check against the
original list if one turns up.

- [x] 1. Flag names: `--globs` (repeatable, `!` negates), `--strictness`
      {cst,smart,ast,relaxed,signature,template}, `-C` (conflicts with
      `-A`/`-B`), `--json=stream` (`=` required). No max-count flag.
- [x] 2. **[P0]** `run` reads `sgconfig.yml`, discovered by walking **up from
      cwd**. `customLanguages.libraryPath` ⇒ dlopen attempt (exit 79) =
      code-execution vector.
- [x] 2a. Isolation layers, tested separately (hostile sgconfig in repo root,
      repo `src/`, and an ancestor of cwd):

      | Case | Result |
      |---|---|
      | no `-c`, cwd = repo | exit 79 (hostile config loaded) |
      | `-c` only, cwd = repo or repo/src | exit 0 |
      | MUR cwd only, path arg = repo or repo/src | exit 0 (path arg does not trigger discovery) |
      | MUR cwd only, hostile config in an **ancestor of cwd** | exit 79 |
      | `-c` + MUR cwd, ancestor hostile present | exit 0 |

      Conclusion: `-c` alone defeats discovery in every case tested; MUR cwd
      alone does **not** (ancestor walk). Design: `-c` is the required
      control; MUR-owned cwd stays as a second layer (cheap, keeps relative
      paths in output predictable). `-c` to a missing file fails closed
      ("Cannot read configuration."); an empty file works. No `SG_*` /
      `AST_GREP_*` env vars found in the binary.
- [x] 3. `--json=stream` fields: `text`, `range` (byteOffset + 0-based
      line/column), `file` (relative to path arg), `lines`, `charCount`,
      `language`, `metaVariables` (absent without metavars). `-C` folds
      context into `lines`.
- [x] 4. Exit codes: 0 match / 1 none / 2 bad arg / 79 config error.
      **[P0]** Malformed pattern still exits 0/1; only stderr says
      `Pattern contains an ERROR node` ⇒ wrapper must surface stderr.
- [x] 5. Language probe: `run -p x -l <L> --stdin </dev/null` ⇒ 1 supported,
      2 unsupported; aliases accepted.
- [x] 6. No `-l` ⇒ language inferred from extension.
- [x] 7. `.gitignore` and hidden files skipped by default.
- [x] 8. Symlinks not followed by default.
- [x] 9. Unreadable files skipped silently, exit 0.
- [x] 10. **[P0]** Output is quadratic on long lines: one 5k-match line =
      127 MB in 0.37 s. ⇒ stream-parse, truncate `lines` per match, byte cap,
      kill child at cap. `HARD_MAX_OUTPUT_BYTES` is required, not defensive.
- [x] 11. SIGTERM ⇒ exit 143; threads only, no child processes.
- [x] 12. `outline` and `lsp` subcommands exist — out of scope, noted.

### serena / LSP (items 13–16) — done

Items 13–15 reconstructed from the risk-tier decision; confirm wording.

- [x] 13. rust-analyzer: with `cargo.buildScripts.enable=false` and
      `procMacro.enable=false`, does a hostile `build.rs` / proc-macro still
      run? (Watch for `cargo metadata` side effects.)
      Moot in v1, answered by item 16: serena hardcodes both settings to
      `True` and MUR cannot pass `initializationOptions` through serena, so
      the disabled configuration is unreachable. Rust is High (`--lsp rust`
      ≡ `rust-full`). Re-test against a v2 shim that can set them.
- [x] 14. clangd: with `--enable-config=false` and no `--query-driver`, is a
      repo `.clangd` / `compile_commands.json` driver ever executed?
      Probed with Apple clangd 21.0.0 against two hostile repos (`.clangd`
      only; `compile_commands.json` only), each naming a marker driver and a
      marker plugin (`-fplugin=` / `-Xclang -load`):
      - **Driver: never executed without `--query-driver`.** Zero
        `System include extraction` attempts across all four runs without it.
        With `--query-driver` matching the repo path, clangd tried to spawn
        the repo driver (stopped only by MUR's outer sandbox). MUR must never
        pass `--query-driver`.
      - **`.clangd`: blocked by `--enable-config=false`.** Without it the
        fragment is loaded (`trusted=false`) and its `Compiler` / `Add` flags
        still reach the compile command. Serena's default argv is
        `clangd --background-index`, without the flag, so MUR must inject it
        via global `ls_extra_args`.
      - **`compile_commands.json`: NOT blocked by `--enable-config=false`.**
        Repo flags, including `-load <dylib>`, reach the final cc1 command.
        Apple clangd 21 did not load the plugin (marker absent; plain
        `clang` with the same flags did load it). Rerun outside the MUR
        sandbox on the clangd serena actually runs on macOS arm64 (upstream
        19.1.2, zip hash matches serena's pin `d3b329b3…`) with
        `--enable-config=false`, against a database carrying both
        `-fplugin=` and `-Xclang -load`: `load_flag_lines=2` (flags reached
        the compile command), `plugin_loaded=0` (plugin not loaded). Same
        result as Apple clangd 21. Not tested: `-fpass-plugin=` and other
        plugin spellings; Linux builds of 19.1.2. macOS code signing is not
        the cause: the upstream binary is ad-hoc, linker-signed, without the
        hardened runtime (`flags=0x20002`) or entitlements, so library
        validation does not apply, and the marker dylib is ad-hoc signed the
        same way.
      - Serena writes a rewritten database into the repo's `.serena/`
        (`compile_commands_dir` default). MUR must point it at a MUR-owned
        directory.
      Evidence: `~/.mur/artifacts/mur/clangd-item14-202610031414/`.
- [x] 15. gopls / jdtls / sourcekit-lsp: confirm import runs build tooling
      and cannot be disabled (justifies "skipped unless named").
      **Design decision, not probed.** These are High and off by default;
      they start only under an explicit `--lsp <lang>`. Unlike Rust and
      C/C++, their safety does not rest on configuration that narrows a
      default-on server, so a probe cannot change the tier or the default.
      Not verified: whether opening a hostile repo runs `go list` / `go`
      toolchain downloads, Gradle/Maven, or SwiftPM manifests, and whether
      any of it can be turned off. Probe in v2, or before any of these is
      moved below High.
- [x] 16. How does serena pass settings to each LSP? (serena-agent
      2.0.0.dev0, source read statically; rust-analyzer 1.98.1 driven by a
      hand-written LSP client that mimics serena, because the serena binary
      itself is not runnable from the MUR seal.)
      - `ls_specific_settings` = global `serena_config.yml` (under
        `SERENA_HOME`) **updated by** project `project.yml` when the project
        is trusted (`serena/project.py:522-529`). Default
        `trusted_project_path_patterns` is `["**"]`
        (`serena_config.py:942`), and the default project folder is
        `$projectDir/.serena` (`serena_config.py:301`). ⇒ **a repo-shipped
        `.serena/project.yml` can set `ls_path` / `ls_base_cmd` = arbitrary
        exec.** MUR must set `trusted_project_path_patterns: []` and a
        MUR-owned `project_serena_folder_location`.
      - Generic keys only control the launch command: `ls_base_cmd`,
        `ls_path`, `ls_args`, `ls_extra_args`
        (`solidlsp/dependency_provider.py`). Per-LS keys exist for some
        servers: gopls `gopls_settings` → `initializationOptions`; clangd
        `ls_path`, `compile_commands_dir`.
      - rust-analyzer: `initializationOptions` are **hardcoded** with
        `buildScripts.enable: True`, `procMacro.enable: True`,
        `checkOnSave: True` (`rust_analyzer.py:487-669`). There is no settings
        key that reaches them. Serena also sends `didSave` before each
        diagnostics request (`rust_analyzer.py:706-715`), which forces flycheck
        (`cargo check`). Flycheck builds and runs `build.rs` even with
        `buildScripts.enable=false` (test `save`: build-script exec attempted).
        ⇒ **Through serena config alone, `--lsp rust` cannot be degraded.**
      - Driving rust-analyzer directly shows what a MUR shim could pin:
        | Case | Init options | Repo `rust-analyzer.toml` | Result |
        |---|---|---|---|
        | A / N | bs off, pm off, checkOnSave off | — | no cargo build, no build.rs |
        | G | same | `cargo.buildScripts.enable = true` | still no build.rs (not overridable) |
        | F | same | `checkOnSave = true` | **flycheck runs** (repo can flip it) |
        | B | bs off, checkOnSave on | `check.overrideCommand = touch …` | override ignored; cargo check ran build.rs |
        | I | + `check.overrideCommand = ["/usr/bin/true"]` | `checkOnSave = true` | no `target/`, no build.rs |
        | J | same as I | `checkOnSave = true` + own `overrideCommand` | repo override ignored; nothing ran |
        | C2 | serena defaults | user-level ratoml (fake HOME) disables bs | build.rs still attempted — user ratoml is no lever |
      - procMacro re-enable from a repo ratoml: **not tested**.
      - **Decision (v1): no shim.** `rust` ≡ `rust-full`; both are High tier
        and must be named explicitly (Phase 3 table). Rationale: a shim that
        rewrites `initialize` means owning rust-analyzer init-option
        compatibility, and the evidence above covers one version (1.98.1)
        only.
      - **Two separate findings, kept separate:** the trust fix (Phase 2
        pre-start checks) closes repo-chosen exec via `ls_path` /
        `ls_base_cmd`; it does **not** change serena's hardcoded
        rust-analyzer init options, so build scripts / proc-macros /
        `cargo check` still run under `--lsp rust`.
      - Deferred to v2: shim set as global `ls_path` rewriting
        `initializationOptions` (bs off, pm off, checkOnSave off,
        `check.overrideCommand` pinned to a no-op; cases I/J). Gate: test
        procMacro re-enable from a repo ratoml, and repeat cases A–J on
        more than one rust-analyzer version.

## Phase 1 — ast-grep wrapper (`ast_grep_search` tool)

- Spawn with `-c <MUR-owned empty sgconfig>` and cwd = MUR-owned dir; repo
  passed as path argument.
- Args exposed: pattern, lang, globs, strictness, context. Lang validated via
  the probe in item 5 (cached).
- Stream-parse stdout line by line; per-match `lines` truncation; cumulative
  byte cap ⇒ kill child, report truncated.
- Map exit codes (item 4); always return stderr warnings to the agent.
- Tests: hostile sgconfig in repo and in cwd ancestor; minified-line blowup;
  malformed pattern; unsupported lang.

## Phase 2 — serena integration (opt-in)

- Pre-start checks (refuse startup if any fails; item 16):
  - MUR-owned serena config dir exists and is used as `SERENA_HOME`.
  - Global `serena_config.yml` has `trusted_project_path_patterns: []`, so a
    repo `.serena/project.yml` can never override `ls_specific_settings`
    (`ls_path` / `ls_base_cmd` = arbitrary exec).
  - `project_serena_folder_location` points at a MUR-owned directory, not
    `$projectDir/.serena`.
  - If C/C++ is enabled: the clangd launch command contains
    `--enable-config=false` (via global `ls_extra_args`), contains no
    `--query-driver`, and `compile_commands_dir` resolves to a MUR-owned
    directory (item 14).
- Expose six read-only tools; tool-deny write tools.
- LSP settings: only launch-command keys are controllable through serena
  (item 16). rust-analyzer init options are serena-hardcoded and not
  degraded in v1.

## Phase 3 — setup consent flow

Flags: `--with-serena` (off), `--no-ast-grep` (ast-grep on by default),
`--lsp <lang>` (repeatable allow-list).

LSP risk tiers:

| Tier | Languages | Default |
|---|---|---|
| Low | Python, TypeScript, PHP, Lua, Ruby | listed, enabled |
| Medium, contained | C/C++ (`--enable-config=false` via `ls_extra_args`, no `--query-driver`, MUR-owned `compile_commands_dir`) | listed, enabled |
| High | Rust (`--lsp rust` ≡ `--lsp rust-full` in v1), Go, Java, Swift | needs explicit `--lsp <lang>` |

In v1 `rust` and `rust-full` behave identically: serena hardcodes
`buildScripts`, `procMacro` and `checkOnSave` on and sends `didSave` before
diagnostics, so `build.rs`, proc-macros and `cargo check` run (item 16). The
setup table must say so in the Rust row. Both values are kept so a v2 shim can
make `rust` the degraded mode without changing the CLI; `rust-full` is never
reached by upgrading `rust`.

C/C++ is Medium because serena's pinned clangd 19.1.2 ignores `-load` /
`-fplugin` from a repo `compile_commands.json` (item 14). The containment is
clangd's, not `--enable-config=false`'s: that flag does not cover the
compilation database. If a later clangd pin or another plugin spelling loads
a repo dylib, C/C++ moves to High.

Output: three tables (install, permissions, LSP risk — skipped languages
listed with the flag that enables them), then one confirmation. Applied plan
written to a setup manifest; re-runs prompt only for the diff.

## Open questions

- Provenance of items 1–15: no original list survives. Named in an earlier
  review: flag names, sgconfig, `--json=stream` fields, lang probe,
  truncation/teardown. All other items were reconstructed for this plan. If
  the original list turns up, diff it against this one.
- v2: rust-analyzer shim (see item 16, deferred).
- Whether `outline` could replace part of serena for symbol listing (out of
  scope for v1).
