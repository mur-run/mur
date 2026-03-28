//! `mur deploy` — Docker Compose deployment management.
//!
//! Wraps `docker compose` commands so users can deploy mur server via the CLI
//! without having to remember compose flags. Config is read from a
//! `docker-compose.yml` in the current directory or an explicit `--file`.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ── Config ────────────────────────────────────────────────────────────────────

/// Locate the compose file. Prefers the path supplied via `--file`, then falls
/// back to `docker-compose.yml` / `compose.yml` in the current directory, then
/// tries the default bundled path next to the mur binary.
pub(crate) fn resolve_compose_file(file: Option<&str>) -> Result<PathBuf> {
    if let Some(f) = file {
        let p = PathBuf::from(f);
        if p.exists() {
            return Ok(p);
        }
        bail!("Compose file not found: {}", f);
    }

    // Standard search order (same as docker compose CLI)
    for candidate in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }

    bail!(
        "No docker-compose.yml found in the current directory.\n\
         Run this command from the mur source directory or pass --file <path>."
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build the base `docker compose -f <file>` command.
fn compose_cmd(file: &Path) -> Command {
    let mut cmd = Command::new("docker");
    cmd.arg("compose").arg("-f").arg(file);
    cmd
}

/// Run a compose subcommand, streaming output to the terminal. Returns an error
/// if the process exits non-zero.
fn run(mut cmd: Command) -> Result<()> {
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to run `docker compose` — is Docker installed and running?")?;

    if !status.success() {
        bail!("`docker compose` exited with status {}", status);
    }
    Ok(())
}

// ── Subcommand handlers ───────────────────────────────────────────────────────

/// `mur deploy up [--build] [--detach]`
pub(crate) fn cmd_deploy_up(file: Option<&str>, build: bool, detach: bool) -> Result<()> {
    let compose_file = resolve_compose_file(file)?;
    println!("  Using compose file: {}", compose_file.display());

    let mut cmd = compose_cmd(&compose_file);
    cmd.arg("up");

    if build {
        cmd.arg("--build");
    }
    if detach {
        cmd.arg("--detach");
    }

    run(cmd)
}

/// `mur deploy down [--volumes]`
pub(crate) fn cmd_deploy_down(file: Option<&str>, volumes: bool) -> Result<()> {
    let compose_file = resolve_compose_file(file)?;
    let mut cmd = compose_cmd(&compose_file);
    cmd.arg("down");

    if volumes {
        cmd.arg("--volumes");
    }

    run(cmd)
}

/// `mur deploy status`
pub(crate) fn cmd_deploy_status(file: Option<&str>) -> Result<()> {
    let compose_file = resolve_compose_file(file)?;
    let mut cmd = compose_cmd(&compose_file);
    cmd.arg("ps");
    run(cmd)
}

/// `mur deploy logs [service] [--follow]`
pub(crate) fn cmd_deploy_logs(
    file: Option<&str>,
    service: Option<&str>,
    follow: bool,
) -> Result<()> {
    let compose_file = resolve_compose_file(file)?;
    let mut cmd = compose_cmd(&compose_file);
    cmd.arg("logs");

    if follow {
        cmd.arg("--follow");
    }
    if let Some(svc) = service {
        cmd.arg(svc);
    }

    run(cmd)
}

/// `mur deploy build`
pub(crate) fn cmd_deploy_build(file: Option<&str>) -> Result<()> {
    let compose_file = resolve_compose_file(file)?;
    let mut cmd = compose_cmd(&compose_file);
    cmd.arg("build");
    run(cmd)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_compose_file(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(
            &path,
            "version: '3'\nservices:\n  mur:\n    image: mur:latest\n",
        )
        .unwrap();
        path
    }

    // ── resolve_compose_file ──────────────────────────────────────────────────

    #[test]
    fn explicit_file_found() {
        let dir = TempDir::new().unwrap();
        let path = make_compose_file(&dir, "my-compose.yml");
        let result = resolve_compose_file(Some(path.to_str().unwrap()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), path);
    }

    #[test]
    fn explicit_file_missing_returns_error() {
        let result = resolve_compose_file(Some("/nonexistent/path/compose.yml"));
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("Compose file not found"));
    }

    #[test]
    fn no_file_in_cwd_returns_error() {
        // Run from a temp dir that has no compose file.
        let dir = TempDir::new().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_compose_file(None);

        std::env::set_current_dir(original).unwrap();
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("No docker-compose.yml found"));
    }

    #[test]
    fn discovers_docker_compose_yml_in_cwd() {
        let dir = TempDir::new().unwrap();
        make_compose_file(&dir, "docker-compose.yml");
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_compose_file(None);

        std::env::set_current_dir(original).unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().file_name().unwrap(), "docker-compose.yml");
    }

    #[test]
    fn discovers_compose_yml_in_cwd() {
        let dir = TempDir::new().unwrap();
        make_compose_file(&dir, "compose.yml");
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_compose_file(None);

        std::env::set_current_dir(original).unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().file_name().unwrap(), "compose.yml");
    }

    #[test]
    fn docker_compose_yml_takes_priority_over_compose_yml() {
        let dir = TempDir::new().unwrap();
        make_compose_file(&dir, "docker-compose.yml");
        make_compose_file(&dir, "compose.yml");
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let result = resolve_compose_file(None);

        std::env::set_current_dir(original).unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().file_name().unwrap(), "docker-compose.yml");
    }

    // ── compose_cmd ───────────────────────────────────────────────────────────

    #[test]
    fn compose_cmd_uses_docker_compose_with_file() {
        let path = PathBuf::from("/tmp/docker-compose.yml");
        let cmd = compose_cmd(&path);
        // Program should be "docker"
        assert_eq!(cmd.get_program(), "docker");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args[0], "compose");
        assert_eq!(args[1], "-f");
        assert_eq!(args[2].to_str().unwrap(), "/tmp/docker-compose.yml");
    }
}
