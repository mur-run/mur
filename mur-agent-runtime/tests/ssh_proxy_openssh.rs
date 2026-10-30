//! ssh-agent proxy against a real OpenSSH client and server (#2213, spec
//! "Binding sign requests to the verified session-bind", Tests:
//! "Integration: real OpenSSH client").
//!
//! A throwaway unprivileged `sshd` on loopback, a real `ssh-agent` upstream
//! holding the user key, and the production proxy (`listener::bind` /
//! `serve`, `self_only_gate`) in between. `ssh` talks only to the proxy.
//!
//! What this proves with real bytes, not fixtures: the client's
//! `session-bind@openssh.com` verifies, the sign request it sends passes
//! every binding rule (2-5) and the policy, and reaches the approval gate
//! with the server's real host key. Approved, the upstream's signature goes
//! back and the login succeeds; denied, nothing is signed and the login
//! fails `hitl_denied`. A run with the host key left out of policy must
//! refuse `host_not_allowed` without asking, so the host fingerprint is the
//! server's, not a constant.
//!
//! No network beyond 127.0.0.1. A missing OpenSSH binary skips locally and
//! fails under CI, like `ssh_agent_dest_constraint.rs`.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mur_agent_runtime::hitl::ssh_gate::SshSignGate;
use mur_agent_runtime::hitl::store::{DecisionStore, Settled};
use mur_agent_runtime::hitl::{HitlApprovals, HitlDecision};
use mur_agent_runtime::ssh_proxy::gate::{GateOutcome, SignAsk, SignGate};
use mur_agent_runtime::ssh_proxy::keys::fingerprint;
use mur_agent_runtime::ssh_proxy::listener::{self, HeldUpstream, Proxy};
use mur_agent_runtime::ssh_proxy::policy::{PinTrust, Policy};
use mur_agent_runtime::ssh_proxy::refusal::{RefusalCode, RefusalSink};
use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};
use mur_common::ssh_proxy_refusals::{self, SshRefusal};

const LOOPBACK: &str = "127.0.0.1";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const SSH_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
const CI_ENV_VAR: &str = "CI";
/// Where unprivileged `sshd` usually lives; `PATH` is tried first.
const SSHD_FALLBACKS: [&str; 2] = ["/usr/sbin/sshd", "/usr/local/sbin/sshd"];
/// Codes that would mean the real client's bytes broke a binding rule.
const BINDING_CODES: [RefusalCode; 6] = [
    RefusalCode::NoSessionBind,
    RefusalCode::SessionBindUnverified,
    RefusalCode::SignDataNotUserauth,
    RefusalCode::SessionIdMismatch,
    RefusalCode::SignRequestInconsistent,
    RefusalCode::ForwardingNotSupported,
];

fn in_ci() -> bool {
    std::env::var_os(CI_ENV_VAR).is_some_and(|v| !v.is_empty() && v != "false" && v != "0")
}

fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// `sshd` must be run by absolute path (it re-execs itself).
fn find_sshd() -> Option<PathBuf> {
    on_path("sshd").or_else(|| {
        SSHD_FALLBACKS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// Kills the child even when an assertion unwinds.
struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: spawn: {e}"));
    assert!(
        out.status.success(),
        "{what} failed: {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn keygen(path: &Path, comment: &str) {
    run(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
            .arg(path),
        "ssh-keygen",
    );
}

fn pub_line(key: &Path) -> String {
    std::fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .to_owned()
}

/// Fingerprint the proxy computes for a `.pub` file's key blob.
fn fp_of(key: &Path) -> String {
    use base64::Engine;
    let line = pub_line(key);
    let b64 = line.split_whitespace().nth(1).expect("pub key field");
    fingerprint(
        &base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap(),
    )
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < START_TIMEOUT, "{what} never came up");
        std::thread::sleep(POLL);
    }
}

fn free_port() -> u16 {
    TcpListener::bind((LOOPBACK, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Everything outside the proxy: server, upstream agent, keys.
struct Rig {
    dir: PathBuf,
    port: u16,
    user_key: PathBuf,
    host_key: PathBuf,
    upstream_sock: PathBuf,
    _sshd: Reaped,
    _agent: Reaped,
    _tmp: tempfile::TempDir,
}

impl Rig {
    fn start(sshd: &Path) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let (user_key, host_key) = (dir.join("user_key"), dir.join("host_key"));
        keygen(&user_key, "mur-proxy-it-user");
        keygen(&host_key, "mur-proxy-it-host");
        let authorized = dir.join("authorized_keys");
        std::fs::write(&authorized, format!("{}\n", pub_line(&user_key))).unwrap();

        let port = free_port();
        let config = dir.join("sshd_config");
        std::fs::write(
            &config,
            format!(
                "ListenAddress {LOOPBACK}:{port}\n\
                 HostKey {host}\n\
                 PidFile {pid}\n\
                 AuthorizedKeysFile {authorized}\n\
                 AuthenticationMethods publickey\n\
                 PubkeyAuthentication yes\n\
                 PasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\n\
                 StrictModes no\n\
                 UsePAM no\n\
                 LogLevel VERBOSE\n",
                host = host_key.display(),
                pid = dir.join("sshd.pid").display(),
                authorized = authorized.display(),
            ),
        )
        .unwrap();
        let sshd = Reaped(
            Command::new(sshd)
                .args(["-D", "-e", "-f"])
                .arg(&config)
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(dir.join("sshd.log")).unwrap())
                .spawn()
                .expect("sshd"),
        );
        wait_for("sshd", || {
            std::net::TcpStream::connect((LOOPBACK, port)).is_ok()
        });

        let upstream_sock = dir.join("up.sock");
        let agent = Reaped(
            Command::new("ssh-agent")
                .arg("-D")
                .arg("-a")
                .arg(&upstream_sock)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("ssh-agent"),
        );
        wait_for("ssh-agent", || upstream_sock.exists());
        run(
            Command::new("ssh-add")
                .env("SSH_AUTH_SOCK", &upstream_sock)
                .arg(&user_key),
            "ssh-add",
        );
        Self {
            dir,
            port,
            user_key,
            host_key,
            upstream_sock,
            _sshd: sshd,
            _agent: agent,
            _tmp: tmp,
        }
    }

    fn entitlement(&self, allow_hosts: Vec<String>) -> SshEntitlement {
        SshEntitlement {
            keys: vec![SshKeyRule {
                fingerprint: fp_of(&self.user_key),
                label: Some("integration user key".into()),
                allow_hosts: allow_hosts
                    .into_iter()
                    .map(|host_key| SshHostRule {
                        host_key,
                        label: None,
                    })
                    .collect(),
            }],
        }
    }

    /// `ssh … true` through the proxy at `proxy_sock`, server host key
    /// pinned in a private known_hosts, no user config, agent key only.
    fn ssh(&self, proxy_sock: &Path) -> Command {
        let known_hosts = self.dir.join("known_hosts");
        let host_pub = pub_line(&self.host_key);
        let mut f = host_pub.split_whitespace();
        let (kt, kb) = (f.next().unwrap(), f.next().unwrap());
        std::fs::write(
            &known_hosts,
            format!("[{LOOPBACK}]:{} {kt} {kb}\n", self.port),
        )
        .unwrap();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .expect("USER or LOGNAME");
        let mut cmd = Command::new("ssh");
        cmd.env("SSH_AUTH_SOCK", proxy_sock)
            .args(["-F", "none", "-T", "-v"])
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "StrictHostKeyChecking=yes"])
            .args(["-o", "PreferredAuthentications=publickey"])
            .args(["-o", "IdentitiesOnly=yes"])
            .args(["-o", "ConnectTimeout=5"])
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
            .arg("-o")
            .arg(format!("IdentityAgent={}", proxy_sock.display()))
            .arg("-i")
            .arg(self.user_key.with_extension("pub"))
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-l")
            .arg(user)
            .args([LOOPBACK, "true"])
            .stdin(Stdio::null());
        cmd
    }
}

/// Stands in for gate B: answers `outcome` and records what it was asked,
/// so the test sees the fingerprints the real client's bind carried.
struct Answering {
    outcome: GateOutcome,
    asked: std::sync::Mutex<Vec<SignAsk>>,
}

impl Answering {
    fn new(outcome: GateOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn asked(&self) -> Vec<SignAsk> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SignGate for Answering {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome {
        self.asked.lock().unwrap().push(ask.clone());
        self.outcome.clone()
    }
}

/// Gate B's decision memory, in memory (the real one is a signed channel).
#[derive(Default)]
struct MemStore(std::sync::Mutex<std::collections::HashMap<String, bool>>);

#[async_trait::async_trait]
impl DecisionStore for MemStore {
    async fn lookup(&self, h: &str) -> Option<Settled> {
        self.0.lock().unwrap().get(h).map(
            |&allow| {
                if allow { Settled::Allow } else { Settled::Deny }
            },
        )
    }
    async fn record(&self, r: mur_common::hitl::HitlResponse) {
        self.0.lock().unwrap().insert(r.action_hash, r.allow);
    }
}

/// The production ssh gate, with a person who approves every prompt the
/// way `tool/hitl_respond` does: by `hitl_id` through the approvals map.
/// Returns the gate and a counter of prompts seen.
fn approving_gate_b() -> (Arc<SshSignGate>, Arc<std::sync::atomic::AtomicUsize>) {
    let approvals: HitlApprovals = Default::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
    let prompts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (map, seen) = (approvals.clone(), prompts.clone());
    tokio::spawn(async move {
        while let Some(n) = rx.recv().await {
            assert_eq!(n["method"], "tool/approval_needed");
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let id = n["params"]["calls"][0]["hitl_id"]
                .as_str()
                .unwrap()
                .to_string();
            if let Some(answer) = map.lock().await.remove(&id) {
                let _ = answer.send(HitlDecision {
                    allow: true,
                    reason: None,
                    surface: Some("test".into()),
                    awaiting_human: false,
                    refusal_code: None,
                });
            }
        }
    });
    let gate = Arc::new(SshSignGate {
        agent: "integration".into(),
        store: Arc::new(MemStore::default()),
        approvals,
        notifier: tx,
        parking: Default::default(),
        wait: SSH_TIMEOUT,
        audit_only: false,
        pending: Default::default(),
    });
    (gate, prompts)
}

/// Start the production proxy for `ent` with `approvals` as its gate, run
/// one `ssh`, return its output and the refusals the proxy recorded (most
/// recent first).
async fn login_through_proxy(
    rig: &Rig,
    ent: SshEntitlement,
    approvals: Arc<dyn SignGate>,
    tag: &str,
) -> (Output, Vec<SshRefusal>) {
    let home = rig.dir.join(format!("home-{tag}"));
    std::fs::create_dir_all(&home).unwrap();
    let sock = listener::socket_path(&home);
    assert!(
        listener::socket_path_fits(&sock),
        "temp path too long: {}",
        sock.display()
    );
    let upstream = HeldUpstream::connect(&rig.upstream_sock)
        .await
        .expect("upstream ssh-agent");
    let proxy = Arc::new(Proxy {
        policy: Policy::build(&ent, PinTrust::Pinned, Policy::UNPINNED_IS_UNTRUSTED),
        upstream: Arc::new(upstream),
        sink: RefusalSink::new(Some(home.clone())),
        // The production gate: `ssh` is this test process's child.
        gate: listener::self_only_gate(std::process::id(), rig.dir.join("no-agents")),
        approvals,
    });
    let serving = tokio::spawn(listener::serve(listener::bind(&sock).unwrap(), proxy));

    // Async, so the proxy keeps serving while `ssh` runs; a child of this
    // process, so the production peer gate admits it.
    let ssh = tokio::process::Command::from(rig.ssh(&sock)).output();
    let out = tokio::time::timeout(SSH_TIMEOUT, ssh)
        .await
        .expect("ssh timed out")
        .expect("ssh spawn");
    serving.abort();
    (out, ssh_proxy_refusals::read(&home))
}

fn explain(out: &Output, rig: &Rig) -> String {
    format!(
        "ssh {:?}\n--- ssh stderr ---\n{}\n--- sshd log ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(rig.dir.join("sshd.log")).unwrap_or_default()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_openssh_login_is_signed_only_when_approved() {
    for bin in ["ssh", "ssh-agent", "ssh-add", "ssh-keygen"] {
        if on_path(bin).is_none() {
            assert!(!in_ci(), "{bin} not on PATH under CI");
            eprintln!("SKIP real_openssh_login: {bin} not on PATH");
            return;
        }
    }
    let Some(sshd) = find_sshd() else {
        assert!(!in_ci(), "sshd not found under CI");
        eprintln!("SKIP real_openssh_login: sshd not found");
        return;
    };
    let rig = Rig::start(&sshd);
    let (key_fp, host_fp) = (fp_of(&rig.user_key), fp_of(&rig.host_key));

    // Allowed key, allowed host, approved through the production gate B:
    // every binding rule and the policy pass, one `tool/approval_needed`
    // goes out, the answer comes back by `hitl_id`, and the upstream's
    // signature logs in.
    let (gate_b, prompts) = approving_gate_b();
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        gate_b.clone(),
        "approved",
    )
    .await;
    assert!(
        out.status.success(),
        "approved login failed; refusals {:?}\n{}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>(),
        explain(&out, &rig)
    );
    assert!(
        refusals.is_empty(),
        "an approved login recorded refusals: {:?}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>()
    );
    assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Same pair again, inside the TTL: remembered, so no second prompt.
    let (out, _) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        gate_b,
        "remembered",
    )
    .await;
    assert!(out.status.success(), "{}", explain(&out, &rig));
    assert_eq!(
        prompts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a remembered approval prompted again"
    );

    // The gate is asked about exactly the real key and the server's real
    // host key.
    let probe = Answering::new(GateOutcome::Allow);
    let (out, _) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        probe.clone(),
        "asked",
    )
    .await;
    assert!(out.status.success(), "{}", explain(&out, &rig));
    let asked = probe.asked();
    assert_eq!(asked.len(), 1, "one prompt per login: {asked:?}");
    assert_eq!(asked[0].key_fp, key_fp);
    assert_eq!(asked[0].host_key_fp, host_fp);

    // Same pair, denied: nothing is signed, the login fails, and the reason
    // is the person's answer, not a binding rule.
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        Answering::new(GateOutcome::Deny),
        "denied",
    )
    .await;
    assert!(
        !out.status.success(),
        "denied login succeeded\n{}",
        explain(&out, &rig)
    );
    let codes: Vec<&str> = refusals.iter().map(|r| r.code.as_str()).collect();
    for c in BINDING_CODES {
        assert!(
            !codes.contains(&c.as_str()),
            "binding rule refused real client bytes: {codes:?}\n{}",
            explain(&out, &rig)
        );
    }
    let last = refusals
        .first()
        .unwrap_or_else(|| panic!("no refusal recorded\n{}", explain(&out, &rig)));
    assert_eq!(
        last.code,
        RefusalCode::HitlDenied.as_str(),
        "{codes:?}\n{}",
        explain(&out, &rig)
    );
    assert_eq!(last.key_fp.as_deref(), Some(key_fp.as_str()));
    assert_eq!(last.host_key_fp.as_deref(), Some(host_fp.as_str()));

    // Same key, server's host key not listed: the bind carried the real
    // server key, so the lookup must miss.
    // The gate must never be asked: a policy refusal is not a prompt.
    let never = Answering::new(GateOutcome::Allow);
    let (out, refusals) =
        login_through_proxy(&rig, rig.entitlement(Vec::new()), never.clone(), "no-host").await;
    assert!(!out.status.success());
    let last = refusals
        .first()
        .unwrap_or_else(|| panic!("no refusal recorded\n{}", explain(&out, &rig)));
    assert_eq!(
        last.code,
        RefusalCode::HostNotAllowed.as_str(),
        "{}",
        explain(&out, &rig)
    );
    assert_eq!(last.host_key_fp.as_deref(), Some(host_fp.as_str()));
    assert!(
        never.asked().is_empty(),
        "host_not_allowed reached the gate"
    );
}
