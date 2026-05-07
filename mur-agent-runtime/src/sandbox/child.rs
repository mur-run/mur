use super::SandboxPolicy;
use std::io;
use std::process::{Child, Command};

pub fn spawn_sandboxed(cmd: Command, _policy: &SandboxPolicy) -> io::Result<Child> {
    let mut cmd = cmd;
    cmd.spawn()
}
