//! `mur agent mcp inspect` status model: binary/description drift checks,
//! optionally backed by a live probe under the agent's sandbox policy.
//! Split out of `agent_mcp_pin.rs` (#1646) — pure code movement.

use super::*;

/// Result of an `inspect` run, expressed as an exit code so the verb
/// is machine-friendly for scripted re-approval flows. Stable contract:
///
/// - `0` Clean — pin matches current state.
/// - `1` BinaryDrift — the binary hash changed since install.
/// - `2` DescriptionDrift — *reserved* for M9.3.5; not produced today.
/// - `3` BothDrifted — *reserved* for M9.3.5.
/// - `4` MissingPin — entry has no `binary_sha256` (pre-M9 entry).
/// - `5` BinaryMissing — pinned binary not on disk anymore.
/// - `6` InterpreterUnprotected — pin covers the interpreter, not the server.
/// - `7` StartupWouldFail — the server did not answer `initialize` when spawned
///   under the agent's own sandbox policy (`--probe` only).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectStatus {
    Clean = 0,
    BinaryDrift = 1,
    #[allow(dead_code)] // landed in M9.3.5 once description re-probe is wired
    DescriptionDrift = 2,
    #[allow(dead_code)] // landed in M9.3.5
    BothDrifted = 3,
    MissingPin = 4,
    BinaryMissing = 5,
    /// Interpreter-launched (`npx @scope/pkg`, `python -m …`): the pin covers
    /// the interpreter, not the server code, so it is reported rather than
    /// enforced. Not a failure — the agent starts — but not protection either.
    InterpreterUnprotected = 6,
    /// The probe spawned the server under the agent's OWN sandbox policy and it
    /// did not answer `initialize`. Distinct from every status above, which are
    /// all statements about files on disk: this one says the agent will fail to
    /// start the server (#1161). Additive code — 0-6 keep their meanings.
    StartupWouldFail = 7,
}

/// Classify one MCP entry's binary against its pin, without printing.
///
/// The single source of truth for pin status: `inspect_one` renders this, and
/// `mur doctor` reports it across every agent. Two callers computing "is this
/// drifted?" separately is exactly how one of them ends up wrong.
pub fn binary_status(entry: &mur_common::agent::McpServerEntry) -> InspectStatus {
    // A vendored entry is pinned by its lockfile, not by the hash of the
    // `node` that runs it — check that first, or the interpreter branch below
    // would report the one verifiable shape as unprotected.
    if let Some(pkg) = &entry.package {
        let lock = pkg.lockfile_path();
        let Ok(actual) = compute_binary_sha256(&lock) else {
            return InspectStatus::BinaryMissing;
        };
        return if actual.eq_ignore_ascii_case(&pkg.lockfile_sha256) {
            InspectStatus::Clean
        } else {
            InspectStatus::BinaryDrift
        };
    }
    let Some(expected) = &entry.binary_sha256 else {
        return InspectStatus::MissingPin;
    };
    if mur_common::exec::is_interpreter_command(&entry.command) {
        return InspectStatus::InterpreterUnprotected;
    }
    let Ok(path) = resolve_command(&entry.command) else {
        return InspectStatus::BinaryMissing;
    };
    let Ok(actual) = compute_binary_sha256(&path) else {
        return InspectStatus::BinaryMissing;
    };
    if actual.eq_ignore_ascii_case(expected) {
        InspectStatus::Clean
    } else {
        InspectStatus::BinaryDrift
    }
}

/// Print pinned vs current state for one MCP entry. Returns the
/// exit-code-shaped status; `cmd_mcp_inspect` lifts that to
/// `std::process::exit` after running through the dispatch.
pub fn inspect_one(agent: &str, entry: &mur_common::agent::McpServerEntry) -> InspectStatus {
    println!("MCP server: {}", entry.name);
    println!("  command:        {}", entry.command);
    if !entry.args.is_empty() {
        println!("  args:           {}", entry.args.join(" "));
    }
    if let Some(p) = &entry.publisher {
        println!("  publisher:      {}", p.name);
        if let Some(h) = &p.homepage {
            println!("                  {h}");
        }
        if let Some(r) = &p.registry_id {
            println!("                  {r}");
        }
    }
    if let Some(t) = &entry.installed_at {
        println!("  installed_at:   {}", t.to_rfc3339());
    }

    // A vendored entry is pinned by its lockfile, not by the hash of the
    // `node` that launches it — report that instead of the binary story.
    if let Some(pkg) = &entry.package {
        println!(
            "  package:        {}@{} ({})",
            pkg.name, pkg.version, pkg.runner
        );
        println!("  install dir:    {}", pkg.install_dir);
        // npm-specific findings, in npm's terms. A PyPI install verifies every
        // hash through uv at install time and is never asked about
        // attestations, so borrowing this wording would report checks that
        // never ran.
        if pkg.runner == "pypi" {
            println!("  hashes:         verified by uv at install (--require-hashes)");
            println!("  provenance:     not queried for PyPI");
        } else {
            match pkg.signatures_missing {
                Some(0) => {
                    println!("  signatures:     all verified against the registry at install")
                }
                Some(n) => {
                    println!("  signatures:     verified, except {n} package(s) publishing none")
                }
                None => println!("  signatures:     not audited at install"),
            }
            match &pkg.provenance {
                Some(p) => println!("  provenance:     {p}"),
                None => println!("  provenance:     none published by this release"),
            }
        }
        println!("  pinned lock:    {}", pkg.lockfile_sha256);
        let lock = pkg.lockfile_path();
        let Ok(actual) = compute_binary_sha256(&lock) else {
            println!("  current lock:   <not readable — install missing?>");
            println!(
                "  status:         INSTALL MISSING — `mur agent mcp vendor <agent> {}` to reinstall",
                entry.name,
            );
            return InspectStatus::BinaryMissing;
        };
        println!("  current lock:   {actual}");
        return if actual.eq_ignore_ascii_case(&pkg.lockfile_sha256) {
            println!("  status:         CLEAN");
            InspectStatus::Clean
        } else {
            println!("  status:         TREE DRIFT");
            println!(
                "  hint:           `mur agent mcp vendor {} {}` to reinstall and re-approve",
                agent, entry.name,
            );
            InspectStatus::BinaryDrift
        };
    }

    let Some(expected) = &entry.binary_sha256 else {
        println!("  pin status:     <unpinned> (pre-M9 entry)");
        println!(
            "  hint:           run `mur agent mcp pin {} {}` to start enforcing rule 6",
            agent, entry.name,
        );
        return InspectStatus::MissingPin;
    };

    println!("  pinned sha256:  {expected}");
    if mur_common::exec::is_interpreter_command(&entry.command) {
        println!(
            "  status:         INTERPRETER-LAUNCHED — pin covers `{}`, not the server code",
            entry
                .command
                .split_whitespace()
                .next()
                .unwrap_or(&entry.command)
        );
        println!(
            "  hint:           not enforced at startup: hashing the interpreter breaks on any \
             unrelated runtime upgrade and still would not cover what it runs. Pin the package \
             version instead (lockfile / integrity hash) if this server matters."
        );
        return InspectStatus::InterpreterUnprotected;
    }
    let path = match resolve_command(&entry.command) {
        Ok(p) => p,
        Err(_) => {
            println!("  current sha256: <binary not found on PATH>");
            println!(
                "  status:         BINARY MISSING — `mur agent mcp remove {}` to clean up, \
                 or restore the binary and re-run inspect",
                entry.name,
            );
            return InspectStatus::BinaryMissing;
        }
    };
    let actual = match compute_binary_sha256(&path) {
        Ok(h) => h,
        Err(e) => {
            println!("  current sha256: <read error: {e}>");
            return InspectStatus::BinaryMissing;
        }
    };
    println!("  current sha256: {actual}");
    if let Some(d) = &entry.description_hash {
        println!("  pinned descr:   {d}");
        println!("                  (pass --probe to verify against live MCP)");
    } else {
        println!(
            "  pinned descr:   <none — re-pin with `mur agent mcp pin {} {}` to capture>",
            agent, entry.name,
        );
    }

    if actual.eq_ignore_ascii_case(expected) {
        println!("  status:         CLEAN");
        InspectStatus::Clean
    } else {
        println!("  status:         BINARY DRIFT");
        println!(
            "  hint:           `mur agent mcp pin {} {}` to re-approve, \
             or `mur agent mcp remove {} {}` to uninstall",
            agent, entry.name, agent, entry.name,
        );
        InspectStatus::BinaryDrift
    }
}

/// Same as `inspect_one` but additionally spawns the MCP and verifies
/// `tools/list` against `entry.description_hash` (B0 rule 6 / M9.3.5).
/// Lights up the `DescriptionDrift` and `BothDrifted` exit codes that
/// M9.4 reserved.
pub fn inspect_one_probed(
    agent: &str,
    entry: &mur_common::agent::McpServerEntry,
    timeout: std::time::Duration,
    policy: &mur_agent_runtime::sandbox::policy::SandboxPolicy,
) -> InspectStatus {
    // Run the binary-side inspect synchronously first so its output
    // appears before the probe results in the user-facing report.
    let binary_status = inspect_one(agent, entry);

    // Skip the probe entirely if the entry has no pinned description
    // hash — there's nothing to compare against.
    let Some(expected_descr) = entry.description_hash.as_deref() else {
        return binary_status;
    };

    println!();
    println!("  Probing live MCP under this agent's sandbox…");
    let probe_entry = match resolve_command(&entry.command) {
        Ok(resolved) => mur_common::agent::McpServerEntry {
            command: resolved.display().to_string(),
            ..entry.clone()
        },
        Err(_) => {
            // Binary already flagged as missing by `inspect_one` above.
            return binary_status;
        }
    };
    match probe_egress::probe_as_runtime_would(agent, &probe_entry, timeout, policy) {
        Ok((current, _)) => {
            let descr_drifted = !current.eq_ignore_ascii_case(expected_descr);
            if descr_drifted {
                println!("  current descr:  {current}");
                println!("  description status: DESCRIPTION DRIFT");
                println!(
                    "  hint:           the MCP's tools/list changed since install; \
                     review the new tool descriptions then \
                     `mur agent mcp pin {} {}` to re-approve, \
                     or `mur agent mcp remove {} {}` to uninstall",
                    agent, entry.name, agent, entry.name,
                );
                match binary_status {
                    InspectStatus::Clean => InspectStatus::DescriptionDrift,
                    InspectStatus::BinaryDrift => InspectStatus::BothDrifted,
                    other => other,
                }
            } else {
                println!("  description status: CLEAN");
                binary_status
            }
        }
        Err(e) => {
            // The probe ran under the agent's own policy, so this is not a
            // diagnostic curiosity: the server will fail the same way when the
            // agent starts. Say that plainly — the pins above still read CLEAN,
            // and "CLEAN" is what sent the last operator hunting for hours.
            println!("  startup status: WOULD FAIL UNDER THIS AGENT'S SANDBOX");
            println!("  probe error:    {e}");
            println!(
                "  hint:           the pins above only say the files are intact. If the error \
                 names a path, grant it with `mur agent perm allow-read {agent} <path>` / \
                 `allow-write`; many MCP servers write state under $HOME on first launch. \
                 Pass --no-probe to inspect the binary alone, or set MUR_MCP_PROBE_TIMEOUT_S \
                 to extend the budget."
            );
            // Worst-wins, the same rule `cmd_mcp_inspect` uses across servers.
            if (binary_status as u8) > (InspectStatus::StartupWouldFail as u8) {
                binary_status
            } else {
                InspectStatus::StartupWouldFail
            }
        }
    }
}
