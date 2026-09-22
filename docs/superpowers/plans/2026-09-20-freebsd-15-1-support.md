# FreeBSD 15.1 First-Class Support Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION SKILL: use `mur-executing-plans` (single worker) or `mur-delegate-dev` (task-by-task delegation). Execute in order, stop at every review/commit gate, and do not edit the pre-existing `.serena/project.yml` change.

**Goal:** Add tested, first-class FreeBSD 15.1 amd64 support with a native `rc.d` service, source and release installers, CI/release artifacts, and English/Traditional Chinese documentation while preserving macOS, Linux, and Windows behavior.

**Architecture:** `src/install.rs` gets an explicit platform model so FreeBSD can never fall through to the Windows descriptor, plus pure helpers for deterministic path, account, descriptor, and guidance tests. FreeBSD installation resolves the invoking human through `SUDO_USER`/the account database, writes a root-owned environment file and `rc.d` script, and delegates lifecycle control to `service(8)` and `sysrc(8)`. Shell installers share those CLI semantics; GitHub Actions performs native checks and packaging in a pinned FreeBSD 15.1 VM.

**Tech Stack:** Rust 2024, anyhow, directories, whoami, target-specific `libc`; POSIX/Bash installer scripts; FreeBSD `rc.subr`, `daemon(8)`, `service(8)`, `sysrc(8)`; GitHub Actions with `vmactions/freebsd-vm@4469451fe39bee80be4066836c5170362e9349f3`.

## Global Constraints

- FreeBSD support is tested and documented specifically for **FreeBSD 15.1 amd64**; do not claim other versions or architectures.
- Use native FreeBSD `rc.d`; do not introduce a generic `nohup`/background wrapper.
- The service runs as the human installer, resolving a non-empty, non-`root` `SUDO_USER` first and otherwise the current non-root account.
- Resolve the selected user's home through the Unix account database; never construct `/home/<user>`.
- FreeBSD service files are `/usr/local/etc/rc.d/mur_model_gateway` (mode `0555`) and `/usr/local/etc/mur-model-gateway.env` (mode `0600`); PID and log paths are `/var/run/mur_model_gateway.pid` and `/var/log/mur-model-gateway.log`.
- `mur-model-gateway install` and `install --system` have identical FreeBSD semantics; `--system` remains Linux-only everywhere else that it was previously rejected.
- Installation does not enable the service silently; print `sysrc mur_model_gateway_enable=YES` and `service mur_model_gateway start` (the setup scripts may execute them).
- Uninstall removes only the generated `rc.d` and environment files; it does not delete the binary, log, PID file, or edit `/etc/rc.conf`.
- Preserve operator-added environment-file lines on reinstall through `merge_env_file`.
- Reject username/home/environment values that could inject shell or rc syntax before writing any FreeBSD file.
- Unsupported operating systems return an explicit error; no non-Windows Unix target may receive a Windows `.cmd` descriptor.
- Existing macOS, Linux, and Windows behavior remains unchanged.
- Shell path handling must remain portable: quote paths and do not hardcode host-specific separators in Rust where `Path`/`PathBuf` applies.
- The FreeBSD GitHub Action is pinned to commit `4469451fe39bee80be4066836c5170362e9349f3`, and the VM release is pinned to `15.1`.
- Publish only the `freebsd-amd64` release asset in this scope; no FreeBSD arm64, pkg repository, ports tree, jail, or firewall work.
- Never edit or commit `.serena/project.yml`.
- Before each implementation task, load `mur-debugging`; use tests first, run the named verification commands, then commit only that task's files.
- Spec: `docs/superpowers/specs/2026-09-20-freebsd-15-1-support-design.md`.

## File Structure

| Path | Responsibility |
|---|---|
| `Cargo.toml` | Add the Unix-only account database dependency without exposing it to Windows builds. |
| `Cargo.lock` | Lock the dependency graph update. |
| `src/install.rs` | Explicit platform/path dispatch, FreeBSD account resolution, validation, `rc.d` rendering, install/status/uninstall, and unit tests. |
| `src/main.rs` | Correct the `--system` CLI help to describe Linux and FreeBSD semantics. |
| `scripts/setup.sh` | Build from source and manage the FreeBSD `rc.d` lifecycle. |
| `scripts/install-release.sh` | Select/verify `freebsd-amd64`, install it, and manage the FreeBSD service. |
| `scripts/auto.sh` | Route FreeBSD directly to system-service setup without Linux GLIBC/session probing. |
| `.github/workflows/ci.yml` | Run fmt, Clippy, tests, and release build natively on FreeBSD 15.1. |
| `.github/workflows/release.yml` | Build/test/package/upload FreeBSD amd64 and make release publication depend on it. |
| `README.md` | English release/platform support summary. |
| `README-tw.md` | Traditional Chinese release/platform support summary. |
| `docs/install.md` | English FreeBSD install, lifecycle, paths, credentials, and troubleshooting details. |
| `docs/install-tw.md` | Traditional Chinese equivalent of the installation guidance. |

---

### Task 1: Make platform and path dispatch explicit

**Files:**
- Modify: `src/install.rs`

**Interfaces:**
- Consumes: existing `InstallPaths`, `LINUX_SYSTEM_UNIT`, `LINUX_SYSTEM_ENV_FILE`.
- Produces:
  - `enum InstallPlatform { Macos, Linux, Freebsd, Windows }`
  - `fn current_platform() -> Result<InstallPlatform>`
  - `fn resolve_paths(platform: InstallPlatform, system: bool, binary: PathBuf, dirs: &directories::BaseDirs) -> Result<InstallPaths>`
  - Constants `FREEBSD_RC_SCRIPT`, `FREEBSD_ENV_FILE`, `FREEBSD_PID_FILE`, `FREEBSD_LOG_FILE`.
  - `InstallPaths::resolve` delegates to `resolve_paths`.

- [ ] **Step 1: Add failing path/dispatch tests.** In `src/install.rs` tests, construct `BaseDirs`, pass a fake binary, and assert FreeBSD resolves exactly:

```rust
assert_eq!(paths.service_file, PathBuf::from("/usr/local/etc/rc.d/mur_model_gateway"));
assert_eq!(paths.env_file, Some(PathBuf::from("/usr/local/etc/mur-model-gateway.env")));
assert_eq!(paths.log_dir, PathBuf::from("/var/log"));
```

Also test `resolve_paths(InstallPlatform::Freebsd, true, ...)` returns the same values as `system = false`, and retain assertions for macOS/Linux/Windows paths. Add an unsupported-platform test around a pure `platform_from_target_os(&str) -> Result<InstallPlatform>` helper so an unknown value returns `unsupported operating system: solaris` rather than Windows.

- [ ] **Step 2: Prove the tests fail.** Run:

```bash
cargo test --lib install::tests::freebsd_paths_are_system_paths
cargo test --lib install::tests::unsupported_os_does_not_fall_through_to_windows
```

Expected: compile failure because the enum/helpers/constants do not exist.

- [ ] **Step 3: Implement explicit dispatch.** Add the four constants, derive `Clone, Copy, Debug, Eq, PartialEq` for `InstallPlatform`, map only `macos`, `linux`, `freebsd`, and `windows`, and use `std::env::consts::OS` in `current_platform`. Keep directory-derived paths for existing platforms. FreeBSD ignores `system`; Linux retains both path modes. Update the `InstallPaths` field comments to include `rc.d` and FreeBSD env files.

- [ ] **Step 4: Run focused and regression tests.** Run:

```bash
cargo test --lib install::tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Expected: all pass; no existing descriptor snapshot changes.

- [ ] **Step 5: Commit only this task.**

```bash
git add src/install.rs
git commit -m "refactor(install): make platform dispatch explicit"
```

---

### Task 2: Resolve and validate the FreeBSD service account

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/install.rs`

**Interfaces:**
- Consumes: `InstallPlatform` from Task 1.
- Produces:
  - Unix-only dependency declaration: `[target.'cfg(unix)'.dependencies] libc = "0.2"`.
  - `#[derive(Clone, Debug, Eq, PartialEq)] struct ServiceAccount { username: String, home: PathBuf }`
  - `fn choose_installer_username(sudo_user: Option<&str>, current_user: &str) -> Result<String>`
  - `#[cfg(unix)] fn lookup_account(username: &str) -> Result<ServiceAccount>` using `getpwnam_r`.
  - `fn validate_service_identity(username: &str, home: &Path) -> Result<()>`.
  - `#[cfg(target_os = "freebsd")] fn resolve_freebsd_service_account() -> Result<ServiceAccount>`.

- [ ] **Step 1: Write failing pure-selection and validation tests.** Cover: non-root `SUDO_USER` wins; empty/root `SUDO_USER` is ignored; direct non-root current user is used; root with no human user fails; usernames containing whitespace, quotes, `$`, backticks, `=`, `/`, newline, or leading `-` fail; absolute ordinary home paths pass; relative homes and homes containing newline, quotes, backticks, `$`, or shell metacharacters fail.

- [ ] **Step 2: Prove the tests fail.** Run `cargo test --lib install::tests::installer_` and expect missing helper/type compile errors.

- [ ] **Step 3: Add the Unix-only dependency and account lookup.** Implement `getpwnam_r` behind `#[cfg(unix)]`, allocate/retry a buffer when it returns `ERANGE`, copy `pw_name` and `pw_dir` into owned Rust values, and reject missing/invalid UTF-8 fields with the username in the error but no secret data. Do not use `unsafe` outside this small lookup function; include a `// SAFETY:` explanation for each call/pointer read. Keep all account selection testable without mutating process-global environment.

- [ ] **Step 4: Implement the FreeBSD environment adapter.** `resolve_freebsd_service_account` reads `SUDO_USER`, obtains `whoami::username()` as fallback, calls `choose_installer_username`, then `lookup_account`. Compile this adapter only on FreeBSD; other hosts test pure selection and may test lookup against their current account.

- [ ] **Step 5: Verify all host targets still compile.** Run:

```bash
cargo test --lib install::tests
cargo check --all-targets
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Expected: pass on the host; `cargo tree -e normal -i libc` shows `libc` available on Unix without adding platform-specific source assumptions.

- [ ] **Step 6: Commit.**

```bash
git add Cargo.toml Cargo.lock src/install.rs
git commit -m "feat(freebsd): resolve service account safely"
```

---

### Task 3: Render the FreeBSD `rc.d` service and lifecycle messages

**Files:**
- Modify: `src/install.rs`

**Interfaces:**
- Consumes: `ServiceAccount`; FreeBSD constants; `render_env_file` and `merge_env_file`.
- Produces:
  - `pub fn render_freebsd_rc(binary: &Path, account: &ServiceAccount, env_file: &Path, pid_file: &Path, log_file: &Path) -> Result<String>`.
  - `fn freebsd_install_guidance() -> &'static str`.
  - `fn freebsd_uninstall_guidance() -> &'static str`.
  - `fn freebsd_status_guidance() -> &'static str`.

- [ ] **Step 1: Add a failing renderer test.** Render with `/usr/local/bin/mur-model-gateway`, user `alice`, home `/home/alice`, and assert verbatim presence of:

```text
#!/bin/sh
. /etc/rc.subr
name="mur_model_gateway"
rcvar="mur_model_gateway_enable"
: ${mur_model_gateway_enable:="NO"}
required_files="/usr/local/bin/mur-model-gateway /usr/local/etc/mur-model-gateway.env"
pidfile="/var/run/mur_model_gateway.pid"
```

Also assert the command is `/usr/sbin/daemon`, its arguments supervise/restart the child, append to `/var/log/mur-model-gateway.log`, run as `alice`, and execute the exact binary; `HOME=/home/alice` is exported. Assert the script does **not** use `eval`, `. /usr/local/etc/mur-model-gateway.env`, `source`, `nohup`, or interpolate unvalidated shell fragments.

- [ ] **Step 2: Add a failing environment-loader test.** Require the generated `start_precmd` to read the env file line-by-line, skip blank/comment lines, accept only keys matching `[A-Za-z_][A-Za-z0-9_]*`, and export each assignment using quoted shell builtins without evaluation. A malformed line must emit an error and return non-zero before `run_rc_command "$1"` starts the daemon.

- [ ] **Step 3: Add failing guidance tests.** Assert install guidance includes `sysrc mur_model_gateway_enable=YES`, `service mur_model_gateway start`, and `tail -f /var/log/mur-model-gateway.log`; status includes `service mur_model_gateway status`; uninstall includes `service mur_model_gateway stop` and `sysrc -x mur_model_gateway_enable`.

- [ ] **Step 4: Prove failure.** Run `cargo test --lib install::tests::freebsd_`; expect undefined renderer/guidance helpers.

- [ ] **Step 5: Implement the renderer.** Use standard `rc.subr` metadata and `run_rc_command "$1"`. Put safe env-file parsing in `start_precmd`; export `HOME` and validated assignments before invoking `/usr/sbin/daemon`. Use `daemon(8)` supervisor/restart mode with the fixed PID/log paths and selected user. Keep every dynamic value validated before formatting. Return `Result<String>` so invalid identities/paths fail before filesystem writes.

- [ ] **Step 6: Verify shell syntax where available.** Run:

```bash
cargo test --lib install::tests::freebsd_
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

On FreeBSD CI (Task 6), additionally render a fixture script and run `/bin/sh -n` on it. Expected: all unit tests pass and shell syntax exits 0.

- [ ] **Step 7: Commit.**

```bash
git add src/install.rs
git commit -m "feat(freebsd): render native rc.d service"
```

---

### Task 4: Wire FreeBSD install, status, uninstall, and CLI help

**Files:**
- Modify: `src/install.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: Tasks 1-3 platform, paths, account, renderer, and guidance helpers.
- Produces:
  - FreeBSD branch in `install(InstallOpts)`.
  - FreeBSD branches in `status()` and `uninstall()`.
  - Platform-aware root-write hint through `fn privilege_retry_hint(platform: InstallPlatform, binary: &Path) -> String`.
  - Updated `--system` help text.

- [ ] **Step 1: Add failing behavior-helper tests.** Factor and test the decisions rather than writing under `/usr/local`: FreeBSD accepts both `system=false` and `system=true`; macOS/Windows still reject `--system`; Linux still accepts it. Assert the FreeBSD retry hint is `sudo <binary> install …` (without requiring `--system`), while Linux system mode retains `sudo <binary> install --system …`.

- [ ] **Step 2: Prove failure.** Run `cargo test --lib install::tests::freebsd_install_`; expect missing policy helpers or wrong Linux-only rejection.

- [ ] **Step 3: Implement the FreeBSD install branch.** Before writing either file: resolve/validate account, render env and `rc.d` content, and read the existing env for merging. Then write env mode `0600` and service mode `0555` with `write_root_owned`. Print both paths, secret-variable editing guidance for `env:VAR` sources (including Codex if applicable), and `freebsd_install_guidance`. Do not call `sysrc` or `service` from Rust.

- [ ] **Step 4: Implement status/uninstall branches.** Status prints binary, rc script, env file, PID file, log file, then the authoritative service command. Uninstall targets only the rc script and env file, reports exact `sudo rm` commands on permission denial, and prints stop/`sysrc -x` guidance without executing it. Ensure Linux still probes user+system paths, while FreeBSD probes its single system path once.

- [ ] **Step 5: Update CLI docs and module comments.** In `src/main.rs`, describe `--system` as Linux systemd selection and a FreeBSD accepted no-op. Update the top of `src/install.rs` and `InstallOpts::system` comments to name all four platforms accurately.

- [ ] **Step 6: Run complete host regression.** Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: pass; existing macOS/Linux/Windows tests stay green.

- [ ] **Step 7: Commit.**

```bash
git add src/install.rs src/main.rs
git commit -m "feat(freebsd): install and manage rc.d service"
```

---

### Task 5: Support FreeBSD in source and release installer scripts

**Files:**
- Modify: `scripts/setup.sh`
- Modify: `scripts/install-release.sh`
- Modify: `scripts/auto.sh`

**Interfaces:**
- Consumes: CLI lifecycle from Task 4; release suffix `freebsd-amd64`.
- Produces: FreeBSD platform branches for source builds, released binaries, automatic setup, health checks, help, and removal instructions.

- [ ] **Step 1: Add shell preflight assertions before editing.** Record the current expected failures:

```bash
rg -n 'FreeBSD|freebsd-amd64|mur_model_gateway' scripts/setup.sh scripts/install-release.sh scripts/auto.sh
```

Expected: no FreeBSD support matches.

- [ ] **Step 2: Extend `scripts/setup.sh`.** Detect `FreeBSD`; reject `--musl` there; accept `--system` as a no-op. Add FreeBSD teardown (`sudo service mur_model_gateway stop`), install (`sudo env SUDO_USER="${SUDO_USER:-$USER}" "$INSTALL_PATH" install ...`), start (`sudo sysrc mur_model_gateway_enable=YES`; `sudo service mur_model_gateway start`), logs, stop/remove hints, and failure guidance. Replace the `/dev/tcp` final fallback with a portable `curl` health/listener probe (or `nc -z` when available) so FreeBSD does not depend on Bash's pseudo-device.

- [ ] **Step 3: Extend `scripts/install-release.sh`.** Detect only `uname -s = FreeBSD` and `uname -m = amd64`; set `ASSET_SUFFIX=freebsd-amd64`; use FreeBSD's `sha256 -c` form (verify its exact FreeBSD 15.1 syntax in CI); install/register with sudo while preserving `SUDO_USER`; enable/start with `sysrc`/`service`; and print FreeBSD-specific health/log/remove output. Keep macOS and Linux command text unchanged.

- [ ] **Step 4: Extend `scripts/auto.sh`.** Add an explicit FreeBSD branch that appends `--system` (documented no-op selecting the one native system mode) and skips every GLIBC, `ldd`, `sort -V`, `XDG_RUNTIME_DIR`, and `systemctl` probe. Keep credential-file detection unchanged.

- [ ] **Step 5: Run host-static checks.** Run:

```bash
bash -n scripts/setup.sh scripts/install-release.sh scripts/auto.sh
shellcheck scripts/setup.sh scripts/install-release.sh scripts/auto.sh 2>/dev/null || true
rg -n 'FreeBSD|freebsd-amd64|service mur_model_gateway|sysrc mur_model_gateway_enable' scripts
```

Expected: Bash syntax succeeds; all three scripts contain their intended FreeBSD branch. Treat actual ShellCheck diagnostics as findings to fix even though absence of ShellCheck is allowed.

- [ ] **Step 6: Commit.**

```bash
git add scripts/setup.sh scripts/install-release.sh scripts/auto.sh
git commit -m "feat(freebsd): support source and release installers"
```

---

### Task 6: Add native FreeBSD 15.1 CI and release packaging

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: all Rust/script work from Tasks 1-5.
- Produces: `freebsd` CI job; `build-freebsd` release job; `mur-model-gateway-<version>-freebsd-amd64.tar.gz` and `.sha256`.

- [ ] **Step 1: Add a separate CI job.** In `.github/workflows/ci.yml`, keep the existing OS matrix and add `freebsd` on `ubuntu-latest` with checkout followed by:

```yaml
- name: Check on FreeBSD 15.1
  uses: vmactions/freebsd-vm@4469451fe39bee80be4066836c5170362e9349f3
  with:
    release: "15.1"
    usesh: true
    prepare: pkg install -y curl
    run: |
      set -eu
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component rustfmt,clippy
      . "$HOME/.cargo/env"
      cargo fmt --check
      cargo clippy --all-targets -- -D warnings
      cargo test
      cargo build --release
```

If `rustup` requires a FreeBSD-specific prerequisite, add the minimal `pkg install` item in `prepare` and record it explicitly; do not float the action ref or release.

- [ ] **Step 2: Add the release job.** In `.github/workflows/release.yml`, add `build-freebsd` after `test`, using the same action SHA/release and Rust setup. Run `cargo test`, `cargo build --release`, package from `target/release`, and generate:

```text
mur-model-gateway-${VERSION}-freebsd-amd64.tar.gz
mur-model-gateway-${VERSION}-freebsd-amd64.tar.gz.sha256
```

Use FreeBSD-native `sha256` output in the two-space filename format accepted by the release installer. Upload with artifact name `freebsd`.

- [ ] **Step 3: Gate release publication.** Change the release job dependency to:

```yaml
needs: [build-macos, build-linux, build-windows, build-freebsd]
```

This must prevent partial tag releases.

- [ ] **Step 4: Add native descriptor syntax validation.** In the FreeBSD CI `run`, execute a focused Rust test that writes the rendered descriptor to a temporary file, then `/bin/sh -n` that fixture. If the test only returns a string, add a test-only helper/binary invocation in `src/install.rs` tests rather than writing production artifacts. Do not start an actual boot service in CI.

- [ ] **Step 5: Validate workflow syntax and pins locally.** Run:

```bash
ruby -e 'require "yaml"; %w[.github/workflows/ci.yml .github/workflows/release.yml].each { |f| YAML.load_file(f); puts "ok #{f}" }'
rg -n 'vmactions/freebsd-vm@4469451fe39bee80be4066836c5170362e9349f3|release: "15.1"|freebsd-amd64|build-freebsd' .github/workflows
```

Expected: both YAML files parse and every action reference is immutable. (Ruby's YAML parser treats unquoted `on` specially but still catches indentation/syntax errors.)

- [ ] **Step 6: Run host regression and commit.**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
git add .github/workflows/ci.yml .github/workflows/release.yml src/install.rs
git commit -m "ci: verify and package FreeBSD 15.1"
```

After push/PR, the FreeBSD job itself is the required native evidence; do not claim native success from local macOS checks.

---

### Task 7: Document tested FreeBSD support in English and Traditional Chinese

**Files:**
- Modify: `README.md`
- Modify: `README-tw.md`
- Modify: `docs/install.md`
- Modify: `docs/install-tw.md`

**Interfaces:**
- Consumes: final CLI/script paths and commands from Tasks 4-6.
- Produces: matching English/Traditional Chinese user guidance with no broader support claim than FreeBSD 15.1 amd64.

- [ ] **Step 1: Update both READMEs.** Add `FreeBSD 15.1 amd64` to release assets; list `rc.d` with launchd/systemd/Task Scheduler; explain that FreeBSD service installation is system-level and needs sudo. Keep the two language versions semantically aligned.

- [ ] **Step 2: Add a FreeBSD section to both install guides.** Include exact commands:

```sh
sudo mur-model-gateway install --token-source env:MUR_MODEL_GATEWAY_OAUTH_TOKEN
sudoedit /usr/local/etc/mur-model-gateway.env
sudo sysrc mur_model_gateway_enable=YES
sudo service mur_model_gateway start
service mur_model_gateway status
tail -f /var/log/mur-model-gateway.log
```

Explain `SUDO_USER`, account-database home resolution, access to `~/.claude`/`~/.codex`, descriptor/env modes, `--system` no-op behavior, amd64-only release installer, conservative uninstall, and the exact stop/`sysrc -x` cleanup commands.

- [ ] **Step 3: Correct global flag/status wording.** Change “Linux only” `--system` text to “Linux system unit; accepted no-op on FreeBSD.” Update environment validation wording to include `rc.d`; update status/uninstall descriptions to cover the FreeBSD service/env/PID/log display and root-owned file removal.

- [ ] **Step 4: Review claims and parity.** Run:

```bash
rg -n -i 'FreeBSD|rc\.d|freebsd-amd64|--system|mur_model_gateway|15\.1|arm64' README.md README-tw.md docs/install.md docs/install-tw.md
rg -n 'FreeBSD(?! 15\.1)' README.md docs/install.md --pcre2
```

Expected: support claims say FreeBSD 15.1 amd64; no FreeBSD arm64 artifact claim; English and Traditional Chinese include the same paths and lifecycle commands.

- [ ] **Step 5: Commit.**

```bash
git add README.md README-tw.md docs/install.md docs/install-tw.md
git commit -m "docs: add FreeBSD 15.1 installation guide"
```

---

### Task 8: Final cross-platform verification and delivery

**Files:**
- No intended source edits; fix only concrete failures in their owning task files and commit those fixes separately.

**Interfaces:**
- Consumes: Tasks 1-7.
- Produces: local regression evidence, native CI evidence, clean scoped diff, and release-asset naming evidence.

- [ ] **Step 1: Run the complete local suite.**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
bash -n scripts/setup.sh scripts/install-release.sh scripts/auto.sh
git diff --check HEAD~7..HEAD
```

Expected: all commands exit 0.

- [ ] **Step 2: Inspect scope and preserve the user's unrelated change.** Run:

```bash
git status --short
git diff --stat c874aee..HEAD
git log --oneline c874aee..HEAD
```

Expected: `.serena/project.yml` remains modified but absent from every implementation commit; only files listed in this plan changed.

- [ ] **Step 3: Push/open a PR and wait for native evidence.** Confirm Linux, macOS, Windows, and `freebsd` CI jobs pass. If opening a PR, end its body with exactly:

```text
Generated with [MUR](https://app.mur.run/products/mur)
```

Do not call FreeBSD support verified until the FreeBSD 15.1 job passes.

- [ ] **Step 4: Rehearse release packaging.** Run `workflow_dispatch` on the release workflow, download the `freebsd` artifact, and verify it contains exactly the binary tarball and checksum named with `freebsd-amd64`. Verify the checksum using FreeBSD 15.1's command form used by `scripts/install-release.sh`.

- [ ] **Step 5: Report completion evidence.** Include commit range, local commands and exit status, CI run URL/status, artifact filenames, and any deliberately unrun root/service smoke test. Do not claim an actual `rc.d` start/stop smoke test unless one was performed on a disposable FreeBSD host.

## Spec-Coverage Review

| Spec requirement | Plan task |
|---|---|
| Explicit FreeBSD dispatch; no Windows fallback | 1 |
| Fixed FreeBSD service/env/PID/log paths | 1, 4 |
| Installer user and account-database home | 2 |
| Identity/injection validation | 2, 3 |
| Native rc.subr + daemon supervision + safe env loading | 3 |
| Install permissions, merge, activation/log hints | 4 |
| Status and conservative uninstall | 4 |
| Source/release/auto installers | 5 |
| FreeBSD 15.1 native CI and immutable action pin | 6 |
| amd64 release artifact and publication dependency | 6 |
| English and Traditional Chinese docs | 7 |
| Existing-platform regression and native evidence | 8 |

## Self-Review Checklist

- No `TBD`, `TODO`, “similar to,” or unnamed error-handling placeholders.
- Every new helper/type is introduced in one task and consumed only by later tasks.
- FreeBSD-only account/environment adapters are cfg-gated; pure policy/rendering helpers remain host-testable.
- The action SHA is concrete and immutable; the VM release and asset architecture are explicit.
- Root writes and service starts are not required by local unit tests.
- `.serena/project.yml` is excluded from every `git add` command.
