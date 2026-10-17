//! #1945: pre-install the pinned intelephense that serena launches through
//! the canonical `ls_base_cmd` (`[node, <entry .js>]`), so the PHP language
//! server runs as a script node loads rather than as the shebang
//! `.bin/intelephense` under the agent-writable `SERENA_HOME`, which the
//! macOS seal refuses to exec.
//!
//! Same shape as the pyright install (3.6b): a fixed dir under
//! `<mur_home>/tools/`, cwd in that dir so no repo `package.json` or `.npmrc`
//! is in scope, and verification by reading what npm installed (the
//! package's own `package.json` version must equal the pin). A re-run that
//! verifies skips npm. `--ignore-scripts` keeps dependency lifecycle scripts
//! from running at install time.

use anyhow::{Context, Result, bail};
use mur_common::serena_ls_cmd::{
    INTELEPHENSE_PACKAGE, INTELEPHENSE_PIN, intelephense_dir, intelephense_entry,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub use super::serena_install::{Installed, Outcome};

/// npm env that could redirect the install (registry, prefix, global
/// config); cleared so only the command line decides.
const NPM_STEERING_ENV: [&str; 4] = [
    "NPM_CONFIG_PREFIX",
    "NPM_CONFIG_GLOBALCONFIG",
    "NPM_CONFIG_USERCONFIG",
    "NPM_CONFIG_REGISTRY",
];
/// npm's public registry; a user `.npmrc` must not pick another one.
const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
/// An empty userconfig so `~/.npmrc` is not read.
const EMPTY_NPMRC: &str = ".npmrc-empty";

/// What the setup manifest records for intelephense.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    /// The `.js` the canonical `ls_base_cmd` runs.
    pub entry: PathBuf,
}

impl Record {
    pub fn for_tools(tools_dir: &Path) -> Self {
        Self {
            version: INTELEPHENSE_PIN.into(),
            entry: intelephense_entry(tools_dir),
        }
    }
}

/// `<mur_home>/tools`.
pub fn tools_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(mur_common::config::MUR_TOOLS_DIR)
}

/// The exact npm invocation.
pub fn install_command(npm: &Path, dir: &Path) -> Command {
    let mut c = Command::new(npm);
    c.current_dir(dir);
    for k in NPM_STEERING_ENV {
        c.env_remove(k);
    }
    c.args(["install", "--prefix"])
        .arg(dir)
        .arg("--userconfig")
        .arg(dir.join(EMPTY_NPMRC))
        .args(["--registry", NPM_REGISTRY])
        .args([
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--save-exact",
        ])
        .arg(format!("{INTELEPHENSE_PACKAGE}@{INTELEPHENSE_PIN}"));
    c
}

/// Judge `tools_dir` by what npm installed, not by trusting the path.
pub fn installed_state(tools_dir: &Path) -> Installed {
    if !intelephense_entry(tools_dir).is_file() {
        return Installed::Missing;
    }
    match recorded_version(&intelephense_dir(tools_dir)) {
        None => Installed::Missing,
        Some(v) if v == INTELEPHENSE_PIN => Installed::Verified,
        Some(_) => Installed::Mismatch,
    }
}

fn recorded_version(dir: &Path) -> Option<String> {
    let manifest = dir
        .join("node_modules")
        .join(INTELEPHENSE_PACKAGE)
        .join("package.json");
    let text = std::fs::read(manifest).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&text).ok()?;
    v.get("version")?.as_str().map(str::to_owned)
}

/// Install (or replace a mismatched install) and verify the result.
pub fn install_with(npm: &Path, tools_dir: &Path) -> Result<(Outcome, Record)> {
    let state = installed_state(tools_dir);
    if state == Installed::Verified {
        return Ok((Outcome::AlreadyInstalled, Record::for_tools(tools_dir)));
    }
    let dir = intelephense_dir(tools_dir);
    if state == Installed::Mismatch {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    std::fs::write(dir.join(EMPTY_NPMRC), b"")
        .with_context(|| format!("write {}", dir.join(EMPTY_NPMRC).display()))?;
    let status = install_command(npm, &dir).status().with_context(|| {
        format!(
            "run {} (npm is an intelephense install prerequisite)",
            npm.display()
        )
    })?;
    if !status.success() {
        bail!("npm install {INTELEPHENSE_PACKAGE}@{INTELEPHENSE_PIN} failed: {status}");
    }
    match installed_state(tools_dir) {
        Installed::Verified => Ok((Outcome::Installed, Record::for_tools(tools_dir))),
        _ => bail!(
            "installed {INTELEPHENSE_PACKAGE} does not match the pin {INTELEPHENSE_PIN} \
             (read from {})",
            dir.display()
        ),
    }
}

#[cfg(test)]
#[path = "intelephense_install_tests.rs"]
mod tests;
