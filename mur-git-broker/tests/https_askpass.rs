#![cfg(unix)]
//! The https credential path through `execute`, end to end: `TokenAuth::new` -> `apply` -> the
//! scrubbed push command -> a child that inherits fd 57. No TLS: `apply` only sets environment
//! and an fd, so a stub `git` that records what it was given proves the same thing.
//!
//! The stub forwards every subcommand except `push` to the real git (so the private repo, the
//! import and the prefetch still run for real) and, on `push`, writes its argv, its environment
//! and whatever fd 57 holds into a directory the test reads back.
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::credential_source::CredentialSource;
use mur_git_broker::enrollment::Endpoint;
use mur_git_broker::{broker::*, error::BrokerError, import::RlimitSpawn, pending::*};
use mur_git_broker::{policy::BrokerLimits, prefetch::*, push::NoAuth};
use secrecy::SecretString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
mod common;
use common::orch::*;
use common::*;

const URL: &str = "https://git.example.com:8443/org/repo.git";
const TOKEN: &str = "s3cr3t-token-value";
const ASKPASS: &str = "/opt/mur/askpass-stub";

struct Fixed(String);
impl CredentialSource for Fixed {
    fn open(&self, _: &SecretPath) -> Result<SecretString, BrokerError> {
        Ok(SecretString::from(self.0.clone()))
    }
}

/// A `git` that records the push and forwards everything else to the real one.
///
/// `bash`, not `sh`: reading fd 57 needs a two-digit redirection, and dash (Debian/Ubuntu's
/// `/bin/sh`) accepts single-digit fds only ("Bad fd number").
fn stub_git(dir: &Path, out: &Path) -> PathBuf {
    let bin = dir.join("git");
    let script = format!(
        r#"#!/bin/bash
for a in "$@"; do
  if [ "$a" = push ]; then
    printf '%s\n' "$@" > '{out}/argv'
    /usr/bin/env > '{out}/env'
    /bin/cat <&57 > '{out}/fd57' 2> '{out}/fd57.err'
    exit 0
  fi
done
exec '{real}' "$@"
"#,
        out = out.display(),
        real = git_bin().display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

struct Rig {
    e: Env,
    b: Broker,
    out: PathBuf,
    /// Holds the stub. Under the target dir rather than `$TMPDIR`, which may be mounted noexec.
    _stub_dir: tempfile::TempDir,
}

/// An https enrollment carrying a credential, a broker on the stub git, and `secret` as what the
/// credential file holds.
fn rig(secret: &str) -> Rig {
    let e = env();
    let out = e.t.path().join("pushed");
    std::fs::create_dir_all(&out).unwrap();
    let stub_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let stub = stub_git(stub_dir.path(), &out);

    let mut p = policy();
    p.canonical_remote_endpoint = URL.into();
    let mut records = records_for(URL, &p);
    records.enrollments[0].credential = Some(Credential {
        secret_path: SecretPath::parse("/secrets/deploy-token").unwrap(),
        username: Username::parse("bot").unwrap(),
    });
    *e.records.0.lock().unwrap() = Ok(Some(records));

    let remote = e.remote.to_str().unwrap().to_string();
    let b = Broker::new(BrokerConfig {
        store_path: e.t.path().join("pending.sqlite"),
        work_root: e.t.path().join("work-root"),
        limits: BrokerLimits::default(),
        git_bin: stub,
        enrollments: Box::new(e.records.clone()),
        // The https URL is never fetched from: prefetch reads the local bare repo instead.
        reader: Box::new(move |_url: &str| {
            Box::new(GitFetchReader {
                remote_url: remote.clone(),
                git_bin: git_bin(),
            })
        }),
        spawn: Box::new(RlimitSpawn),
        auth: Box::new(NoAuth),
        askpass_bin: PathBuf::from(ASKPASS),
        credentials: Box::new(Fixed(secret.into())),
        clock: e.clock.clone(),
    })
    .unwrap();
    Rig {
        e,
        b,
        out,
        _stub_dir: stub_dir,
    }
}

fn submit(r: &Rig) -> (RequestKey, String) {
    let mut d = action_for(&r.e.old, &r.e.new, "refs/heads/agent/x");
    d.request_id = "r1".into();
    // Stamp from the enrollment as it is now, as intake does: its digests cover the credential,
    // so a bare `policy()` would (rightly) no longer match.
    let p =
        r.e.records
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .enrollments[0]
            .policy();
    d.canonical_remote_endpoint = URL.into();
    d.remote_policy_digest = p.remote_digest();
    d.ref_policy_digest = p.digest();
    d.enrollment_epoch = EPOCH;
    d.repo_identity = IDENTITY.into();
    let pack = range_pack(&r.e.work, &r.e.new, Some(&r.e.old), false);
    let k = key("r1");
    let h = r.b.submit_request(&k, &d, &pack).unwrap();
    (k, h)
}

fn recorded(r: &Rig, name: &str) -> String {
    std::fs::read_to_string(r.out.join(name)).unwrap_or_else(|_| panic!("push never wrote {name}"))
}
fn env_lines(r: &Rig) -> Vec<String> {
    recorded(r, "env").lines().map(str::to_owned).collect()
}
fn has(lines: &[String], l: &str) -> bool {
    lines.iter().any(|x| x == l)
}

#[test]
fn an_https_push_gets_the_askpass_wiring_and_the_token_on_fd_57() {
    let r = rig(TOKEN);
    let (k, h) = submit(&r);
    assert_eq!(
        r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap(),
        State::Succeeded
    );
    let env = env_lines(&r);
    // Survives the runner's env_clear, because apply runs after it.
    assert!(has(&env, &format!("GIT_ASKPASS={ASKPASS}")), "{env:?}");
    assert!(has(&env, "MUR_ASKPASS_FD=57"), "{env:?}");
    assert!(
        has(&env, "MUR_ASKPASS_ORIGIN=https://bot@git.example.com:8443"),
        "{env:?}"
    );
    assert!(has(&env, "GIT_CONFIG_COUNT=3"), "{env:?}");
    assert!(has(&env, "GIT_CONFIG_KEY_0=credential.username"), "{env:?}");
    assert!(has(&env, "GIT_CONFIG_VALUE_0=bot"), "{env:?}");
    assert!(has(&env, "GIT_CONFIG_KEY_1=credential.helper"), "{env:?}");
    assert!(has(&env, "GIT_CONFIG_VALUE_1="), "{env:?}");
    assert!(
        has(&env, "GIT_CONFIG_KEY_2=http.followRedirects"),
        "{env:?}"
    );
    assert!(has(&env, "GIT_CONFIG_VALUE_2=false"), "{env:?}");
    // The runner's own scrubbing is still in force next to it.
    assert!(has(&env, "GIT_TERMINAL_PROMPT=0"), "{env:?}");
    assert!(has(&env, "GIT_CONFIG_NOSYSTEM=1"), "{env:?}");
    // fd 57 was open in the child and held exactly the token.
    assert_eq!(recorded(&r, "fd57"), TOKEN);
    assert_eq!(recorded(&r, "fd57.err"), "");
}

#[test]
fn the_token_never_appears_in_argv_or_the_environment() {
    let r = rig(TOKEN);
    let (k, h) = submit(&r);
    r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap();
    assert!(!recorded(&r, "argv").contains(TOKEN));
    assert!(!recorded(&r, "env").contains(TOKEN));
    assert!(
        recorded(&r, "argv").contains(URL),
        "pushes to the bound URL"
    );
}

#[test]
fn an_oversized_token_stops_before_git_is_run_and_nothing_is_pushed() {
    let r = rig(&"x".repeat(mur_git_broker::askpass::MAX_TOKEN_BYTES + 1));
    let (k, h) = submit(&r);
    let err = r.b.on_approval(&k, proof("e1", "r1", &h)).unwrap_err();
    assert!(
        matches!(
            err,
            BrokerError::CredentialUnreadable("credential_unusable")
        ),
        "{err:?}"
    );
    assert!(
        !r.out.join("env").exists(),
        "fail-closed: no push without the credential"
    );
}

#[test]
fn an_endpoint_with_userinfo_cannot_be_enrolled() {
    // The second line of defence in `TokenAuth::new` is redundant only because of this.
    for url in [
        "https://user@git.example.com/org/repo.git",
        "https://user:pw@git.example.com/org/repo.git",
    ] {
        assert!(Endpoint::parse(url).is_err(), "{url} must not parse");
    }
}
