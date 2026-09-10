# `rust-build` capability — making "this agent can compile" one installable unit

Status: **Designed, not implemented.** Written 2026-09-10 after assembling the
grant set by hand on a live machine and getting it wrong twice.

## Problem

Letting an agent build a Rust project takes five grants that must all be
right, and no surface tells the user what the set is. Assembling it by hand on
one machine produced three separate failures, in this order, each with the
same error text:

1. **`cargo` granted as an absolute path.** `~/.cargo/bin/cargo` is a rustup
   proxy; the kernel checks the toolchain binary it re-execs. `policy.rs:452`
   scans every toolchain's `bin/` — but only for entries given as a **bare
   name**. An absolute path takes the no-search branch and grants one file.
2. **No build lane.** Build scripts, proc-macro shims and test binaries are
   exec'd from hash-suffixed paths under `target/` that do not exist until the
   build creates them. `perm allow-spawn-dir <agent> <project>/target`.
3. **No linker.** `/usr/bin/cc` is exempt as a system path and execs the
   active toolchain's `clang`, which is in no grant. Only visible at link
   time — a build gets all the way through compilation first.

Every one of them surfaces as `Operation not permitted`. A user who fixes one
sees the same message again and reasonably concludes the fix did not work.

The unit the user actually wants is not five grants. It is "this agent can
build this project".

## Non-goals

- A general toolchain-capability abstraction. Rust first; generalise when a
  second instance exists and shows what varies.
- A separate consent path. Capability install already asks; reuse it.
- Hub UI. That is a surface on top of this, not an alternative to it.
- Multi-project in one install. `--project` takes one path; grants union, so
  installing twice for two projects is already additive.

## Approach

**Chosen: extend the existing `Capability`.** `mur-common/src/capability.rs`
already carries MCP servers, skill refs, program dependencies and suggested
entitlements, and `mur-core/src/cmd/capability.rs:27` already unions those
entitlements into a profile after consent. What is missing is three specific
things, below — not a new container.

Rejected: **a dedicated `mur agent build-lane` command.** Smaller (no schema
change), but "give this agent an ability" already has a container, and a
second door means two things to learn and a second place for the next
toolchain to be bolted on.

Rejected: **Hub-only.** Best experience, leaves CLI users out, and still needs
every mechanism below underneath it.

## Design

### 1. `CapabilityEntitlements` cannot express a build lane

It carries `spawn_programs`, `network_hosts`, `filesystem_read`
(`capability.rs:24`). Add two:

```rust
    /// Directories under which any executable may be spawned — the build
    /// lane. A toolchain that compiles and then runs its own output cannot be
    /// expressed as a list of binaries.
    #[serde(default)]
    pub spawn_dirs: Vec<String>,
    #[serde(default)]
    pub filesystem_write: Vec<String>,
```

Both union into the profile the same way the existing three do
(`capability.rs` `union_extend`). Without `spawn_dirs` no compiling capability
is expressible at all; without `filesystem_write` the agent can read the
project but not write `target/`.

### 2. Two of the five grants are not fixed strings

The linker path is machine-specific and the build lane is project-specific, so
the manifest holds placeholders resolved at install:

| Placeholder | Resolved from |
|---|---|
| `${project}` | the `--project` argument, canonicalised |
| `${cc}` | the active toolchain's `clang`, via `/var/db/xcode_select_link`, falling back to `/Library/Developer/CommandLineTools/usr/bin` |

`${cc}` **must** be resolved by the same code `policy.rs:441-450` already uses
to build `spawn_search_dirs`. Two resolvers that answer differently is how a
grant and the exec it is supposed to authorise drift apart — the failure that
produced this spec. Extract that resolution into one function and call it from
both.

The manifest is then a fixed shape:

```yaml
name: rust-build
entitlements:
  spawn_programs: [cargo, rustc, "${cc}"]   # bare names for the rustup proxies
  spawn_dirs: ["${project}/target"]
  filesystem_read: ["${project}"]
  filesystem_write: ["${project}"]
```

`cargo` and `rustc` stay bare on purpose: that is the only form that reaches
the rustup toolchain scan. `${cc}` resolves to an absolute path because clang
is not a proxy and no scan applies.

### 3. Volume pre-check before writing anything

If the resolved `${project}` or `${cc}` appears in the agent's `unreachable`
record (spec `2026-09-10-unreachable-grants-design.md`), installation stops
and prints that record's guidance. Granting five entitlements for a tree the
process cannot open produces exactly the "everything says ✓ and nothing works"
state this capability exists to prevent.

This consumes the unreachable record; it does not re-probe. A probe run by the
CLI would answer for the user's shell, which has access — the same mistake
`doctor::removable_volume_hint` makes today.

### 4. The canary build runs INSIDE the agent

After install, dial the running agent and have it build the workspace's
smallest crate in the user's own project.

**It cannot run in the CLI process.** The CLI has the user's access, so a
successful compile there proves nothing about the agent. This is not a
theoretical concern: it is the single reason the pre-existing removable-volume
check has never fired, and it is the mistake that cost a full session before
this spec was written.

Three outcomes, three distinct messages:

| Outcome | What is said |
|---|---|
| build succeeds | verified — the capability works, with the crate and duration |
| exec denied | the missing grant, named — this is `spawn_denied_path`'s hint (spec `2026-09-10-spawn-denial-attribution-design.md`) |
| agent not running, or its `bash` tool is gated | **"installed, NOT verified"**, naming which of the two blocked it |

The third row is not an edge case. An agent whose tool policy lacks
`allow bash` parks the call at a HITL gate that no one is present to answer,
and the call times out. Reporting that as success would make the capability a
liar in exactly the situation it was built for.

### 5. Testing

| Test | Guards against |
|---|---|
| `${cc}` resolves on a machine with Xcode selected | the common path |
| `${cc}` resolves with only Command Line Tools installed | the machine without Xcode |
| `${cc}` unresolvable → install refuses, naming what is missing | silently granting a placeholder |
| `spawn_dirs` reaches `profile.entitlements.processes.spawn.allowed_dirs` | the field existing but not being wired into the union |
| canary success → "verified" | — |
| canary exec-denied → names the missing grant | the message that sends the user to the right fix |
| agent not running / bash gated → says NOT verified | the liar case above |
| manifest with `${cc}` deliberately omitted → canary FAILS | a canary that passes when the grant set is incomplete proves nothing |

The last row is the one that matters. A canary that cannot fail is decoration.

## Sequencing

`2026-09-10-unreachable-grants-design.md` first — §3 reads its record.
`2026-09-10-spawn-denial-attribution-design.md` supplies §4's middle row.
Neither blocks a first cut: without them, §3 skips its pre-check and §4's
exec-denied message is less specific.
