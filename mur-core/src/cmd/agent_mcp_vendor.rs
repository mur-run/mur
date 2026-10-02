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
use mur_common::agent::{AgentProfile, McpPackagePin, McpServerEntry};
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

// ── PyPI / uv ───────────────────────────────────────────────────────────────
//
// Same shape as the npm path, one ecosystem over:
//
//   requirements.lock  ← `uv pip compile --generate-hashes` (a sha256 per
//                        package across the resolved tree)
//   venv/              ← `uv pip install --require-hashes`, which verifies
//                        every one of those hashes as it installs — a check
//                        npm's install does not perform
//   launch             ← `venv/bin/<console-script>`
//
// A venv rather than `--target`: scripts written into a `--target` directory
// do a bare `from pkg import main` with no `sys.path` handling, so they only
// run when PYTHONPATH points at the target — and `McpServerEntry` has no env
// to set it in. A venv's script execs the venv's own interpreter and runs from
// anywhere with an empty environment.

/// Resolve `name==version` into `requirements.lock` with a hash per package.
fn uv_compile(dir: &Path, name: &str, version: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(dir.join("requirements.in"), format!("{name}=={version}\n"))
        .context("write requirements.in")?;
    let out = std::process::Command::new("uv")
        .args([
            "pip",
            "compile",
            "requirements.in",
            "--generate-hashes",
            "-o",
            "requirements.lock",
            "--quiet",
        ])
        .current_dir(dir)
        .output()
        .map_err(|e| anyhow::anyhow!("run uv pip compile: {e} (is uv on PATH?)"))?;
    if !out.status.success() {
        bail!(
            "uv could not resolve {name}=={version}: {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    Ok(())
}

/// Create the venv and install the locked set into it, hashes enforced.
fn uv_install(dir: &Path) -> Result<PathBuf> {
    let venv = dir.join("venv");
    let out = std::process::Command::new("uv")
        .args(["venv", "venv", "--quiet"])
        .current_dir(dir)
        .output()
        .map_err(|e| anyhow::anyhow!("run uv venv: {e}"))?;
    if !out.status.success() {
        bail!(
            "uv venv failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let python = venv.join("bin").join("python");
    let out = std::process::Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&python)
        .args(["--require-hashes", "-r", "requirements.lock", "--quiet"])
        .current_dir(dir)
        .output()
        .map_err(|e| anyhow::anyhow!("run uv pip install: {e}"))?;
    if !out.status.success() {
        bail!(
            "uv pip install failed (hashes are enforced, so this includes a \
             hash mismatch): {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    Ok(venv)
}

/// Find the console script the package installed into the venv.
///
/// Read from the package's own `entry_points.txt` rather than guessing at
/// `bin/<name>`: a distribution's script is frequently named differently from
/// the distribution (`mcp-server-time` vs `mcp_server_time`), and picking the
/// wrong file would launch someone else's program.
fn resolve_console_script(venv: &Path, name: &str) -> Result<PathBuf> {
    let normalized = name.to_lowercase().replace(['-', '.'], "_");
    let site = glob_site_packages(venv)?;
    let mut entry_points = None;
    for entry in std::fs::read_dir(&site)
        .with_context(|| format!("read {}", site.display()))?
        .filter_map(|e| e.ok())
    {
        let file_name = entry.file_name().to_string_lossy().to_lowercase();
        if file_name.ends_with(".dist-info")
            && file_name
                .replace('-', "_")
                .starts_with(&format!("{normalized}_"))
        {
            entry_points = Some(entry.path().join("entry_points.txt"));
            break;
        }
    }
    let ep = entry_points
        .filter(|p| p.is_file())
        .ok_or_else(|| anyhow::anyhow!("`{name}` installed no entry_points.txt to launch from"))?;

    let body = std::fs::read_to_string(&ep).with_context(|| format!("read {}", ep.display()))?;
    let scripts = console_scripts(&body);
    let script = match scripts.len() {
        0 => bail!("`{name}` declares no console script, so there is nothing to launch"),
        1 => scripts[0].clone(),
        _ => bail!(
            "`{name}` declares {} console scripts ({}); vendoring needs exactly one",
            scripts.len(),
            scripts.join(", "),
        ),
    };
    let path = venv.join("bin").join(&script);
    if !path.is_file() {
        bail!(
            "console script `{script}` is missing from {}",
            venv.display()
        );
    }
    path.canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))
}

/// Script names under `[console_scripts]` in an `entry_points.txt`.
fn console_scripts(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[console_scripts]";
            continue;
        }
        if in_section && let Some((name, _)) = line.split_once('=') {
            let name = name.trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The venv's `site-packages`, whose path carries the interpreter version.
fn glob_site_packages(venv: &Path) -> Result<PathBuf> {
    let lib = venv.join("lib");
    let entry = std::fs::read_dir(&lib)
        .with_context(|| format!("read {}", lib.display()))?
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("python"))
        .ok_or_else(|| anyhow::anyhow!("no python*/ under {}", lib.display()))?;
    Ok(entry.path().join("site-packages"))
}

/// Vendor a PyPI package: resolve with hashes, install into a venv that
/// verifies them, and point the entry at the console script.
fn vendor_python(entry: &mut McpServerEntry, dir: &Path, name: &str, version: &str) -> Result<()> {
    uv_compile(dir, name, version)?;
    let venv = uv_install(dir)?;
    let script = resolve_console_script(&venv, name)?;
    let lock = crate::cmd::agent_mcp_pin::compute_binary_sha256(&dir.join("requirements.lock"))
        .context("hash requirements.lock")?;

    entry.command = script.display().to_string();
    entry.args = vec![];
    entry.binary_sha256 = None;
    entry.package = Some(McpPackagePin {
        runner: "pypi".into(),
        name: name.to_string(),
        version: version.to_string(),
        install_dir: dir.display().to_string(),
        lockfile_sha256: lock,
        // uv enforced every hash during install; there is no separate registry
        // signature step to record, and PyPI attestations aren't queried here.
        signatures_missing: None,
        provenance: None,
    });
    Ok(())
}

/// Install `name@version` into `dir` with npm, scripts disabled.
pub(crate) fn npm_install(dir: &Path, name: &str, version: &str) -> Result<()> {
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

/// Outcome of `npm audit signatures` over the installed tree.
pub(crate) struct SignatureAudit {
    /// Packages whose registry signature failed to verify. Non-empty means the
    /// bytes on disk are not what the registry signed.
    pub(crate) invalid: Vec<String>,
    /// Packages that published no signature at all.
    pub(crate) missing: u32,
}

/// Parse `npm audit signatures --json`.
///
/// Split out from the subprocess call so the contract can be tested without
/// npm, a network, or an installed tree — this is the part that decides
/// whether a vendor is refused.
fn parse_audit(body: &str) -> Option<SignatureAudit> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let names = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .map(|e| {
                        e.get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("<unnamed>")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(SignatureAudit {
        invalid: names("invalid"),
        missing: names("missing").len() as u32,
    })
}

/// Verify that the installed tree came from the registry.
///
/// The lockfile hash proves the tree hasn't changed since install; it says
/// nothing about where those bytes came from, and would pin a poisoned cache
/// as happily as a clean one. Registry signatures close exactly that gap, and
/// they close it here — at install time, with network already in hand — rather
/// than costing anything at startup.
///
/// `Ok(None)` when the audit could not run (npm too old, offline). Refusing to
/// vendor over an unavailable audit would trade a real capability for a check
/// that is advisory by nature.
pub(crate) fn audit_signatures(dir: &Path) -> Result<Option<SignatureAudit>> {
    let out = match std::process::Command::new("npm")
        .args(["audit", "signatures", "--json"])
        .current_dir(dir)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("warning: could not run `npm audit signatures` ({e}); skipping");
            return Ok(None);
        }
    };
    // Exit status is non-zero when anything failed to verify, so parse the
    // body regardless and let its contents decide.
    let body = String::from_utf8_lossy(&out.stdout);
    match parse_audit(&body) {
        Some(a) => Ok(Some(a)),
        None => {
            eprintln!(
                "warning: `npm audit signatures` returned output this version can't read; skipping",
            );
            Ok(None)
        }
    }
}

/// Pull the SLSA predicate type out of `npm view <spec> dist.attestations --json`.
///
/// Split from the subprocess call so the shape is testable offline. An empty
/// body is npm's way of saying the field doesn't exist, which is the common
/// case and not an error.
fn parse_provenance(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    v.get("provenance")?
        .get("predicateType")?
        .as_str()
        .map(str::to_string)
}

/// Ask the registry whether this release published build provenance.
///
/// Only the vendored package itself is queried. `npm audit signatures` already
/// verifies any attestations across the whole tree; what it doesn't tell us is
/// whether *this* package has one, and asking per-dependency would be a
/// hundred network round-trips for a field that is recorded, never enforced.
fn query_provenance(name: &str, version: &str) -> Option<String> {
    let out = std::process::Command::new("npm")
        .args([
            "view",
            &format!("{name}@{version}"),
            "dist.attestations",
            "--json",
        ])
        .output()
        .ok()?;
    parse_provenance(&String::from_utf8_lossy(&out.stdout))
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
    if matches!(
        mur_common::mcp_package::runner_for(&entry.command),
        Some(mur_common::mcp_package::Runner::Python)
    ) {
        vendor_python(entry, &dir, name, version)?;
        return Ok(dir);
    }
    npm_install(&dir, name, version)?;

    let audit = audit_signatures(&dir)?;
    if let Some(a) = &audit
        && !a.invalid.is_empty()
    {
        bail!(
            "refusing to vendor: {} package(s) failed registry signature verification ({}). \
             The bytes on disk are not what the registry signed — do not run this server \
             until you know why.",
            a.invalid.len(),
            a.invalid.join(", "),
        );
    }

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
        signatures_missing: audit.as_ref().map(|a| a.missing),
        provenance: query_provenance(name, version),
    });
    Ok(dir)
}

/// Grant the profile what the *rewritten* entry needs in order to launch:
/// the new interpreter in the spawn allowlist, and read on the install dir.
///
/// `mcp add` already syncs the spawn allowlist, because the sandbox launches
/// only from it. Vendoring REPLACES that command (`npx <pkg>` becomes
/// `node <script>`) and puts the script somewhere the agent has never been
/// granted, so it has to sync both halves or the entry it just wrote can
/// never start.
///
/// The install dir needs its own narrow grant: a read grant broad enough to
/// cover `~/.mur` also reaches the credential store, and such a grant is
/// dropped whole rather than carved (`sandbox::linux::partition_read_grants`
/// — Landlock has no deny rule). So the wide grant a user is most likely to
/// already have is exactly the one that does not help here.
///
/// Both failures surface only as `Operation not permitted` (blocked exec) or
/// `exited before replying` (spawned, could not read its own script) — the
/// runtime never gets to say "permission", which is why this must not be left
/// to the operator.
fn sync_launch_entitlements(profile: &mut AgentProfile, command: &str, install_dir: &Path) {
    let ent = &mut profile.entitlements;
    if !ent.processes.spawn.allowed.iter().any(|a| a == command) {
        ent.processes.spawn.allowed.push(command.to_string());
    }
    let dir = install_dir.display().to_string();
    if !ent.filesystem.read.iter().any(|p| p == &dir) {
        ent.filesystem.read.push(dir);
    }
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
    let launch_command = entry.command.clone();
    sync_launch_entitlements(&mut profile, &launch_command, &dir);
    crate::cmd::agent::save_profile(&path, &mut profile)?;

    println!("Vendored `{server_id}` for agent `{agent}`:");
    println!("  installed:   {}@{version}", pin.name);
    println!("  directory:   {}", dir.display());
    println!("  lockfile:    sha256:{}", pin.lockfile_sha256);
    println!(
        "  granted:     spawn `{launch_command}`, read {}",
        dir.display()
    );
    // These two lines describe npm-specific checks. Printing npm's wording for
    // a PyPI install would state things that never happened — "not audited
    // (npm too old, or offline)" when npm was never involved, and "none
    // published" when nothing was ever asked.
    if pin.runner == "pypi" {
        println!("  hashes:      every package verified by uv during install (--require-hashes)");
        println!("  provenance:  not queried for PyPI");
    } else {
        match pin.signatures_missing {
            Some(0) => println!("  signatures:  every package verified against the registry"),
            Some(n) => println!(
                "  signatures:  verified, except {n} package(s) that publish none \
                 (common for older releases)"
            ),
            None => println!("  signatures:  not audited (npm too old, or offline)"),
        }
        match &pin.provenance {
            Some(p) => println!("  provenance:  published ({p})"),
            None => println!(
                "  provenance:  none published — the registry can attest these bytes, \
                 but not where they were built"
            ),
        }
    }
    println!("\nRestart the agent to launch from the vendored copy.");
    Ok(())
}

#[cfg(test)]
mod tests;
