//! `mur agent mcp vendor` — install a package-runner MCP server into a
//! directory MUR owns, so its contents can actually be verified.
//!
//! ## Why this exists
//!
//! `command: npx, args: ["@yawlabs/fetch-mcp"]` resolves through the package
//! manager at every spawn. Nothing about that is pinnable: the binary hash
//! covers `npx` (see `mur_common::exec::is_interpreter_command`), and even a
//! version in the spec only fixes *which release* is requested, not the bytes
//! that end up running.
//!
//! Vendoring moves the install under `~/.mur/mcp-packages/<agent>/<server>/`
//! and rewrites the entry to launch the resolved bin directly with `node`. The
//! agent then starts with no resolution step and no network, from a tree
//! nothing else writes to.
//!
//! ## What the pin covers, and what it doesn't
//!
//! The fingerprint is the SHA-256 of `package-lock.json`, which npm fills with
//! an integrity hash for **every** package in the dependency tree. One small
//! file therefore covers the whole tree, and startup verification costs the
//! same whether `node_modules` is 150 KB or 40 MB.
//!
//! It pins what was *installed*. Editing a file inside `node_modules` after
//! the fact does not change the lockfile, so this detects a re-install or a
//! dependency swap, not post-install tampering with the checked-out files.
//! Catching that needs a full tree hash at every startup; that cost is not
//! obviously worth paying, and pretending otherwise would repeat the mistake
//! this whole line of work exists to fix — claiming coverage that isn't there.
//!
//! Installs run with `--ignore-scripts`. A package's `postinstall` is arbitrary
//! code execution at install time, which is precisely the thing being guarded
//! against; a server that cannot start without its install scripts is not one
//! to vendor silently.

use anyhow::{Context, Result, bail};
use mur_common::agent::{McpPackagePin, McpServerEntry};
use std::path::{Path, PathBuf};

/// Where MUR keeps vendored MCP packages for `agent`/`server`.
pub fn install_dir(mur_home: &Path, agent: &str, server: &str) -> PathBuf {
    mur_home.join("mcp-packages").join(agent).join(server)
}

/// SHA-256 (lowercase hex) of the install's lockfile.
pub fn lockfile_sha256(install_dir: &Path) -> Result<String> {
    let lock = install_dir.join("package-lock.json");
    crate::cmd::agent_mcp_pin::compute_binary_sha256(&lock)
        .with_context(|| format!("hash {}", lock.display()))
}

/// Install `name@version` into `dir` with npm, scripts disabled.
fn npm_install(dir: &Path, name: &str, version: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let spec = format!("{name}@{version}");
    let out = std::process::Command::new("npm")
        .arg("install")
        .arg(&spec)
        .arg("--prefix")
        .arg(dir)
        .args([
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("run npm install: {e} (is npm on PATH?)"))?;
    if !out.status.success() {
        bail!(
            "npm install {spec} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    Ok(())
}

/// Read the package's `bin` entry and return the absolute script path to run.
///
/// npm's `bin` is either a string (single binary named after the package) or a
/// map of names to paths. With several, the one matching the package's own
/// name wins; otherwise the choice is ambiguous and the caller must say which.
fn resolve_bin(dir: &Path, name: &str) -> Result<PathBuf> {
    let pkg_json = dir.join("node_modules").join(name).join("package.json");
    let raw = std::fs::read_to_string(&pkg_json)
        .with_context(|| format!("read {}", pkg_json.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", pkg_json.display()))?;

    let rel =
        match v.get("bin") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Object(map)) => {
                let short = name.rsplit('/').next().unwrap_or(name);
                let pick = map
                .get(short)
                .or_else(|| if map.len() == 1 { map.values().next() } else { None })
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "package `{name}` exposes {} binaries ({}); vendoring needs exactly one",
                        map.len(),
                        map.keys().cloned().collect::<Vec<_>>().join(", "),
                    )
                })?;
                pick.to_string()
            }
            _ => bail!("package `{name}` declares no `bin`, so there is nothing to launch"),
        };

    let abs = dir.join("node_modules").join(name).join(&rel);
    if !abs.is_file() {
        bail!("`bin` points at {}, which does not exist", abs.display());
    }
    abs.canonicalize()
        .with_context(|| format!("canonicalize {}", abs.display()))
}

/// Vendor `entry`'s package: install it, repoint the entry at the installed
/// script, and record the lockfile fingerprint.
///
/// Mutates `entry` only after every fallible step has succeeded, so a failed
/// vendor leaves the agent exactly as it was.
pub fn vendor_entry(
    entry: &mut McpServerEntry,
    mur_home: &Path,
    agent: &str,
    name: &str,
    version: &str,
) -> Result<PathBuf> {
    let dir = install_dir(mur_home, agent, &entry.name);
    npm_install(&dir, name, version)?;
    let bin = resolve_bin(&dir, name)?;
    let lock = lockfile_sha256(&dir)?;

    entry.command = "node".to_string();
    entry.args = vec![bin.display().to_string()];
    entry.binary_sha256 = None; // the node binary's hash was never the point
    entry.package = Some(McpPackagePin {
        runner: "npm".into(),
        name: name.to_string(),
        version: version.to_string(),
        install_dir: dir.display().to_string(),
        lockfile_sha256: lock,
    });
    Ok(dir)
}

/// `mur agent mcp vendor <agent> <server> [--version V] [--force]`
pub fn cmd_mcp_vendor(
    agent: &str,
    server_id: &str,
    version: Option<String>,
    force: bool,
) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let entry = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server `{server_id}` not found on agent `{agent}`"))?;

    // The package to vendor comes from the entry's own launch args, so this
    // never invents a target the user didn't already approve.
    let spec =
        mur_common::mcp_package::parse_spec(&entry.command, &entry.args).ok_or_else(|| {
            anyhow::anyhow!(
                "`{server_id}` is not launched through a package runner \
                 (command: `{}`), so there is no package to vendor",
                entry.command,
            )
        })?;

    let version = match version.or(spec.version.clone()) {
        Some(v) => v,
        None => {
            let runner = mur_common::mcp_package::runner_for(&entry.command)
                .ok_or_else(|| anyhow::anyhow!("unsupported package runner"))?;
            mur_common::mcp_package::resolve_current_version(runner, &spec.name)?
        }
    };

    if !force {
        println!("About to vendor MCP `{server_id}` on agent `{agent}`:");
        println!("  package:     {}@{version}", spec.name);
        println!(
            "  install to:  {}",
            install_dir(&mur_home, agent, server_id).display()
        );
        println!(
            "  launch:      node <install>/node_modules/{}/<bin>",
            spec.name
        );
        println!("  was:         {} {}", entry.command, entry.args.join(" "));
        println!("\nInstall scripts are disabled (--ignore-scripts).");
        print!("Proceed? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("read confirmation from stdin")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("vendoring cancelled");
        }
    }

    let dir = vendor_entry(entry, &mur_home, agent, &spec.name, &version)?;
    let pin = entry.package.clone().expect("set by vendor_entry");
    crate::cmd::agent::save_profile(&path, &mut profile)?;

    println!("Vendored `{server_id}` for agent `{agent}`:");
    println!("  installed:   {}@{version}", pin.name);
    println!("  directory:   {}", dir.display());
    println!("  lockfile:    sha256:{}", pin.lockfile_sha256);
    println!("\nRestart the agent to launch from the vendored copy.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(dir: &Path, name: &str, bin: serde_json::Value) {
        let p = dir.join("node_modules").join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("package.json"),
            serde_json::json!({ "name": name, "bin": bin }).to_string(),
        )
        .unwrap();
    }

    fn touch(dir: &Path, name: &str, rel: &str) {
        let f = dir.join("node_modules").join(name).join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"// entry\n").unwrap();
    }

    #[test]
    fn resolves_a_string_bin() {
        let d = tempfile::tempdir().unwrap();
        write_pkg(d.path(), "solo", serde_json::json!("dist/index.js"));
        touch(d.path(), "solo", "dist/index.js");
        let got = resolve_bin(d.path(), "solo").unwrap();
        assert!(got.ends_with("dist/index.js"));
        assert!(got.is_absolute(), "the launch path must not depend on cwd");
    }

    #[test]
    fn picks_the_bin_matching_the_scoped_package_name() {
        let d = tempfile::tempdir().unwrap();
        write_pkg(
            d.path(),
            "@yawlabs/fetch-mcp",
            serde_json::json!({ "fetch-mcp": "dist/index.js", "other": "dist/other.js" }),
        );
        touch(d.path(), "@yawlabs/fetch-mcp", "dist/index.js");
        let got = resolve_bin(d.path(), "@yawlabs/fetch-mcp").unwrap();
        assert!(got.ends_with("dist/index.js"));
    }

    /// Guessing which of several binaries to launch would silently run the
    /// wrong program, so an ambiguous `bin` map has to fail loudly.
    #[test]
    fn refuses_an_ambiguous_bin_map() {
        let d = tempfile::tempdir().unwrap();
        write_pkg(
            d.path(),
            "many",
            serde_json::json!({ "a": "a.js", "b": "b.js" }),
        );
        let err = resolve_bin(d.path(), "many").unwrap_err().to_string();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn reports_a_bin_path_that_does_not_exist() {
        let d = tempfile::tempdir().unwrap();
        write_pkg(d.path(), "ghost", serde_json::json!("dist/missing.js"));
        let err = resolve_bin(d.path(), "ghost").unwrap_err().to_string();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn a_package_with_no_bin_cannot_be_vendored() {
        let d = tempfile::tempdir().unwrap();
        write_pkg(d.path(), "lib-only", serde_json::Value::Null);
        let err = resolve_bin(d.path(), "lib-only").unwrap_err().to_string();
        assert!(err.contains("no `bin`"), "got: {err}");
    }

    #[test]
    fn lockfile_hash_changes_with_the_lockfile() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("package-lock.json"), b"{\"v\":1}").unwrap();
        let a = lockfile_sha256(d.path()).unwrap();
        std::fs::write(d.path().join("package-lock.json"), b"{\"v\":2}").unwrap();
        let b = lockfile_sha256(d.path()).unwrap();
        assert_ne!(a, b, "a changed dependency tree must change the pin");
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn install_dir_is_scoped_per_agent_and_server() {
        let home = Path::new("/tmp/murhome");
        assert_ne!(
            install_dir(home, "a1", "fetch"),
            install_dir(home, "a2", "fetch"),
            "two agents must not share one install they can both invalidate",
        );
    }
}
