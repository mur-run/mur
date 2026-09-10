# Unreachable grants — when the sandbox installed a grant the process still cannot use

Status: **Designed, not implemented.** Written 2026-09-10 from a live failure
in which every permission surface reported success while the agent could not
read a single file in the granted tree.

## Problem

`mur agent perm list-paths qa` printed:

```
READ
  ✓ /Volumes/Firecuda4tb/Projects/mur
WRITE
  ✓ /Volumes/Firecuda4tb/Projects/mur
```

At the same moment, inside that agent:

```
ls:   /Volumes/Firecuda4tb/Projects/mur: Operation not permitted
head: /Volumes/Firecuda4tb/Projects/mur/Cargo.toml: Operation not permitted
```

Both statements are true. MUR did install the grant; the kernel accepted it;
`SandboxRecord.dropped` is legitimately empty. The access is refused one layer
further out — macOS gates the external volume itself, and the agent process
has no grant for it. `/Volumes` enumerates fine (it is on the system disk);
everything under `/Volumes/Firecuda4tb` is EPERM. `~/.mur` is unaffected
because it is a real directory on the internal disk; only `.cargo`, `.rustup`
and `Projects` are symlinked onto the external volume — which is also why
`cargo` itself could not be exec'd, since it lives there too.

The sandbox was ruled out directly: MUR's profile was reproduced with
`sandbox-exec` — `(allow default)`, the write-deny baseline, and all fourteen
read denies — and nothing broke. The difference is the process, not the policy.

The cost of the false ✓ is that it sends the reader somewhere else. In the
session that produced this spec it produced a confident diagnosis of the spawn
allowlist, a config change, a restart, and a committed design — all for a
mechanism that was not the cause.

### The check that exists and cannot fire

`mur-core/src/cmd/agent/doctor.rs:120` already carries
`removable_volume_hint()`, its `probe_volumes()` helper, three unit tests, and
a written user-facing string (`mur_common::REMOVABLE_VOLUME_EPERM_HINT`). It
has never fired, for two independent reasons:

1. **It probes the wrong path.** It reads `/Volumes`, which is on the system
   disk and readable. The denial begins at `/Volumes/<volume>`.
2. **It probes from the wrong process.** It runs inside `cmd_doctor` — the
   user's own shell, which has volume access. The process whose access matters
   is the agent.

This change does not add a mechanism. It connects the existing one to a
truthful source.

## Approach

Considered three sources of truth for "can this agent actually reach this
grant":

**A. The runtime probes itself at seal time and records the answer in
`running.lock`.** (Chosen.) Only the agent's own process credentials decide the
answer, so the agent is the only honest witness. One record, read by every
surface. The obvious objection — a lock written at startup goes stale — does
not apply: granting volume access requires restarting the agent anyway, so a
seal-time record is exactly as fresh as the situation permits.

**B. Probe on demand over A2A.** Fresher and needs no schema change, but only
answers while the agent is running and reachable — and half of what `doctor`
exists for is agents that are not. It also makes every surface a dialer.

**C. Infer CLI-side from the mount point.** No probe: warn when a grant lives
on a non-system volume. Cheapest, and wrong — it cannot distinguish "external
volume" from "external volume you already authorised", so it fires on healthy
machines. This is the machine version of the misdiagnosis that produced this
spec.

## Design

### 1. Data contract

`SandboxRecord` gains one field, reusing the existing `DroppedGrant` shape:

```rust
/// Grants the kernel accepted but the process cannot actually reach
/// (e.g. macOS gating an external volume). Distinct from `dropped`,
/// which never reached the kernel at all.
#[serde(default)]
pub unreachable: Vec<DroppedGrant>,
```

`#[serde(default)]` keeps existing locks deserialisable. It is a separate
field, not an addition to `dropped`: that field's contract is "did not reach
the kernel", and widening it would make the one honest signal ambiguous.

### 2. The probe

**When:** after `sandbox::apply()` returns and before the agent reports ready.
The seal is in force by then, so what the probe observes is what the agent can
actually do.

**What:** one probe per `fs_read` / `fs_write` grant root — `read_dir` for a
directory, a metadata read for a file. Grant lists carry both (several agents
grant `~/.mur/config.yaml` by name), and `read_dir` on a file returns ENOTDIR,
which would silently classify a genuinely unreachable file as healthy.

**Classification:** only EPERM (`os error 1`) marks a grant unreachable.
ENOENT is a dead grant and already belongs to `doctor::dead_grants`. Every
other errno is left alone — the same discipline `removable_volume_hint`
already documents ("we must not hijack unrelated errors").

**Cost:** one `read_dir` per grant, once per start.

**Failure is not fatal.** This is diagnostic information, not a security
boundary. An agent with unreachable grants must still start — otherwise
nothing is left running to report that it is crippled.

### 3. Surfaces

**`mur agent perm list-paths`** — a grant listed in `unreachable` prints `✗`
with the hint instead of `✓`. This is the defect being fixed.

**`mur agent doctor`** — delete `probe_volumes` and the
`removable_volume_hint(probe_volumes)` call site; read the lock's `unreachable`
instead. The hint constant stays; only its source changes.

**Hub** — same lock, rendered as a banner, with a button opening
`x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`.
macOS allows navigating the user to that pane and nothing more: the copy must
not imply the toggle will be flipped for them.

**`mur agent create`** — prints one line stating that volume access is verified
on first start. It does **not** probe: `cmd_create` does not start the agent
today, and making it do so is a larger behavioural change than this feature
warrants (service installation, unexpected processes). The intent of an early
warning is met instead by making the first start loud: when the probe finds
anything unreachable, the runtime emits a warning naming the paths and the
hint, rather than burying it in the lock.

### 4. The hint string is wrong for launchd-started agents

Current text (`mur-common/src/removable_volume.rs:15`) tells the user to grant
access to "MUR Hub (or the app that launched this agent)". An agent started by
launchd has no such app; the binary needing the grant is
`mur-agent-runtime` itself, which every per-agent symlink resolves to. The
string must name the binary, and keep the Hub case for Hub-launched agents.

## Testing

| Test | Guards against |
|---|---|
| lock lists a grant in `unreachable` → `list-paths` prints ✗ and the hint | the false ✓, the defect itself |
| lock lists none → prints ✓ | blanket false alarms |
| probe: EPERM → recorded; ENOENT → not recorded; other errno → not recorded | misclassification; preserves the existing discipline |
| a file grant (not a directory) that is unreachable is recorded | `read_dir` on a file returns ENOTDIR and would read as healthy |
| a lock without the field deserialises and prints ✓ | migration safety |
| `doctor` no longer calls `probe_volumes` | regressing to probing the wrong path from the wrong process |

The first and last are the ones that fail if this is implemented shallowly.

## Related

`2026-09-10-spawn-denial-attribution-design.md` came out of the same incident
and shares its shape: a denial the agent cannot explain. That change makes an
exec denial name its own grant; this one makes a filesystem grant admit it is
not usable. Neither subsumes the other — an agent can hold a perfectly
reachable tree and still be denied a binary, and vice versa.
