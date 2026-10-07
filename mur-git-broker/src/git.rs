//! The scrubbed git runner (F21-A, §3 item 6): a git invocation that inherits nothing from the
//! caller's environment and can never run a hook.
use crate::oid::ObjectFormat;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct GitOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum GitError {
    Spawn(String),
    Timeout,
    Signal,
}

pub struct GitRunner {
    git: PathBuf,
    git_dir: PathBuf,
}

impl GitRunner {
    pub fn new(git_bin: PathBuf, git_dir: PathBuf) -> Self {
        Self {
            git: git_bin,
            git_dir,
        }
    }

    /// A scrubbed command, **not yet spawned**.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.git);
        c.env_clear()
            .env("PATH", self.git.parent().unwrap_or(Path::new("/usr/bin")))
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args);
        c
    }

    pub fn run(&self, args: &[&str], timeout: Duration) -> Result<GitOutput, GitError> {
        run_with_timeout(self.command(args), timeout)
    }

    pub fn init_bare(git_bin: &Path, path: &Path, fmt: ObjectFormat) -> Result<(), GitError> {
        let mut c = Command::new(git_bin);
        c.env_clear()
            .env("PATH", git_bin.parent().unwrap_or(Path::new("/usr/bin")))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args([
                "init",
                "-q",
                "--bare",
                &format!("--object-format={}", fmt.as_git_arg()),
            ])
            .arg(path);
        let out = run_with_timeout(
            c,
            Duration::from_secs(crate::constants::DEFAULT_GIT_TIMEOUT_SECS),
        )?;
        if out.code == 0 {
            Ok(())
        } else {
            Err(GitError::Spawn(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ))
        }
    }
}

fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        b
    })
}

/// Spawn with null stdin; drain both pipes on threads (a full pipe must not deadlock the wait);
/// poll until the deadline, then kill.
pub fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<GitOutput, GitError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| GitError::Spawn(e.to_string()))?;
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .map_err(|e| GitError::Spawn(e.to_string()))?
        {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::Timeout);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let (stdout, stderr) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    match status.code() {
        Some(code) => Ok(GitOutput {
            code,
            stdout,
            stderr,
        }),
        None => Err(GitError::Signal),
    }
}
