use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::hitl::{HitlRequest, HitlResponse, RiskTier, Unanswered};

use super::*;

/// `path` as a YAML single-quoted scalar. Double quotes would read the `\U`
/// in a Windows path (`C:\Users\...`) as an escape and fail to parse.
pub(crate) fn yaml_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

/// A minimal valid profile whose `filesystem.write` is exactly `write`.
/// Shared with the dispatch-site tests that need a real member on disk.
pub(crate) fn profile_yaml(name: &str, write: &[&Path]) -> String {
    let write: Vec<String> = write.iter().map(|p| yaml_path(p)).collect();
    format!(
        r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: {name}
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: {{ tone: neutral, risk: cautious, verbosity: low }}
sys_prompt_file: "sys_prompt.md"
model: {{ provider: ollama, name: "llama3.2:3b", params: {{ temperature: 0.2, max_tokens: 4096 }} }}
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: {{ enabled: false, bind: "" }}
communication: {{ accepts_from: ["*"], sends_to: [] }}
capabilities: []
entitlements:
  network:
    inbound: {{ ports: [] }}
    outbound: {{ mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: {{ mode: system }} }}
  filesystem: {{ read: [], write: [{}], deny: [] }}
  processes: {{ spawn: {{ mode: allowlist, allowed: [] }} }}
  syscalls: {{ mode: default }}
  limits: {{ memory_mb: 512, file_descriptors: 1024, processes: 32 }}
notifications: {{ on_task_complete: [], on_error: [], on_shutdown: [] }}
retry:
  llm: {{ max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout] }}
  tool: {{ max_retries: 1, backoff: fixed, initial_delay_ms: 500 }}
lifecycle: {{ restart: on_failure }}
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
"#,
        write.join(", ")
    )
}

struct Fixture {
    tmp: tempfile::TempDir,
    channel_id: String,
    project: std::path::PathBuf,
}

impl Fixture {
    fn new(member: &str, write: &[&Path]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        crate::channel_writer::plant_writer_identity(home);
        let agent = home.join("agents").join(member);
        std::fs::create_dir_all(&agent).unwrap();
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(agent.join("profile.yaml"), profile_yaml(member, write)).unwrap();
        let svc = ChannelService::open(home).unwrap();
        let channel_id = svc.create_for_workflow("grant-test").unwrap().id;
        Self {
            tmp,
            channel_id,
            project,
        }
    }

    fn home(&self) -> &Path {
        self.tmp.path()
    }

    fn ctx(&self, unanswered: Unanswered, yes: bool) -> GrantContext<'_> {
        GrantContext {
            mur_home: self.home(),
            channel_id: &self.channel_id,
            run_id: "run-grant",
            policy: GatePolicy {
                yes,
                unanswered,
                auto_approve_tiers: vec![],
            },
            job_count: 3,
        }
    }

    fn requests(&self) -> Vec<HitlRequest> {
        let svc = ChannelService::open(self.home()).unwrap();
        svc.load_events(&self.channel_id)
            .unwrap()
            .iter()
            .filter(|e| e.kind == EventKind::HitlRequest)
            .filter_map(|e| serde_json::from_value(e.payload.clone()).ok())
            .collect()
    }

    fn answer(&self, hitl_id: &str, allow: bool) {
        let req = self
            .requests()
            .into_iter()
            .find(|r| r.hitl_id == hitl_id)
            .unwrap();
        let svc = ChannelService::open(self.home()).unwrap();
        crate::channel_writer::append_as_writer(
            &svc,
            self.home(),
            &self.channel_id,
            ROUTER_AGENT,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            serde_json::to_value(HitlResponse {
                hitl_id: req.hitl_id,
                action_hash: req.action_hash,
                allow,
                reason: "test".into(),
                surface: "cli".into(),
            })
            .unwrap(),
            None,
        )
        .unwrap();
    }

    fn grant_up_front(&self, member: &str) {
        std::fs::write(
            self.home().join("agents").join(member).join("profile.yaml"),
            profile_yaml(member, &[&self.project]),
        )
        .unwrap();
    }

    fn write_list(&self, member: &str) -> Vec<String> {
        let yaml =
            std::fs::read_to_string(self.home().join("agents").join(member).join("profile.yaml"))
                .unwrap();
        let p: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        p.entitlements.filesystem.write
    }
}

#[tokio::test]
async fn allowed_and_explicit_is_silent() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    f.grant_up_front("w");
    let targets = targets_for(["w", "w", "w"], &project, false);
    assert_eq!(targets.len(), 1, "three jobs to one member → one target");
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::AlreadyAllowed);
    assert!(f.requests().is_empty(), "fast path raises no HITL");
}

#[tokio::test]
async fn allowed_but_inferred_still_asks_and_writes_nothing() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    f.grant_up_front("w");
    let before = f.write_list("w");
    let targets = targets_for(["w"], &project, true);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    let GrantOutcome::Blocked(BlockReason::Deferred { hitl_id }) = &out[0].1 else {
        panic!("inferred must ask: {:?}", out[0].1);
    };
    let req = f.requests().pop().unwrap();
    assert_eq!(&req.hitl_id, hitl_id);
    assert_eq!(req.tier, RiskTier::Write);
    assert!(req.summary.contains("Inferred"), "{}", req.summary);
    assert!(!req.summary.contains("Add `"), "allowed: no grant sentence");
    assert_eq!(req.tool_input["inferred"], true);
    assert_eq!(req.tool_input["grant"], false);

    // Approve → AlreadyAllowed, profile untouched.
    f.answer(hitl_id, true);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::AlreadyAllowed);
    assert_eq!(f.write_list("w"), before);
}

#[tokio::test]
async fn denied_list_blocks_without_asking() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    let yaml = profile_yaml("w", &[&project])
        .replace("deny: []", &format!("deny: [{}]", yaml_path(&project)));
    std::fs::write(f.home().join("agents/w/profile.yaml"), yaml).unwrap();
    let targets = targets_for(["w"], &project, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::Blocked(BlockReason::Denied));
    assert!(f.requests().is_empty());
    let msg = block_message(&out, &f.channel_id).unwrap();
    assert!(msg.contains("filesystem.deny"), "{msg}");
}

#[tokio::test]
async fn missing_target_blocks_without_asking() {
    let f = Fixture::new("w", &[]);
    let gone = f.project.join("nope");
    let targets = targets_for(["w"], &gone, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::Blocked(BlockReason::TargetMissing));
    assert!(f.requests().is_empty());
}

/// #1607 regression: not granted, explicit cwd, human says no → nothing is
/// written anywhere and the dispatch is blocked with a reason.
#[tokio::test]
async fn refused_grant_writes_nothing() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    let targets = targets_for(["w"], &project, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    let GrantOutcome::Blocked(BlockReason::Deferred { hitl_id }) = &out[0].1 else {
        panic!("{:?}", out[0].1);
    };
    let req = f.requests().pop().unwrap();
    assert!(req.summary.starts_with("Add `"), "{}", req.summary);
    assert!(!req.summary.contains("Inferred"));
    assert!(
        req.summary.contains("not running as a service"),
        "no service installed for a tmp agent: {}",
        req.summary
    );
    f.answer(hitl_id, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert!(matches!(
        out[0].1,
        GrantOutcome::Blocked(BlockReason::Refused(_))
    ));
    assert!(f.write_list("w").is_empty(), "denied: profile untouched");
    assert!(!mur_common::entitlements_pin::pin_path(f.home(), "w").exists());
    let msg = block_message(&out, &f.channel_id).unwrap();
    assert!(msg.contains("refused"), "{msg}");
}

/// Approved and the member is not running: the grant lands, the pin advances,
/// and dispatch may proceed (its next start applies it).
#[tokio::test]
async fn approved_grant_is_written_and_sealed() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    let targets = targets_for(["w"], &project, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    let GrantOutcome::Blocked(BlockReason::Deferred { hitl_id }) = &out[0].1 else {
        panic!("{:?}", out[0].1);
    };
    f.answer(hitl_id, true);
    let out = ensure_write_grants(&f.ctx(Unanswered::Defer, false), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::Granted { restarted: false });
    let canon = std::fs::canonicalize(&project).unwrap();
    assert_eq!(f.write_list("w"), vec![canon.to_string_lossy().to_string()]);
    let yaml = std::fs::read_to_string(f.home().join("agents/w/profile.yaml")).unwrap();
    let p: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert!(matches!(
        mur_common::entitlements_pin::check(f.home(), "w", &p.entitlements).unwrap(),
        mur_common::entitlements_pin::PinCheck::Match
    ));
    // The decision is on the channel for the Hub.
    let svc = ChannelService::open(f.home()).unwrap();
    let notes: Vec<_> = svc
        .load_events(&f.channel_id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::Note && e.payload["kind"] == "delegation.write_grant")
        .collect();
    assert!(
        notes
            .iter()
            .any(|e| e.payload["outcome"] == "granted(restarted=false)"),
        "{notes:?}"
    );
}

/// `--yes` (and a `write` tier pre-approval) satisfy the gate like any other.
#[tokio::test]
async fn yes_auto_approves_a_grant() {
    let f = Fixture::new("w", &[]);
    let project = f.project.clone();
    let targets = targets_for(["w"], &project, false);
    let out = ensure_write_grants(&f.ctx(Unanswered::Wait, true), &targets)
        .await
        .unwrap();
    assert_eq!(out[0].1, GrantOutcome::Granted { restarted: false });
    assert_eq!(f.write_list("w").len(), 1);
}

#[test]
fn prompt_combines_inferred_and_grant_in_one_gate() {
    let t = DelegationTarget {
        member: "w".into(),
        dir: "/tmp/p".into(),
        inferred: true,
    };
    let s = prompt(&t, false, true, 2);
    assert!(s.starts_with("No target directory was given"), "{s}");
    assert!(s.contains("restart w (waits"), "{s}");
    let s = prompt(&t, false, false, 2);
    assert!(s.contains("mur agent restart w"), "{s}");
    assert!(s.contains("this dispatch will fail"), "{s}");
}

#[test]
fn yaml_path_round_trips_windows_and_quoted_paths() {
    for raw in [
        r"C:\Users\runneradmin\AppData\Local\Temp\x",
        "/tmp/it's here",
    ] {
        let doc = format!("p: {}", yaml_path(Path::new(raw)));
        let v: std::collections::BTreeMap<String, String> = serde_yaml_ng::from_str(&doc).unwrap();
        assert_eq!(v["p"], raw);
    }
}
