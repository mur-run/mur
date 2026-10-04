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
- [x] 17. End-to-end matrix through serena itself (serena-agent
      2.0.0.dev0, run outside the MUR seal on Darwin arm64, 2026-10-04,
      isolated `solidlsp_dir`; `~/.solidlsp` untouched). Each language:
      cold start + warm start, cross-file `find_referencing_symbols` on a
      two-file fixture.
      | Language | Cold result | Cold download | Note |
      |---|---|---|---|
      | Java | PASS | 406.7 MB | warm 4.5 s |
      | Kotlin | PASS | 1245.2 MB | heaviest; cold 154 s |
      | Dart | PASS | 565.7 MB | |
      | Dart (missing `flutter` import) | PASS | 0 MB (shared) | unresolved import does not break cross-file refs |
      | Rust | PASS | 0 MB | rust-analyzer from PATH |
      | Bash | PASS | 72.2 MB | npm install + pinned shellcheck |
      | C# / F# | FAIL @create | — | needs .NET runtime 10.0 / 8.0; serena does not install it |
      | Ruby | FAIL @create | — | `gem install` into system Ruby → `Gem::FilePermissionError` (`/Library/Ruby/Gems/2.6.0`); hits every macOS stock-Ruby user |
      | Scala | FAIL @create | — | `coursier is not installed or not in PATH.` |
      | Elixir | FAIL @create | — | `Elixir is not installed.` |
      Warm starts download ~0 MB for every passing language, so serena does
      not re-fetch per start.
      Hostile-repo cases (marker files written by repo code, server held 30 s
      after the query so async build steps get their turn):
      - **RS1 — `build.rs` marker: written.** Opening the repo under serena's
        defaults runs `build.rs`; no user action needed. Confirms item 16
        end-to-end.
      - **K1 — `settings.gradle.kts` marker: written**; `build.gradle.kts`
        marker absent because the Gradle import then failed (`Unable to
        import a Gradle project: The supplied build action failed with an
        exception.`; root cause not captured — log was in the cleaned
        temp dir). Settings scripts are arbitrary Kotlin, so **opening a
        Kotlin/Gradle repo executes repo code.**
      - R1–R4 (Ruby): no markers, but ruby-lsp never started (initialize /
        create failed), so this is **not** evidence that Ruby is safe.
      - N1 (.NET): blocked, no `dotnet` on the host.
      - Low-tier hostile cases (run 2026-10-04T03:04Z, serena 2.0.0.dev0):
        - T1 (TypeScript): **marker written.** A repo-shipped
          `node_modules/typescript` is preferred by the server (log:
          `Using Typescript version (workspace) 5.9.3`), and its
          `tsserver.js` runs on open. **Opening a TS repo with its own
          `node_modules/typescript` executes repo code.**
        - T2 (TypeScript): `tsconfig.json` `compilerOptions.plugins` →
          repo `node_modules/evil-plugin`: no marker (server used the
          bundled TypeScript; plugin not loaded).
        - T1/T2 re-run with tsserver pinned (run 2026-10-04T03:18Z):
          the probe injected `initializationOptions.tsserver.path` =
          serena's own `ts-lsp/node_modules/typescript/lib/tsserver.js`.
          T1 and T2: **no marker**; every log shows
          `Using Typescript version (user-setting) 5.9.3`. Cold and warm
          still PASS (symbol `greet`, cross-file ref `b.ts`; warm 0 bytes
          downloaded). typescript-language-server 5.1.3 resolves tsserver
          user-setting > workspace > bundled, so a valid pinned path means
          the repo copy is never consulted. The pin was applied by
          monkeypatching `_create_base_initialize_params` inside the probe:
          serena 2.0.0.dev0 has no setting for it (its TS
          `ls_specific_settings` take only version and timeout, and
          `initializationOptions` is hard-coded to
          `disableAutomaticTypingAcquisition`).
        - P1 (Python): `pyrightconfig.json` `venvPath`/`venv` → repo
          wrapper `python`: no marker; pyright read the venv layout without
          executing the interpreter (`Assuming Python version 3.13.2`).
        - P2 (Python): unconfigured repo `.venv/bin/python`: no marker.
        - PH1 (PHP, negative control): `composer.json` scripts +
          `vendor/autoload.php`: no marker, as expected.
        - L1 (Lua): `.luarc.json` `runtime.plugin` → repo `plugin.lua`: no
          marker. LuaLS only warned (`The current settings try to load the
          plugin at this location … malicious plugin may harm your
          computer`). This depends on LuaLS gating plugins behind a trust
          prompt that serena does not answer; a serena or LuaLS change
          there would flip the result.
      Evidence: `~/.mur/artifacts/mur/serena-lsp-matrix/` (`report.md`,
      `results/`; the earlier RS1/K1 run is kept in
      `runs/2026-10-04T0228Z/`).

## Phase 1 — ast-grep wrapper (`ast_grep_search` tool) — done

**Status:** done. Config bounds, the wrapper, and output handling landed in
#1655; the `mur-search` skill routing (`mur-core/src/skills/mur_search.yaml`)
landed in #1663. Added beyond this plan: the `paths` and `max_results` tool
arguments, and the `search.ast_grep.max_match_bytes` config bound (the
per-match cap that `text` / `lines` truncation uses).

- Spawn with `-c <MUR-owned empty sgconfig>` and cwd = MUR-owned dir; repo
  passed as path argument.
- Args exposed: pattern, lang, globs, strictness, context. Lang validated via
  the probe in item 5 (cached).
- Stream-parse stdout line by line; per-match `lines` truncation; cumulative
  byte cap ⇒ kill child, report truncated.
- Map exit codes (item 4); always return stderr warnings to the agent.
- Tests: hostile sgconfig in repo and in cwd ancestor; minified-line blowup;
  malformed pattern; unsupported lang.

## Phase 2 — serena integration (opt-in) — done

**Status:** done. Tasks 2.1–2.7 plus the C9 follow-up landed in #1669
(`860cb98d`..`59f5b567`). Layer A: serena tests 36 passed, clippy
`--all-targets -D warnings` and fmt clean. Layer B: 15/15 PASS
(2026-10-04T01:35:27Z, C1–C9 incl. C9 missing-key and wrong-type cases).

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
- Expose five read-only tools (was six; see 2.1); everything else is
  dropped by an allow-list, not a deny-list.
- LSP settings: only launch-command keys are controllable through serena
  (item 16). rust-analyzer init options are serena-hardcoded and not
  degraded in v1.

### Phase 2 decisions

- **D1. Typed `kind: serena`, no generic `env` field.** The three serena
  checks are serena-specific; a typed kind gives them one hook point, and
  `SERENA_HOME` is computed by the runtime, never written in the profile. A
  generic `env` would let any MCP entry set environment variables and widen
  the supply-chain surface.
- **D2. Five tools, not six.** `get_diagnostics_for_file` is excluded: for
  Rust it sends `didSave` first, which runs `cargo check` (item 16). No
  sixth tool is added to fill the count. The implementation PR must say
  "plan said six, shipped five" with this reason.
- **D3. No install in Phase 2.** Phase 2 only launches an already-installed
  serena safely. A missing `SERENA_HOME` or config refuses startup; it never
  triggers an install. Install (uv venv, `--require-hashes`, consent) is
  Phase 3.

### Phase 2 facts found while splitting tasks (serena-agent 2.0.0.dev0)

- **Repo `.serena/` fallback.** `get_project_serena_folder`
  (`serena_config.py:1489-1507`) uses the configured folder only if it
  *exists*; otherwise it falls back to `$projectDir/.serena` when that
  exists. Pointing `project_serena_folder_location` at a MUR dir is not
  enough — the resolved MUR folder must exist before spawn (2.3 check C4).
- **serena rewrites its own global config at runtime.** Activating a project
  not yet registered calls `add_project_from_path` → `_persist_projects` →
  `_save()` (`agent.py:1531`, `serena_config.py:1373-1416`). So
  `SERENA_HOME` must be writable by the child, and the preflight cannot be
  startup-only: it re-runs at every spawn (2.4).
- **Dashboard on by default.** `web_dashboard: bool = True`
  (`serena_config.py:898`); MUR launches with
  `--enable-web-dashboard false --open-web-dashboard false`.
- **Tool names** are snake_case of the class name minus `Tool`
  (`tools_base.py:162-168`).

### Phase 2 tasks

Branch from `origin/main`. Files touched are listed per task; no file may
pass 800 lines (`mcp_client.rs` is 651, `b0.rs` 751 — logic goes in a new
module, those files get call sites only).

- [x] 2.1 **Type.** `mur-common/src/agent/mcp.rs`: add
      `pub kind: Option<McpServerKind>` to `McpServerEntry`
      (`#[serde(default, skip_serializing_if = "Option::is_none")]`), and
      `enum McpServerKind { Serena }` (`rename_all = "snake_case"`), and
      `pub project: Option<PathBuf>` (same serde attributes) — the one
      project serena serves (D4).
      Absent ⇒ today's behaviour, byte-identical serialization.
      Tests: round-trip with and without `kind` / `project`; unknown kind is
      a parse error, not silently ignored.
- [x] 2.2 **Module + paths.** New `mur-agent-runtime/src/mcp/serena.rs`
      (registered in `mur-agent-runtime/src/mcp/mod.rs`):
      - `pub const SERENA_TOOL_ALLOWLIST: [&str; 5] = ["get_symbols_overview",
        "find_symbol", "find_referencing_symbols", "find_implementations",
        "find_declaration"]`.
      - `pub struct SerenaPaths { home, config_file, projects_dir }` and
        `pub fn serena_paths(agent_home: &Path) -> SerenaPaths` — derived
        from agent home only, directory names as constants.
      - `pub fn launch_env(&SerenaPaths) -> Vec<(String, String)>` →
        `SERENA_HOME` only.
      - `pub fn launch_args(project_root: &Path) -> Vec<String>` →
        `--project <root>`, both dashboard flags `false`.
- [x] 2.3 **Preflight (pure).** `pub fn preflight(paths: &SerenaPaths,
      project_root: &Path) -> Result<(), SerenaPreflightError>` in
      `serena.rs`; parses `config_file` with `serde_yaml_ng` (already a
      dependency). One error variant per check, each naming the file, key,
      found value and expected value:
      - C1 `paths.home` is a directory.
      - C2 `config_file` exists and parses.
      - C3 `trusted_project_path_patterns` is **present** and `[]`. A
        missing key fails: serena's default is `["**"]`
        (`serena_config.py:942`).
      - C4 `project_serena_folder_location`, after `$projectDir` /
        `$projectFolderName` substitution, is under `paths.projects_dir`,
        and that resolved folder **exists** (fallback, see facts).
      - C5 `fixed_tools` equals `SERENA_TOOL_ALLOWLIST` (as a set);
        `excluded_tools` and `included_optional_tools` are empty.
      - C6 `web_dashboard` is `false`.
      - C7 no `ls_base_cmd` in any `ls_specific_settings` language;
        `ls_path`, if set, canonicalizes under `<mur_home>/tools/` (3.6b),
        else refuse (item 16); a symlink leading out is refused. Checks
        global config and MUR `project.yml`. The same canonical-under-tools
        rule binds setup's exec lanes (3.6c): C7 covers what serena launches,
        3.6c covers what the seal lets run.
      - C8 if C/C++ is enabled: clangd args contain `--enable-config=false`,
        contain no `--query-driver`, and `compile_commands_dir` resolves
        under `paths.projects_dir` (item 14). Exact settings key names are
        confirmed from serena source before coding, not guessed.
      Tests: one passing fixture; one failing fixture per check; a hostile
      repo with `.serena/project.yml` setting `ls_path` while C4's folder is
      missing ⇒ C4 refuses.
- [x] 2.4 **Hook: refuse startup.** `mur-agent-runtime/src/supervisor_runner/prepare.rs`, right
      after the `verify_mcp_supply_chain` call (~line 129), add
      `crate::mcp::serena::verify_entries(&profile.inner.enabled_mcp_servers(),
      agent_home).map_err(|e| anyhow::anyhow!(e))?;` — for every enabled
      entry with `kind: Some(Serena)`: `project` must be `Some`, absolute and
      an existing directory (else refuse; never fall back to cwd), then
      `preflight(&serena_paths(agent_home), project)`; same
      fail-closed path as rules 11/6, before the hook chain (which only
      warns).
- [x] 2.5 **Hook: every spawn.** `mur-agent-runtime/src/protocol/mcp_client.rs`,
      `StdioMcpClient::spawn` (~lines 262-283): when `entry.kind ==
      Some(Serena)`, re-run `preflight` with `entry.project` (serena
      rewrites its own config — see facts), then `std_cmd.envs(serena::launch_env(..))` and append
      `serena::launch_args(..)`. A failed preflight returns `McpError` and
      nothing is spawned. `spawn` needs the agent home: expose it read-only
      from `SandboxPolicy` (its `launch_chain` already holds it) rather than
      adding a parameter through `McpPool`.
- [x] 2.6 **Hook: tool allow-list.** `mur-agent-runtime/src/tools/registry.rs`, the discovery
      loop (~line 129, next to the `ToolPolicy::Deny` skip): for a serena
      entry, skip any `t.name` not in `SERENA_TOOL_ALLOWLIST`. This is the
      MUR-side gate; C5 is the serena-side one. Both, because the child can
      rewrite its own config. Test: a fake tools/list containing write tools
      and `get_diagnostics_for_file` registers exactly the five.
- [x] 2.7 **Docs.** `docs/architecture/mcp-supply-chain.md`: a `kind:
      serena` section — what C1–C9 cover, what they cannot (code running
      *inside* an LSP the user enabled; rust-analyzer per item 16), why no
      generic `env` field (D1), why five tools (D2), and the agent-writable
      config gap (G1) with all four parts: the risk, when it can be
      exploited, why v1 accepts it, the v2 fix. Also, found during 2.3/2.4:
      - C3 is the real guard for C7: serena ignores an untrusted project's
        `ls_specific_settings` (`serena/project.py:522-530`), so an
        agent-written `ls_path` in `project.yml` is ignored, and breaking C3
        refuses startup. C7 mainly catches a wrong or drifted global config
        (G1); it reads `project.yml` too as defense in depth, since a
        trusted one overrides the global (`project.py:523-525`). C8 reads
        only the global config.
      - C8 is conservative on purpose: it also applies when the MUR
        folder's `project.yml` is missing or has no readable language list,
        because serena then auto-detects languages. Cost: a first start of a
        non-C++ repo needs `project.yml` or the clangd lock-down; the C8
        error names both fixes. Over-refusing is accepted; under-checking
        is not.
      - C4 also canonicalizes, so a symlink in `projects_dir` that leads
        back into the repo is refused (not in the original task text).
      - C9 (#1688): `projects` must be present (a list, or null). serena
        raises without it and its message does not name the file.
      - Phase 2 never writes `serena_config.yml`; a missing or incomplete
        config refuses. Generating a complete config is Phase 3, and
        pre-filling `projects` is part of the G1 v2 fix (#1688 point 2).
      - Known gap, accepted for v1: `cpp_ccls` (ccls) is not checked. C7
        still confines `ls_path` / blocks `ls_base_cmd` for every language, and ccls
        is never serena's default; v2 reviews ccls's own config loading.
      - Exec lanes (follow-up from 3.6b / 3.6c): every directory setup grants
        exec on lies under `<mur_home>/tools/` after `canonicalize` —
        serena's venv and Python, pyright's venv and Python. The doc states
        that scope, not "serena's venv Python lives in uv's global dir"; it
        also updates the C7 row from "no `ls_path`" to "`ls_path`
        canonicalizes under `<mur_home>/tools/`" (3.6b).

### Phase 2 acceptance (two layers)

- **Layer A — unit + static (MUR agent can run):** tests for 2.1–2.6 pass;
  `cargo clippy --all --all-targets --no-deps --locked -- -D warnings` and
  `cargo fmt --all -- --check` clean.
- **Layer B — end-to-end (fleet or user; serena does not run inside the MUR
  seal: `bad interpreter: Operation not permitted`, same as item 16):**
  1. Valid config ⇒ agent starts, tools/list shows exactly the five.
  2. Each of C1–C9 broken in turn ⇒ startup refused with that check's error
     (C8 needs a C/C++-enabled fixture with clangd installed).
  3. Hostile repo `.serena/project.yml` with `ls_path` ⇒ marker never runs.
  4. Config rewritten between two spawns to `trusted_project_path_patterns:
     ["**"]` ⇒ second spawn refused (2.5).
  5. No dashboard port is listening after start.

**Phase 2 is not complete until Layer B passes.** Layer A green alone may
merge as a PR, but the checkbox for Phase 2 stays open.

### Phase 2 decisions (round 2)

- **D4. `project:` on the entry, written at setup.** `activate_project` is
  not exposed, so the project is fixed at launch. The path is written into
  the profile's `mcp_servers` entry when serena is set up (Phase 3 writes
  it; in Phase 2 a test fixture or the user does), never inferred from the
  session cwd at spawn time: cwd moves mid-session, and a serena respawn
  costs seconds to tens of seconds. A fixed input is also what lets 2.5
  re-run the same preflight at every spawn. `profile.yaml` is in
  `SELF_PROTECTED_WRITE_ONLY` (`mur-agent-runtime/src/sandbox/policy/mod.rs:57`), so the agent
  cannot retarget it.

### Phase 2 known gaps (not solved in v1)

- **G1. Agent-writable serena config.** *Status: known gap, accepted for
  v1 — not solved.*
  - *Why it is writable.* No read-only list covering serena files exists
    yet: the runtime's write-deny list is `SELF_PROTECTED_AGENT_FILES`
    (`mur-agent-runtime/src/sandbox/policy/mod.rs:39-44`) = `profile.yaml`, `identity.key`,
    `identity.pub`, `rotations.jsonl`. `SERENA_HOME` (agent home) is inside
    the agent's write grant, so its file tools can edit
    `serena_config.yml` and the MUR-owned `project.yml`.
  - *When it can be exploited.* Only at the **next spawn**. serena reads
    the global config once (`mcp.py:354` → `agent.py:590`) and builds
    language servers from that in-memory copy (`project.py:522`). The one
    runtime re-read, `_persist_projects` (`serena_config.py:1385`), loads
    the disk copy only to write the project list back; it does not replace
    the running settings. A running serena is unaffected; the next spawn
    runs 2.5's preflight, which refuses (C3/C5/C6/C7).
  - *`ls_path` (3.6b).* An agent-written `ls_path` in `project.yml` is
    ignored while C3 holds; in the global config, C7 refuses it.
  - *Why v1 accepts it.* Every exploit path goes through a spawn, and every
    spawn is preflighted; the 2.6 MUR-side allow-list holds regardless of
    the config. Same treatment as the shim deferred to v2.
  - *v2 fix.* Pre-register the project in `serena_config.yml` at setup, so
    serena never calls `_persist_projects` → `_save()` (facts above), then
    add `serena_config.yml` to the write-deny list. Needs proof that a
    pre-registered project never triggers a save before the deny lands.

## Phase 3 — setup consent flow

Flags: `--with-serena` (off), `--no-ast-grep` (ast-grep on by default),
`--lsp <lang>` (repeatable allow-list).

LSP risk tiers:

| Tier | Languages | Default |
|---|---|---|
| Low | Python, PHP, Lua | listed, enabled |
| Medium, contained | C/C++ (`--enable-config=false` via `ls_extra_args`, no `--query-driver`, MUR-owned `compile_commands_dir`) | listed, enabled |
| High | Rust (`--lsp rust` ≡ `--lsp rust-full` in v1), Kotlin, TypeScript, Go, Java, Swift, Ruby | needs explicit `--lsp <lang>` |

Rust, Kotlin and TypeScript are High on evidence, not by design decision:
item 17 shows each executes repo code on open (RS1 `build.rs`, K1
`settings.gradle.kts`, T1 repo `node_modules/typescript/lib/tsserver.js`).
The setup table must say "opening a repo runs its build scripts" in the
Rust and Kotlin rows, and "opening a repo runs its own TypeScript" in the
TypeScript row.

Python, PHP and Lua are Low on evidence (P1, P2, PH1, L1: no marker). Lua's
result rests on LuaLS refusing an unanswered plugin trust prompt, so the
matrix's L1 case must be re-run whenever the pinned serena or LuaLS version
changes.

Go, Java and Swift remain High by design decision (item 15).

Ruby is High provisionally (fail closed): `bundle exec ruby-lsp` evaluates
the repo `Gemfile`, and item 17 could not rule that out because ruby-lsp
never started. It drops to Low only if a re-run of R1–R4 on a working
ruby-lsp host leaves no marker (see Open questions).

Not yet tiered, so not offered by setup in v1: Dart, Bash, C#, F#, Scala,
Elixir. Their hostile-repo behaviour has not been probed.

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

### Phase 3 decisions

- **P3-D1 Command:** a standalone `mur code-nav setup --agent <name>`, the
  same shape as `mur browser setup`, so existing agents can be set up later.
  Not a `mur agent create` flag.
- **P3-D2 serena install:** `uv tool install serena-agent==<pin>` with
  `UV_TOOL_DIR` / `UV_TOOL_BIN_DIR` pointed at a MUR-owned directory under
  `<mur_home>/tools/serena/<pin>/`. uv is listed as a prerequisite in the
  install table (it is needed at runtime anyway: serena launches pyright via
  `uvx`, see item 17). MUR does not install uv itself.
- **P3-D3 LSP acquisition:** verify per language what serena fetches before
  writing the install table (item 17 below).

### Item 17 — how serena obtains each v1 language server

Static source read of serena-agent 2.0.0.dev0 (the copy Layer B ran,
installed by `uv tool` from git). Not yet confirmed by launching serena per
language; that run has to happen outside the MUR seal, like Layer B.

`SERENA_HOME` redirects serena's LS cache: `ls_manager.py:66` passes
`solidlsp_dir=serena_user_home_dir`, so downloads land in
`<agent_home>/serena/language_servers/static/`, not `~/.solidlsp`.

| Language | Server | How serena gets it | Pin | Integrity check | Runtime prerequisite |
|---|---|---|---|---|---|
| Python | pyright | `uvx pyright==<pin>` on first start (`LanguageServerDependencyProviderUvx`) | 1.1.403 | uv's resolver (PyPI), no sha in serena | uv at setup only (MUR pre-installs, launches via `ls_path`, 3.6b); **node on PATH** (else pyright's `nodeenv` downloads node at runtime) |
| TypeScript | typescript-language-server + typescript | `npm install --prefix ./ pkg@pin` in the serena-managed dir | 5.1.3 / 5.9.3 | none (npm registry); no `--ignore-scripts` | node + npm on PATH |
| PHP | intelephense | same npm path | 1.14.4 | none; no `--ignore-scripts` | node + npm |
| Lua | lua-language-server | PATH first, else GitHub release download | 3.15.0 | sha256 per asset, host allow-list | none |
| C/C++ | clangd | GitHub release download | 19.1.2 | sha256 (`d3b329b3…` osx-arm64) | none |
| Ruby | ruby-lsp | **see below** | 0.26.8 | none | ruby; bundler / gem. Pre-install cannot contain it: `gem install` writes to the user's **global** gem dir, outside the agent home |
| Rust | rust-analyzer | PATH or `~/.cargo/bin` only; never downloaded | user's | n/a | user-installed |
| Go | gopls | PATH only (`cmd="gopls"`) | user's | n/a | go + gopls |
| Swift | sourcekit-lsp | PATH only | user's | n/a | Xcode / toolchain |
| Java | jdtls | downloads jdtls, a JRE (21.0.x) and Gradle | serena's | not checked here | none |

Findings that change the Phase 3 design:

1. **Ruby is not Low.** `ruby_lsp.py:199` treats any repo with a `Gemfile`
   as a Bundler project; if `Gemfile.lock` mentions `ruby-lsp` it launches
   `bundle exec ruby-lsp` (`:235`), which evaluates the repo's Gemfile
   (Ruby code) and loads the repo's bundle. If `bundle` is not on PATH it
   falls back to the repo's own `bin/bundle` (`:208`), a repo-controlled
   executable. It also picks rbenv / mise / asdf / rvm from repo files
   (`.ruby-version`, `.tool-versions`). Otherwise `gem install ruby-lsp -v
   <pin>` writes to the user's global gem dir (`:252`). **Decision: Ruby
   is High** (explicit `--lsp ruby`), signed off. Same class as Rust's
   `build.rs`: a repo with a `Gemfile` makes serena run repo-controlled
   code. The tier table above is updated.
2. **npm installs run lifecycle scripts.** TypeScript and PHP install without
   `--ignore-scripts`. The packages are pinned and come from the registry,
   not the repo, and the cwd is serena's own dir, so this is a supply-chain
   risk, not a repo-controlled one. Keep Low; name it in the install table.
   **Known gap, not blocking:** adding `--ignore-scripts` is deferred until
   TS / PHP support is actually exercised in Phase 3 testing.
3. **First-start network.** Python / TS / PHP / Lua / C/C++ / Java fetch on
   the first LS start, from inside the running agent. Either setup pre-warms
   the cache (start serena once per enabled language during setup) or the
   agent's seal must allow that egress. **Decision: pre-install with a
   pinned version, launched through `ls_path`** (3.6b, below). The agent seal grants no egress for language servers.
4. **Runtime PATH prerequisites** (uv, node/npm, go, sourcekit-lsp,
   rust-analyzer) are checked by setup and shown in the install table; a
   missing one disables that language with a reason, it does not fail setup.
   **Disabling is never silent:** setup prints one line per disabled
   language naming the language and the missing tool (e.g. `python
   disabled: uv not found on PATH`), and the setup manifest records it, so a
   user never believes a language is on when it is off.

### 3.6b — pre-install decision (finding 3)

**Decision: setup installs each LSP, pinned, into a MUR-managed dir;
serena launches it via `ls_specific_settings.<lang>.ls_path`; the agent
never runs uv and gets no egress.** Consent covers the downloads (3.6's
`yes`); failures surface at setup. First language: Python (pyright). An
earlier draft chose "pre-warm the uv cache + `UV_OFFLINE=1`"; rejected
below. serena supports `ls_path` (`solidlsp/dependency_provider.py:187`):
"...launched directly, bypassing uv entirely."

**Measured** (uv 0.6.8, macOS arm64, network blocked via dead proxy
`127.0.0.1:9`, LSP `initialize` sent by a script):

| # | Route | Result |
|---|---|---|
| 1 | `uvx -p 3.13 --from pyright==1.1.403 pyright-langserver`, warm cache, `UV_OFFLINE=1` | `initialize OK` |
| 2 | Same, cache made read-only | **fails**: `failed to open file .../cache/sdists-v9/.git: Permission denied (os error 13)` |
| 3 | uvx warm-up with only `UV_CACHE_DIR` + `UV_PYTHON_INSTALL_DIR` pinned | **fails**: `Operation not permitted ... "~/.local/share/uv/tools/.tmp…"` |
| 4 | `uv tool install pyright==1.1.403` into a fixed dir, whole dir read-only, no `UV_*` env | `initialize OK` |
| 5 | Route 4, no node on PATH | **fails**: `nodeenv failed; for more reliable node.js binaries try ...` |

**Why not uvx + `UV_OFFLINE=1`:** uvx writes its cache at every start
(row 2), so G2's read-only requirement cannot hold on that route; it also
needs four pinned env vars per language (row 3 adds `UV_TOOL_DIR`). And
uv's index entries go stale after ~10 minutes, after which an offline start
without `UV_OFFLINE=1` fails (`error sending request for url
(https://pypi.org/simple/pyright/)`). With `ls_path` the runtime never
calls uv, so that failure mode is gone by construction.

**Hard requirements (same weight as C1–C9: not met → refuse to start, not
a warning):**

1. **Pinned install, launched via `ls_path`.** `uv tool install
   pyright==<pin>` with `UV_TOOL_DIR` / `UV_TOOL_BIN_DIR` /
   `UV_PYTHON_INSTALL_DIR` under `<mur_home>/tools/pyright/<pin>/` (3.3's
   pattern; `<pin>` = serena's `PYRIGHT_VERSION`, `1.1.403`). Setup writes
   its `pyright-langserver` as `ls_specific_settings.python.ls_path` in
   `serena_config.yml`; verification reads uv's install record (as 3.3).
2. **Preflight confines `ls_path`** (C7, rewritten): canonical path under
   `<mur_home>/tools/`, checked in the global config and the MUR folder's
   `project.yml`. Rationale and the C3 relationship: 2.7, G1.
3. **The agent seal allows exec (read-only) from the install dir** and its
   Python dir; without it the server cannot run (MUR's seal refused exec
   under `$TMPDIR`: `Operation not permitted`).
4. **Python requires node on PATH** (row 5). The pyright wheel has no
   `nodejs-wheel`; with no node it calls `nodeenv` (network). Missing node
   disables Python with a reason (finding 4); no download fallback.
5. **Install never opens the user's repo.**
6. **Missing install fails loudly:** `python LSP not installed — re-run setup`.

**From source reading, not run:**

- pyright does not run npm: the `pyright==1.1.403` wheel bundles
  `<wheel>/pyright/dist/langserver.index.js`; `_utils.py` uses it when
  versions match (`using bundled pyright`), and the langserver path passes
  `quiet=True`, so the PyPI JSON "newer version" check is skipped.
- TS / PHP: serena runs `npm install` only when
  `os.path.exists(executable)` is false, so a pre-installed server is not
  re-fetched; `"disableAutomaticTypingAcquisition": True` stops tsserver
  fetching `@types/*`. Whether TS / PHP move to `ls_path` is decided when
  they are implemented.

**G2 (exec dirs read-only to the agent): closed for Python by this
route** — `<mur_home>/tools/` is outside every agent's write grant and row 4
runs from a read-only dir. Still open for
`<agent_home>/serena/language_servers/` (servers serena installs itself):
an accepted gap, named in `mcp-supply-chain.md`, until G1's v2 list.

**Proven vs. not:** rows 1–5 ran as a script in MUR's own seal, not in a
real agent through serena; end-to-end is task 3.6b's acceptance.

### Phase 3 tasks

- [x] 3.1 Planner (pure): flags + detected prerequisites → plan with three
      tables (install, permissions, LSP risk). High (Rust, Kotlin,
      TypeScript, Go, Java, Swift, Ruby) needs `--lsp <lang>`; Rust row
      states `rust` ≡ `rust-full`. Missing prerequisites produce a visible
      "disabled: <tool> not found" row (finding 4). Untiered languages
      (Dart, Bash, C#, F#, Scala, Elixir) are refused by name.
      `mur-core/src/cmd/code_nav/plan.rs`; `AST_GREP_PINNED_VERSION` moved
      to `mur-common` so the installer and the resolver share one pin.
- [x] 3.2 ast-grep install: download 0.45.3, verify sha256 (per-platform
      constants), place at `binary_path()` under `<mur_home>/tools/ast-grep/`.
      `mur-core/src/cmd/code_nav/ast_grep_install.rs`. Two pins per platform:
      the release zip (must equal CI's table; a test enforces it) and the
      extracted binary, so a re-run verifies what is on disk without a
      download. Only the exact `ast-grep[.exe]` entry is placed (not `sg`).
      `ast_grep_binary_path()` moved to `mur-common`; the resolver delegates.
- [x] 3.3 serena install per P3-D2; pin recorded in the setup manifest.
      `mur-core/src/cmd/code_nav/serena_install.rs`. `serena-agent
      2.0.0.dev0` is not on PyPI (latest there is 1.x), so the pin is the
      upstream commit the Phase 0/2 findings and the item 17 matrix ran
      against (`SERENA_GIT_REV`), plus `--exclude-newer` at that commit's
      date so transitive versions cannot drift. `uv tool install` runs with
      `UV_TOOL_DIR` / `UV_TOOL_BIN_DIR` confined to
      `<mur_home>/tools/serena/<pin>/` and `--no-config`; MUR does not install
      uv. Verification reads uv's `direct_url.json`: dist-info version and
      resolved commit must both equal the pin; a verified re-run skips uv, a
      mismatch reinstalls. `Record` is the manifest entry; task 3.6 writes it.
      **One isolation pattern for every uv-installed tool (serena and
      pyright, 3.6b):** `uv tool install` into `<mur_home>/tools/<tool>/<pin>/`
      with `--python-preference only-managed` and `UV_PYTHON_INSTALL_DIR` =
      `<mur_home>/tools/<tool>/<pin>/python/`, so each tool's interpreter is a
      uv-managed Python inside its own dir — never a conda / system Python on
      PATH, never uv's shared `~/.local/share/uv/python/`. Each tool keeps its
      own copy (a duplicated ~tens-of-MB CPython per tool): accepted, because
      sharing one managed Python would widen every tool's exec grant to it.
      Verification also reads `pyvenv.cfg`: a venv whose `home` is not under
      the tool's `python/` dir is `Mismatch`, so an install made before this
      rule is rebuilt by the next setup rather than granted. Serena half:
      task 3.6c.
- [x] 3.4 Config generator: full-field `serena_config.yml` from serena's
      template (no load-time "migration" rewrite, #1688), `projects`
      pre-filled, C1–C9 values, C/C++ `ls_extra_args`. Run preflight on the
      result; it must pass.
      `mur-core/src/cmd/code_nav/serena_config.rs`. The template is read
      from the pinned install, not vendored (serena is GPL-3.0-or-later),
      and refused unless its sha256 equals `SERENA_CONFIG_TEMPLATE_SHA256`.
      At the pin the template lists every field `SerenaConfig` maps. Owned
      top-level keys (with their block lines) are dropped and MUR's values
      appended; the rest keep serena's values and comments. `auth_secret` is
      owned too: serena generates and re-saves one when it is empty. The
      clangd lock-down is always written, since C8 applies whenever serena
      may auto-detect C/C++. Written owner-only (0600, serena's own mode)
      via temp + rename, then `preflight()` from the runtime runs on it.
      Proof: the ignored live test loads the file with serena's own
      `SerenaConfig.from_config_file` and requires it byte-identical
      afterwards; with an empty `auth_secret` the same test fails
      ("serena rewrote the file"), so it detects a re-save.
- [x] 3.5 Profile entry: `kind: serena`, `project:`, `command` at the pinned
      path. `mur-core/src/cmd/code_nav/serena_entry.rs`, pure (3.6 saves the
      profile after consent). Shape is Layer B's: `args` =
      `start-mcp-server --transport stdio`; `--project` and the dashboard
      flags are not written (the runtime appends them at every spawn).
      `project` is canonicalized and must be an existing directory;
      `binary_sha256` pins the entry point; `command` is added to the spawn
      allow-list. Re-runs are idempotent (`installed_at` ignored); a
      same-named entry without `kind: serena` is refused, never overwritten.
- [x] 3.6 Consent + apply: print tables, require typed `yes` or `--yes`,
      write the setup manifest; re-runs prompt only for the diff.
      `mur code-nav setup --agent <a> [--with-serena --project <dir>]
      [--lsp <lang>]… [--no-ast-grep] [--yes]`; `cmd/code_nav/{setup,
      consent,serena_project}.rs`. `--project` is required with serena (no
      cwd guess). Apply order: ast-grep, serena, MUR-owned `project.yml`
      (from the pinned `project.template.yml`, sha-checked; must precede the
      config because C8 reads its language list), `serena_config.yml`
      (`auth_secret` kept across re-runs), profile entry, grants. Grants
      include read on the project and spawn-dir on serena's venv `bin` and
      the real interpreter dir (Layer B's two lanes, derived from the
      entry point's shebang). Manifest: `<mur_home>/setup/code-nav/<agent>.json`,
      outside every agent's write grant; a corrupt one is an error, never
      "nothing consented". Re-runs never revoke: a dropped language stays in
      `project.yml` and is reported as still enabled. An identical re-run
      asks nothing and rewrites both serena files byte-identically. Setup
      runs on `spawn_blocking` (blocking HTTP in the async dispatcher
      panicked).
- [x] 3.6b Pre-install pyright, launch via `ls_path` (see `### 3.6b`). To
      build: install row, `ls_path` in `serena_config.yml`, new C7, seal
      exec grant. **Built** (install row, `ls_path`, C7, seal grant).
      **Acceptance:** offline `initialize` proven by LSP script against the
      pyright that `mur code-nav setup` installed (bundled `dist/`, no npm,
      nothing written to `$HOME`); serena's own provider resolves the launch
      command to `[ls_path, --stdio]`. **Proven in a real agent seal:**
      serena's log shows `Starting language server process via command:
      ['<mur_home>/tools/pyright/1.1.403/bin/pyright-langserver', '--stdio']`,
      `Pyright language server 1.1.403 starting`, no npm activity, and a
      `find_symbol --include-info` call returns pyright hover text.
- [x] 3.6c serena's own Python under `<mur_home>/tools/` (3.3's isolation
      pattern, applied to serena; pyright already has it). To build:
      - serena's install command adds `--python-preference only-managed` and
        `UV_PYTHON_INSTALL_DIR=<mur_home>/tools/serena/<pin>/python`;
        `installed_state` treats a venv `home` outside that dir as
        `Mismatch` (one `pyvenv.cfg` check shared with pyright).
      - Setup refuses, not grants, an exec lane that does not canonicalize
        under `<mur_home>/tools/`: `interpreter_lanes` (3.6) runs
        `canonicalize`, so a symlink leading out of the tools dir is refused
        by name. With 3.6b that makes four lanes, all under
        `<mur_home>/tools/`: serena's venv `bin` and its Python's `bin`,
        pyright's venv `bin` and its Python's `bin`.
      - Runtime env: none added. The runtime never calls uv for serena or
        pyright (3.6b), the venv shebangs are absolute, and D1 rules out a
        generic `env` field; `UV_PYTHON_INSTALL_DIR` is install-time only.
      **Acceptance:** on a machine whose serena venv uses uv's shared
      Python, re-running setup rebuilds the venv (uv prints "requested Python
      interpreter does not match"), the summary's exec lanes contain no
      `~/.local/share/uv/python/` entry and do contain
      `<mur_home>/tools/serena/<pin>/python/<cpython-…>/bin`, the profile's
      `kind: serena` / `project:` are unchanged, and a serena symbol query
      still answers. Stale manifest `granted` lines (e.g. an earlier conda
      lane) are history, not grants; clearing them is optional.
- [ ] 3.7 (optional, #1688) Hash the config before/after launch; warn on
      rewrite.
- [x] 3.8 Docs: README, docs site, product page, `mcp-supply-chain.md`.
      README: command tree (35) and an integrations paragraph.
      `mcp-supply-chain.md`: C7 row rewritten, exec-lane scope added.
      mur-server branch `docs/code-nav`: `commands.md` index row and
      subcommand row (35), product-page card. A dedicated docs-site page is
      deferred until code-nav has more than `setup`.

## Open questions

- Provenance of items 1–15: no original list survives. Named in an earlier
  review: flag names, sgconfig, `--json=stream` fields, lang probe,
  truncation/teardown. All other items were reconstructed for this plan. If
  the original list turns up, diff it against this one.
- v2: rust-analyzer shim (see item 16, deferred).
- Ruby is provisionally High because `bundle exec ruby-lsp` evaluates the
  repo `Gemfile` (Ruby code). Item 17 could not test this: ruby-lsp did not
  start on stock macOS Ruby. Re-run R1–R4 on a host with rbenv/mise +
  ruby-lsp; if no case writes a marker, Ruby may drop to Low. If any marker
  appears, Ruby stays High and its setup row must say "opening a repo runs
  its Gemfile".
- Gradle import failure root cause in K1 (keep the IntelliJ log on re-run).
- TypeScript containment: the mechanism is proven (item 17, pinned T1/T2
  re-run: no marker, cold/warm PASS) but not shippable yet. serena exposes
  no way to set `initializationOptions.tsserver.path`, so
  `ls_specific_settings` cannot do it. TypeScript stays High until one of:
  (a) serena upstream accepts a TS setting that forwards `tsserver.path`
  (preferred; MUR then sets it in the global `serena_config.yml`;
  filed as https://github.com/oraios/serena/issues/2129), or
  (b) MUR launches serena through a wrapper that injects the path. (b)
  patches a private method and breaks silently on serena upgrades, so it
  needs a startup assertion that the server reports
  `source: user-setting`, failing closed to High otherwise. Either way,
  re-run T1/T2 through the shipping path (not the probe hook) before moving
  TypeScript to "Medium, contained".
- Whether `outline` could replace part of serena for symbol listing (out of
  scope for v1).
