//! The public-key install: nothing typed reaches a remote shell (K6), and the
//! remote script itself, run here by a local `sh`.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use mur_ssh_request::RequestHost;

use super::advisory::{AdvisoryResult, REMOTE_NOOP, SSH_OWN_FAILURE, advisory_argv, classify};
use super::install::{
    InstallRequest, Installed, MARK_ADDED, MARK_PRESENT, REMOTE_SCRIPT, check, install_argv,
    known_hosts_file, parse_result, remote_command,
};

/// A well-formed ed25519 public line (the key bytes are 0..32).
const LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f mur-agent-bot";
const KNOWN: &str = "gh.example ssh-ed25519 AAAAH\n";

fn host(name: &str, port: Option<u16>) -> RequestHost {
    RequestHost::parse(name, port).unwrap()
}

fn req<'a>(host: &'a RequestHost, user: &'a str, line: &'a str) -> InstallRequest<'a> {
    InstallRequest {
        host,
        user,
        public_line: line,
        known_hosts: KNOWN,
    }
}

fn strs(argv: &[std::ffi::OsString]) -> Vec<String> {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Where `needle` appears, as exact arguments.
fn at(argv: &[String], needle: &str) -> Vec<usize> {
    argv.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == needle)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn the_user_follows_dash_l_and_the_host_follows_the_separator() {
    let kh = Path::new("/k/known");
    let (h1, h2) = (host("gh.example", None), host("box.example", Some(2222)));
    let a = strs(&install_argv(&req(&h1, "git", LINE), kh).unwrap());
    let b = strs(&install_argv(&req(&h2, "deploy", LINE), kh).unwrap());

    for (argv, user, name, port) in [
        (&a, "git", "gh.example", "22"),
        (&b, "deploy", "box.example", "2222"),
    ] {
        let u = at(argv, user);
        assert_eq!(u.len(), 1, "{argv:?}");
        assert_eq!(argv[u[0] - 1], "-l");
        let h = at(argv, name);
        assert_eq!(h.len(), 1, "{argv:?}");
        assert_eq!(argv[h[0] - 1], "--");
        assert_eq!(h[0], argv.len() - 2, "only the remote command follows");
        let p = at(argv, "-p");
        assert_eq!(argv[p[0] + 1], port);
        for must in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "GlobalKnownHostsFile=/dev/null",
            "UserKnownHostsFile=\"/k/known\"",
        ] {
            assert_eq!(at(argv, must).len(), 1, "{must} in {argv:?}");
        }
        assert!(
            !argv.iter().any(|x| x.contains("AAAAC3")),
            "the key line goes in on stdin, never in argv"
        );
    }
    assert_eq!(a.last(), b.last(), "nothing is interpolated");
    assert_eq!(a.last().unwrap(), &remote_command());
    assert_eq!(remote_command(), format!("exec sh -c '{REMOTE_SCRIPT}'"));
}

#[test]
fn the_remote_script_survives_its_wrapper() {
    assert!(
        !REMOTE_SCRIPT.contains('\''),
        "it sits inside single quotes"
    );
    assert!(!REMOTE_SCRIPT.contains('!'), "csh history expansion");
    assert!(REMOTE_SCRIPT.contains(MARK_ADDED) && REMOTE_SCRIPT.contains(MARK_PRESENT));
}

#[test]
fn a_bad_user_or_key_line_is_refused_before_any_process_runs() {
    let h = host("gh.example", None);
    for user in ["-oProxyCommand=x", "a b", "", "a;b", "$(id)"] {
        assert!(check(&req(&h, user, LINE)).is_err(), "{user:?}");
    }
    let two = format!("{LINE}\n{LINE}");
    for line in [
        two.as_str(),
        "ssh-ed25519 AAAAC3Nz\tx",
        "",
        "not a key",
        "ssh-rsa AAAAB3NzaC1yc2E= me",
        "command=\"x\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f",
    ] {
        let e = check(&req(&h, "git", line)).unwrap_err();
        assert!(e.to_string().contains("nothing was sent"), "{line:?}: {e}");
    }
    let mut no_keys = req(&h, "git", LINE);
    no_keys.known_hosts = " \n";
    assert!(check(&no_keys).is_err(), "no host key to check against");
    check(&req(&h, "git", LINE)).unwrap();
}

#[test]
fn only_exit_zero_with_a_mark_is_success() {
    assert_eq!(
        parse_result(Some(0), "mur-key-added\n", "").unwrap(),
        Installed::Added
    );
    assert_eq!(
        parse_result(Some(0), "motd\nmur-key-present\n", "").unwrap(),
        Installed::AlreadyPresent
    );
    let e = parse_result(Some(255), "", "a\nb\nPermission denied (publickey).\n").unwrap_err();
    let e = e.to_string();
    assert!(
        e.contains("exit 255") && e.contains("Permission denied"),
        "{e}"
    );
    assert!(parse_result(Some(1), "mur-key-added\n", "").is_err());
    assert!(parse_result(Some(0), "", "").is_err(), "no mark, no claim");
    assert!(parse_result(None, "mur-key-added\n", "").is_err());
}

#[test]
fn the_known_hosts_file_is_private_and_gone_after_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let f = known_hosts_file(Some(dir.path()), "gh.example ssh-ed25519 AAAAH").unwrap();
    let path = f.path().to_path_buf();
    assert_eq!(path.parent(), Some(dir.path()));
    assert_eq!(fs::read_to_string(&path).unwrap(), KNOWN);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(f);
    assert!(!path.exists());
    assert!(known_hosts_file(None, KNOWN).is_err());
}

/// `REMOTE_SCRIPT` exactly as the server's shell would get it.
fn run_remote(home: &Path, stdin: &str) -> String {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(remote_command())
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_remote_script_adds_the_line_once() {
    let home = tempfile::tempdir().unwrap();
    let auth = home.path().join(".ssh/authorized_keys");
    let input = format!("{LINE}\n");

    assert_eq!(run_remote(home.path(), &input), MARK_ADDED);
    assert_eq!(fs::read_to_string(&auth).unwrap(), input);
    assert_eq!(mode(&auth), 0o600);
    assert_eq!(mode(auth.parent().unwrap()), 0o700);

    let before = fs::read(&auth).unwrap();
    assert_eq!(run_remote(home.path(), &input), MARK_PRESENT);
    assert_eq!(fs::read(&auth).unwrap(), before, "byte-identical");
}

#[test]
fn the_remote_script_keeps_what_was_there() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    fs::create_dir(&ssh).unwrap();
    let auth = ssh.join("authorized_keys");
    // No trailing newline: the appended key must not join this line.
    fs::write(&auth, "ssh-ed25519 AAAAother someone").unwrap();

    assert_eq!(run_remote(home.path(), &format!("{LINE}\n")), MARK_ADDED);

    assert_eq!(
        fs::read_to_string(&auth).unwrap(),
        format!("ssh-ed25519 AAAAother someone\n{LINE}\n")
    );
}

#[test]
fn the_remote_script_treats_the_line_as_data() {
    let home = tempfile::tempdir().unwrap();
    let odd = "ssh-ed25519 AAAA $(touch pwned) `id` .* \\n";
    assert_eq!(run_remote(home.path(), &format!("{odd}\n")), MARK_ADDED);
    let auth = home.path().join(".ssh/authorized_keys");
    assert_eq!(fs::read_to_string(&auth).unwrap(), format!("{odd}\n"));
    assert_eq!(run_remote(home.path(), &format!("{odd}\n")), MARK_PRESENT);
    // A regex-looking line is matched as a fixed string, not as a pattern.
    assert_eq!(run_remote(home.path(), "ssh-ed25519 AAAA .*\n"), MARK_ADDED);
}

#[test]
fn the_advisory_test_offers_only_the_dedicated_key() {
    let h = host("gh.example", Some(2200));
    let argv =
        strs(&advisory_argv(Path::new("/s/id_ed25519"), &h, "git", Path::new("/k/known")).unwrap());
    assert_eq!(at(&argv, "IdentitiesOnly=yes").len(), 1);
    let i = at(&argv, "-i");
    assert_eq!(argv[i[0] + 1], "/s/id_ed25519");
    let u = at(&argv, "git");
    assert_eq!(argv[u[0] - 1], "-l");
    let hi = at(&argv, "gh.example");
    assert_eq!(argv[hi[0] - 1], "--");
    assert_eq!(argv.last().unwrap(), REMOTE_NOOP);
    assert_eq!(at(&argv, "BatchMode=yes").len(), 1);
    assert_eq!(at(&argv, "StrictHostKeyChecking=yes").len(), 1);
    assert!(
        i[0] < hi[0] && u[0] < hi[0],
        "options precede the separator"
    );
}

#[test]
fn exit_255_is_the_only_refusal() {
    let refused = classify(Some(SSH_OWN_FAILURE), "x\nPermission denied (publickey).\n");
    assert!(
        matches!(&refused, AdvisoryResult::NotAccepted(w) if w.contains("Permission denied")),
        "{refused:?}"
    );
    for code in [0, 1, 2, 127, 128, 254] {
        assert_eq!(classify(Some(code), ""), AdvisoryResult::Accepted, "{code}");
    }
    assert!(matches!(classify(None, ""), AdvisoryResult::NotAccepted(_)));
    assert!(
        refused
            .text()
            .contains("advisory: run outside the agent's sandbox")
    );
    assert!(
        AdvisoryResult::Accepted
            .text()
            .contains("advisory: run outside the agent's sandbox")
    );
}
