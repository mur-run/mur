//! Setup's smoke launch (#1981 Layer 3): start each language server MUR
//! installed under the agent's own seal and complete one `initialize`.
//!
//! The launch runs in a child `mur code-nav smoke-ls` that seals itself
//! first ([`super::smoke_probe`] says why it cannot run in-process). Only the
//! servers MUR pre-installs are launched: the others serena fetches itself on
//! first start, so there is nothing on disk yet to launch.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::consent::Manifest;
use super::node_ls_install;
use super::plan::Lang;
use super::smoke_probe::{Report, SMOKE_TIMEOUT, server_command, stop};
use mur_common::serena_ls_cmd::{self, ManagedNodeLs};

/// How often the parent checks whether the probe has exited.
const POLL: Duration = Duration::from_millis(50);

/// After the probe exits, how long its pipes get to reach EOF. A server
/// descendant the probe failed to stop could otherwise hold them open.
const POST_EXIT_DRAIN: Duration = Duration::from_secs(2);

/// Hidden subcommand path the child is started with.
pub const SUBCOMMAND: [&str; 2] = ["code-nav", "smoke-ls"];

/// The flag serena appends to every server it launches.
const STDIO: &str = "--stdio";

/// One server to launch: the language it serves and its exact argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub lang: Lang,
    pub cmd: Vec<String>,
}

/// The launches for `langs`, as serena would start them with `m`'s installs.
pub fn targets(m: &Manifest, langs: &[Lang], mur_home: &Path) -> Vec<Target> {
    let tools = node_ls_install::tools_dir(mur_home);
    let node = |ls: &ManagedNodeLs, rec: &Option<node_ls_install::Record>| {
        rec.as_ref()
            .and_then(|_| serena_ls_cmd::canonical_base_cmd(ls.ls_id, &tools))
    };
    langs
        .iter()
        .filter_map(|&lang| {
            let mut cmd = match lang {
                Lang::Python => vec![m.pyright.as_ref()?.bin.to_str()?.to_owned()],
                Lang::Php => node(&serena_ls_cmd::INTELEPHENSE, &m.intelephense)?,
                Lang::TypeScript => node(&serena_ls_cmd::TYPESCRIPT, &m.typescript)?,
                _ => return None,
            };
            cmd.push(STDIO.to_owned());
            Some(Target { lang, cmd })
        })
        .collect()
}

/// How long the parent waits for the sealed probe beyond the server's own
/// `initialize` budget: sealing, the stderr drain, and process start-up.
const PROBE_SLACK: Duration = Duration::from_secs(15);

/// Launch every target; print one line each. `Err` unless every target
/// answered `initialize` — a launch that could not be verified is not a pass.
pub fn verify(
    agent: &str,
    project: &Path,
    targets: &[Target],
    unmanaged: &[Lang],
    out: &mut dyn Write,
) -> Result<()> {
    if targets.is_empty() && unmanaged.is_empty() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("locate the mur binary")?;
    let deadline = SMOKE_TIMEOUT + PROBE_SLACK;
    verify_with(agent, targets, unmanaged, out, |t| {
        launch(&exe, agent, project, &t.cmd, deadline)
    })
}

/// [`verify`] with the launch injected.
pub(crate) fn verify_with(
    agent: &str,
    targets: &[Target],
    unmanaged: &[Lang],
    out: &mut dyn Write,
    mut launch: impl FnMut(&Target) -> Report,
) -> Result<()> {
    if targets.is_empty() && unmanaged.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nSmoke launch under agent '{agent}''s sandbox:")?;
    let mut failed: Vec<&'static str> = Vec::new();
    let mut unverified: Vec<&'static str> = Vec::new();
    for t in targets {
        let name = t.lang.flag();
        match launch(t) {
            Report::Ok => writeln!(out, "  ✓ {name}: initialize answered")?,
            Report::Unverified { detail } => {
                writeln!(out, "  ? {name}: not verified — {detail}")?;
                unverified.push(name);
            }
            Report::Failed { detail } => {
                writeln!(out, "  ✗ {name}: {detail}")?;
                for hint in exec_hints(agent, &t.cmd) {
                    writeln!(out, "      {hint}")?;
                }
                failed.push(name);
            }
        }
    }
    if !unmanaged.is_empty() {
        let names: Vec<_> = unmanaged.iter().map(|l| l.flag()).collect();
        writeln!(
            out,
            "  · not launched: {} (serena downloads these itself on first start)",
            names.join(", ")
        )?;
    }
    let mut problems = Vec::new();
    if !failed.is_empty() {
        problems.push(format!(
            "failed to start under the agent's sandbox: {} (fix the line(s) marked ✗)",
            failed.join(", ")
        ));
    }
    if !unverified.is_empty() {
        problems.push(format!(
            "not verified: {} (see the line(s) marked ?)",
            unverified.join(", ")
        ));
    }
    if problems.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "language server smoke launch did not pass — {}. \
         Installs and grants above are kept; re-run setup once that is addressed.",
        problems.join("; ")
    )
}

/// Run the sealed child and read its one-line report, giving up (and
/// killing its whole group) after `deadline`.
fn launch(exe: &Path, agent: &str, project: &Path, cmd: &[String], deadline: Duration) -> Report {
    let mut c = Command::new(exe);
    c.args(SUBCOMMAND)
        .arg("--agent")
        .arg(agent)
        .arg("--project")
        .arg(project)
        .arg("--")
        .args(cmd);
    match server_command(&mut c).spawn() {
        Ok(child) => collect(child, deadline),
        Err(e) => Report::Failed {
            detail: format!("could not start the smoke probe: {e}"),
        },
    }
}

/// Wait at most `deadline` for `child` to exit, then parse its report. A
/// probe still running at the deadline is stopped with its process group.
pub(crate) fn collect(mut child: Child, deadline: Duration) -> Report {
    drop(child.stdin.take());
    let read = |r: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = read(child.stdout.take().map(|r| Box::new(r) as _));
    let stderr = read(child.stderr.take().map(|r| Box::new(r) as _));
    let start = Instant::now();
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if start.elapsed() < deadline => std::thread::sleep(POLL),
            _ => break false,
        }
    };
    if !exited {
        stop(&mut child);
        return Report::Failed {
            detail: format!(
                "the smoke probe did not finish within {}s and was stopped",
                deadline.as_secs()
            ),
        };
    }
    let out = stdout.recv_timeout(POST_EXIT_DRAIN).unwrap_or_default();
    let err = stderr.recv_timeout(POST_EXIT_DRAIN).unwrap_or_default();
    parse_report(&out, &err)
}

/// The child's report: the last stdout line that parses. Anything else is a
/// probe that died before reporting, quoted from its stderr.
pub(crate) fn parse_report(stdout: &[u8], stderr: &[u8]) -> Report {
    let parsed = String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Report>(l).ok());
    parsed.unwrap_or_else(|| {
        let err = String::from_utf8_lossy(stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty());
        Report::Failed {
            detail: format!(
                "the smoke probe died without a report{}",
                last.map(|l| format!(": {}", l.trim())).unwrap_or_default()
            ),
        }
    })
}

/// Exec grants the seal would refuse for `cmd`, as the command that grants
/// each. macOS only: elsewhere the seal does not gate exec by path.
#[cfg(target_os = "macos")]
fn exec_hints(agent: &str, cmd: &[String]) -> Vec<String> {
    use mur_agent_runtime::sandbox::{SandboxPolicy, macos::exec_permitted, search_dirs};
    let Ok(home) = crate::cmd::agent::resolve_mur_home() else {
        return Vec::new();
    };
    let Ok((_, profile)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return Vec::new();
    };
    let policy =
        SandboxPolicy::from_entitlements(&profile.entitlements, &home.join("agents").join(agent));
    let path = search_dirs::mcp_child_path(policy.spawn_mode);
    blocked_dirs(cmd, &path, |p| exec_permitted(&policy, p))
        .into_iter()
        .map(|d| {
            format!(
                "the sandbox does not let it exec {}; grant it with: \
                 mur agent perm allow-spawn-dir {agent} {}",
                d.display(),
                d.display()
            )
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn exec_hints(_agent: &str, _cmd: &[String]) -> Vec<String> {
    Vec::new()
}

/// The directories of `cmd[0]` (resolved on `path`, canonical) and of its
/// `#!` interpreter that `permitted` refuses.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn blocked_dirs(
    cmd: &[String],
    path: &std::ffi::OsStr,
    permitted: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let Some(prog) = cmd.first() else {
        return Vec::new();
    };
    let Ok(bin) = mur_common::exec::resolve_command_in(path, prog) else {
        return Vec::new();
    };
    let mut hops = vec![bin.clone()];
    if let Some(interp) = shebang(&bin) {
        hops.push(interp);
    }
    let mut out = Vec::new();
    for hop in hops {
        let real = std::fs::canonicalize(&hop).unwrap_or(hop);
        if !permitted(&real)
            && let Some(dir) = real.parent()
            && !out.iter().any(|d: &PathBuf| d == dir)
        {
            out.push(dir.to_path_buf());
        }
    }
    out
}

/// The absolute interpreter of a `#!/abs/path` line. `env` shebangs are left
/// to `mur agent doctor`, which already judges them.
fn shebang(bin: &Path) -> Option<PathBuf> {
    use std::io::Read;
    let mut head = [0u8; 256];
    let n = std::fs::File::open(bin).ok()?.read(&mut head).ok()?;
    let line = std::str::from_utf8(&head[..n]).ok()?.lines().next()?;
    let interp = line.strip_prefix("#!")?.split_whitespace().next()?;
    let p = PathBuf::from(interp);
    (p.is_absolute() && p.file_name()? != "env").then_some(p)
}

#[cfg(all(test, unix))]
#[path = "smoke_tests.rs"]
mod tests;
