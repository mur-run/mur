# Binary Attestation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refuse to spawn `mur-agent-runtime` unless its signature is valid and belongs to MUR's team, gated to release builds.

**Architecture:** A shared helper in `mur-common` (`binary_attestation.rs`) whose build-marker const is set by `build.rs` env, runs `codesign --verify --strict -R "=designated => anchor apple generic and certificate leaf[subject.OU] = \"<TEAM_ID>\""` on macOS release builds and is a no-op otherwise. Mounted at all three spawn paths (CLI detached, launchd/systemd, Hub sidecar). The release pipeline signs the runtime in the matrix job (where the tar.gz is assembled) and in the Hub job, and sets the marker env.

**Tech Stack:** Rust (edition 2024), `codesign` subprocess, GitHub Actions (release.yml + ci.yml).

## Global Constraints

- Fail-closed on `Err` at every mount point — no warn-and-continue.
- Negative controls are not optional in tests (launch-chain rule).
- Verification runs only when the spawner binary was built with `MUR_EMBED_RELEASE_MARKER=1` (release pipeline only) — never gated on `debug_assertions`.
- Build-time fail-closed: marker set but `MUR_APPLE_TEAM_ID` missing → build fails.
- macOS-only verification; Linux/Windows are no-ops.
- Canonicalize the target path before verifying (symlink + `/var`→`/private/var`).
- The `=` prefix is required in the `-R` requirement string (full requirement, not named).

---

### Task 1: `mur-common` build marker + attestation helper

**Files:**
- Modify: `mur-common/build.rs`
- Create: `mur-common/src/binary_attestation.rs`
- Modify: `mur-common/src/lib.rs` (register module)
- Create: `scripts/test-signing-identity.sh` (macOS test identity, reused by CI)
- Modify: `.github/workflows/ci.yml` (macOS test job runs the identity script)

**Interfaces:**
- Produces: `mur_common::binary_attestation::{IS_EMBEDDED_RELEASE: bool, APPLE_TEAM_ID: &str, verify_runtime_signature(path: &Path) -> Result<(), AttestError>, verify_with_requirement(path: &Path, requirement: &str) -> Result<(), AttestError> /* #[doc(hidden)] */, AttestError: Debug + Display + Error}`. `AttestError::VerificationFailed { path: PathBuf, stderr: String }` and `AttestError::Io { path: PathBuf, source: std::io::Error }`.

- [ ] **Step 1: Extend `mur-common/build.rs`**

```rust
use std::process::Command;

fn main() {
    // (existing MUR_GIT_SHA block, unchanged)

    // Without rerun-if-env-changed, flipping the env vars later would keep
    // the stale consts baked into the crate — a silent attestation gap.
    println!("cargo:rerun-if-env-changed=MUR_EMBED_RELEASE_MARKER");
    println!("cargo:rerun-if-env-changed=MUR_APPLE_TEAM_ID");
    let marker = std::env::var("MUR_EMBED_RELEASE_MARKER").is_ok();
    let team_id = std::env::var("MUR_APPLE_TEAM_ID").unwrap_or_default();
    if marker && team_id.is_empty() {
        panic!("MUR_EMBED_RELEASE_MARKER=1 requires MUR_APPLE_TEAM_ID to be set");
    }
    println!("cargo:rustc-env=MUR_EMBEDDED_RELEASE={}", if marker { "1" } else { "0" });
    println!("cargo:rustc-env=MUR_APPLE_TEAM_ID={team_id}");
}
```

- [ ] **Step 2: Write the failing tests in `mur-common/src/binary_attestation.rs`** (tests ship with the module; run them before the implementation below exists to see them fail on "module not found")

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Compile-time gates: this binary is built without the release marker in
    // CI, so IS_EMBEDDED_RELEASE is false here — the skip behavior is the
    // negative control for every behavioral test below.
    #[test]
    fn dev_build_never_verifies() {
        assert!(!IS_EMBEDDED_RELEASE);
        // verify_runtime_signature on a garbage path must still be Ok in dev:
        assert!(verify_runtime_signature(Path::new("/nonexistent/nope")).is_ok());
    }

    #[test]
    fn production_requirement_binds_anchor_and_team() {
        let req = production_requirement();
        assert!(req.contains("anchor apple generic"), "req: {req}");
        assert!(req.contains("subject.OU"), "req: {req}");
        assert!(req.starts_with("=designated =>"), "req: {req}");
    }

    // ── Behavioral matrix (macOS + test identity) ────────────────────────
    // These run only when MUR_TEST_SIGNING_OU is set (CI macOS job runs
    // scripts/test-signing-identity.sh first). Negative controls included.
    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mur-attest-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn test_ou() -> Option<String> {
        std::env::var("MUR_TEST_SIGNING_OU").ok()
    }

    #[test]
    fn unsigned_file_fails_test_requirement() {
        let Some(ou) = test_ou() else {
            eprintln!("skipping: MUR_TEST_SIGNING_OU not set");
            return;
        };
        let dir = test_dir("unsigned");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let req = format!("=designated => certificate leaf[subject.OU] = \"{ou}\"");
        let err = verify_with_requirement(&f, &req).expect_err("unsigned must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adhoc_signed_fails_test_requirement() {
        let Some(ou) = test_ou() else { eprintln!("skipping"); return };
        let dir = test_dir("adhoc");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("codesign").args(["--force", "-s", "-"]).arg(&f).output().unwrap();
        assert!(out.status.success(), "ad-hoc sign failed: {}", String::from_utf8_lossy(&out.stderr));
        let req = format!("=designated => certificate leaf[subject.OU] = \"{ou}\"");
        let err = verify_with_requirement(&f, &req).expect_err("ad-hoc (no OU) must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_ou_fails_test_requirement() {
        let Some(ou) = test_ou() else { eprintln!("skipping"); return };
        let dir = test_dir("wrongou");
        let f = dir.join("runtime");
        std::fs::write(&f, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("codesign")
            .args(["--force", "-s", &format!("Mur Test ({ou})")])
            .arg(&f)
            .output()
            .unwrap();
        assert!(out.status.success(), "sign failed: {}", String::from_utf8_lossy(&out.stderr));
        let wrong = format!("=designated => certificate leaf[subject.OU] = \"WRONGTEAM000\"");
        let err = verify_with_requirement(&f, &wrong).expect_err("wrong OU must fail");
        assert!(matches!(err, AttestError::VerificationFailed { .. }));
        // Positive control: the same signed binary passes with the right OU.
        let right = format!("=designated => certificate leaf[subject.OU] = \"{ou}\"");
        verify_with_requirement(&f, &right).expect("matching OU must pass");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::os::unix::fs::PermissionsExt;
}
```

- [ ] **Step 3: Run tests to verify they fail to compile** (module not registered)

Run: `cargo test -p mur-common --lib binary_attestation`
Expected: error `could not find binary_attestation in mur_common` (module not registered yet).

- [ ] **Step 4: Implement `mur-common/src/binary_attestation.rs`**

```rust
//! Binary attestation: verify that a spawned `mur-agent-runtime` carries a
//! valid signature from MUR's Developer ID team (launch-chain follow-on,
//! spec 2026-08-12). Gated to release builds by the build marker.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// True when this binary was built with `MUR_EMBED_RELEASE_MARKER=1`
/// (the release pipeline). Dev builds never verify.
pub const IS_EMBEDDED_RELEASE: bool = env!("MUR_EMBEDDED_RELEASE") == "1";

/// MUR's Apple Developer Team ID (empty in dev builds; build.rs panics if the
/// marker is set without it).
pub const APPLE_TEAM_ID: &str = env!("MUR_APPLE_TEAM_ID");

/// The designated requirement used in production: valid signature chaining to
/// Apple plus a leaf certificate owned by MUR's team.
pub(crate) fn production_requirement() -> String {
    format!(
        "=designated => anchor apple generic and certificate leaf[subject.OU] = \"{APPLE_TEAM_ID}\""
    )
}

/// Verify `path` is a legitimate runtime binary. No-op unless this is a
/// macOS release build. Fail-closed: any verification error is returned.
pub fn verify_runtime_signature(path: &Path) -> Result<(), AttestError> {
    if !IS_EMBEDDED_RELEASE || !cfg!(target_os = "macos") {
        return Ok(());
    }
    // Canonicalize so a symlink or /var → /private/var redirect is verified
    // on the real file.
    let real = path
        .canonicalize()
        .map_err(|e| AttestError::Io { path: path.to_path_buf(), source: e })?;
    verify_with_requirement(&real, &production_requirement())
}

/// Testable core: run `codesign --verify --strict -R <requirement>` on `path`.
#[doc(hidden)]
pub fn verify_with_requirement(path: &Path, requirement: &str) -> Result<(), AttestError> {
    let out = Command::new("codesign")
        .args(["--verify", "--strict", "-R", requirement])
        .arg(path)
        .output()
        .map_err(|e| AttestError::Io { path: path.to_path_buf(), source: e })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AttestError::VerificationFailed {
            path: path.to_path_buf(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

#[derive(Debug)]
pub enum AttestError {
    /// The binary failed the designated requirement.
    VerificationFailed { path: PathBuf, stderr: String },
    /// Could not read/canonicalize the path or run codesign.
    Io { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for AttestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerificationFailed { path, stderr } => write!(
                f,
                "runtime binary at {} failed signature verification: {stderr}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(f, "cannot verify runtime binary at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for AttestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::VerificationFailed { .. } => None,
        }
    }
}
```

Register the module in `mur-common/src/lib.rs` (alphabetical, near `bridge`): `pub mod binary_attestation;`

- [ ] **Step 5: Run tests to verify they pass** (dev build: gate tests pass; behavioral tests print "skipping")

Run: `cargo test -p mur-common --lib binary_attestation`
Expected: 3 pass (`dev_build_never_verifies`, `production_requirement_binds_anchor_and_team`, and one behavioral test that skips). On macOS with `MUR_TEST_SIGNING_OU` set (Step 7), the behavioral matrix runs.

- [ ] **Step 6: Write `scripts/test-signing-identity.sh`** (macOS-only; creates a throwaway self-signed identity with a fixed OU and installs it in a temp keychain)

```bash
#!/bin/bash
# Create a throwaway self-signed code-signing identity for the attestation
# behavioral tests. macOS only; exits 0 with a notice on other platforms.
# Prints the OU (via MUR_TEST_SIGNING_OU in GITHUB_ENV when under CI).
set -euo pipefail
if [ "$(uname)" != "Darwin" ]; then
  echo "test-signing-identity: not macOS, skipping"
  exit 0
fi
OU="${MUR_TEST_TEAM_ID:-TESTTEAMID123}"
CN="Mur Test ($OU)"
KC_DIR="${TMPDIR:-/tmp}/mur-attest-keychain"
rm -rf "$KC_DIR"; mkdir -p "$KC_DIR"
KC="$KC_DIR/test.keychain"
P12="$KC_DIR/test.p12"
PASS="mur"
openssl req -x509 -newkey rsa:2048 -keyout "$KC_DIR/key.pem" -out "$KC_DIR/cert.pem" \
  -days 1 -nodes -subj "/OU=$OU/CN=$CN" >/dev/null 2>&1
openssl pkcs12 -export -out "$P12" -inkey "$KC_DIR/key.pem" -in "$KC_DIR/cert.pem" \
  -passout "pass:$PASS" >/dev/null 2>&1
security create-keychain -p "$PASS" "$KC"
security import "$P12" -k "$KC" -P "$PASS" -T /usr/bin/codesign >/dev/null 2>&1
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PASS" "$KC" >/dev/null 2>&1
security default-keychain -s "$KC"
security unlock-keychain -p "$PASS" "$KC"
echo "test-signing-identity: identity '$CN' ready in $KC"
echo "MUR_TEST_SIGNING_OU=$OU" >> "${GITHUB_ENV:-/dev/null}"
```

- [ ] **Step 7: CI hook — run the identity script on the macOS test job**

In `.github/workflows/ci.yml`, in the `Test (${{ matrix.os }})` job, add a step before the cargo test step, gated to macOS:

```yaml
      - name: Create test signing identity (macOS)
        if: runner.os == 'macOS'
        run: bash scripts/test-signing-identity.sh
```

- [ ] **Step 8: Full gate + commit**

Run: `cargo test -p mur-common && cargo fmt --check && cargo clippy -p mur-common --all-targets -- -D warnings`
Then:
```bash
git add mur-common/build.rs mur-common/src/binary_attestation.rs mur-common/src/lib.rs scripts/test-signing-identity.sh .github/workflows/ci.yml
git commit -m "feat(common): binary attestation helper with build-marker gating

codesign requirement binds anchor apple generic + team OU; no-op in dev
builds and off-macOS. Behavioral matrix runs on macOS CI with a throwaway
self-signed identity; negative controls are mandatory.
"
```

### Task 2: Mount at CLI start paths (`mur-core/src/cmd/agent/start.rs`)

**Files:**
- Modify: `mur-core/src/cmd/agent/start.rs`

**Interfaces:**
- Consumes: `mur_common::binary_attestation::verify_runtime_signature`
- Produces: `fn verify_runtime_at(symlink: &Path) -> Result<()>` — canonicalizes then verifies; used by all three paths inside `cmd_start`.

- [ ] **Step 1: Write the failing unit test** (`start.rs` has no test module today — append `#[cfg(test)] mod tests` at the end of the file)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn verify_runtime_at_surfaces_resolution_errors() {
        // Even in a dev build (where verification is a no-op), a target that
        // cannot be resolved must fail — the mount never spawns blind.
        let err = verify_runtime_at(Path::new("/nonexistent/mur_agent_nope")).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/mur_agent_nope"), "{err}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core --lib cmd::agent::start`
Expected: FAIL — `verify_runtime_at` not defined.

- [ ] **Step 3: Implement** — add after `cmd_start`'s helper imports:

```rust
/// Resolve a runtime symlink (canonicalizing through symlinks and the
/// /var → /private/var redirect) and verify its signature. Dev builds
/// verify nothing but still resolve, so a broken target always errors.
fn verify_runtime_at(symlink: &Path) -> Result<()> {
    let real = symlink.canonicalize().with_context(|| format!("resolve {}", symlink.display()))?;
    mur_common::binary_attestation::verify_runtime_signature(&real)
        .map_err(|e| anyhow::anyhow!("{e}"))
}
```

Then call it before each of the three spawn paths inside `cmd_start`:

1. launchd block — before the `launchctl kickstart` command (after `plist.exists()`):
```rust
verify_runtime_at(&resolve_bin_dir()?.join(format!("mur_agent_{name}")))?;
```
2. systemd block — before `systemctl start` (no-op off-macOS; harmless on Linux):
```rust
verify_runtime_at(&resolve_bin_dir()?.join(format!("mur_agent_{name}")))?;
```
3. detached spawn — after the `symlink.exists()` check, before opening logs:
```rust
verify_runtime_at(&symlink)?;
```

- [ ] **Step 4: Run to verify it passes + full mur-core test**

Run: `cargo test -p mur-core --lib cmd::agent::start && cargo test -p mur-core --lib`
Expected: new test passes; existing start-path tests (if any) still pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/start.rs
git commit -m "feat(agent): attest the runtime before every CLI start path

launchd kickstart, systemd start and the detached symlink spawn all go
through verify_runtime_at; a broken or swapped binary refuses to start.
"
```

### Task 3: Mount at the Hub sidecar spawn (`mur-gui-core`)

**Files:**
- Modify: `mur-gui-core/src/sidecar.rs`

**Interfaces:**
- Consumes: `mur_common::binary_attestation::verify_runtime_signature`

- [ ] **Step 1: Write the failing test** (in the existing `#[cfg(test)]` module at the bottom of `sidecar.rs`; `find_runtime_binary` honors the `MUR_AGENT_RUNTIME_BIN` env override and rejects a directory via `is_real_binary`)

```rust
#[test]
fn spawn_runtime_fails_cleanly_on_invalid_target() {
    // A directory is not an executable runtime: the pre-spawn resolution
    // (find → verify → spawn) must fail with the human-readable error, never
    // panic and never spawn. The signature-verification mount lives in this
    // same fail-before-spawn path; its behavior is covered in mur-common
    // with a test requirement (Task 1) — this test pins the structural guard.
    let dir = std::env::temp_dir().join(format!("mur-sidecar-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", &dir) };
    let err = spawn_runtime("attest-test", std::path::Path::new("/tmp")).unwrap_err();
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.contains("agent runtime not found"), "unexpected error: {err}");
}
```

- [ ] **Step 2: Run to verify the baseline**

Run: `cargo test -p mur-gui-core --lib sidecar`
Expected: the test passes against today's code (the guard already exists at find-time) — that is the point: the mount adds a second gate on the same fail-before-spawn path, which a dev build cannot toggle. The release-build path is covered by Task 1's behavioral matrix.

- [ ] **Step 3: Implement** — in `spawn_runtime`, after `find_runtime_binary()`:

```rust
    let runtime_bin = find_runtime_binary().map_err(|e| {
        format!(
            "agent runtime not found ({e}). Reinstall MUR so mur-agent-runtime \
             is available, or run build.sh to install it."
        )
    })?;
    mur_common::binary_attestation::verify_runtime_signature(&runtime_bin).map_err(|e| {
        format!(
            "{e} — the runtime binary may have been swapped (launch-chain \
             protection covers writes, attestation covers swaps). Fix: mur \
             update --restart-agents, or reinstall MUR."
        )
    })?;
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-gui-core --lib`
```bash
git add mur-gui-core/src/sidecar.rs
git commit -m "feat(gui-core): attest the runtime before sidecar spawn

Hub-spawned runtimes are verified identically to CLI spawns; a swapped
binary surfaces as a human-readable error in the Hub UI.
"
```

### Task 4: Release pipeline — sign the runtime and set the marker

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `docs/superpowers/specs/2026-08-12-binary-attestation-design.md` (correct the pipeline section: the tar.gz is assembled in the matrix job, so signing happens there, not in the PKG job)

**Facts the task relies on (all verified 2026-08-12):**
- The tar.gz (the brew release asset) is built in the `Build (aarch64-apple-darwin)` matrix row — the only macOS matrix row (x86_64-apple-darwin is commented out) — and packaged unsigned at `Package (Unix)` → `tar czf ... mur-agent-runtime` (line 152).
- `Package macOS (DMG + PKG)` extracts that tar.gz but its `pkg-root` does **not** contain the runtime — signing there would not reach the shipped artifact.
- The Hub job builds sidecars and copies the runtime to `mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin` (step `Build Hub UI + mur sidecars (parallel)`, lines 382-392) before the `Build, sign, bundle .dmg` step; certs are imported at `Import Apple signing certs` (lines 434-438).
- Cert import action is `apple-actions/import-codesign-certs@v2` with `p12-file-base64: ${{ secrets.APPLE_SIGNING_CERT }}` and `p12-password: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}` (both existing usages agree).
- Codesign identity pattern: `codesign --force --options runtime --timestamp --sign "Developer ID Application: $APPLE_TEAM_NAME ($APPLE_TEAM_ID)"` with `APPLE_TEAM_NAME`/`APPLE_TEAM_ID` set in step env (existing usages at lines 221-233, 280-284).

- [ ] **Step 1: Amend the spec's Pipeline section** — replace the three bullets with:

```markdown
### Pipeline changes (`.github/workflows/release.yml`)

1. `Build (aarch64-apple-darwin)` matrix row: import the Apple certs, sign
   `target/aarch64-apple-darwin/release/mur-agent-runtime` in place before the
   tar.gz is assembled (the tar.gz is the brew release asset and carries the
   signed runtime), and export `MUR_EMBED_RELEASE_MARKER=1` +
   `MUR_APPLE_TEAM_ID` for the cargo build.
2. Hub job: export the same two env vars for the sidecar build, and sign the
   copied `mur-agent-runtime-aarch64-apple-darwin` sidecar before the tauri
   bundling step.
3. The PKG/DMG job is unchanged — its pkg-root does not contain the runtime.
```

- [ ] **Step 2: Matrix job — marker env (macOS row only)**

Add a step before `Build (native)` (line 132):

```yaml
      - name: Set release-marker env (macOS)
        if: matrix.target == 'aarch64-apple-darwin'
        run: |
          echo "MUR_EMBED_RELEASE_MARKER=1" >> "$GITHUB_ENV"
          echo "MUR_APPLE_TEAM_ID=${{ secrets.APPLE_TEAM_ID }}" >> "$GITHUB_ENV"
```

(Do not set the marker on Linux/Windows rows — verification is a no-op there, but the Team ID secret should not reach those runners.)

- [ ] **Step 3: Matrix job — import certs + sign the runtime (macOS row only)**

Add after the `Build (native)` step (line 136) and before `Package (Unix)` (line 144). Use the exact input names from the existing usages:

```yaml
      - name: Import Apple signing certs (macOS)
        if: matrix.target == 'aarch64-apple-darwin'
        uses: apple-actions/import-codesign-certs@v2
        with:
          p12-file-base64: ${{ secrets.APPLE_SIGNING_CERT }}
          p12-password: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}

      - name: Sign mur-agent-runtime (macOS)
        if: matrix.target == 'aarch64-apple-darwin'
        run: |
          codesign --force --options runtime --timestamp \
            --sign "Developer ID Application: $APPLE_TEAM_NAME ($APPLE_TEAM_ID)" \
            target/aarch64-apple-darwin/release/mur-agent-runtime
          codesign --verify --verbose target/aarch64-apple-darwin/release/mur-agent-runtime
        env:
          APPLE_TEAM_NAME: ${{ secrets.APPLE_TEAM_NAME }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
```

- [ ] **Step 4: Hub job — marker env + sign the sidecar**

In the Hub job, add an env block to the `Build Hub UI + mur sidecars (parallel)` step (line 382, which currently has no env):

```yaml
        env:
          MUR_EMBED_RELEASE_MARKER: '1'
          MUR_APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
```

And a new step between `Import Apple signing certs` (line 434) and `Build, sign, bundle .dmg` (line 440) — the certs must be in the keychain first:

```yaml
      - name: Sign agent-runtime sidecar
        run: |
          codesign --force --options runtime --timestamp \
            --sign "Developer ID Application: $APPLE_TEAM_NAME ($APPLE_TEAM_ID)" \
            mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin
          codesign --verify --verbose mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin
        env:
          APPLE_TEAM_NAME: ${{ secrets.APPLE_TEAM_NAME }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
```

- [ ] **Step 5: Validate the workflow YAML parses**

Run: `ruby -e "require 'yaml'; YAML.load_file('.github/workflows/release.yml'); puts 'yaml ok'"` (ruby + YAML ship on macOS; pyyaml is not reliably present)
Expected: `yaml ok`. Then `cargo fmt --check` is unaffected (no Rust change) — skip it.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml docs/superpowers/specs/2026-08-12-binary-attestation-design.md
git commit -m "ci(release): sign mur-agent-runtime and set the attestation marker

The tar.gz is assembled in the matrix job, so that is where the runtime is
signed; the Hub sidecar is signed before bundling. Marker env is exported
only for the macOS rows (Build matrix + Hub), keeping dev and non-macOS
builds unverified per the spec.
"
```

### Task 5: Docs — README mention

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add one sentence to the launch-chain grant bullet**

Find the bullet added by #924 (mentions `runtime-doctor` and "decide what starts next") and extend it:

```markdown
and the runtime binary itself is signed by MUR's Developer ID in release
builds — a swapped binary is refused at spawn, never run.
```

(Adjust wording to the bullet's existing voice; do not create a new bullet if the grant bullet already covers the launch chain.)

- [ ] **Step 2: Verify + commit**

Run: `grep -n "attest\|swapped" README.md` (one hit expected)
```bash
git add README.md
git commit -m "docs: mention runtime attestation next to the launch-chain grant bullet"
```

### Task 6: Full gate + PR

- [ ] **Step 1: Full gate**

```bash
cargo fmt --check
cargo test --workspace --all-targets   # ORT_STRATEGY=download on this machine; nextest for mur-core per repo convention
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all green, no new warnings.

- [ ] **Step 2: Verify the diff contains only intended files**

```bash
git log --oneline main..HEAD
git diff --stat main..HEAD
```

Expected: 6 commits (spec + 5 tasks), files: mur-common (build.rs, binary_attestation.rs, lib.rs), scripts/test-signing-identity.sh, .github/workflows/ci.yml, mur-core/src/cmd/agent/start.rs, mur-gui-core/src/sidecar.rs, .github/workflows/release.yml, spec, README.md.

- [ ] **Step 3: Push + open the PR** (per repo convention: title `feat: binary attestation (launch-chain follow-on)`, body summarizing the six commits and noting the negative-control rule; do NOT auto-merge — supervised PRs merge after review)
