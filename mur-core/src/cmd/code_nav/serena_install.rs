//! Task 3.3: install the pinned serena-agent (decision P3-D2).
//!
//! `uv tool install` with `UV_TOOL_DIR` / `UV_TOOL_BIN_DIR` /
//! `UV_PYTHON_INSTALL_DIR` confined to `<mur_home>/tools/serena/<pin>/`,
//! with `--python-preference only-managed` so the venv's interpreter is a
//! uv-managed one under that dir rather than whatever Python is on PATH. `serena-agent` `2.0.0.dev0` is not on
//! PyPI (latest there is 1.x), so the pin is the exact upstream commit every
//! Phase 0/2 finding and the item 17 matrix ran against, plus a dependency
//! resolution cutoff at that commit's date so transitive versions cannot
//! drift forward. MUR does not install uv itself.
//!
//! Verification reads uv's own install record (`direct_url.json` in the
//! serena-agent dist-info): the version in the dist-info name and the
//! resolved commit must both equal the pin. A re-run that verifies skips uv.

use super::plan::{SERENA, SERENA_PIN, tool_dir};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// PyPI / dist name uv records the tool under.
const PACKAGE: &str = "serena-agent";
const SERENA_GIT_URL: &str = "https://github.com/oraios/serena";
/// Upstream commit whose `pyproject.toml` declares version `SERENA_PIN`.
/// Bump together with `SERENA_PIN` and `SERENA_EXCLUDE_NEWER`.
pub const SERENA_GIT_REV: &str = "8a3ce35cae29a93748842ba463231d6c78d181e7";
/// Committer date of `SERENA_GIT_REV`: uv ignores any dependency release
/// published after it.
pub const SERENA_EXCLUDE_NEWER: &str = "2026-09-29T10:49:50Z";
/// Under the managed dir: uv's tool environments, and its entry-point links.
const UV_TOOL_SUBDIR: &str = "uv-tools";
const BIN_SUBDIR: &str = "bin";
/// Where uv puts the Python it fetches for the tool env.
pub(super) const PYTHON_SUBDIR: &str = "python";
/// The venv metadata file naming the base interpreter.
pub(super) const PYVENV_CFG: &str = "pyvenv.cfg";
const ENTRY_POINT: &str = "serena";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Missing,
    Verified,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    AlreadyInstalled,
}

/// What task 3.6 writes into the setup manifest for serena.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    pub git_rev: String,
    pub exclude_newer: String,
    pub bin: PathBuf,
}

impl Record {
    pub fn for_dir(dir: &Path) -> Self {
        Self {
            version: SERENA_PIN.into(),
            git_rev: SERENA_GIT_REV.into(),
            exclude_newer: SERENA_EXCLUDE_NEWER.into(),
            bin: serena_binary_path_in(dir),
        }
    }
}

/// `<mur_home>/tools/serena/<pin>/` — the planner's install-row dir.
pub fn serena_dir(mur_home: &Path) -> PathBuf {
    tool_dir(mur_home, SERENA, SERENA_PIN)
}

/// The entry point uv links into `<dir>/bin/` (`.exe` shim on Windows).
pub fn serena_binary_path_in(dir: &Path) -> PathBuf {
    let name = format!("{ENTRY_POINT}{}", std::env::consts::EXE_SUFFIX);
    dir.join(BIN_SUBDIR).join(name)
}

/// The exact uv invocation. `--no-config` keeps a stray `uv.toml` from
/// redirecting the index; cwd is the managed dir so no repo
/// `pyproject.toml` is in scope.
pub fn install_command(uv: &Path, dir: &Path, reinstall: bool) -> Command {
    let mut c = Command::new(uv);
    c.current_dir(dir)
        .env("UV_TOOL_DIR", dir.join(UV_TOOL_SUBDIR))
        .env("UV_TOOL_BIN_DIR", dir.join(BIN_SUBDIR))
        .env("UV_PYTHON_INSTALL_DIR", dir.join(PYTHON_SUBDIR))
        .args(["tool", "install", "--no-config"])
        // Without this uv reuses its global managed Python (or the first one
        // on PATH), and setup would then grant exec of that shared `bin` dir.
        .args(["--python-preference", "only-managed"])
        .args(["--exclude-newer", SERENA_EXCLUDE_NEWER]);
    if reinstall {
        c.args(["--force", "--reinstall"]);
    }
    c.arg(format!("{PACKAGE} @ git+{SERENA_GIT_URL}@{SERENA_GIT_REV}"));
    c
}

/// Judge `dir` by uv's install record, not by trusting the path.
pub fn installed_state(dir: &Path) -> Installed {
    if !serena_binary_path_in(dir).is_file() {
        return Installed::Missing;
    }
    let Some((version, commit)) = recorded_pin(dir) else {
        return Installed::Missing;
    };
    if version == SERENA_PIN
        && commit.as_deref() == Some(SERENA_GIT_REV)
        && managed_interpreter(&dir.join(UV_TOOL_SUBDIR).join(PACKAGE), dir)
    {
        Installed::Verified
    } else {
        Installed::Mismatch
    }
}

/// The tool venv at `env`'s base interpreter (`home` in `pyvenv.cfg`) is the
/// uv-managed one under `<dir>/python/`. An install made before
/// `only-managed` points elsewhere; treating it as a mismatch makes the next
/// run reinstall instead of granting exec of that foreign `bin` dir. Shared
/// with the pyright install.
pub(super) fn managed_interpreter(env: &Path, dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(env.join(PYVENV_CFG)) else {
        return false;
    };
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "home")
        .is_some_and(|(_, v)| Path::new(v.trim()).starts_with(dir.join(PYTHON_SUBDIR)))
}

/// Install (or replace a mismatched install) and verify the result.
pub fn install_with(uv: &Path, dir: &Path) -> Result<(Outcome, Record)> {
    let state = installed_state(dir);
    if state == Installed::Verified {
        return Ok((Outcome::AlreadyInstalled, Record::for_dir(dir)));
    }
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let status = install_command(uv, dir, state == Installed::Mismatch)
        .status()
        .with_context(|| format!("run {} (uv is a serena prerequisite)", uv.display()))?;
    if !status.success() {
        bail!("uv tool install {PACKAGE} failed: {status}");
    }
    match installed_state(dir) {
        Installed::Verified => Ok((Outcome::Installed, Record::for_dir(dir))),
        _ => bail!(
            "installed {PACKAGE} does not match the pin {SERENA_PIN} @ {SERENA_GIT_REV} \
             (read from {})",
            dir.display()
        ),
    }
}

/// (version from the dist-info dir name, git commit from `direct_url.json`).
fn recorded_pin(dir: &Path) -> Option<(String, Option<String>)> {
    let prefix = format!("{}-", PACKAGE.replace('-', "_"));
    let env = dir.join(UV_TOOL_SUBDIR).join(PACKAGE);
    walkdir::WalkDir::new(env)
        .max_depth(5)
        .into_iter()
        .filter_map(|e| e.ok())
        .find_map(|e| {
            let name = e.file_name().to_str()?;
            let version = name.strip_prefix(&prefix)?.strip_suffix(".dist-info")?;
            if !e.file_type().is_dir() {
                return None;
            }
            let commit = std::fs::read(e.path().join("direct_url.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["vcs_info"]["commit_id"].as_str().map(String::from));
            Some((version.to_string(), commit))
        })
}

#[cfg(test)]
#[path = "serena_install_tests.rs"]
mod tests;
