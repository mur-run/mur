//! Task 3.6b: install the pinned pyright that serena launches via
//! `ls_specific_settings.python.ls_path`, so the agent never runs uv and
//! needs no egress for the Python language server.
//!
//! Same shape as the serena install (3.3): `uv tool install` with
//! `UV_TOOL_DIR` / `UV_TOOL_BIN_DIR` / `UV_PYTHON_INSTALL_DIR` confined to
//! `<mur_home>/tools/pyright/<pin>/`, cwd in that dir so no repo
//! `pyproject.toml` is in scope (requirement 5: install never opens the
//! user's repo). Verification reads uv's install record: the dist-info
//! version must equal the pin. A re-run that verifies skips uv.
//!
//! The wheel bundles `langserver.index.js`, so at run time pyright needs only
//! `node` on `PATH` (measured, row 5); without node it would call `nodeenv`
//! and download, which the plan disables Python for instead.

use super::plan::{PYRIGHT, PYRIGHT_PIN, tool_dir};
use super::serena_install::{
    Installed, Outcome, PYTHON_SUBDIR, SERENA_EXCLUDE_NEWER, managed_interpreter, pin_uv,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// PyPI / dist name uv records the tool under.
const PACKAGE: &str = "pyright";
/// Under the managed dir: uv's tool environments and entry-point links (the
/// fetched Python goes to `PYTHON_SUBDIR`, shared with the serena install).
const UV_TOOL_SUBDIR: &str = "uv-tools";
const BIN_SUBDIR: &str = "bin";
/// The entry point serena launches (`ls_path`).
const ENTRY_POINT: &str = "pyright-langserver";
/// Dependency resolution cutoff: the serena pin's date, the environment
/// serena's `PYRIGHT_VERSION` was chosen against. Bump with either pin.
pub const PYRIGHT_EXCLUDE_NEWER: &str = SERENA_EXCLUDE_NEWER;
/// dist-info lives at `<env>/lib/pythonX.Y/site-packages/` (unix) or
/// `<env>/Lib/site-packages/` (Windows).
const DIST_INFO_SEARCH_DEPTH: usize = 5;

/// What the setup manifest records for pyright.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    pub exclude_newer: String,
    /// The `ls_path` written into `serena_config.yml`.
    pub bin: PathBuf,
}

impl Record {
    pub fn for_dir(dir: &Path) -> Self {
        Self {
            version: PYRIGHT_PIN.into(),
            exclude_newer: PYRIGHT_EXCLUDE_NEWER.into(),
            bin: langserver_path_in(dir),
        }
    }
}

/// `<mur_home>/tools/pyright/<pin>/` — the planner's install-row dir.
pub fn pyright_dir(mur_home: &Path) -> PathBuf {
    tool_dir(mur_home, PYRIGHT, PYRIGHT_PIN)
}

/// The entry point uv links into `<dir>/bin/` (`.exe` shim on Windows).
pub fn langserver_path_in(dir: &Path) -> PathBuf {
    let name = format!("{ENTRY_POINT}{}", std::env::consts::EXE_SUFFIX);
    dir.join(BIN_SUBDIR).join(name)
}

/// The exact uv invocation. `--no-config` keeps a stray `uv.toml` from
/// redirecting the index; `pin_uv` does the same for inherited env.
pub fn install_command(uv: &Path, dir: &Path, reinstall: bool) -> Command {
    let mut c = Command::new(uv);
    c.current_dir(dir)
        .env("UV_TOOL_DIR", dir.join(UV_TOOL_SUBDIR))
        .env("UV_TOOL_BIN_DIR", dir.join(BIN_SUBDIR))
        .env("UV_PYTHON_INSTALL_DIR", dir.join(PYTHON_SUBDIR))
        .args(["tool", "install", "--no-config"])
        // Without this uv takes the first Python on PATH (a conda or system
        // one), and setup would then grant exec of that interpreter's whole
        // `bin` dir. A uv-managed Python lands in `<dir>/python/`.
        .args(["--python-preference", "only-managed"])
        .args(["--exclude-newer", PYRIGHT_EXCLUDE_NEWER]);
    pin_uv(&mut c);
    if reinstall {
        c.args(["--force", "--reinstall"]);
    }
    c.arg(format!("{PACKAGE}=={PYRIGHT_PIN}"));
    c
}

/// Judge `dir` by uv's install record, not by trusting the path.
pub fn installed_state(dir: &Path) -> Installed {
    if !langserver_path_in(dir).is_file() {
        return Installed::Missing;
    }
    match recorded_version(dir) {
        None => Installed::Missing,
        Some(v) if v == PYRIGHT_PIN && managed_interpreter(&env_dir(dir), dir) => {
            Installed::Verified
        }
        Some(_) => Installed::Mismatch,
    }
}

/// uv's tool env for pyright.
fn env_dir(dir: &Path) -> PathBuf {
    dir.join(UV_TOOL_SUBDIR).join(PACKAGE)
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
        .with_context(|| {
            format!(
                "run {} (uv is a pyright install prerequisite)",
                uv.display()
            )
        })?;
    if !status.success() {
        bail!("uv tool install {PACKAGE}=={PYRIGHT_PIN} failed: {status}");
    }
    match installed_state(dir) {
        Installed::Verified => Ok((Outcome::Installed, Record::for_dir(dir))),
        _ => bail!(
            "installed {PACKAGE} does not match the pin {PYRIGHT_PIN} (read from {})",
            dir.display()
        ),
    }
}

/// Version from the `pyright-<v>.dist-info` dir name.
fn recorded_version(dir: &Path) -> Option<String> {
    let prefix = format!("{PACKAGE}-");
    let env = dir.join(UV_TOOL_SUBDIR).join(PACKAGE);
    walkdir::WalkDir::new(env)
        .max_depth(DIST_INFO_SEARCH_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .find_map(|e| {
            let name = e.file_name().to_str()?;
            let v = name.strip_prefix(&prefix)?.strip_suffix(".dist-info")?;
            Some(v.to_string())
        })
}

#[cfg(test)]
#[path = "pyright_install_tests.rs"]
mod tests;
