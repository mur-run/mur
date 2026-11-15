//! The setup steps over fakes: the verdict table, host selection, the spawn
//! change, and `apply_and_restart` against a temp `MUR_HOME` with an
//! injected lifecycle, so no service manager or process is touched.

use std::path::Path;

use anyhow::{Result, anyhow};
use mur_common::agent::{
    ProcessesEntitlement, SpawnMode, SshEntitlement, SshHostRule, SshKeyRule, SshKnownHost,
};
use mur_common::identity::AgentIdentity;

use super::super::edit::{AllowOutcome, HostGrant};
use super::super::resolve::ScannedKey;
use super::super::tests::{fp, on_disk, write_agent};
use super::apply::{Lifecycle, Restart};
use super::preapprove::{Pair, Preapproval, allowed_pairs, skip_reason_on};
use super::verdict::{HostChoice, select, verdict};
use super::*;
use mur_agent_runtime::sandbox::policy::ExecBackend;

pub(super) const AGENT: &str = "bot";
pub(super) const HOST: &str = "gh.example";
const OFFER: plan::Dedicated = plan::Dedicated::Offer;

fn target() -> mur_ssh_request::RequestHost {
    mur_ssh_request::RequestHost::parse(HOST, None).unwrap()
}

pub(super) fn scanned(c: char) -> ScannedKey {
    ScannedKey {
        fingerprint: fp(c),
        entry: SshKnownHost {
            host: HOST.into(),
            key_type: "ssh-ed25519".into(),
            key_base64: format!("AAAA{c}"),
        },
    }
}

#[test]
fn verdict_follows_the_table() {
    let scan = [scanned('A'), scanned('B')];
    assert_eq!(
        verdict(&scan, &[], false),
        (Verdict::Unknown { downgraded: false }, vec![])
    );
    assert_eq!(
        verdict(&scan, &[fp('B')], false),
        (Verdict::Trusted, vec![fp('B')])
    );
    assert_eq!(
        verdict(&scan, &[fp('B')], true),
        (Verdict::Unknown { downgraded: true }, vec![]),
        "a match the agent could have written proves nothing"
    );
    assert_eq!(
        verdict(&scan, &[fp('Z')], false),
        (Verdict::Differs, vec![])
    );
}

#[test]
fn select_never_grants_more_than_the_table_allows() {
    let scan = [scanned('A'), scanned('B')];
    let none = HostChoice::default();
    let unknown = Verdict::Unknown { downgraded: false };

    let got = select(&target(), &scan, &Verdict::Trusted, &[fp('A')], &none).unwrap();
    assert_eq!(got, vec![scanned('A')]);

    assert!(
        select(&target(), &scan, &unknown, &[], &none).is_err(),
        "nothing vouched"
    );
    let tick = HostChoice {
        pasted: None,
        ticked: vec![fp('B'), fp('Q')],
    };
    assert_eq!(
        select(&target(), &scan, &unknown, &[], &tick).unwrap(),
        vec![scanned('B')],
        "a ticked fingerprint the host never offered is ignored"
    );

    let paste_missing = HostChoice {
        pasted: Some(fp('Q')),
        ticked: vec![],
    };
    let e = select(&target(), &scan, &unknown, &[], &paste_missing).unwrap_err();
    assert!(e.to_string().contains("did not offer"), "{e}");

    // Differs: only the operator's fingerprint gets through.
    let e = select(&target(), &scan, &Verdict::Differs, &[], &tick).unwrap_err();
    assert!(e.to_string().contains("`ssh-keygen -R gh.example`"), "{e}");
    let paste = HostChoice {
        pasted: Some(fp('A')),
        ticked: vec![],
    };
    assert_eq!(
        select(&target(), &scan, &Verdict::Differs, &[], &paste).unwrap(),
        vec![scanned('A')]
    );
}

fn procs(mode: SpawnMode, allowed: &[&str]) -> ProcessesEntitlement {
    let mut p = mur_common::agent::AgentProfile::default_for_tests()
        .entitlements
        .processes;
    p.spawn.mode = mode;
    p.spawn.allowed = allowed.iter().map(|s| s.to_string()).collect();
    p
}

#[test]
fn spawn_change_adds_ssh_and_none_needs_strict() {
    let mut p = procs(SpawnMode::None, &[]);
    assert_eq!(SpawnChange::needed(&p), Some(SpawnChange::NoneToStrict));
    SpawnChange::NoneToStrict.apply(&mut p);
    assert_eq!(p.spawn.mode, SpawnMode::Strict);
    assert_eq!(SpawnChange::needed(&p), None);

    let mut p = procs(SpawnMode::Strict, &["git"]);
    assert_eq!(SpawnChange::needed(&p), Some(SpawnChange::AddSsh));
    SpawnChange::AddSsh.apply(&mut p);
    SpawnChange::AddSsh.apply(&mut p);
    assert_eq!(p.spawn.allowed, vec!["git", "ssh"], "added once");
}

fn pair(key: char, host: char) -> Pair {
    Pair {
        key_fp: fp(key),
        key_label: None,
        host_key_fp: fp(host),
        host_label: None,
    }
}

#[test]
fn a_preapproval_never_widens_the_grant() {
    let ssh = SshEntitlement {
        keys: vec![SshKeyRule {
            fingerprint: fp('A'),
            label: None,
            allow_hosts: vec![SshHostRule {
                host_key: fp('H'),
                label: None,
                known_hosts: vec![],
            }],
        }],
    };
    let asked = [pair('A', 'H'), pair('A', 'X'), pair('B', 'H')];
    assert_eq!(allowed_pairs(&ssh, &asked), vec![pair('A', 'H')]);
}

#[derive(Default)]
struct FakeLife {
    pid: Option<u32>,
    fail_stop: bool,
    fail_start: bool,
    calls: Vec<&'static str>,
}

impl Lifecycle for FakeLife {
    fn running_pid(&self) -> Option<u32> {
        self.pid
    }
    fn stop(&mut self, _pid: u32) -> Result<()> {
        self.calls.push("stop");
        if self.fail_stop {
            return Err(anyhow!("still alive"));
        }
        self.pid = None;
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        self.calls.push("start");
        if self.fail_start {
            return Err(anyhow!("not ready"));
        }
        Ok(())
    }
}

fn approval(spawn: Option<SpawnChange>, consented: bool, restart: bool) -> Approval {
    Approval {
        key_fp: fp('A'),
        key_label: Some("laptop".into()),
        hosts: vec![HostGrant {
            host_key_fp: fp('H'),
            label: Some(HOST.into()),
            known_host: Some(scanned('H').entry),
        }],
        spawn,
        spawn_consented: consented,
        restart,
        install: None,
    }
}

/// For approvals with no install: reaching the installer is the failure.
struct NeverInstall;

impl super::install::Installer for NeverInstall {
    fn install(&self, _: &super::install::InstallRequest) -> Result<super::install::Installed> {
        panic!("no install was chosen")
    }
}

pub(super) struct Home {
    _tmp: tempfile::TempDir,
    _env: mur_common::test_env::EnvGuard,
}

impl Home {
    pub(super) fn new() -> (Self, std::path::PathBuf) {
        let mut env = mur_common::test_env::EnvGuard::hold();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        env.set_var("MUR_HOME", &home);
        write_agent(&home, AGENT);
        AgentIdentity::generate()
            .save(&home.join("agents").join(AGENT))
            .unwrap();
        (
            Self {
                _tmp: tmp,
                _env: env,
            },
            home,
        )
    }
}

fn granted(home: &Path) -> bool {
    let p = on_disk(home, AGENT);
    p.entitlements
        .ssh
        .keys
        .iter()
        .any(|k| k.fingerprint == fp('A') && k.allow_hosts.iter().any(|h| h.host_key == fp('H')))
}

#[test]
fn none_to_strict_without_its_consent_writes_nothing() {
    let (_h, home) = Home::new();
    let mut life = FakeLife::default();
    let e = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(Some(SpawnChange::NoneToStrict), false, true),
        &mut life,
        &NeverInstall,
    )
    .unwrap_err();
    assert!(e.to_string().contains("own consent"), "{e}");
    assert!(!granted(&home));
    assert!(life.calls.is_empty());
}

#[test]
fn no_restart_writes_the_grant_and_touches_nothing_else() {
    let (_h, home) = Home::new();
    let mut life = FakeLife {
        pid: Some(42),
        ..Default::default()
    };
    let a = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(Some(SpawnChange::AddSsh), false, false),
        &mut life,
        &NeverInstall,
    )
    .unwrap();
    assert_eq!(a.outcome, AllowOutcome::Added);
    assert_eq!(a.restart, Restart::Skipped);
    assert_eq!(a.preapproval, Preapproval::NotAttempted);
    assert!(life.calls.is_empty());
    assert!(granted(&home));
    let p = on_disk(&home, AGENT);
    assert!(
        p.entitlements
            .processes
            .spawn
            .allowed
            .iter()
            .any(|b| b == "ssh")
    );
}

/// Spelled out per platform, not derived from `skip_reason`, so a change
/// to the predicate cannot pass by changing the expectation with it.
pub(super) fn expect_preapproval(_home: &Path, got: &Preapproval) {
    if cfg!(target_os = "macos") {
        assert_eq!(
            got,
            &Preapproval::Recorded {
                pairs: 1,
                reset_channel: None
            }
        );
    } else {
        assert!(
            matches!(got, Preapproval::Skipped { .. }),
            "only Seatbelt keeps the signing key from the agent: {got:?}"
        );
    }
}

#[test]
fn a_preapproval_needs_a_seal_that_hides_the_signing_key() {
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    ent.fail_closed_on_sandbox_error = true;
    assert_eq!(skip_reason_on(&ent, Some(ExecBackend::Seatbelt)), None);
    let linux = skip_reason_on(&ent, Some(ExecBackend::Landlock)).expect("Landlock is skipped");
    assert!(linux.contains("signing key"), "{linux}");
    assert!(skip_reason_on(&ent, None).is_some());
    ent.fail_closed_on_sandbox_error = false;
    assert!(
        skip_reason_on(&ent, Some(ExecBackend::Seatbelt)).is_some(),
        "an agent that may start unsealed could sign its own decision"
    );
}

#[test]
fn known_hosts_is_writable_unless_sealed_every_run() {
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    ent.fail_closed_on_sandbox_error = true;
    assert!(plan::sealed_every_run(&ent, Some(ExecBackend::Landlock)));
    assert!(!plan::sealed_every_run(&ent, None));
    ent.fail_closed_on_sandbox_error = false;
    assert!(!plan::sealed_every_run(&ent, Some(ExecBackend::Seatbelt)));

    let (home, user) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let kh = user.path().join("known_hosts");
    let policy =
        mur_agent_runtime::sandbox::policy::SandboxPolicy::from_entitlements(&ent, home.path());
    assert!(
        !plan::agent_can_write(&policy, &kh),
        "the policy alone denies it"
    );
    assert!(
        plan::may_write(&ent, &policy, &kh),
        "an unsealed run is not bound by the policy"
    );
}

#[test]
fn a_match_an_unsealed_agent_could_have_written_still_needs_verifying() {
    let (home, user) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    ent.fail_closed_on_sandbox_error = false;
    let host = mur_ssh_request::RequestHost::parse(HOST, None).unwrap();
    let probe = FakeProbe {
        stored: vec![fp('H')],
        known_hosts: Some(user.path().join("known_hosts")),
    };
    let p = plan(&probe, AGENT, home.path(), &ent, host.clone(), None, OFFER).unwrap();
    // Control: the same match with the seal enforced every run is trusted,
    // so the downgrade below comes from the unsealed posture alone.
    let mut sealed = ent.clone();
    sealed.fail_closed_on_sandbox_error = true;
    let c = plan(&probe, AGENT, home.path(), &sealed, host, None, OFFER).unwrap();
    if plan::sealed_every_run(&sealed, ExecBackend::current()) {
        assert_eq!(c.verdict, Verdict::Trusted);
    }
    assert_eq!(p.verdict, Verdict::Unknown { downgraded: true });
    assert!(
        select(
            &target(),
            &p.scanned,
            &p.verdict,
            &p.matched,
            &HostChoice::default()
        )
        .is_err(),
        "nothing granted without an explicit check"
    );
}

#[test]
fn a_running_agent_is_stopped_preapproved_and_started() {
    let (_h, home) = Home::new();
    let mut life = FakeLife {
        pid: Some(42),
        ..Default::default()
    };
    let a = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(None, false, true),
        &mut life,
        &NeverInstall,
    )
    .unwrap();
    assert_eq!(life.calls, vec!["stop", "start"]);
    assert_eq!(a.restart, Restart::Restarted);
    assert!(a.restart.line(AGENT).is_none());
    expect_preapproval(&home, &a.preapproval);
}

#[test]
fn a_stopped_agent_is_preapproved_and_left_stopped() {
    let (_h, home) = Home::new();
    let mut life = FakeLife::default();
    let a = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(None, false, true),
        &mut life,
        &NeverInstall,
    )
    .unwrap();
    assert!(life.calls.is_empty());
    assert_eq!(a.restart, Restart::WasStopped);
    expect_preapproval(&home, &a.preapproval);
}

#[test]
fn a_stop_failure_keeps_the_grant_and_records_nothing() {
    let (_h, home) = Home::new();
    let mut life = FakeLife {
        pid: Some(42),
        fail_stop: true,
        ..Default::default()
    };
    let a = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(None, false, true),
        &mut life,
        &NeverInstall,
    )
    .unwrap();
    assert!(matches!(a.restart, Restart::StopFailed(_)));
    assert_eq!(a.preapproval, Preapproval::NotAttempted);
    assert_eq!(life.calls, vec!["stop"]);
    assert!(granted(&home), "the write is never undone after it lands");
}

#[test]
fn a_start_failure_is_reported_not_rolled_back() {
    let (_h, home) = Home::new();
    let mut life = FakeLife {
        pid: Some(42),
        fail_start: true,
        ..Default::default()
    };
    let a = apply_and_restart(
        &home,
        AGENT,
        0,
        &approval(None, false, true),
        &mut life,
        &NeverInstall,
    )
    .unwrap();
    assert!(matches!(a.restart, Restart::StartFailed(_)));
    assert!(
        a.restart
            .line(AGENT)
            .unwrap()
            .contains("mur agent start bot")
    );
    assert!(granted(&home));
}

struct FakeProbe {
    stored: Vec<String>,
    known_hosts: Option<std::path::PathBuf>,
}

impl probe::Probe for FakeProbe {
    fn agent_keys(&self) -> Vec<probe::AgentKey> {
        vec![key('A'), key('B')]
    }
    fn unloaded_keys(&self) -> Vec<probe::AgentKey> {
        vec![key('B'), key('C')]
    }
    fn scan(&self, _host: &mur_ssh_request::RequestHost) -> Result<Vec<ScannedKey>> {
        Ok(vec![scanned('H')])
    }
    fn stored_host_keys(&self, _host: &mur_ssh_request::RequestHost) -> Vec<String> {
        self.stored.clone()
    }
    fn user_known_hosts(&self) -> Option<std::path::PathBuf> {
        self.known_hosts.clone()
    }
}

fn key(c: char) -> probe::AgentKey {
    probe::AgentKey {
        fingerprint: fp(c),
        comment: format!("key-{c}"),
        path: None,
    }
}

#[test]
fn plan_reads_keys_scan_and_spawn_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ent = mur_common::agent::AgentProfile::default_for_tests().entitlements;
    ent.processes = procs(SpawnMode::None, &[]);
    let host = mur_ssh_request::RequestHost::parse(HOST, None).unwrap();
    let probe = FakeProbe {
        stored: vec![fp('H')],
        known_hosts: None,
    };
    let p = plan(
        &probe,
        AGENT,
        tmp.path(),
        &ent,
        host.clone(),
        Some("git".into()),
        OFFER,
    )
    .unwrap();
    assert_eq!(p.agent, AGENT);
    assert_eq!(p.host, host);
    assert_eq!(p.user.as_deref(), Some("git"));
    assert_eq!(p.keys_loaded, vec![key('A'), key('B')]);
    assert_eq!(
        p.keys_unloaded,
        vec![key('C')],
        "a loaded key is not listed twice"
    );
    assert_eq!((p.verdict, p.matched), (Verdict::Trusted, vec![fp('H')]));
    assert_eq!(p.spawn, Some(SpawnChange::NoneToStrict));
    if let Some(surface) = p.exec_surface {
        let text = surface.consent_text();
        assert!(
            !text.contains("#2343"),
            "#2343 is fixed; the card says what Strict opens"
        );
        assert!(!text.contains("only ssh") && !text.contains("shell only"));
        if cfg!(target_os = "linux") {
            assert!(
                !surface.reach.rw_exec,
                "Strict read/write grants carry no Execute"
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "plan never writes"
    );
}

#[test]
fn the_forget_command_names_the_entry_known_hosts_stores() {
    use super::verdict::{differs_text, forget_cmd};
    use mur_ssh_request::RequestHost;
    let on = |h: &str, p: Option<u16>| forget_cmd(&RequestHost::parse(h, p).unwrap());
    assert_eq!(on("gh.example", None), "ssh-keygen -R gh.example");
    // `[gh.example]:2222` is what `known_hosts` stores for a non-default
    // port; quoted, because `[` is a shell glob.
    assert_eq!(
        on("gh.example", Some(2222)),
        "ssh-keygen -R '[gh.example]:2222'"
    );
    assert_eq!(on("::1", Some(2222)), "ssh-keygen -R '[::1]:2222'");
    let t = differs_text(&RequestHost::parse("gh.example", Some(2222)).unwrap());
    assert!(t.contains("for gh.example:2222:"), "{t}");
    assert!(t.contains("`ssh-keygen -R '[gh.example]:2222'`"), "{t}");
}
