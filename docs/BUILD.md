# Building, testing and linting MUR

Operational build notes for the Rust workspace. `CLAUDE.md` points here; keep
compile/lint/test mechanics in this file, not there. Packaging scripts
(`build.sh`, installers) are documented in [`BUILD-SCRIPTS.md`](BUILD-SCRIPTS.md);
releases in the `mur-release` skill.

## Lint (CI's exact invocation)

```bash
# --all-targets is load-bearing: without it test code is not linted and CI fails.
cargo clippy --all --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
```

## Tests

```bash
cargo nextest run --workspace            # full suite
cargo nextest run -p mur-core --lib -E 'test(/fleet::review::/)'   # one area
```

- `.cargo/config.toml` sets `RUST_MIN_STACK = 33554432`. `mur-core`'s clap
  CLI-parse tests overflow the default 2 MiB stack in debug builds and
  SIGABRT without it. nextest has no `[env]` of its own, so it lives there.
- nextest settings: `.config/nextest.toml`.
- Prefer `--lib` scoped runs while iterating; `--tests` also builds every
  integration-test binary and is much slower.

## Workspace layout that affects builds

- Rust edition 2024 — `let` chains are stable (`if let … && let …`).
- `mur-agent-gui` and `mur-hub-gui` are Tauri 2 apps **excluded from the
  workspace**. Build them via their own manifests
  (`cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`), so
  `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2.
- `mur-agent-runtime` must not depend on `mur-core` (that pulls LanceDB +
  Arrow into every agent process). Shared on-disk state goes in its own small
  crate below both — see the Architecture section of `CLAUDE.md`.

## Per-turn build dirs (turn tracks)

An agent turn that runs in a turn track builds into its own target dir,
`target/turns/<track>/` (`CARGO_TARGET_DIR` is exported only when the track
root has a `Cargo.toml`). The first `cargo` command of the turn clones the
main target's `<profile>/{deps,build,.fingerprint}` dirs into it (APFS
`clonefile`, a plain copy elsewhere), so most crates come up `Fresh`.
Each profile's `incremental` dir and final binaries are skipped: they are path-keyed and
useless in a track. The clone needs no extra build — it only reuses what the
main target already finished, so a build script that never completed there
still runs in the turn.

- The clone is skipped (the build runs cold) when free disk is under
  `min_free_gib`, or on any clone error. Orphan turn dirs are swept first.
- `cargo clean` deletes every turn dir too; a live turn's next build is cold.
- Tune or disable it in `~/.mur/config.yaml`:

```yaml
turn_track:
  warm_build_cache:
    enabled: true             # false = every turn builds cold
    triggers: [cargo]         # first word of a command that triggers the clone
    cache_dirs: [target]      # project-relative build-cache dirs
    skip_subdirs: [incremental]
    min_free_gib: 20
    turn_target_subdir: turns # target/<this>/<track>/
```

## Build profiles

- `Cargo.toml` `[profile.dev]` is CI-tuned: `debug = 0`, `incremental = false`.
- `.cargo/config.toml` overrides it locally: `debug = 2`, `incremental = true`.
  CI forces them back off with `CARGO_INCREMENTAL=0` and
  `CARGO_PROFILE_DEV_DEBUG=0` (env vars beat config files).
- Setting `CARGO_PROFILE_DEV_INCREMENTAL` under `[env]` does **not** work:
  `[env]` only reaches processes cargo spawns, not cargo's own config.

## Troubleshooting

### `whisper-rs-sys` build script fails inside a sandbox (sccache cache dir)

> **MUR agents need no manual exports.** The agent's bash tool defaults
> `SCCACHE_DIR`, `SCCACHE_SERVER_UDS` and `ORT_CACHE_DIR` into its scratch dir
> (`$TMPDIR`) for every command, only for variables that are not already set
> (#1860). The socket gives the agent its own sccache server: a server already
> running on the machine ignores a new `SCCACHE_DIR`, which is why a cache dir
> alone is not always enough. To use your own server, set `SCCACHE_SERVER_PORT`
> or `SCCACHE_SERVER_UDS` yourself and the socket default is skipped. The rest
> of this section applies to other sandboxed shells.

> **Sandboxed agents (MUR, Claude Code, any seatbelt/sandbox-exec shell):
> point sccache at a directory you can write before building.**

**Symptom** — `cargo build` / `clippy` / `nextest` fails with:

```text
error: failed to run custom build command for `whisper-rs-sys v0.15.0`
  sccache: error: failed to create directory `~/Library/Caches/Mozilla.sccache/preprocessor`: Operation not permitted (os error 1)
  make[2]: *** [ggml/src/CMakeFiles/ggml-base.dir/ggml.c.o] Error 254
```

**Cause** — `whisper-rs-sys` (pulled in by `mur-agent-runtime`'s `whisper-rs`)
builds the vendored whisper.cpp/ggml with CMake. ggml's
`option(GGML_CCACHE … ON)` makes CMake use `ccache` or `sccache` automatically
whenever one is on `PATH`, even though the repo never configures it locally.
sccache's default cache on macOS is `~/Library/Caches/Mozilla.sccache`, which a
sandboxed shell may not write.

**Fix** — give sccache a writable cache directory (any path inside your write
entitlement; MUR agents should use `$TMPDIR`):

```bash
mkdir -p "$TMPDIR/sccache"
SCCACHE_DIR="$TMPDIR/sccache" cargo clippy --all --all-targets --no-deps --locked -- -D warnings
```

Alternatives: take sccache off `PATH` for the build, or pass
`GGML_CCACHE=OFF` through the CMake environment. Outside a sandbox nothing
needs changing.

### `whisper-rs-sys` build script fails with `Server startup failed: File exists`

**Symptom** — the cache dir is writable (see above), yet the build still fails:

```text
error: failed to run custom build command for `whisper-rs-sys v0.15.0`
  sccache: error: Server startup failed: File exists (os error 17)
```

**Cause** — no sccache server is listening on `SCCACHE_SERVER_UDS` yet, so
the parallel CMake compiles of ggml each try to start one and the losers fail.
MUR agents get a socket of their own (#1860), so the first build in a fresh
agent or worktree always starts from no server. Reproduced with sccache 0.14:
16 parallel `sccache cc -c` with no server fail 1 in 16 every round; with the
server started first, 0 in 16. A socket file left behind by a killed server is
not the cause — `--start-server` succeeds over it.

**Fix** — the agent bash tool now starts the server on the agent's socket
before any command that runs `cargo` or `sccache`, when nothing is listening
there. Outside an agent, or with an older runtime, start it once yourself
with the same environment the build will use:

```bash
sccache --start-server
cargo nextest run --workspace --no-fail-fast
```

`sccache --show-stats` succeeding is a quick check that the server is up. The
server exits after `SCCACHE_IDLE_TIMEOUT` seconds idle (sccache default 600);
the agent re-checks before each build, so that needs no action.

### `ort-sys` build script fails inside a sandbox (ONNX Runtime cache dir)

**Symptom** — `cargo build` / `clippy` fails in `ort-sys`'s build script while
extracting the prebuilt ONNX Runtime archive. A half-written leftover from an
earlier interrupted download can make a retry fail the same way.

**Cause** — `ort-sys` unpacks the download into `~/Library/Caches/ort.pyke.io`
on macOS, which a sandboxed shell may not write.

**Fix** — point it at a writable directory (MUR agents should use `$TMPDIR`):

```bash
mkdir -p "$TMPDIR/ort-cache"
ORT_CACHE_DIR="$TMPDIR/ort-cache" cargo clippy --all --all-targets --no-deps --locked -- -D warnings
```

`ORT_CACHE_DIR` is read first by `ort-sys` (`src/internal/dirs.rs`), ahead of
the platform default. If a previous run was interrupted, delete the partial
directory it left under that cache before retrying. Outside a sandbox nothing
needs changing.

### Build scripts fail inside a sandbox in a `.worktrees/` checkout

**Symptom** — in a worktree under `.worktrees/`, the very first build script
fails before any code compiles:

```text
error: failed to run custom build command for `quote v1.0.46`
  could not execute process `…/.worktrees/<name>/target/debug/build/quote-…/build-script-build` (never executed)
  Operation not permitted (os error 1)
```

**Cause** — the sandbox profile lets the shell write the worktree's own
`target/` but not execute binaries from it, so every compiled build script is
refused. This is unrelated to the sccache / ort cache dirs above; setting those
does not help.

**Fix** — point the worktree at a target dir the sandbox allows executing from,
e.g. the main checkout's `target/`, and remove the worktree's partial
`target/`:

```bash
rm -rf target
CARGO_TARGET_DIR=<main-checkout>/target cargo check --workspace --tests --locked
```

Sharing one target dir between checkouts means their builds serialize on its
lock and invalidate each other's incremental state when the branches differ;
for a long-lived worktree use a dedicated subdirectory such as
`<main-checkout>/target/wt-<name>`. Outside a sandbox nothing needs changing.

### Stale crate after switching branches (`cannot find function … in module`)

An `E0425` on a symbol that plainly exists in the source usually means stale
incremental artifacts. Clean only that crate, not the whole target dir:

```bash
cargo clean -p <crate>      # e.g. cargo clean -p mur-browser
```

### `target/debug` grows to tens of GB

The bulk is not third-party dependencies. It is stale hash variants of the
workspace's own artifacts in `target/debug/deps/`: every feature set, profile
tweak, or branch switch leaves another `libmur_core-<hash>.rlib` and another
copy of each integration-test binary behind, and cargo never removes the old
ones. With the local `debug = 2` profile a single integration-test binary can
reach a few GB.

Before reaching for `cargo clean`, drop only artifacts that no build has
touched recently (the same rule `cargo sweep --time 2` applies):

```bash
find target/debug -type f -mtime +2 -delete
find target/debug -type d -empty -delete
```

The next build recompiles whatever it still needs. Each worktree under
`.worktrees/` has its own `target/`; deleting the `target/` of an idle
worktree is safe and does not touch its source.

### `serena_install` `with_fake_uv` tests fail inside a sandbox

`cmd::code_nav::serena_install::tests::with_fake_uv::*` write a fake `uv`
script to a temp dir and execute it. A sandbox that denies exec from the temp
dir fails them with `Operation not permitted (os error 1)`. This is the
environment, not the code; run those four outside the sandbox (or in CI).
