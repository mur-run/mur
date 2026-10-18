//! The sealed half of setup's smoke launch (#1981 Layer 3): the hidden
//! `mur code-nav smoke-ls` subcommand.
//!
//! `sandbox::child::spawn_sandboxed` does not narrow a child to a policy (it
//! drops the cage); a child is confined only by inheriting its parent's seal.
//! An unsealed `mur code-nav setup` launching a server directly would
//! therefore prove nothing. So this process applies the agent's own seal to
//! itself — the same [`mur_agent_runtime::sandbox::apply`] the supervisor
//! calls — and only then starts the server, which inherits it exactly as
//! serena's servers do inside the agent.
//!
//! The whole probe runs on one thread: Landlock confines the calling thread
//! (and what it spawns), so sealing and spawning must not be split.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long a server gets to answer `initialize`. pyright and the node
/// servers answer in well under a second on a warm disk; this bounds a hang.
pub const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);

/// A frame larger than this is not an `initialize` reply.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Lines of the server's stderr quoted back on failure.
const STDERR_TAIL_LINES: usize = 8;

/// The request id of our one `initialize`.
const INIT_ID: u64 = 1;

/// What one smoke launch proved. Serialized as the child's single stdout line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Report {
    /// `initialize` answered under the agent's seal.
    Ok,
    /// The server could not start or answer under the seal.
    Failed { detail: String },
    /// No seal could be applied here, so the launch would prove nothing.
    Unverified { detail: String },
}

/// Seal this process as `agent`, start `cmd` in `project`, complete one
/// `initialize` round-trip.
pub fn run(agent: &str, project: &Path, cmd: &[String]) -> Report {
    use mur_agent_runtime::sandbox;

    let Some((prog, args)) = cmd.split_first() else {
        return failed("no language-server command given");
    };
    let profile = match crate::cmd::agent::load_profile_for_edit(agent) {
        Ok((_, p)) => p,
        Err(e) => return failed(format!("load agent profile: {e:#}")),
    };
    let agent_home = match crate::cmd::agent::resolve_mur_home() {
        Ok(h) => h.join("agents").join(agent),
        Err(e) => return failed(format!("resolve MUR home: {e:#}")),
    };
    // Built before the seal: everything the launch needs from disk is read
    // while it still can be.
    let policy = sandbox::SandboxPolicy::from_entitlements(&profile.entitlements, &agent_home);
    let path = sandbox::search_dirs::mcp_child_path(policy.spawn_mode);
    let scratch = policy
        .scratch_dir
        .as_deref()
        .map(|d| mur_agent_runtime::agent_paths::scratch_env(d).to_vec())
        .unwrap_or_default();

    if sandbox::current_process_sealed() {
        return Report::Unverified {
            detail: "this shell already runs inside a sandbox, and macOS refuses a second \
                     seal; run setup from an ordinary terminal to verify"
                .into(),
        };
    }
    match sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        Ok(s) => {
            return Report::Unverified {
                detail: format!(
                    "the agent's sandbox is not enforcing on this host ({}), so a launch \
                     here proves nothing",
                    s.platform
                ),
            };
        }
        Err(e) => return failed(format!("the agent's sandbox refused to apply: {e:#}")),
    }

    let resolved = mur_common::exec::resolve_command_in(&path, prog)
        .unwrap_or_else(|_| std::path::PathBuf::from(prog));
    let mut c = Command::new(&resolved);
    c.args(args)
        .current_dir(project)
        .env("PATH", &path)
        .envs(scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match c.spawn() {
        Ok(child) => handshake(child, project, SMOKE_TIMEOUT),
        Err(e) => failed(format!("could not start {}: {e}", resolved.display())),
    }
}

fn failed(detail: impl Into<String>) -> Report {
    Report::Failed {
        detail: detail.into(),
    }
}

/// Send `initialize`, wait up to `timeout` for its reply, then stop `child`.
pub(crate) fn handshake(mut child: Child, root: &Path, timeout: Duration) -> Report {
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        return failed("server stdio was not piped");
    };
    let err_tail = std::thread::spawn(move || tail(stderr, STDERR_TAIL_LINES));

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        let got = loop {
            match read_frame(&mut r) {
                Ok(v) if v.get("method").is_none() && v.get("id") == Some(&json!(INIT_ID)) => {
                    break Ok(v);
                }
                Ok(_) => continue, // a notification such as window/logMessage
                Err(e) => break Err(e),
            }
        };
        let _ = tx.send(got);
    });

    let sent = stdin.write_all(&frame(&initialize(root)));
    let outcome = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    drop(stdin);
    let quoted = err_tail.join().unwrap_or_default();
    let with_tail = |msg: String| {
        if quoted.is_empty() {
            failed(msg)
        } else {
            failed(format!("{msg}; its stderr ended:\n{quoted}"))
        }
    };

    match outcome {
        Ok(Ok(v)) if v.get("result").is_some() => Report::Ok,
        Ok(Ok(v)) => with_tail(format!(
            "initialize was refused: {}",
            v.get("error").unwrap_or(&Value::Null)
        )),
        Ok(Err(e)) => match sent {
            Err(w) => with_tail(format!("the server exited before reading initialize ({w})")),
            Ok(()) => with_tail(format!(
                "the server exited before answering initialize ({e})"
            )),
        },
        Err(_) => with_tail(format!(
            "no answer to initialize within {}s",
            timeout.as_secs()
        )),
    }
}

/// The LSP `initialize` request for a workspace at `root`.
pub(crate) fn initialize(root: &Path) -> Value {
    let uri = url::Url::from_directory_path(root)
        .map(String::from)
        .unwrap_or_default();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    json!({
        "jsonrpc": "2.0",
        "id": INIT_ID,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": uri,
            "capabilities": {},
            "workspaceFolders": [{ "uri": uri, "name": name }],
        },
    })
}

/// One LSP base-protocol frame.
pub(crate) fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Read one LSP frame. EOF before a complete frame is an error.
pub(crate) fn read_frame(r: &mut impl BufRead) -> std::io::Result<Value> {
    use std::io::{Error, ErrorKind};
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "stdout closed"));
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let len = len.ok_or_else(|| Error::new(ErrorKind::InvalidData, "no Content-Length"))?;
    if len > MAX_FRAME {
        return Err(Error::new(ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| Error::new(ErrorKind::InvalidData, e))
}

/// The last `n` lines `r` produced, read to EOF.
fn tail(r: impl Read, n: usize) -> String {
    let mut lines: std::collections::VecDeque<String> = Default::default();
    for line in BufReader::new(r).lines().map_while(Result::ok) {
        if lines.len() == n {
            lines.pop_front();
        }
        lines.push_back(line);
    }
    lines
        .into_iter()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(all(test, unix))]
#[path = "smoke_probe_tests.rs"]
mod tests;
