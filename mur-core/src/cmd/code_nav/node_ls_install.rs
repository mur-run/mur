//! Pre-install a pinned node-run language server that serena launches
//! through the canonical `ls_base_cmd` (`[node, <entry .js>]`), so the
//! server runs as a script node loads rather than as a shebang `.bin` shim
//! under the agent-writable `SERENA_HOME`, which the macOS seal refuses to
//! exec. Started for intelephense (#1945); generalized for TypeScript
//! (#1981). Which servers exist is the table in
//! [`mur_common::serena_ls_cmd::MANAGED`].
//!
//! Same shape as the pyright install (3.6b): a fixed dir under
//! `<mur_home>/tools/`, cwd in that dir so no repo `package.json` or `.npmrc`
//! is in scope, and verification by reading what npm installed (the
//! server package's own `package.json` version must equal the pin). A
//! re-run that verifies skips npm. `--ignore-scripts` keeps dependency
//! lifecycle scripts from running at install time, and `--cache` keeps
//! npm's cache under the tools root so nothing needs `~/.npm` (#1981).

use anyhow::{Context, Result, bail};
use mur_common::serena_ls_cmd::ManagedNodeLs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub use super::serena_install::{Installed, Outcome};

/// npm env that could redirect the install (registry, prefix, global
/// config, cache); cleared so only the command line decides.
const NPM_STEERING_ENV: [&str; 5] = [
    "NPM_CONFIG_PREFIX",
    "NPM_CONFIG_GLOBALCONFIG",
    "NPM_CONFIG_USERCONFIG",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_CACHE",
];
/// npm's public registry; a user `.npmrc` must not pick another one.
const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
/// An empty userconfig so `~/.npmrc` is not read.
const EMPTY_NPMRC: &str = ".npmrc-empty";
/// npm's cache, under the tools root and shared by every managed server.
const NPM_CACHE_DIR: &str = ".npm-cache";

/// What the setup manifest records for one managed server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: String,
    /// The `.js` the canonical `ls_base_cmd` runs.
    pub entry: PathBuf,
}

impl Record {
    pub fn for_tools(ls: &ManagedNodeLs, tools_dir: &Path) -> Self {
        Self {
            version: ls.pin().into(),
            entry: ls.entry(tools_dir),
        }
    }
}

/// `<mur_home>/tools`.
pub fn tools_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(mur_common::config::MUR_TOOLS_DIR)
}

/// `<tools_dir>/.npm-cache`.
pub fn npm_cache_dir(tools_dir: &Path) -> PathBuf {
    tools_dir.join(NPM_CACHE_DIR)
}

/// The exact npm invocation for `ls`, installing into `dir`
/// (`ls.dir(tools_dir)`, whose grandparent is the tools root).
pub fn install_command(ls: &ManagedNodeLs, npm: &Path, dir: &Path) -> Command {
    let tools = dir.parent().and_then(Path::parent).unwrap_or(dir);
    let mut c = Command::new(npm);
    c.current_dir(dir);
    for k in NPM_STEERING_ENV {
        c.env_remove(k);
    }
    c.args(["install", "--prefix"])
        .arg(dir)
        .arg("--userconfig")
        .arg(dir.join(EMPTY_NPMRC))
        .arg("--cache")
        .arg(npm_cache_dir(tools))
        .args(["--registry", NPM_REGISTRY])
        .args([
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--save-exact",
        ])
        .args(ls.packages.iter().map(|(p, v)| format!("{p}@{v}")));
    c
}

/// Judge `tools_dir` by what npm installed, not by trusting the path.
pub fn installed_state(ls: &ManagedNodeLs, tools_dir: &Path) -> Installed {
    if !ls.entry(tools_dir).is_file() {
        return Installed::Missing;
    }
    let dir = ls.dir(tools_dir);
    let (server, _) = ls.packages[0];
    if recorded_version(&dir, server).is_none() {
        return Installed::Missing;
    }
    // Every package, the server included, must be exactly its pin.
    let pinned = ls
        .packages
        .iter()
        .all(|(p, v)| recorded_version(&dir, p).as_deref() == Some(*v));
    if !pinned {
        return Installed::Mismatch;
    }
    Installed::Verified
}

fn recorded_version(dir: &Path, package: &str) -> Option<String> {
    let manifest = dir.join("node_modules").join(package).join("package.json");
    let text = std::fs::read(manifest).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&text).ok()?;
    v.get("version")?.as_str().map(str::to_owned)
}

/// Install (or replace a mismatched install) and verify the result.
pub fn install_with(ls: &ManagedNodeLs, npm: &Path, tools_dir: &Path) -> Result<(Outcome, Record)> {
    let state = installed_state(ls, tools_dir);
    if state == Installed::Verified {
        return Ok((Outcome::AlreadyInstalled, Record::for_tools(ls, tools_dir)));
    }
    let dir = ls.dir(tools_dir);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    std::fs::write(dir.join(EMPTY_NPMRC), b"")
        .with_context(|| format!("write {}", dir.join(EMPTY_NPMRC).display()))?;
    let (server, pin) = ls.packages[0];
    let status = install_command(ls, npm, &dir).status().with_context(|| {
        format!(
            "run {} (npm is a {server} install prerequisite)",
            npm.display()
        )
    })?;
    if !status.success() {
        bail!("npm install {server}@{pin} failed: {status}");
    }
    match installed_state(ls, tools_dir) {
        Installed::Verified => Ok((Outcome::Installed, Record::for_tools(ls, tools_dir))),
        _ => bail!(
            "installed {server} does not match the pins {:?} (read from {})",
            ls.packages,
            dir.display()
        ),
    }
}

#[cfg(test)]
#[path = "node_ls_install_tests.rs"]
mod tests;
