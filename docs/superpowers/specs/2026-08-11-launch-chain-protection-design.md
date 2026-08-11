# Launch-chain protection

**Status:** design, approved 2026-08-11
**Scope:** Spec 1 of 2. `agent_create` (Spec 2) and runtime binary attestation (follow-on) are out of scope and described only where they constrain this design.

## Problem

An agent's filesystem write grants can cover the files that decide **what MUR starts next and with what authority**. On the machine this was found on, the seeded concierge held three such grants, each independently sufficient to leave the sandbox:

| Grant | Why it escapes |
|---|---|
| `~/.mur/agents` | `self_protected()` denies only the agent's **own** `profile.yaml` + `identity.key`, so a sibling's profile (its entitlements) and its `identity.key` (its Ed25519 signing authority) are writable. Rewrite a sibling profile and it comes up unrestricted at its next restart. Replace a sibling's key and per-actor verify-on-fold in `mur-core::channel_verify` validates forged events, because it checks against the key that was just replaced. |
| `~/.local/bin` | `system_exec_paths()` (`policy.rs:715`) hardcodes `~/.local/bin` for **every** agent, and `mur-agent-runtime` plus the `mur_agent_*` symlinks live there. That binary is exec'd *before* it applies its own sandbox, so replacing it owns every agent on the machine. |
| `~/Library/LaunchAgents` | A dropped plist is loaded by launchd at next login, outside any sandbox. |

None of these require the agent to `exec` anything, so `spawn.mode: allowlist` does not constrain them. All three were accepted by `mur agent perm allow-write` without comment.

### Why "list the dangerous paths" is the wrong frame

The set of paths whose contents run code later is open-ended — `~/.zshenv`, `~/.config/autostart`, git hooks, login items, `at`/`cron`. A blocklist loses that race permanently.

The set that matters here is closed, because MUR owns it:

> **A sandbox that can be edited from inside it is not a boundary. It is a delay.**

The files that must be unwritable are exactly the inputs to the *next* trust decision: what gets started, and what it is allowed to do once started. MUR knows its own `mur_home`, its own `bin_dir`, and its own plist naming convention, so this set is derivable rather than guessed.

## The protected set

```
<mur_home>/agents/**                       except <mur_home>/agents/<self>/
<mur_home>/agents/<self>/profile.yaml      (existing SELF_PROTECTED, unchanged)
<mur_home>/agents/<self>/identity.key      (existing SELF_PROTECTED, unchanged)
<bin_dir>/mur-agent-runtime
<bin_dir>/mur_agent_*
~/Library/LaunchAgents                     macOS
~/Library/LaunchDaemons                    macOS
~/.config/systemd/user                     Linux
~/.config/autostart                        Linux
```

`<bin_dir>` is `MUR_AGENT_BIN_DIR` when set, else `~/.local/bin`.

**`identity.key` is protected against reads, not only writes.** Reading a sibling's signing key is as fatal as replacing it — the holder can forge that agent's channel events with no write at all, and verify-on-fold will accept them because the key on disk is untouched and still matches. This is the one member of the set where read protection carries the same weight as write protection, and it is also the one where macOS gives the least help: the SBPL baseline is `(allow default)` for reads, so the tool gate is the whole defence there (see *Deliberately not covered*).

**The whole of `~/.local/bin` is deliberately NOT protected.** An agent that drops a binary there and runs it is still inside its own sandbox — bounded, and no worse than the `bash` tool it already has. Only `mur-agent-runtime` and the `mur_agent_*` symlinks matter, because those are exec'd before any sandbox exists. Protecting the whole directory would break an agent installing a tool for itself while buying nothing.

## Enforcement

Four layers. They are not redundant: each catches a case the others structurally cannot.

### 1. Tool gate — `mur-agent-runtime/src/tools/fs_policy.rs`

Add `is_protected_launch_chain(canonical, mur_home, bin_dir, self_name) -> bool`, checked in `check_write_entitlement` **before** the existing `fs.deny` / `fs.write` list comparison, and unconditionally — no entitlement can satisfy it.

This must be a **predicate, not a path list**. `self_protected()` appends concrete paths to `fs.deny`, which cannot cover an agent directory that does not exist yet: an agent could create `<mur_home>/agents/newname/profile.yaml` precisely because that path was absent when the list was built. A predicate is closed under new agents.

This layer is also the only one that produces a legible error. The kernel returns a bare `EPERM`, which reads identically to "not granted", "granted but dropped", and "structurally forbidden".

### 2. macOS kernel — `sandbox/macos.rs`

`build_sbpl_profile` currently emits all allows, then all denies. Extend to a third tier, since SBPL is last-match-wins:

```
(allow file-write* (subpath "<mur_home>/agents/<self>"))    existing agent_home grant
(deny  file-write* (subpath "<mur_home>/agents"))           new
(allow file-write* (subpath "<mur_home>/agents/<self>"))    re-allow, must follow the deny
(deny  file-write* (subpath "<mur_home>/agents/<self>/profile.yaml"))   existing
(deny  file-write* (subpath "<mur_home>/agents/<self>/identity.key"))   existing
(deny  file-write* (subpath "<bin_dir>/mur-agent-runtime"))  new
(deny  file-write* (subpath "<autostart dir>"))              new, per entry
```

`mur_agent_*` symlinks are enumerated at seal time. A symlink created afterwards is not covered, which is acceptable: a new symlink only matters if something starts it, and that requires either a profile (denied) or a plist (denied) or a human.

Denying `file-write*` on a target covers unlink and rename-over, so delete-and-replace is closed.

### 3. Linux kernel — `sandbox/linux.rs`

**Landlock cannot express deny-within-allow.** `apply_linux` builds `path_beneath_rules(policy.fs_write, …)`, a pure allow-list; there is no deny rule to add. The carve-out in §2 is not expressible.

Therefore on Linux: at seal time, drop any `fs_write` entry that contains a protected path, and record it as a dropped grant. Fail-closed. Grant-time refusal (§4) makes this rare; when it does fire, `doctor` reports it rather than leaving the user to guess.

This is a real behavioural difference between platforms and must be documented as one, not papered over.

### 4. Grant time — `mur-core/src/cmd/agent/perm.rs`

`allow-write` refuses a path that intersects the protected set, naming which rule it hit. `allow-read` refuses a path that covers any `identity.key`, for the reason given above. Nothing reaches the profile.

Wire in the existing `is_overbroad_root` (`cmd/agent/cli/access.rs:46`), currently used only by the cwd-consent flow, so `/`, `$HOME`, and any path shallower than two normal components are refused here too. A write grant on `$HOME` is equivalent to no sandbox and should be as hard to obtain as it is to reason about.

`policy.rs:700 is_guarded_prefix` already encodes the same judgement for spawn prefixes. Three mechanisms expressing one idea should converge on one shared helper rather than drift.

## Migration

Existing profiles are **not rewritten**. An upgrade that silently edits entitlements is the same class of surprise this spec exists to remove.

`mur agent runtime-doctor` reports each grant that the protected set now neutralises:

```
mur: write grant has no effect: /Users/…/.mur/agents
     → protected launch chain (sibling profile.yaml / identity.key)
     → remove with: mur agent perm deny-path mur /Users/…/.mur/agents
```

The grant is inert either way; removing it is cosmetic and left to the user.

## Deliberately not covered

- **Code that runs later under the human's authority.** A write grant on `~` or a project directory still allows edits to `.zshrc`, git hooks, `Makefile`, or CI config. Launch-chain protection does not address this and must not be described as if it does. The mitigations are the overbroad-root refusal in §4 and documentation — not a larger blocklist.
- **Reads on macOS.** The SBPL baseline is `(allow default)` with a deny-all only for writes (`macos.rs:161-165`; a full `(deny default)` would break dyld). Read protection on macOS is the tool gate alone. Linux Landlock does enforce reads.
- **Agent creation.** Blocked by this spec, restored by Spec 2.

## Testing

Each claim above gets a test that fails if the claim stops holding.

| Test | Asserts |
|---|---|
| `protected_predicate_covers_sibling_agents` | a path under `agents/<other>/` is protected; the same path under `agents/<self>/` is not |
| `protected_predicate_covers_agents_created_after_seal` | a sibling directory that did not exist when the policy was built is still protected — the regression a path list would miss |
| `protected_predicate_spares_the_rest_of_bin_dir` | `<bin_dir>/mur-agent-runtime` protected, `<bin_dir>/some-tool` not |
| `sibling_identity_key_is_read_protected` | a read of `agents/<other>/identity.key` is refused by the tool gate even when a read grant covers the directory |
| `sbpl_reallows_self_after_agents_deny` | in the emitted profile, the `agents/<self>` allow appears **after** the `agents` deny, and the self profile/key denies after that — ordering is the mechanism |
| `linux_drops_write_grant_containing_protected_path` | the entry is absent from `fs_write` and present in the dropped-grant report |
| `perm_refuses_protected_and_overbroad_paths` | exec test against the real `mur` binary, with a negative control showing a neighbouring unprotected path still succeeds |
| `doctor_reports_neutralised_grants` | a profile holding all three real-world grants produces three findings |

The negative controls are not optional. A protection test that passes because the code path never ran looks identical to one that passes because the protection works.

## Follow-on (not scheduled here)

Verify `mur-agent-runtime`'s Developer ID signature before exec. That catches a swapped binary regardless of how it was swapped, and so covers the case where the protected set is bypassed by something outside MUR entirely.
