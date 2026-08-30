# Agent Permission Truthfulness

Status: design (2026-08-30). Repo: `mur`. Subproject A of two — see
[Out of scope](#out-of-scope) for B.

## Problem

An agent is denied a filesystem write and nothing anywhere can tell the user
why. From a live session:

```
you › git commit and push
agent › .git 是唯讀的:不能建立 '.git/index.lock':Operation not permitted,
        沙盒沒給我寫入權 —— 三個 commit 得由你執行:
```

The agent read the diff, wrote the commit message, and handed the work back.
It could not say which path was missing, which command would grant it, or
whether a grant already existed but had not taken effect. Neither could the
user: there is no way to ask what an agent may touch.

## Three states, and only one of them is visible

The symptoms all follow from one gap. There are three distinct facts, and MUR
presents the first while enforcing the third.

| | what it is | where it lives | observable today |
|---|---|---|---|
| **granted** | what the user asked for | `profile.yaml` `entitlements.filesystem` | yes |
| **admitted** | what the policy layer accepted after discarding grants it cannot honour | `SandboxPolicy::from_entitlements`, in memory | **no** |
| **enforced** | what the kernel actually received, and whether `sandbox_init` succeeded | SBPL text passed to `sandbox_init` | **no** |

`granted` is editable at any time. `admitted` and `enforced` are fixed for the
lifetime of the process: a seatbelt profile **cannot be widened after it is
sealed**. That is a platform property, not a limitation to route around.

### admitted ≠ granted

`SandboxPolicy::from_entitlements` drops any entitlement path that does not
exist when the profile is sealed — a dead grant destabilises unrelated write
checks (Issue 16). The drop is logged and nothing else:

```
WARN filesystem write entitlement path does not exist on disk; agent will NOT
     have write access to it (dropping dead grant to avoid destabilizing the
     sandbox profile — Issue 16) path=/Volumes/Firecuda4tb/Projects/cc-proxy
```

The grant stays in `profile.yaml`. So the configuration says the agent has
write access to `cc-proxy` and the kernel says it does not, permanently and
invisibly. This is live on the author's machine today.

### enforced ≠ admitted

`supervisor.rs:389-421` distinguishes three outcomes and surfaces none of them
outside the log:

```
399  B1 sandbox applied but not enforcing on this platform
410  B1 sandbox applied                                      ← enforcing
416  B1 sandbox::apply failed and fail_closed_on_sandbox_error=true  ← aborts
421  B1 sandbox::apply failed; running advisory-only (B0 remains active)
```

Line 421 is the one that matters: the kernel sandbox is not installed at all,
only the advisory B0 hooks remain, and the agent therefore has **more** access
than `granted` — the opposite of every other failure here. Nothing reports it.

## What already works — do not rebuild this

The *set* side is in good shape, and a fix that duplicates it is wasted work.

- **`perm.rs:29`** warns when the target agent is running:
  `warning: '<name>' is running; restart required for changes to take effect`,
  followed by the exact `mur agent restart` command.
- **`reject_dead_grant` (`perm.rs:298`)** refuses a grant whose path does not
  exist *at grant time*, with a `mkdir -p` suggestion. Its doc comment already
  names this exact failure: "the CLI reports success, the profile lists the
  path, `restart` says it applied, and the kernel still returns EPERM with
  nothing in between explaining why."
- `mur agent perm` already splits read from write (`allow-read` / `allow-write`
  / `deny-path`), and covers hosts, spawn, limits and tool policy.

The gaps are entirely on the **observe** side and at the **denial** site:

| gap | why the set side cannot close it |
|---|---|
| no way to ask what is effective now | `list-hosts` and `tool-list` exist; the filesystem half has no counterpart |
| a path that existed at grant time and vanished later | `reject_dead_grant` runs at grant time; `cc-proxy` is exactly this case |
| the denial explains nothing | the EPERM is returned to a child process, not to MUR |

## Design

### C1 — record the seal in `running.lock`

At the point the lock is written, add a `sandbox` block:

```json
"sandbox": {
  "enforcing": true,
  "mode": "macos-sbpl",
  "granted_digest": "sha256:…",
  "admitted": { "read": [...], "write": [...], "deny": [...] },
  "dropped": [
    {"path": "/Volumes/Firecuda4tb/Projects/cc-proxy", "verb": "write",
     "reason": "missing at seal"}
  ]
}
```

**Why `running.lock` and not a new file.** The lock is written once at startup
by the same process, is keyed to the pid, dies with the process, and already
carries derived truth of exactly this kind (`build_sha`, `card_digest`). The
effective entitlements have the same lifecycle as the lock — written once,
never updated, valid exactly as long as the process. A second file would be a
second lifecycle to keep correct, for one fact.

**Verified safe to extend.** `LockFile` already uses `#[serde(default)]` for
added fields (`build_sha`, `proto_version`). There is exactly one production
writer — `supervisor.rs:688`, with the complete struct — and no
read-modify-write anywhere, so the narrow-DTO field erasure of #957 cannot
occur here. The only other write to the path is `fs::write(&lock_path, b"{}")`
inside `spawn_bridge_for_test_with_id`, a test helper.

**Ordering is already correct.** `sandbox::apply` runs at `supervisor.rs:389`;
the lock is written at `supervisor.rs:688`. Every field above — including
`enforcing`, which is only known after the apply returns — is available at
write time. No reordering required.

`granted_digest` is a digest of the entitlements the profile was built from. It
exists so any surface can derive "grants were added after this agent started —
restart to apply" by comparing it against `profile.yaml` on disk. Derived, not
tracked: the same principle as the skills fingerprint, which is computed from
disk rather than bumped by writers precisely so no writer can forget.

### C2 — `mur agent perm list-paths`

Two columns, granted against effective, one line per path:

```
WRITE
  /Volumes/…/mur                     effective
  /Volumes/…/cc-proxy                dropped   path missing at seal
  /Volumes/…/new-project             pending   added after start — restart to apply

READ
  …

sandbox: enforcing (macos-sbpl), sealed 2026-08-30 10:05:38
```

`dropped` comes from C1's record. `pending` is derived from `granted_digest`.
When `enforcing` is false the header says so first, because it subsumes
everything below it.

### C3 — the denial explains itself

**This is not "improve our error message."** The `EPERM` in the transcript above
was returned to `git`, not to MUR; MUR only ever saw git's stderr. There is no
syscall to intercept.

So: when a `bash` tool call fails and its stderr matches the
`Operation not permitted` shape with a path, classify that path against the
in-memory policy and append one advisory paragraph — not gate, not retry, not
rewrite the command. Three classifications, all already known in-process:

| case | appended text |
|---|---|
| path not in the write grants | names `mur agent perm allow-write <path>` and the restart |
| granted but dropped at seal | says the grant exists and why it was discarded |
| granted after the seal | says a restart is required, names the command |

**Phrase it as fact, not cause.** "This path is not in the agent's write
grants" is checkable; "that is why the command failed" is a claim that a
read-only mount would falsify. Same discipline as the time-bound hint in
`open_item`: narrow detector, advisory output, a control test pinning that
ordinary failures collect nothing.

C3 depends on C1's *data*, which is in memory, but not on C1's *file*. **It can
ship on its own, and it is the only component that helps at the moment the user
was actually in.**

### C4 — `mur agent doctor` findings

Doctor is already the offline audit surface. Two findings, from data C1 makes
available:

- a granted path that does not exist on disk → will be dropped at the next seal
  (answers "what will break when this agent restarts", without it running)
- a running agent whose `granted_digest` no longer matches `profile.yaml` →
  pending grants, restart to apply

Warn-only, exit code unchanged — matching `mur model doctor`, which is
deliberately advisory because a gate for something the user cannot immediately
fix gets switched off.

### Order

**C3 → C1 → C2 → C4.** C3 first because it delivers the transcript's fix alone.
C1 then unlocks C2 and C4, which are small on top of it.

## Rejected

**Recompute the effective set on demand instead of recording it (C1's obvious
alternative).** `list-paths` would re-run `from_entitlements` against the
current `profile.yaml`. It is cheaper and always fresh, and it **reproduces the
bug inside the diagnostic**: it reports what *would* be sealed now, not what
*was* sealed. On the author's machine that means reporting the 10:26 grants as
effective while the process sealed at 10:05 denies them. Recorded here because
it is the intuitive answer and will otherwise be proposed again.

**A new `effective-entitlements.json`.** Two files, one fact, two lifecycles.
See C1.

**Hot-apply a grant to a running agent.** A seatbelt profile cannot be widened
after `sandbox_init`. Not a design choice.

**Watch `profile.yaml` and restart the agent on change.** Restarting an agent
is a spend-and-surprise action; unattended automatic restarts are the class of
behaviour this project keeps deliberately opt-in and off by default.

**An A2A `entitlements/get` method.** More authoritative in principle, but C1's
record *is* the process's self-report, written to disk instead of answered over
a socket — and it can answer for an agent that has since stopped, which a dial
cannot. Worth adding later for live introspection; not needed for any component
here.

## Testing

- The drop itself is already covered
  (`policy.rs:1409 from_entitlements_drops_dead_read_write_but_keeps_dead_deny`
  — note that a dead *deny* is deliberately kept: denying a path that does not
  exist is harmless and survives the path appearing later). What is new is the
  pairing: the drop must also appear in the recorded `dropped` list with its
  reason. Testing the drop alone passes today and proves nothing about C1.
- A lock written with the new block round-trips through `LockFile` unchanged,
  and a lock **without** it (an agent from before this change) still
  deserialises — `#[serde(default)]` is load-bearing.
- `pending` is derived, not stored: mutating `profile.yaml` after the lock is
  written flips the derived state with no writer involved.
- `enforcing: false` reaches `list-paths` and doctor. This is the state with no
  natural test fixture; it needs an injected apply failure.
- C3's classifier: the transcript's own stderr string classifies correctly, and
  a control set of ordinary command failures (exit 1, not found, test failure)
  classifies as nothing at all.

Every test above should be seen to fail before it is trusted — the invariants
here are all of the "silently does nothing" kind, which is exactly the shape
that passes for the wrong reason.

## Out of scope

**Subproject B — somewhere to see and set this.** A Hub permissions page, and
the network half of the same question. B is deliberately second: it is a
display layer, and building it on a source that cannot distinguish granted from
effective would render the `cc-proxy` grant as "granted" in a nicer font.

**Issue #1085** — bridge liveness is classified from a `running.lock` mtime that
nothing refreshes. Found while auditing the lock for C1, unrelated to it, and
fixed separately. It does constrain C1 in one way: whatever #1085 chooses must
not introduce a second writer to `running.lock` without revisiting C1's
"exactly one production writer" premise.

## Context

- `mur-agent-runtime/src/sandbox/policy.rs` — `from_entitlements`, the drop
- `mur-agent-runtime/src/sandbox/macos.rs` — SBPL generation; baseline is
  `(deny file-write* (subpath "/"))` with recursive `subpath` allows
- `mur-agent-runtime/src/supervisor.rs:389` — the seal; `:688` — the lock write
- `mur-common/src/agent.rs:1156` — `LockFile`
- `mur-core/src/cmd/agent/perm.rs` — the set side, already correct
- `mem:project_agent_network_sandbox_long_term` — the network half, for B
