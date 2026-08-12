# Binary Attestation Design

**Status:** Draft
**Date:** 2026-08-12
**Scope:** Follow-on to launch-chain protection (#924, merged `7e7dbbac`).

## Goal

Verify `mur-agent-runtime`'s signature before MUR spawns it, so a swapped binary
is refused regardless of how the swap happened. Launch-chain protection makes the
binary's location unwritable by agents; attestation covers the case where the
binary was replaced by something **outside MUR entirely** (the human, a package
manager, a stray install script, another user).

## Background

The launch-chain spec's follow-on section names this exactly:

> Verify `mur-agent-runtime`'s Developer ID signature before exec. That catches a
> swapped binary regardless of how it was swapped, and so covers the case where
> the protected set is bypassed by something outside MUR entirely.

Current state (verified 2026-08-12):

- Release pipeline signs `mur`, `mur-mcp-server`, `murmurd` with Developer ID +
  hardened runtime + timestamp (`.github/workflows/release.yml`), but the
  `mur-agent-runtime` shipped in the tar.gz is **not signed**. Signing the runtime
  in the pipeline is a precondition for this design.
- Spawn paths: CLI detached spawn (`mur-core/src/cmd/agent/start.rs` path 3),
  launchd kickstart / systemd load (start.rs paths 1-2), Hub sidecar supervisor
  (`mur-gui-core` sidecar module). Restart flows through the same paths.
- Dependency direction: `mur-gui-core` depends on `mur-common` only — the shared
  helper must live in `mur-common` (or below it), not in `mur-core` or
  `mur-agent-runtime`.

## Design

### Verification helper — `mur-common/src/binary_attestation.rs`

```rust
pub const IS_EMBEDDED_RELEASE: bool;   // build.rs, from MUR_EMBED_RELEASE_MARKER
pub fn verify_runtime_signature(path: &Path) -> Result<(), AttestError>;
```

Decision tree (in order):

1. `!IS_EMBEDDED_RELEASE` → `Ok`. Dev builds (`cargo run`, `build.sh --install`)
   are not Developer ID signed; requiring a signature there would break every
   development workflow. The build marker is the gate, not `debug_assertions` —
   a locally-built `--release` binary still runs unrestricted.
2. `!cfg!(target_os = "macos")` → `Ok`. Developer ID is macOS-only. Linux and
   Windows attestation are out of scope (see below).
3. macOS + embedded release: run
   `codesign --verify --strict -R "=anchor apple generic and certificate leaf[subject.OU] = \"<TEAM_ID>\"" <path>`.
   One call binds both properties that matter: the binary has a valid signature
   issued by Apple (Developer ID), and that signature belongs to MUR's team. The
   `=` prefix makes `-R` parse the argument as inline requirement text rather
   than a named requirement or a file path. (`=designated => ...` is NOT valid
   verification syntax — `designated` is only a read-side token — and its
   implication-reading would pass any binary without a designated requirement.
   Verified empirically 2026-08-12: wrong OU exits 3, unsigned exits 1, correct
   team OU exits 0.) No unsafe FFI, no signature parsing; `codesign` is present
   on every macOS install.
4. Any failure of the `codesign` invocation (nonzero exit, missing binary,
   malformed path) → `Err` (fail-closed).

Team ID source: `build.rs` reads `MUR_APPLE_TEAM_ID` (injected by the release
pipeline from the existing `APPLE_TEAM_ID` secret). If the marker is set but the
Team ID is absent, the build **fails to compile** — fail-closed at build time.

### Mount points (all spawn paths)

| Path | Location | When |
|---|---|---|
| CLI detached spawn | `mur-core/src/cmd/agent/start.rs` path 3 | before `Command::new(&symlink).spawn()` |
| launchd / systemd | `mur-core/src/cmd/agent/start.rs` paths 1-2 | before `launchctl kickstart` / `systemctl start` (restart flows through the same paths) |
| Hub sidecar supervisor | `mur-gui-core` sidecar spawn | before every spawn |

A failure at any mount point refuses the spawn and returns the attestation error.

### Error handling

Fail-closed with a message that names the risk and the fix:

```
runtime binary at <path> failed signature verification
  <agent> not started: the binary may have been swapped — launch-chain protection
  covers writes, attestation covers swaps. Fix: mur update --restart-agents, or
  reinstall MUR.
```

### Known limitation (structural, stated honestly)

launchd `KeepAlive` / systemd `Restart=on-failure` respawn the runtime without
going through MUR code. Attestation covers every MUR-triggered spawn; the
supervisor-driven respawn path has no verification point and none is claimed.
This is the same boundary the launch-chain design accepted for autostart units.

### Pipeline changes (`.github/workflows/release.yml`)

1. macOS job: add `mur-agent-runtime` to the existing `codesign --force --options
   runtime --timestamp` invocation that already signs `mur`, `mur-mcp-server`,
   `murmurd`.
2. Set `MUR_EMBED_RELEASE_MARKER=1` and `MUR_APPLE_TEAM_ID=$APPLE_TEAM_ID` for
   the `mur` build in the macOS job and for the Hub `.app` build job (both are
   spawners).
3. Verify the Hub `.app`'s sidecar runtime (the copy of `mur-agent-runtime`
   bundled in the app) is also signed; add it to the signing step if it is not.

## Testing

Every protection test ships its negative control — a test that passes because the
verification never ran is indistinguishable from one that passes because the
protection works (rule established in the launch-chain spec).

The requirement string is parameterized: the public helper uses the production
requirement (anchor apple generic + MUR team OU), and a test-only entry point
accepts a requirement string. CI's macOS job has no Developer ID certificate, and
a self-signed cert **cannot** satisfy `anchor apple generic` (its chain does not
anchor at Apple), so behavioral tests run against the same code path with a test
requirement.

| Test | Positive | Negative control |
|---|---|---|
| production requirement shape | the requirement used by `verify_runtime_signature` contains both `anchor apple generic` and `subject.OU` bindings (string assertion) | — |
| marker off → skip | unsigned binary, no marker → `Ok` | — |
| marker on, non-macOS | (Linux CI) unsigned binary → `Ok` | — |
| marker on, macOS, valid Team ID (test requirement) | CI macOS job: self-signed cert with `OU` = the test Team ID → `Ok` | same binary with a different `OU` → `Err` |
| marker on, macOS, unsigned | ad-hoc / unsigned binary → `Err` | same binary with marker off → `Ok` |
| mount integration | spawn path calls `verify_runtime_signature` before exec | verification failure → agent is not spawned (process list unchanged) |

## Out of scope

- **Linux / Windows attestation.** No Developer ID; Linux has no equivalent
  code-signing mechanism MUR can rely on. Launch-chain write protection remains
  the Linux story. A GPG-based scheme is possible but belongs in its own spec.
- **Verifying `mur` itself or the Hub `.app`.** The spawner is trusted; the
  threat model is a swapped *runtime*.
- **Notarization checking.** `codesign --verify` checks the signature; checking
  stapled notarization ticket state adds nothing to the swapped-binary story.

## Constraints carried from launch-chain

- Fail-closed on `Err` everywhere — no warn-and-continue at a mount point.
- Negative controls are not optional in tests.
- The protected-set write story (#924) is untouched; this layers on top.
