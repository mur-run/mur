//! Parent-pid oracle: how a sealed Linux runtime still walks process lineage.
//!
//! Three gates name a peer by walking its ancestry — the ssh-agent proxy's
//! self-only gate, the A2A `accepts_from` gate, and the CLI-spawn shim
//! ticket. On Linux the walk reads `/proc/<pid>/stat`, but the runtime seals
//! itself with Landlock and is granted only `/proc/self` (resolved at rule
//! creation to the runtime's own entry). After the seal every other pid's
//! `stat` is `EACCES`, the walk stops at its first step, and each of the
//! agent's own children reads as the user — the proxy then refuses the
//! agent's own `ssh`.
//!
//! Granting `/proc` back would hand every `bash` child the same-uid
//! `environ`/`fd`/`mem` of other processes. Instead the runtime spawns this
//! helper *before* the seal, so it stays unconfined, and asks it one
//! question over a pipe: "what is pid N's parent?". The pipe ends are
//! close-on-exec in the runtime, so no child of the agent can reach the
//! helper, and the answer is a ppid — nothing a `ps` would not show.
//!
//! Wire: the helper prints [`READY`] on a line of its own, then answers each
//! request line (a decimal pid) with a line holding the parent pid or
//! [`UNKNOWN`]. It exits on stdin EOF, i.e. when the runtime goes away.
//! Anything printed before `READY` is skipped (a test harness re-exec prints
//! its banner first).

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// `<runtime> ppid-oracle` runs the helper.
pub const SUBCOMMAND: &str = "ppid-oracle";
/// The handshake line the helper prints before serving.
pub const READY: &str = "mur-ppid-oracle-ready";
/// The answer for a pid whose parent could not be read.
pub const UNKNOWN: &str = "-";

/// When `argv` names the oracle subcommand, serve on stdin/stdout and exit;
/// otherwise return. Called from the supervisor entrypoint before every side
/// effect, like the ssh bypass probe.
pub fn exit_if_oracle(argv: &[String]) {
    if argv.get(1).map(String::as_str) == Some(SUBCOMMAND) {
        let _ = serve(io::stdin().lock(), io::stdout().lock(), read_ppid);
        std::process::exit(0);
    }
}

/// Answer requests from `input` on `out` until EOF.
pub fn serve(
    input: impl BufRead,
    mut out: impl Write,
    parent: impl Fn(u32) -> Option<u32>,
) -> io::Result<()> {
    writeln!(out, "{READY}")?;
    out.flush()?;
    for line in input.lines() {
        match line?.trim().parse::<u32>().ok().and_then(&parent) {
            Some(p) => writeln!(out, "{p}")?,
            None => writeln!(out, "{UNKNOWN}")?,
        }
        out.flush()?;
    }
    Ok(())
}

/// Read `pid`'s parent from procfs. `None` off Linux, for a gone process, or
/// when the sandbox denies the read.
#[cfg(target_os = "linux")]
pub fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` may hold spaces and parens; the fields after the LAST ')' are
    // fixed: state, then ppid.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
pub fn read_ppid(_pid: u32) -> Option<u32> {
    None
}

/// The runtime's end of a running helper.
pub struct Oracle {
    _child: Child,
    io: Mutex<(ChildStdin, BufReader<ChildStdout>)>,
}

impl Oracle {
    /// Spawn `cmd` as the helper and wait for its handshake.
    pub fn spawn(mut cmd: Command) -> io::Result<Self> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(io::Error::other("ppid oracle: pipes missing"));
        };
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ppid oracle exited before its handshake",
                ));
            }
            if line.trim_end() == READY {
                break;
            }
        }
        Ok(Self {
            _child: child,
            io: Mutex::new((stdin, reader)),
        })
    }

    /// Ask for `pid`'s parent. `None` on any failure: the lineage walk then
    /// stops, which every caller treats as "not descended" (fail closed).
    pub fn parent_of(&self, pid: u32) -> Option<u32> {
        let mut guard = self.io.lock().ok()?;
        let (w, r) = &mut *guard;
        writeln!(w, "{pid}").ok()?;
        w.flush().ok()?;
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        line.trim().parse().ok()
    }
}

static INSTALLED: OnceLock<Oracle> = OnceLock::new();

/// Start the helper. Must run before the seal: a helper spawned afterwards
/// inherits the same Landlock domain and can read nothing either. Never
/// fails startup — without it the lineage gates fail closed, as before.
#[cfg(target_os = "linux")]
pub fn install_before_seal() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "ppid oracle: cannot resolve own executable; lineage gates will fail closed");
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.arg(SUBCOMMAND);
    match Oracle::spawn(cmd) {
        Ok(o) => {
            let _ = INSTALLED.set(o);
        }
        Err(e) => tracing::warn!(
            error = %e,
            "ppid oracle failed to start; lineage gates will fail closed"
        ),
    }
}

/// The helper, when [`install_before_seal`] started one.
pub fn installed() -> Option<&'static Oracle> {
    INSTALLED.get()
}

#[cfg(test)]
#[path = "ppid_oracle_tests.rs"]
mod tests;
