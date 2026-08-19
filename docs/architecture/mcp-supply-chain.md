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
| Unsigned binary (macOS/Windows) | — | **yes** — rule 11 refuses startup | — |

`mur doctor` reports every one of these states across all agents, so the answer arrives before a failed startup does.

---

## Decisions, with reasons

### Rules 6 and 11 are admission control, not hooks

Both were written to `return Err(...)` from `B0SafetyHook::on_startup` with the documented intent of refusing startup. `HookChain::on_startup` is an observe-only phase: it returns `()` and folds every hook error into `warn!`. **Neither rule had ever stopped anything.** An agent on this machine sat in `BINARY DRIFT` for three days across several restarts while `mur agent status` reported it healthy.

They now live in `verify_mcp_supply_chain` in `mur-agent-runtime/src/hooks/b0.rs`, which the supervisor calls with `?`. (#793)

### MUR re-pins its own bundled MCP server

The supervisor refreshes `~/.mur/mcp-servers/mur-mcp-server` from the binary beside the running `mur` at every start, so **every upgrade drifts that pin by construction**. Enforcement without a re-pin would turn a routine `mur` upgrade into every agent refusing to start.

This is not a weakening: that binary was written moments earlier by this runtime from its own installation. Its trust anchor is "same install as the runtime", not a hash recorded weeks ago, and anyone able to replace it has already replaced `mur`. Third-party entries are never re-pinned. (#793)

### Interpreter-launched entries are reported, not enforced

For `command: npx, args: [@scope/pkg]` the pin hashes **npx**. Enforcing it would brick agents on any unrelated Node upgrade while covering none of the code that actually runs. Six agents on this machine were in exactly that state; all six drifted entries were `npx`, all seven direct-binary entries were clean. (#795)

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

## What a pin does not say

**"The same code", never "safe code".** What bounds the damage from a compromised MCP server is the agent's entitlements and sandbox, not its hash. A vendored, signed, provenance-carrying server still runs with everything that agent was granted.

**And on macOS, that bound does not exist for MCP servers at all.** SBPL is not inherited across `exec`, so a server the runtime spawns runs with the *user's* privileges — not the agent's entitlements. Linux is different in kind, not degree: Landlock and seccomp ARE inherited, so there the child really does run under the agent's policy. Until the pre-fork launcher lands (`mur-agent-runtime/src/sandbox/child.rs` names the design and the one call to activate), installing an MCP server on macOS is a trust decision about the whole machine, and no amount of pinning changes that. `mur agent perm show` and `mur agent doctor <name>` both say so rather than letting the entitlement list imply otherwise.

The open follow-on is to connect the two: a server whose provenance cannot be verified is a reason to suggest narrower entitlements at install time — protection that still works when detection fails.

---

## Where this lives in code

| Concern | Location |
|---|---|
| Startup enforcement (rules 6 + 11) | `mur-agent-runtime/src/hooks/b0.rs` — `verify_mcp_supply_chain` |
| Called by | `mur-agent-runtime/src/supervisor_runner.rs` (with `?`, before the hook chain) |
| Re-pin of MUR's own bundle | `mur-agent-runtime/src/mcp_repin.rs` |
| Pin status classification | `mur-core/src/cmd/agent_mcp_pin.rs` — `binary_status` |
| Vendoring + signature audit + provenance | `mur-core/src/cmd/agent_mcp_vendor.rs` |
| Deep audit | `mur-core/src/cmd/agent_mcp_deep_audit.rs` |
| Package spec parsing / version resolution | `mur-common/src/mcp_package.rs` |
| Which lockfile a pin covers | `mur-common/src/agent.rs` — `McpPackagePin::lockfile_path` |
| Fleet-wide reporting | `mur-core/src/cmd/misc.rs` — `report_mcp_pins`, behind `mur doctor` |

User-facing documentation: https://app.mur.run/docs/core/mcp-pinning
