//! The OpenSSH rig shared by the ssh-agent proxy integration tests: a
//! throwaway unprivileged `sshd` on loopback, a real `ssh-agent` upstream,
//! keys, and the helpers that drive `ssh` through the production proxy.
//! Moved verbatim from `ssh_proxy_openssh.rs` (plan Task 16.8); each test
//! crate uses only part of it.
#![allow(dead_code)]

use std::net::TcpListener;
pub use std::path::{Path, PathBuf};
pub use std::process::{Child, Command, Output, Stdio};
pub use std::sync::Arc;
use std::time::{Duration, Instant};

pub use mur_agent_runtime::hitl::ssh_gate::SshSignGate;
pub use mur_agent_runtime::hitl::store::{DecisionStore, Settled};
pub use mur_agent_runtime::hitl::{HitlApprovals, HitlDecision};
pub use mur_agent_runtime::ssh_proxy::gate::{GateOutcome, SignAsk, SignGate};
pub use mur_agent_runtime::ssh_proxy::keys::fingerprint;
pub use mur_agent_runtime::ssh_proxy::listener::{self, DedicatedUpstream, HeldUpstream, Proxy};
pub use mur_agent_runtime::ssh_proxy::policy::{PinTrust, Policy};
pub use mur_agent_runtime::ssh_proxy::refusal::{RefusalCode, RefusalSink};
pub use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};
pub use mur_common::ssh_proxy_refusals::{self, SshRefusal};

pub const LOOPBACK: &str = "127.0.0.1";
pub const START_TIMEOUT: Duration = Duration::from_secs(10);
pub const SSH_TIMEOUT: Duration = Duration::from_secs(30);
pub const POLL: Duration = Duration::from_millis(50);
pub const CI_ENV_VAR: &str = "CI";
/// Where unprivileged `sshd` usually lives; `PATH` is tried first.
pub const SSHD_FALLBACKS: [&str; 2] = ["/usr/sbin/sshd", "/usr/local/sbin/sshd"];
/// What `sshd` logs when its pre-auth child cannot enter its own sandbox.
/// macOS refuses `sandbox_init` inside an already-sandboxed process (an agent
/// shell, for one), so every connection is reset before key exchange.
pub const NESTED_SANDBOX_MARKER: &str = "sandbox_init: Operation not permitted";
/// How long the probe waits for the server's identification banner.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Codes that would mean the real client's bytes broke a binding rule.
/// Which userauth request the client signs (RFC 4252 §7 vs. OpenSSH
/// `PROTOCOL` §2.5). `ssh -vvv` names the method it used in
/// `sign_and_send_pubkey: using <method> with <keytype> <fp>`; the test
/// reads that line so a server that does not offer hostbound cannot make
/// the hostbound case pass silently as plain publickey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Publickey,
    Hostbound,
}

impl Method {
    pub fn ssh_option(self) -> &'static str {
        match self {
            Method::Publickey => "PubkeyAuthentication=unbound",
            Method::Hostbound => "PubkeyAuthentication=host-bound",
        }
    }
    pub fn wire_name(self) -> &'static str {
        match self {
            Method::Publickey => "publickey",
            Method::Hostbound => "publickey-hostbound-v00@openssh.com",
        }
    }
    /// The `debug3` line `sshconnect2.c` prints just before signing.
    pub fn debug_line(self) -> String {
        format!("using {} with ", self.wire_name())
    }
}

pub const BINDING_CODES: [RefusalCode; 6] = [
    RefusalCode::NoSessionBind,
    RefusalCode::SessionBindUnverified,
    RefusalCode::SignDataNotUserauth,
    RefusalCode::SessionIdMismatch,
    RefusalCode::SignRequestInconsistent,
    RefusalCode::ForwardingNotSupported,
];

pub fn in_ci() -> bool {
    std::env::var_os(CI_ENV_VAR).is_some_and(|v| !v.is_empty() && v != "false" && v != "0")
}

pub fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// `sshd` must be run by absolute path (it re-execs itself).
pub fn find_sshd() -> Option<PathBuf> {
    on_path("sshd").or_else(|| {
        SSHD_FALLBACKS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// Opens one connection and reads the server banner. A healthy `sshd`
/// answers with `SSH-`; one that cannot sandbox its pre-auth child resets
/// the connection and logs [`NESTED_SANDBOX_MARKER`]. Any other failure is
/// left for the test itself to report with the full log.
fn sshd_unusable(dir: &Path, port: u16) -> Option<String> {
    use std::io::Read;
    let mut banner = [0u8; 4];
    let answered = std::net::TcpStream::connect((LOOPBACK, port))
        .and_then(|mut s| {
            s.set_read_timeout(Some(PROBE_TIMEOUT))?;
            s.read_exact(&mut banner)
        })
        .is_ok_and(|()| &banner == b"SSH-");
    if answered {
        return None;
    }
    let log = dir.join("sshd.log");
    let start = Instant::now();
    while start.elapsed() < PROBE_TIMEOUT {
        if std::fs::read_to_string(&log).is_ok_and(|l| l.contains(NESTED_SANDBOX_MARKER)) {
            return Some(format!(
                "sshd cannot sandbox its pre-auth child ({NESTED_SANDBOX_MARKER}); \
                 this process is already inside a macOS sandbox"
            ));
        }
        std::thread::sleep(POLL);
    }
    None
}

/// Kills the child even when an assertion unwinds.
pub struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(cmd: &mut Command, what: &str) {
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

pub fn keygen(path: &Path, comment: &str) {
    run(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
            .arg(path),
        "ssh-keygen",
    );
}

pub fn pub_line(key: &Path) -> String {
    std::fs::read_to_string(key.with_extension("pub"))
        .unwrap()
        .trim()
        .to_owned()
}

/// Fingerprint the proxy computes for a `.pub` file's key blob.
pub fn fp_of(key: &Path) -> String {
    use base64::Engine;
    let line = pub_line(key);
    let b64 = line.split_whitespace().nth(1).expect("pub key field");
    fingerprint(
        &base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap(),
    )
}

pub fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < START_TIMEOUT, "{what} never came up");
        std::thread::sleep(POLL);
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind((LOOPBACK, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Everything outside the proxy: server, upstream agent, keys.
pub struct Rig {
    pub dir: PathBuf,
    pub port: u16,
    pub user_key: PathBuf,
    pub host_key: PathBuf,
    pub upstream_sock: PathBuf,
    pub _sshd: Reaped,
    pub _agent: Reaped,
    pub _tmp: tempfile::TempDir,
}

impl Rig {
    /// Starts the rig, or returns `None` (local only) when this host cannot
    /// run `sshd` at all because it is already inside a macOS sandbox. Under
    /// CI that is a hard failure, same as a missing `sshd`.
    pub fn start(sshd: &Path, test: &str) -> Option<Self> {
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
        if let Some(reason) = sshd_unusable(&dir, port) {
            assert!(!in_ci(), "{reason} under CI");
            eprintln!("SKIP {test}: {reason}");
            return None;
        }

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
        Some(Self {
            dir,
            port,
            user_key,
            host_key,
            upstream_sock,
            _sshd: sshd,
            _agent: agent,
            _tmp: tmp,
        })
    }

    pub fn entitlement(&self, allow_hosts: Vec<String>) -> SshEntitlement {
        SshEntitlement {
            keys: vec![SshKeyRule {
                fingerprint: fp_of(&self.user_key),
                label: Some("integration user key".into()),
                allow_hosts: allow_hosts
                    .into_iter()
                    .map(|host_key| SshHostRule {
                        host_key,
                        label: None,
                        known_hosts: Vec::new(),
                    })
                    .collect(),
            }],
        }
    }

    /// `ssh … true` through the proxy at `proxy_sock`, server host key
    /// pinned in a private known_hosts, no user config, agent key only.
    pub fn ssh(&self, proxy_sock: &Path, method: Method) -> Command {
        self.ssh_as(proxy_sock, method, &self.user_key)
    }

    /// [`Rig::ssh`] offering `identity` instead of the user key.
    pub fn ssh_as(&self, proxy_sock: &Path, method: Method, identity: &Path) -> Command {
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
            .args(["-F", "none", "-T", "-vvv"])
            .args(["-o", method.ssh_option()])
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
            .arg(identity.with_extension("pub"))
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
pub struct Answering {
    outcome: GateOutcome,
    asked: std::sync::Mutex<Vec<SignAsk>>,
}

impl Answering {
    pub fn new(outcome: GateOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }
    pub fn asked(&self) -> Vec<SignAsk> {
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
pub struct MemStore(pub std::sync::Mutex<std::collections::HashMap<String, bool>>);

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
pub fn approving_gate_b() -> (Arc<SshSignGate>, Arc<std::sync::atomic::AtomicUsize>) {
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
pub async fn login_through_proxy(
    rig: &Rig,
    ent: SshEntitlement,
    approvals: Arc<dyn SignGate>,
    method: Method,
    tag: &str,
) -> (Output, Vec<SshRefusal>) {
    login_with_upstreams(rig, ent, approvals, method, tag, None, &rig.user_key).await
}

/// [`login_through_proxy`] with a dedicated upstream beside the user one,
/// offering `identity` (its `.pub`) as the only key.
pub async fn login_with_upstreams(
    rig: &Rig,
    ent: SshEntitlement,
    approvals: Arc<dyn SignGate>,
    method: Method,
    tag: &str,
    dedicated: Option<Arc<DedicatedUpstream>>,
    identity: &Path,
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
        dedicated,
        sink: RefusalSink::new(Some(home.clone())),
        marks: Default::default(),
        // The production gate: `ssh` is this test process's child.
        gate: listener::self_only_gate(std::process::id(), rig.dir.join("no-agents")),
        approvals,
    });
    let serving = tokio::spawn(listener::serve(listener::bind(&sock).unwrap(), proxy));

    // Async, so the proxy keeps serving while `ssh` runs; a child of this
    // process, so the production peer gate admits it.
    let ssh = tokio::process::Command::from(rig.ssh_as(&sock, method, identity)).output();
    let out = tokio::time::timeout(SSH_TIMEOUT, ssh)
        .await
        .expect("ssh timed out")
        .expect("ssh spawn");
    serving.abort();
    (out, ssh_proxy_refusals::read(&home))
}

/// The client signed a `method` request: its debug line says so, and no
/// other method's line appears (a fallback after a refused first attempt
/// would print both).
pub fn assert_signed_with(out: &Output, method: Method, rig: &Rig) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&method.debug_line()),
        "ssh did not sign a {} request\n{}",
        method.wire_name(),
        explain(out, rig)
    );
    let other = match method {
        Method::Publickey => Method::Hostbound,
        Method::Hostbound => Method::Publickey,
    };
    assert!(
        !stderr.contains(&other.debug_line()),
        "ssh also signed a {} request\n{}",
        other.wire_name(),
        explain(out, rig)
    );
}

pub fn explain(out: &Output, rig: &Rig) -> String {
    format!(
        "ssh {:?}\n--- ssh stderr ---\n{}\n--- sshd log ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(rig.dir.join("sshd.log")).unwrap_or_default()
    )
}
