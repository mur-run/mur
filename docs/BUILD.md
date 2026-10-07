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

## Build profiles

- `Cargo.toml` `[profile.dev]` is CI-tuned: `debug = 0`, `incremental = false`.
- `.cargo/config.toml` overrides it locally: `debug = 2`, `incremental = true`.
  CI forces them back off with `CARGO_INCREMENTAL=0` and
  `CARGO_PROFILE_DEV_DEBUG=0` (env vars beat config files).
- Setting `CARGO_PROFILE_DEV_INCREMENTAL` under `[env]` does **not** work:
  `[env]` only reaches processes cargo spawns, not cargo's own config.

## Troubleshooting

### `whisper-rs-sys` build script fails inside a sandbox (sccache cache dir)

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

### Stale crate after switching branches (`cannot find function … in module`)

An `E0425` on a symbol that plainly exists in the source usually means stale
incremental artifacts. Clean only that crate, not the whole target dir:

```bash
cargo clean -p <crate>      # e.g. cargo clean -p mur-browser
```

### `serena_install` `with_fake_uv` tests fail inside a sandbox

`cmd::code_nav::serena_install::tests::with_fake_uv::*` write a fake `uv`
script to a temp dir and execute it. A sandbox that denies exec from the temp
dir fails them with `Operation not permitted (os error 1)`. This is the
environment, not the code; run those four outside the sandbox (or in CI).
