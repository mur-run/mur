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
use std::path::{Path, PathBuf};

/// Resolve `command` to an absolute path on disk.
///
/// - If `command` is already absolute or contains a path separator, canonicalize
///   it (resolves symlinks).
/// - Otherwise consult `PATH` (and try a `.exe` suffix on Windows). Returns the
///   first match found, canonicalized.
///
/// Returns an error if the binary can't be located.
pub fn resolve_command(command: &str) -> Result<PathBuf> {
    let p = Path::new(command);
    if p.is_absolute() || command.contains('/') || command.contains('\\') {
        return p
            .canonicalize()
            .with_context(|| format!("canonicalize {command}"));
    }
    let path_var = std::env::var_os("PATH")
        .ok_or_else(|| anyhow::anyhow!("PATH env var unset; cannot resolve `{command}`"))?;
    for dir in std::env::split_paths(&path_var) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
