//! Shared executable-path resolution.
//!
//! A single source of truth for turning a command (bare program name or path)
//! into the absolute, symlink-resolved binary that will actually be executed.
//! Used by both install-time MCP pinning (`mur agent mcp pin`) and the runtime
//! startup verification (B0 rules 6 & 11) so a bare `command` like `node`
//! resolves identically across the two passes — otherwise the runtime hashes a
//! CWD-relative path that doesn't exist and silently skips the pin/signature
//! check while `Command::new` runs the PATH-resolved binary.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// File name of the MUR MCP server binary.
#[cfg(windows)]
const MCP_SERVER_BIN: &str = "mur-mcp-server.exe";
#[cfg(not(windows))]
const MCP_SERVER_BIN: &str = "mur-mcp-server";

/// Canonical location MUR keeps its own copy of the MCP server binary:
/// `~/.mur/mcp-servers/mur-mcp-server` (honors `$MUR_HOME`). Stable across how
/// `mur` itself was installed (brew / cargo / source) and across upgrades, so
/// agent profiles can pin this path once and never go stale.
pub fn bundled_mcp_server_path() -> PathBuf {
    crate::trust::mur_home()
        .join("mcp-servers")
        .join(MCP_SERVER_BIN)
}

/// Ensure [`bundled_mcp_server_path`] exists and matches the `mur-mcp-server`
/// shipped alongside the running `mur` binary, copying it into place when
/// missing or out of date. Returns the canonical target path.
///
/// Source resolution: the sibling of the current executable first (brew, cargo
/// and source builds all colocate the two binaries), then `mur-mcp-server` on
/// `PATH`. If no source is found but a copy already exists, that copy is
/// returned (usable, just can't self-update). Errors only when there is neither
/// a source nor an existing copy.
///
/// Call this BEFORE the kernel sandbox seals — the copy needs write access to
/// `~/.mur`.
pub fn ensure_bundled_mcp_server() -> Result<PathBuf> {
    let target = bundled_mcp_server_path();
    match locate_mcp_server_source() {
        Some(src) => {
            install_if_stale(&src, &target)?;
            Ok(target)
        }
        None if target.is_file() => Ok(target),
        None => bail!(
            "mur-mcp-server not found next to `mur` or on PATH, and no copy at {}",
            target.display()
        ),
    }
}

/// The `mur-mcp-server` to copy from: sibling of `mur` first, then PATH.
fn locate_mcp_server_source() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(MCP_SERVER_BIN);
        if sibling.is_file() {
            return sibling.canonicalize().ok();
        }
    }
    resolve_command(MCP_SERVER_BIN).ok()
}

/// Copy `src` to `target` unless `target` already byte-matches it. Idempotent;
/// writes via a uniquely-named temp file + rename in the target dir so the swap
/// is atomic and never leaves a half-written binary an agent might try to spawn;
/// sets mode 0755 on unix.
fn install_if_stale(src: &Path, target: &Path) -> Result<()> {
    if target.is_file() && sha256_file(src)? == sha256_file(target)? {
        return Ok(());
    }
    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target {} has no parent", target.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    // Unique temp name so two agents starting at once don't clobber each other.
    let tmp = dir.join(format!(".{MCP_SERVER_BIN}.{}.tmp", std::process::id()));
    std::fs::copy(src, &tmp)
        .with_context(|| format!("copy {} -> {}", src.display(), tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

/// Stream-hash `path` SHA-256 (64 KiB chunks; lowercase hex).
fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Launchers that run *other* code: hashing one of these tells you nothing
/// about the MCP server it starts.
const INTERPRETERS: &[&str] = &[
    "npx", "node", "bunx", "bun", "deno", "python", "python3", "uv", "uvx", "pipx", "ruby", "perl",
    "sh", "bash", "zsh",
];

/// Whether `command` launches an MCP server through an interpreter or package
/// runner rather than being the server binary itself.
///
/// This decides whether a `binary_sha256` pin means anything. For
/// `command: npx, args: @yawlabs/fetch-mcp` the pin hashes **npx** — so it
/// breaks on every unrelated Node upgrade while saying nothing at all about
/// `@yawlabs/fetch-mcp`, which npx resolves and may fetch fresh at run time.
/// Enforcing such a pin is both fragile and hollow; the honest report is that
/// the server code is unprotected.
///
/// Real coverage for these needs a package-level pin (version + integrity),
/// which is a different mechanism than hashing a file on disk.
pub fn is_interpreter_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or(command);
    let stem = Path::new(first)
        .file_stem() // also strips .exe / .cmd on Windows
        .and_then(|s| s.to_str())
        .unwrap_or(first);
    INTERPRETERS.contains(&stem.to_ascii_lowercase().as_str())
}

/// Resolve `command` to an absolute path on disk.
///
/// - If `command` is already absolute or contains a path separator, canonicalize
///   it (resolves symlinks).
/// - Otherwise consult `PATH` (and try a `.exe` suffix on Windows). Returns the
///   first match found, canonicalized.
///
/// Returns an error if the binary can't be located.
pub fn resolve_command(command: &str) -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH")
        .ok_or_else(|| anyhow::anyhow!("PATH env var unset; cannot resolve `{command}`"))?;
    resolve_command_in(&path_var, command)
}

/// [`resolve_command`] against an explicit PATH value instead of the ambient
/// env. The runtime resolves MCP commands against [`augmented_path_var`] for
/// BOTH the B0 admission checks and the actual spawn, so the file that gets
/// hashed is provably the file that gets exec'd.
pub fn resolve_command_in(path_var: &std::ffi::OsStr, command: &str) -> Result<PathBuf> {
    let p = Path::new(command);
    if p.is_absolute() || command.contains('/') || command.contains('\\') {
        return p
            .canonicalize()
            .with_context(|| format!("canonicalize {command}"));
    }
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .with_context(|| format!("canonicalize {}", candidate.display()));
        }
        #[cfg(target_os = "windows")]
        {
            let with_exe = dir.join(format!("{command}.exe"));
            if with_exe.is_file() {
                return with_exe
                    .canonicalize()
                    .with_context(|| format!("canonicalize {}", with_exe.display()));
            }
        }
    }
    bail!("could not find `{command}` on PATH");
}

/// The ambient PATH plus the well-known install dirs that GUI/launchd parents
/// omit (`/opt/homebrew/bin`, `/usr/local/bin`, `~/.local/bin`).
///
/// MCP entries store the command as the user typed it (`node`, `uvx`,
/// `python3`); which binary that names must not depend on WHO spawned the
/// runtime. A terminal hands it the user's full PATH and
/// `mur agent install-service` derives a rich one into the unit file — but a
/// Hub-spawned sidecar inherits the GUI's minimal PATH, which is how
/// `command: node` works in a shell and dies under the Hub with nothing
/// pointing here. Ambient entries keep priority; only missing standard dirs
/// are appended, so an explicit PATH override still wins.
pub fn augmented_path_var() -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs_list: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut extras: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        extras.push(home.join(".local/bin"));
    }
    for e in extras {
        if !dirs_list.contains(&e) {
            dirs_list.push(e);
        }
    }
    std::env::join_paths(dirs_list).unwrap_or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_commands_are_recognised_including_paths_and_args() {
        for c in [
            "npx",
            "node",
            "python3",
            "uvx",
            "bunx",
            "deno",
            "sh",
            "/opt/homebrew/bin/npx",
            "npx @yawlabs/fetch-mcp",
            "NPX",
            "npx.cmd",
        ] {
            assert!(
                is_interpreter_command(c),
                "`{c}` should count as an interpreter"
            );
        }
    }

    #[test]
    fn real_server_binaries_are_not_interpreters() {
        for c in [
            "mur-mcp-server",
            "/Users/x/.mur/mcp-servers/mur-mcp-server",
            "agent-browser",
            "mur-research-gateway",
            "nodemon-ish",
        ] {
            assert!(!is_interpreter_command(c), "`{c}` is the server itself");
        }
    }

    #[test]
    fn errors_on_missing_binary() {
        assert!(resolve_command("definitely-not-a-real-binary-xyz123").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_bare_program_on_path_to_absolute() {
        // The whole point: a bare program name resolves to an absolute path.
        // (The runtime pin check used to open it relative to CWD and soft-fail.)
        let resolved = resolve_command("sh").expect("sh is on PATH");
        assert!(
            resolved.is_absolute(),
            "expected absolute, got {resolved:?}"
        );
        assert!(resolved.exists());
    }

    #[test]
    fn absolute_path_is_canonicalized() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let resolved = resolve_command(tmp.path().to_str().unwrap()).unwrap();
        assert!(resolved.is_absolute());
    }

    #[test]
    fn augmented_path_appends_standard_dirs_without_reordering_ambient() {
        // Read-only against the ambient env (other tests resolve on PATH in
        // parallel, so no set_var here).
        let ambient: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
        let aug: Vec<PathBuf> = std::env::split_paths(&augmented_path_var()).collect();
        assert!(
            aug.starts_with(&ambient),
            "ambient PATH must keep priority: {aug:?}"
        );
        for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
            let d = PathBuf::from(d);
            let in_ambient = ambient.iter().filter(|x| **x == d).count();
            let in_aug = aug.iter().filter(|x| **x == d).count();
            // Appended when absent; an ambient PATH that already lists it
            // (even more than once) is passed through untouched.
            assert_eq!(
                in_aug,
                in_ambient.max(1),
                "{d:?}: expected append-only-when-absent"
            );
        }
    }

    #[test]
    fn resolve_command_in_uses_the_given_path_not_the_env() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fake-mcp");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        let var = std::env::join_paths([dir.path().to_path_buf()]).unwrap();
        let found = resolve_command_in(&var, "fake-mcp").unwrap();
        assert_eq!(found, exe.canonicalize().unwrap());
        assert!(
            resolve_command_in(std::ffi::OsStr::new(""), "fake-mcp").is_err(),
            "an empty path var must not fall back to the ambient PATH"
        );
    }

    #[test]
    fn install_if_stale_copies_then_is_idempotent_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-bin");
        let target = dir.path().join("mcp-servers/mur-mcp-server"); // parent must be created
        std::fs::write(&src, b"v1").unwrap();

        // Missing target -> copied.
        install_if_stale(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "target must be executable");
        }

        // Unchanged source -> no-op, still v1.
        install_if_stale(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");

        // Updated source -> refreshed.
        std::fs::write(&src, b"v2-newer").unwrap();
        install_if_stale(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2-newer");
    }
}
