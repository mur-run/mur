//! `ssh -G` resolution for the install connection (plan K7): the agent and
//! identity files the user's own `ssh` would use, `IdentityAgent` included.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use mur_ssh_request::RequestHost;

use super::ssh_config::{AgentSocket, Expand, Resolved, existing_files, expand, parse, query_argv};

fn no_env(_: &str) -> Option<String> {
    None
}

fn x(home: &Path) -> Expand<'_> {
    Expand {
        home,
        local_user: Some("david"),
        host: "box.example",
        remote_user: "deploy",
        port: 2222,
    }
}

#[test]
fn an_identity_agent_and_its_files_are_read() {
    let out = "user deploy\nidentitiesonly no\nidentityagent /Users/d/op.sock\n\
               identityfile none\nidentityfile ~/.ssh/work_key\n";
    assert_eq!(
        parse(out, no_env),
        Resolved {
            agent: AgentSocket::Path("/Users/d/op.sock".into()),
            files: vec!["~/.ssh/work_key".into()],
        }
    );
}

#[test]
fn no_identity_agent_line_means_the_inherited_socket() {
    let out = "identityfile ~/.ssh/id_rsa\nidentityfile ~/.ssh/id_mldsa44_ed25519\n";
    let r = parse(out, no_env);
    assert_eq!(r.agent, AgentSocket::Inherited);
    assert_eq!(r.files.len(), 2, "OpenSSH 10 defaults come from ssh itself");
}

#[test]
fn agent_values_none_env_name_and_variables() {
    let one = |v: &str| {
        parse(&format!("identityagent {v}\n"), |k| {
            (k == "OP").then(|| "/s/op".into())
        })
        .agent
    };
    assert_eq!(one("none"), AgentSocket::Disabled);
    assert_eq!(one("SSH_AUTH_SOCK"), AgentSocket::Inherited);
    assert_eq!(one("$OP"), AgentSocket::Path("/s/op".into()));
    assert_eq!(one("${OP}"), AgentSocket::Path("/s/op".into()));
    assert_eq!(one("$UNSET"), AgentSocket::Disabled);
}

#[test]
fn identity_file_tokens_expand_or_give_up() {
    let home = Path::new("/h/d");
    let x = x(home);
    assert_eq!(expand("~/.ssh/k", &x), Some(PathBuf::from("/h/d/.ssh/k")));
    assert_eq!(
        expand("%d/.ssh/%r@%h:%p-%u%%", &x),
        Some(PathBuf::from("/h/d/.ssh/deploy@box.example:2222-david%"))
    );
    assert_eq!(expand("/abs/k", &x), Some(PathBuf::from("/abs/k")));
    assert_eq!(expand("~other/.ssh/k", &x), None);
    assert_eq!(
        expand("%C", &x),
        None,
        "unknown token: left to ssh's config"
    );
    let no_user = Expand {
        local_user: None,
        ..x
    };
    assert_eq!(expand("%u", &no_user), None);
}

#[test]
fn only_files_that_exist_are_kept_in_order_once() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("work_key"), "k").unwrap();
    fs::write(ssh.join("id_ed25519"), "k").unwrap();
    fs::create_dir(ssh.join("id_rsa")).unwrap();
    let r = Resolved {
        agent: AgentSocket::Inherited,
        files: [
            "~/.ssh/work_key",
            "~/.ssh/id_rsa",
            "~/.ssh/missing",
            "~/.ssh/id_ed25519",
            "%d/.ssh/work_key",
        ]
        .map(String::from)
        .to_vec(),
    };
    assert_eq!(
        existing_files(&r, &x(home.path())),
        vec![ssh.join("work_key"), ssh.join("id_ed25519")]
    );
}

#[test]
fn the_query_targets_the_install_host_with_options_before_the_separator() {
    let h = RequestHost::parse("box.example", Some(2222)).unwrap();
    let argv: Vec<String> = query_argv(&h, "deploy")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        argv,
        ["-G", "-p", "2222", "-l", "deploy", "--", "box.example"]
    );
}

/// The real `ssh -G` on this machine, with a config in a temp dir. Skipped
/// where no `ssh` is installed.
#[test]
fn the_real_ssh_resolves_an_identity_agent_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config");
    fs::write(
        &cfg,
        "Host box.example\n  IdentityAgent /s/op.sock\n  IdentityFile /s/work_key\n",
    )
    .unwrap();
    let Ok(out) = Command::new(mur_ssh_request::SSH_BINARY)
        .arg("-F")
        .arg(&cfg)
        .args(query_argv(
            &RequestHost::parse("box.example", None).unwrap(),
            "deploy",
        ))
        .output()
    else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse(&String::from_utf8_lossy(&out.stdout), no_env);
    assert_eq!(r.agent, AgentSocket::Path("/s/op.sock".into()));
    assert_eq!(r.files, vec!["/s/work_key".to_string()]);
}
