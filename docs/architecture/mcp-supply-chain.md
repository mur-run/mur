# MCP Supply Chain — What Each Check Covers, and What It Cannot

**Status**: Current as of v2.59.0 + the follow-ups merged after it (#798, #799, #800, uvx).
**Issue thread**: #791 → #796. Implementation: #793, #795, #797, #798, #799, #800.

MUR runs MCP servers as child processes with the entitlements of the agent that owns them. This document records what the integrity checks around that actually guarantee — and the reasoning behind the ones deliberately not built, so they don't get re-proposed every six months.

The reasoning is collected here because it was previously spread across six PR descriptions and one issue.

---

## The constraint that shapes everything

**Every pin lives in `profile.yaml`, which is writable by the same principal as the thing it describes.**

`binary_sha256` describes a binary; `lockfile_sha256` describes an installed tree. An attacker who can edit either can edit the recorded hash just as easily, in the same file, with the same permissions.

So with one exception (`--deep`, below), these checks are **change detection**, not anti-tamper:

- ✅ they catch software that swapped something without telling you — a package manager re-resolving a floating version, an upgrade replacing a binary, a new upstream release arriving through a cache
- ❌ they do not catch an adversary with local code execution, who rewrites the expectation alongside the artifact

Both are worth having. Confusing them is what produced the defect this whole thread exists to fix.

---

## Coverage by entry shape

| Entry shape | Pinned on | Enforced at startup? | Blind to |
|---|---|---|---|
| Direct binary (`mur-mcp-server`, `agent-browser`) | sha256 of the binary | **yes** — refuses startup on drift | anything the binary loads at run time |
| Interpreter, unvendored (`npx @scope/pkg`) | nothing meaningful | **no** — reported as unprotected | everything: the hash covers `npx`, not the server |
| Interpreter, version-pinned (npx with an `@1.2.3` suffix) | which release is *requested* | no | the bytes of that release |
| Vendored npm (`node <install>/…`) | sha256 of `package-lock.json` | **yes** | post-install edits inside `node_modules` |
| Vendored PyPI (`<install>/venv/bin/<script>`) | sha256 of `requirements.lock` | **yes** | post-install edits inside the venv |
| Unsigned *native* binary (macOS/Windows) | — | **yes** — rule 11 refuses startup | a signed binary whose chain does not validate locally |
| Interpreter script (`npx-cli.js`, a `#!` wrapper) | — | **no** — not a signable image | — |

`mur doctor` reports every one of these states across all agents, so the answer arrives before a failed startup does.

---

## Decisions, with reasons

### Rules 6 and 11 are admission control, not hooks

Both were written to `return Err(...)` from `B0SafetyHook::on_startup` with the documented intent of refusing startup. `HookChain::on_startup` is an observe-only phase: it returns `()` and folds every hook error into `warn!`. **Neither rule had ever stopped anything.** An agent on this machine sat in `BINARY DRIFT` for three days across several restarts while `mur agent status` reported it healthy.

They now live in `verify_mcp_supply_chain` in `mur-agent-runtime/src/hooks/b0.rs`, which the supervisor calls with `?`. (#793)

### MUR re-pins the MCP servers it ships itself

The supervisor refreshes `~/.mur/mcp-servers/mur-mcp-server` from the binary beside the running `mur` at every start, so **every upgrade drifts that pin by construction**. Enforcement without a re-pin would turn a routine `mur` upgrade into every agent refusing to start.

This is not a weakening: that binary was written moments earlier by this runtime from its own installation. Its trust anchor is "same install as the runtime", not a hash recorded weeks ago, and anyone able to replace it has already replaced `mur`. Third-party entries are never re-pinned. (#793)

The same reasoning covers MUR's **other** shipped servers, and originally did not reach them. `mur-research-gateway` — what `mur deep-research setup` installs — rides in the same release, lands in the same install directory, and is replaced by the same upgrade, but was pinned by a bare command name like any third-party entry. One `mur` upgrade left every deep-research worker crash-looping at boot on `B0 rule 6: MCP \`research-gateway\` changed since install`; launchd restarts it, it fail-closes again, and `mur agent status` reports only `stopped`.

The exemption is a **property, not a list of names** — a list goes stale the day MUR ships another server. An entry is first-party when it resolves into the directory holding the running runtime's own executable **and** carries MUR's `mur-` name prefix. Both halves are load-bearing:

- **Directory alone** would silently drop pin enforcement for an unrelated third-party MCP binary the user keeps in `~/.local/bin` — a crowded directory on a normal machine, unlike a Homebrew Cellar.
- **Name alone** would trust any binary wearing MUR's name from anywhere on PATH.

Resolution canonicalizes, so a symlinked install (Homebrew's `bin` into its Cellar) compares equal.

### Rule 6 resolves against the PATH the spawn uses

Rule 6 hashed the binary `mur_common::exec::resolve_command` found on the **ambient** PATH, while the spawn resolves against `augmented_path_var` (`protocol/mcp_client.rs`, `supervisor_runner/prepare.rs`). The two disagree exactly where it matters: `augmented_path_var` appends `~/.local/bin`, where an installed MUR lives since #935, and launchd/systemd hand the runtime a PATH without it. A first-party binary found only there resolved for the spawn but not for the check — verification soft-failed with "command not resolvable, continuing" and the binary then ran **entirely unpinned**. Both passes now consult one PATH.

### Interpreter-launched entries are reported, not enforced

For `command: npx, args: [@scope/pkg]` the pin hashes **npx**. Enforcing it would brick agents on any unrelated Node upgrade while covering none of the code that actually runs. Six agents on this machine were in exactly that state; all six drifted entries were `npx`, all seven direct-binary entries were clean. (#795)

### Rule 11 checks signatures, not scripts

Resolving an entry's `command` canonicalizes through symlinks, so `npx` lands on `npm/bin/npx-cli.js` — a JavaScript file. `codesign` can never verify one, so the check was not strict, it was **unsatisfiable**: an agent given two `npx` MCP servers could not boot again, and the failure hint told the user to run `codesign` on a `.js` file. (#1326)

The scope is now the file header — Mach-O or PE — and not a list of interpreter names, because `npx` today is `bunx`/`pnpm dlx`/`uvx` tomorrow and such a list goes stale in exactly the direction that bricks agents. Everything rule 11 protected before, it still protects: a native image that cannot be verified refuses startup.

What covers an interpreter-launched entry is the row above — nothing, until it is vendored. That was already true; the signature check never added anything to it.

### Windows verifies through `wintrust.dll`, not `signtool`

`signtool` ships with the Windows SDK, not with Windows. Shelling out to it meant that on a stock machine *every* native MCP binary failed rule 11 with "signtool could not be spawned" — the same shape as the `npx` defect above: a startup gate on a question the machine could not answer and the operator could not fix. (#1332)

`WinVerifyTrust` is in `wintrust.dll`, present on every install, so the question is now answerable everywhere.

The policy is presence and integrity, **not trust chain**, which matches what macOS already did — `codesign -dv` reports whether a signature is there, it does not demand the chain validate locally. No signature, a digest that does not match the bytes, or an explicit distrust refuses the startup. An expired certificate or a root this machine does not trust does not: rule 11 exists to catch a binary that was swapped, a swapped binary fails the digest, and an expired cert is for the publisher to reissue — not something the agent's operator can act on.

The FFI is kept to a single function returning the raw status, with the policy in a pure `wintrust_verdict` compiled and tested on every platform. Code only a Windows CI runner can execute is code nobody reads a test failure for.

### Admission covers what the agent spawns

Rules 6 and 11 read `enabled_mcp_servers()`, not every entry in the profile. A disabled server never reaches `McpPool`, so letting one refuse startup made `mur agent mcp disable` — the recovery the failure message itself points at — unable to recover anything. (#1326)

### Vendoring: a MUR-owned install, fingerprinted by the lockfile

`mur agent mcp vendor` installs the exact version under `~/.mur/mcp-packages/<agent>/<server>/` and repoints the entry at the installed script. The agent then starts with no resolution step and no network.

The fingerprint is the lockfile, because it already contains an integrity hash for **every** package in the tree: 47 KB standing in for 37 MB of `node_modules`, and the cost of checking does not grow with the dependency tree. That affordability is what lets it run at every startup. (#797)

### A venv, not `--target`, for Python

Measured, not assumed. A console script written into a `--target` directory does a bare `from pkg import main` with no `sys.path` handling, so it runs only when `PYTHONPATH` points at the target — and `McpServerEntry` has no env to set it in. A venv's script execs the venv's own interpreter; verified running from `/` under `env -i`.

`uv pip install --require-hashes` also verifies every hash as it installs, which npm's install does not.

### Registry signatures at vendor time, not startup

`npm audit signatures` proves the bytes came from the registry — something a content hash cannot, since it would pin a poisoned cache as faithfully as a clean one. 100% coverage today (105/105 packages on a real install), two seconds, at install time. An **invalid** signature aborts the vendor by name; **missing** signatures are counted and reported, since refusing over them would buy strictness rather than safety. (#798)

### Provenance recorded, never required

A SLSA attestation ties a release to a source repo and CI run — the only signal here that can catch a *malicious publish*, which byte-pinning faithfully preserves rather than detects. Coverage is 11 of 105 packages; gating on it would refuse most of the ecosystem. (#799)

### Deliberately NOT built: tree hashing at startup

The intuitive next step — hash the whole installed tree so post-install edits are caught — is rejected:

1. It pays 37 MB of hashing at every agent start, forever, growing with the tree.
2. It buys protection only against an adversary who, per the constraint at the top, would rewrite the expected hash instead.

Shipping it would claim a protection that does not exist, which is precisely the defect #791 was filed for.

### `--deep` instead: the one check whose reference isn't local

`mur agent mcp inspect --deep` reinstalls from the pinned lockfile and diffs against **what the registry serves now**. Its reference value is not on the machine being audited, so it sees a locally-edited tree even when the pin was edited to match — the only check here with that property.

It is a command rather than a startup check because it costs a full reinstall. Reproducibility was the load-bearing assumption and was measured: `npm install` and a later `npm ci` from the same lockfile produce 4876 byte-identical files. Symlinks are skipped — npm's `.bin` shims are regenerated per install from metadata the lockfile already covers. (#800)

---

## `kind: serena` — a launch policy, not a pin

serena (LSP-backed code navigation, opt-in) is the first MCP entry whose risk is not the bytes of the server but **what its config tells it to launch**. A repo-controlled or agent-edited config can name any executable as a language server. So a `kind: serena` entry gets a typed launch policy on top of the pins above. Plan: `docs/superpowers/plans/2026-10-03-code-nav-astgrep-serena-plan.md`, Phase 2. Upstream behaviour cited below is serena-agent 2.0.0.dev0.

### What MUR does at launch

- **`SERENA_HOME`** is computed by the runtime as `<agent home>/serena` and set *after* the inherited, policy and seal env, so nothing upstream can override it. If the sandbox policy carries no agent home (`LaunchChain::is_inert()`), a serena entry refuses (`NoAgentHome`) rather than guess one.
- **`--project <root>`** comes from the entry's own `project:` field, which is written at setup into `profile.yaml`. That file is write-protected (`SELF_PROTECTED_WRITE_ONLY`), so the agent cannot retarget it. `project:` is never inferred from the session cwd.
- **The dashboard is forced off**: `--enable-web-dashboard false --open-web-dashboard false`. serena defaults it to on.

### The checks, C1–C9

`preflight` runs at **startup** (`verify_entries`, before the agent comes up) **and at every spawn** (`launch_additions`, before the child exists). The spawn re-run is not optional: serena rewrites its own `serena_config.yml` while it runs (registering a new project calls `_save()`), so a startup-only check is stale by the next spawn. Every refusal names the file, the key, what was found and what was expected.

| Check | Requires | Why |
|---|---|---|
| C1 | `SERENA_HOME` is an existing directory | A missing home never triggers an install (that is Phase 3); it refuses |
| C2 | `serena_config.yml` is readable YAML with a mapping root | Everything below reads it |
| C3 | `trusted_project_path_patterns: []` | Absent means serena's default `["**"]`, i.e. every repo is trusted and the project config in its own `.serena` folder may set `ls_path` / `ls_base_cmd` |
| C4 | `project_serena_folder_location` resolves under `<SERENA_HOME>/projects`, exists, and still lies there after `canonicalize` | serena uses the configured folder only if it *exists*, else falls back to the repo's own `.serena` folder. The canonicalize step refuses a symlink under `<SERENA_HOME>/projects` that leads back into the repo |
| C5 | `fixed_tools` is exactly the five allow-listed tools; `excluded_tools` and `included_optional_tools` empty or absent | serena-side tool gate |
| C6 | `web_dashboard: false` | Absent means serena's default `true` |
| C7 | No `ls_specific_settings.<lang>` sets `ls_base_cmd`; an `ls_path`, if set, is absolute, exists, and still lies under `<mur_home>/tools/` after `canonicalize`. Checked in the global config and in the MUR folder's `project.yml` / `project.local.yml` | Either key replaces the language-server executable — arbitrary exec. `ls_path` is allowed only so setup can point serena at the pyright it pre-installed; a symlink leading out of the tools root is refused |
| C8 | When C/C++ may run: the last `enable-config` in the effective clangd args is exactly `--enable-config=false`, with no `-`/`--query-driver*` and no `@file` argument; `compile_commands_dir` resolves under `<SERENA_HOME>/projects` | clangd reads repo `.clangd` files and can be told to run arbitrary compiler drivers |
| C9 | `projects` is present and a list (empty or null is fine) | serena's loader raises without it, and its error does not name the file; C9 refuses first with a `serena C9:` message |

**Phase 2 never writes `serena_config.yml`.** A missing or incomplete config refuses (C1, C2, C3–C9); nothing is generated or repaired. This matters because serena treats *any* missing mapped field as a migration and rewrites the whole file, filling an absent `trusted_project_path_patterns` with `["**"]` (#1688). Generating a complete config is Phase 3 (install), and pre-filling `projects` there is part of the G1 v2 fix below.

**Why C8 reads only the global config, and C7 reads more.** serena ignores a project's `ls_specific_settings` for an untrusted project (upstream serena, `project.py` lines 522–530). With C3 holding, no project is trusted, so the global file is the only place those settings can come from. C7 reads the MUR folder's `project.yml` and `project.local.yml` anyway, as defense in depth: if C3 ever lapsed, a trusted project's settings would override the global ones.

**Exec lanes: what the seal lets run.** C7 covers what serena *launches*; the seal grants cover what it is *allowed* to execute. `mur code-nav setup` grants exec only on directories that lie under `<mur_home>/tools/` after `canonicalize` — serena's venv and the uv-managed Python it was built with, and pyright's venv and Python. Both installs pass `--python-preference only-managed` with `UV_PYTHON_INSTALL_DIR` inside the tool's own directory, so neither borrows a system, Homebrew, conda, or uv-global interpreter; a venv whose `pyvenv.cfg` `home` points elsewhere is treated as a mismatch and rebuilt. A lane that resolves outside the tools root is refused before anything is written. Setup is additive and never revokes: a lane granted by an older setup stays in `profile.yaml` until removed with `mur agent perm`.

**C8 is conservative on purpose.** It applies when the global config has `ls_specific_settings.cpp`, when the MUR folder's `project.yml` lists `cpp`, **and** when that `project.yml` is missing or has no readable language list — because serena then auto-detects languages and C/C++ cannot be ruled out. The cost: the first start of a non-C++ repo needs either a `project.yml` that excludes `cpp` or the clangd lock-down. The C8 error names both fixes. Over-refusing is accepted; under-checking is not.

**C8 follows clangd's and serena's parsing, not string equality.** LLVM lets the last occurrence of an option win, accepts `-opt` as well as `--opt`, and expands `@file` response files, so `--enable-config=false --enable-config=true` would otherwise pass. serena lowercases language names and migrates the legacy `languages` / `language` keys, and merges `project.local.yml` over `project.yml`; C8 reads the language list the same way. clangd also prepends the `CLANGD_FLAGS` env var to its argv, which C8 cannot see, so the serena child is spawned with `CLANGD_FLAGS` removed.

### The tool gate, twice

Only five tools are ever registered: `get_symbols_overview`, `find_symbol`, `find_referencing_symbols`, `find_implementations`, `find_declaration` (`SERENA_TOOL_ALLOWLIST`). It is an allow-list, so a tool added by a future serena release stays hidden until it is reviewed and listed.

The gate exists on both sides. C5 checks serena's own config. `admit_tools` filters `tools/list` in the MUR registry, and it holds no matter what the child lists, because the child can rewrite its config after preflight.

### What these checks cannot cover

- **Code running inside a language server the user enabled.** The checks control *which* executable starts and with which launch flags. They do not make that server safe. rust-analyzer is the main case: serena hardcodes its init options, and MUR only controls the launch command, so build scripts, proc-macros and `cargo check` still run under Rust support. A shim that rewrites `initializationOptions` is deferred to v2.
- **ccls.** `cpp_ccls` is not checked. C7 still blocks `ls_path` / `ls_base_cmd` for every language, and ccls is never serena's default. v2 reviews how ccls loads its own config.
- **The per-agent bound.** As with every MCP server, serena runs with the agent's entitlements (see the next section). None of the checks narrow that.

### Decisions, with reasons

**D1 — a typed `kind`, no generic `env` field.** The serena checks are serena-specific; a typed kind gives them one hook point, and `SERENA_HOME` is computed by the runtime, never written in the profile. A generic `env:` on MCP entries would let any entry set any environment variable — a new supply-chain surface for every server, added to solve one server's problem.

**D2 — five tools, not six.** The plan originally listed six. `get_diagnostics_for_file` is excluded: for Rust it sends `didSave` first, which runs `cargo check` — the build-script exec path above, reachable from a read-looking tool. No sixth tool was added to fill the count.

### Known gap G1: the agent can write serena's config

*Status: known gap, accepted for v1 — not solved.*

- **The risk.** `SERENA_HOME` is under the agent home, inside the agent's write grant. The runtime's write-deny list (`SELF_PROTECTED_AGENT_FILES` in `mur-agent-runtime/src/sandbox/policy/mod.rs`) covers `profile.yaml` and the identity files only. So the agent's own file tools can edit `serena_config.yml` and the MUR-owned `project.yml` — for example, adding an `ls_path`.
- **When it can be exploited.** Only at the **next spawn**. serena reads its global config once at startup and builds language servers from that in-memory copy. The one runtime re-read (`_persist_projects`) loads the disk copy only to write the project list back; it does not replace the running settings. A running serena is unaffected.
- **Why v1 accepts it.** Every exploit path goes through a spawn, and every spawn re-runs C1–C9, so an edited config refuses (C3/C5/C6/C7) instead of launching. The MUR-side `admit_tools` gate holds regardless of the config.
- **The v2 fix.** Pre-register the project in `serena_config.yml` at setup, so serena never reaches `_persist_projects` → `_save()`, then add `serena_config.yml` to the write-deny list. This needs proof first that a pre-registered project never triggers a save, or the deny breaks serena.

---

## What a pin does not say

**"The same code", never "safe code".** What bounds the damage from a compromised MCP server is the agent's entitlements and sandbox, not its hash. A vendored, signed, provenance-carrying server still runs with everything that agent was granted.

**And the bound is per-AGENT, not per-server.** A spawned MCP server inherits the agent's sandbox in full — Landlock/seccomp on Linux, a seatbelt sandbox across `fork`+`exec` on macOS (the mechanism `sandbox-exec(1)` is built on; verified empirically, see `mur-agent-runtime/src/sandbox/child.rs`). Ordering holds it: the supervisor seals before the MCP pool is built, and the pool spawns lazily on first tool use.

What does not exist is granularity: no server can be given a *narrower* cage than the agent itself, because that needs a second `sandbox_init` in the child — the pre-fork launcher tracked in `child.rs`. So installing an MCP server grants it everything that agent was granted, and no amount of pinning changes that. `mur agent perm show` and `mur agent doctor <name>` both say so rather than letting the entitlement list imply per-server scoping.

> An earlier revision of this section claimed macOS children were unconfined. That was wrong — it restated a code comment nobody had tested. The empirical check is in `child.rs`; re-run it before restating either way.

The open follow-on is to connect the two: a server whose provenance cannot be verified is a reason to suggest narrower entitlements at install time — protection that still works when detection fails.

---

## Where this lives in code

| Concern | Location |
|---|---|
| Startup enforcement (rules 6 + 11) | `mur-agent-runtime/src/hooks/b0.rs` — `verify_mcp_supply_chain` |
| Called by | `mur-agent-runtime/src/supervisor_runner/prepare.rs` — `prepare_runtime` (with `?`, before the hook chain) |
| Re-pin of MUR's own servers (bundle + siblings) | `mur-agent-runtime/src/mcp_repin.rs` — `repin_first_party` |
| Pin status classification | `mur-core/src/cmd/agent_mcp_pin.rs` — `binary_status` |
| Vendoring + signature audit + provenance | `mur-core/src/cmd/agent_mcp_vendor.rs` |
| Deep audit | `mur-core/src/cmd/agent_mcp_deep_audit.rs` |
| Package spec parsing / version resolution | `mur-common/src/mcp_package.rs` |
| Which lockfile a pin covers | `mur-common/src/agent/mcp.rs` — `McpPackagePin::lockfile_path` |
| Fleet-wide reporting | `mur-core/src/cmd/misc.rs` — `report_mcp_pins`, behind `mur doctor` |
| serena preflight (C1–C9) | `mur-agent-runtime/src/mcp/serena/preflight.rs` — `preflight` |
| serena startup gate | `mur-agent-runtime/src/mcp/serena/mod.rs` — `verify_entries`, called from `supervisor_runner/prepare.rs` |
| serena spawn gate + launch env/args | `mur-agent-runtime/src/mcp/serena/mod.rs` — `launch_additions`, called from `protocol/mcp_client.rs` — `StdioMcpClient::spawn` |
| serena tool allow-list | `mur-agent-runtime/src/mcp/serena/mod.rs` — `SERENA_TOOL_ALLOWLIST`, `admit_tools`, called from `mur-agent-runtime/src/tools/registry.rs` |

User-facing documentation: https://app.mur.run/docs/core/mcp-pinning
