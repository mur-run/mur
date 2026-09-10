# Spawn-denial attribution — making a kernel EPERM say what to do about it

Status: **Designed, not implemented.** Written 2026-09-10 from a live failure
where a correctly-intentioned permission grant took a whole session to
diagnose. The record of what was rejected is part of the point.

## Problem

An agent was granted `cargo` through the Hub permission UI. It still could not
build. Every observable surface said the grant was fine, and the only signal
the agent could produce was:

```
cargo: Operation not permitted (os error 1)
```

That string names no policy, no grant, and no fix. The agent reported it
faithfully — "此 sandbox 禁止執行 Cargo" — and handed the work back to the
user, which is the one outcome delegation exists to avoid. Three separate
turns in that conversation ended that way.

Two independent defects produced it.

**1. The grant was the wrong shape, and nothing said so.** `~/.cargo/bin/cargo`
is a rustup *proxy*: the binary the kernel actually checks at exec time is
`<rustup_home>/toolchains/<tc>/bin/cargo`. `sandbox/policy.rs:452-468` knows
this and scans every toolchain's `bin/` — but only for allowlist entries given
as a **bare name**. An entry containing `/` takes the no-search branch
(`policy.rs:500`), granting exactly one file and deriving one prefix
(`.cargo`), so `.rustup/toolchains/**` is never granted. The Hub UI offers a
free-text "Add program…" field, so an absolute path is the natural thing for a
user to type, and it is the one shape that disables the only code path that
understands rustup.

**2. The attribution mechanism exists and did not fire.**
`tools/denial.rs:144` `spawn_denied_path()` turns exactly this class of EPERM
into a route — its own test fixture is `/Users/d/.cargo/bin/cargo`. It
requires exit code `126` **and** a stderr line matching bash's own
`<absolute path>: Operation not permitted`. The observed denial matched
neither: rustup exits `1`, reports the argv0 as typed (`cargo`, no leading
`/`), and appends `(os error 1)` because it is a Rust `io::Error` Display, not
a bash message. The denial happened one exec level below bash, so bash never
described it.

The mechanism only recognises denials it produced itself.

## Correction (same day, after the design was approved)

The opening evidence is a misreading, and the design has to survive it.

`cargo: Operation not permitted (os error 1)` was almost certainly **not** a
spawn-allowlist denial. Later probing established that the agent has no access
to the external volume at all — it cannot read a byte of the granted project
tree, and `~/.cargo/bin` is on that volume too, so the binary could not be
exec'd for a reason that has nothing to do with `spawn.allowed`. See
`2026-09-10-unreachable-grants-design.md`.

What survives unchanged:

- The resolver asymmetry is real code behaviour. An allowlist entry containing
  `/` still skips the rustup-toolchain scan (`policy.rs:500`), and a user
  typing an absolute path into Hub's free-text field still gets a grant that
  cannot cover a proxy's re-exec.
- The attribution gap is real. `spawn_denied_path()` still recognises only the
  denial shape bash produces itself.

What this changes:

- The **authoritative verdict** (component 3) moves from a nicety to the thing
  that keeps this change from making the problem worse. Under the rejected
  "widen the string match only" alternative, this exact string would have been
  reported as "cargo is not in your allowlist" — a confident, wrong answer that
  sends the user to edit a grant that was never the problem. That is precisely
  what happened to the human and the model in the originating session.
- The **third outcome** is now mandatory, not optional. When the trigger fires
  and every spawn candidate resolves as granted, the hint must not fall silent:
  that combination means the denial is a filesystem or volume denial, and the
  hint should say so and point at the unreachable-grants record rather than
  leaving the reader where they started.

### The real spawn denial did surface — one layer later

After the volume access was granted, `cargo test` in the same agent ran,
compiled, and died at linking:

```
cc: error: can't exec '/Volumes/.../Xcode.app/Contents/Developer/Toolchains/\
XcodeDefault.xctoolchain/usr/bin/clang' (errno=Operation not permitted)
```

That one *is* a spawn-allowlist denial: `/usr/bin/cc` is exempt as a system
path, execs the active toolchain's `clang`, and that path was in no grant.
Granting it (the shape `rustsmith` already carried) made the build pass —
881 tests, exit 0. So the premise this spec was written on is sound; it was
attached to the wrong observation.

It also produces a **third error shape**, and it breaks the current detector
in a way neither earlier shape does:

| Source | Exit | Text |
|---|---|---|
| bash, exec denied | 126 | `bash: /path/to/bin: Operation not permitted` |
| rustup proxy | 1 | `cargo: Operation not permitted (os error 1)` |
| cc → clang | 101 | `cc: error: can't exec '/path/to/clang' (errno=Operation not permitted)` |

The third one puts the path **in single quotes in the middle of the line**,
not as a `<path>:` prefix. `spawn_denied_path()` extracts its path by
stripping a `": Operation not permitted"` suffix and taking what precedes it,
which yields nothing here.

So token extraction cannot be a suffix strip against one known layout. The
trigger collects **candidates** from the matching line — quoted runs, and
whitespace-delimited words that look like a path or a bare program name — and
the verdict tests each against the sealed grants. Guessing which shape the
next tool will use is how the current detector ended up recognising only its
own; enumerating candidates and asking the policy does not have that failure
mode.

## Non-goals

Deliberately out of scope; each is its own change:

- Hub permission panels (restart-required banner, dropped grants, grant
  preview).
- A spawn-side counterpart to `perm list-paths` — the reconciliation of
  declared grants against what the seal installed.
- Toolchain presets replacing the free-text "Add program…" field.
- Auto-granting on denial. Fail-open defeats the entire model.
- Auto-restarting an agent when its permissions change.

## Design

Three components, all in `mur-agent-runtime`.

### 1. `resolve_spawn_candidates(name) -> Vec<PathBuf>`

Extracted from `sandbox/policy.rs:452-520` without behaviour change. Resolves
one allowlist entry to **every** absolute path it may exec to, including each
rustup toolchain's `bin/`. Two callers: policy construction (as today) and
denial attribution (new).

This is the shared floor the deferred spawn-reconciliation work also needs.
Extracting it here means that change is a caller, not a re-implementation.

### 2. `SEALED_SPAWN: OnceLock<SpawnGrants>`

`sandbox/mod.rs::apply()` builds a `SandboxPolicy`, hands it to the kernel, and
drops it — only `SandboxStatus` survives, in the existing `SANDBOX_STATUS`
`OnceLock`. Store the spawn half alongside it: `spawn_allowed_paths`,
`spawn_allowed_prefixes`, `spawn_mode`.

This is the only non-lying source of truth for the question "was this binary
granted?":

- The profile on disk may have been edited after the seal. That is exactly
  what happened here — the grant was written 11 minutes after the running
  process sealed its sandbox — so a disk-based check would have answered
  "granted" about a policy the kernel never received.
- `SandboxRecord.granted_digest` (`mur-common/src/agent.rs:1294`) is a digest
  of `entitlements.filesystem` **only**. Spawn grants are not in it. (This
  also means editing spawn grants does not set `grants_drifted`, so the
  restart-required signal is blind to this whole class of edit. Noted here;
  fixing it belongs to the Hub-panel change.)

### 3. Split `spawn_denied_path()` into trigger and verdict

**Trigger (wide).** Any non-zero exit whose stderr contains
`Operation not permitted`. Drop both the `126` requirement and the
leading-`/` requirement.

**Verdict (authoritative).** Resolve the extracted token through
`resolve_spawn_candidates()` and compare every candidate against
`SEALED_SPAWN`. Emit the hint only if at least one candidate is ungranted.

Making the verdict authoritative is what lets the trigger be generous: a false
trigger costs one lookup that answers "granted", and produces nothing.

Checking every candidate — not just the first — is load-bearing. In the
observed case `~/.cargo/bin/cargo` **is** granted; stopping there returns
"allowed" and silently emits nothing, which is worse than the status quo
because it looks like a considered answer.

### Data flow

```
rustup proxy execs toolchain cargo  →  kernel EPERM
  → "cargo: Operation not permitted (os error 1)"
  → trigger: contains "Operation not permitted" ✓
  → token "cargo" → resolve_spawn_candidates()
       ~/.cargo/bin/cargo                        → in SEALED_SPAWN ✓
       ~/.rustup/toolchains/stable-*/bin/cargo   → absent ✗
       ~/.rustup/toolchains/1.98.0-*/bin/cargo   → absent ✗
  → hint, naming the layer that was blocked rather than the layer that was granted
```

### Hint text

```
[blocked by sandbox]
  cargo resolved to /Users/…/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo,
  which is not in this agent's program allowlist.
  (/Users/…/.cargo/bin/cargo IS granted, but it is a rustup proxy — the binary
   it re-execs is the one the kernel checks.)
  Fix (must be run by the user, not by this agent):
    mur agent perm allow-spawn <agent> cargo
    mur agent perm allow-spawn-dir <agent> <project>/target
    mur agent restart <agent>
```

The existing `who_can_exec` delegation routes are kept and appended. They
answer a different question ("who else could run this for me") and remain
useful; they are simply not an answer for a Hub user with no fleets.

Both grant lines appear together because a Rust build needs both: `cargo` and
`rustc` as bare names, and the project's `target/` as a build lane, since build
scripts, proc-macro shims and test binaries are exec'd from hash-suffixed paths
that do not exist until the build creates them.

## Self-widening guard

Printing a runnable fix into tool output teaches the agent a command that
widens its own permissions. The escalation path predates this change — an
agent holding both `mur` in its spawn allowlist and write access to
`~/.mur/agents` can already run `mur agent perm allow-spawn <self> …` and wait
for a restart; the observed agent holds both — but making it discoverable
changes its risk, so this change carries the guard rather than deferring it.

`mur agent perm`'s mutating subcommands refuse when `MUR_AGENT_NAME` is set,
printing "this must be run by the user" instead.

**Honest limit:** this raises the bar, it does not close the hole. An agent
can `env -u MUR_AGENT_NAME`. Closing it means not granting one agent both
`mur` execution and `~/.mur/agents` write — a `mur agent doctor` check, out of
scope here, and named as a follow-up rather than implied to be solved.

## Rejected alternatives

**Widen the string match only.** One function, no new state — but it makes the
tool assert a sandbox denial from another program's error prose. A program
hitting EPERM on a file write would be reported as an ungranted binary,
sending the user to fix the wrong thing.

**Ask the kernel (macOS unified log Sandbox events).** Authoritative, but the
sandboxed process likely cannot read the log, it is macOS-only, and it races
the tool's own return.

**Pre-flight the command string.** Scanning for program names before exec
cannot see rustup's second-level exec, which is the whole failure. Viable
later as a complement, never as the mechanism.

## Testing

| Test | Guards against |
|---|---|
| `cargo: Operation not permitted (os error 1)` at exit 1 → hint naming the toolchain path | the regression itself |
| `~/.cargo/bin/cargo` granted, toolchain path not → hint **still** emitted | the silent wrong answer from stopping at the first candidate |
| every candidate present in `SEALED_SPAWN` → **no** hint | false positives from the widened trigger |
| `bash: ./x: Permission denied` (genuinely non-executable) → no hint | the widened trigger breaking the existing negative case |
| `cc: error: can't exec '/x/clang' (errno=Operation not permitted)` at exit 101 → hint naming `/x/clang` | the third shape: quoted path mid-line, no 126, no `<path>:` prefix |
| the same line with `/x/clang` granted → no hint | candidate extraction must not fire on a path the seal actually holds |
| `MUR_AGENT_NAME` set → `perm allow-spawn` refuses and the profile is unchanged | a guard that prints but does not actually block the write |

The first two are the ones that fail if the design is implemented shallowly.
