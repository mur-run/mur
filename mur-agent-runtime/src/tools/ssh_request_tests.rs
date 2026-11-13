use super::*;
use mur_common::agent::{SpawnEntitlement, SpawnMode, SshHostRule, SshKeyRule, SshKnownHost};
use mur_common::ssh_proxy_status::SshProxyState;

fn processes(mode: SpawnMode) -> ProcessesEntitlement {
    ProcessesEntitlement {
        spawn: SpawnEntitlement {
            mode,
            allowed: vec![],
            allowed_dirs: vec![],
        },
    }
}

fn granted_ssh(known_host: &str) -> SshEntitlement {
    SshEntitlement {
        keys: vec![SshKeyRule {
            fingerprint: "SHA256:key".into(),
            label: Some("deploy".into()),
            allow_hosts: vec![SshHostRule {
                host_key: "SHA256:hostkey".into(),
                label: None,
                known_hosts: vec![SshKnownHost {
                    host: known_host.into(),
                    key_type: "ssh-ed25519".into(),
                    key_base64: "AAAA".into(),
                }],
            }],
        }],
    }
}

fn tool(home: &std::path::Path, ssh: SshEntitlement) -> SshRequestTool {
    SshRequestTool {
        agent_home: home.to_path_buf(),
        facts: SshFacts::new(&ssh, &processes(SpawnMode::Any)),
    }
}

fn pending(home: &std::path::Path) -> usize {
    mur_ssh_request::read_pending(home, chrono::Utc::now()).len()
}

#[tokio::test]
async fn an_allowed_host_answers_with_live_state_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    ssh_proxy_status::write(
        home.path(),
        &SshProxyStatus {
            state: SshProxyState::Boundary,
            code: None,
            explanation: "sandbox refuses the upstream".into(),
            at_unix_ms: 0,
        },
    )
    .unwrap();
    let t = tool(home.path(), granted_ssh("Build.Example.com"));
    let out = t
        .execute(serde_json::json!({"host": "BUILD.example.com", "reason": "deploy"}))
        .await
        .unwrap();
    assert!(out.text.contains("already allowed"), "{}", out.text);
    assert!(out.text.contains("key deploy"), "{}", out.text);
    assert_no_fingerprint(&out.text);
    assert!(out.text.contains("boundary"));
    assert!(out.text.contains("can be exec'd: yes"));
    assert_eq!(pending(home.path()), 0, "no file for an allowed host");
}

#[tokio::test]
async fn a_non_default_port_matches_the_bracketed_known_hosts_name() {
    let home = tempfile::tempdir().unwrap();
    let t = tool(home.path(), granted_ssh("[build.example.com]:2222"));
    let on_22 = t
        .execute(serde_json::json!({"host": "build.example.com", "reason": "x"}))
        .await
        .unwrap();
    assert!(
        on_22.text.contains("Request recorded"),
        "port 22 is not granted"
    );
    let on_2222 = t
        .execute(serde_json::json!({"host": "build.example.com", "port": 2222, "reason": "x"}))
        .await
        .unwrap();
    assert!(on_2222.text.contains("already allowed"), "{}", on_2222.text);
}

#[tokio::test]
async fn an_unknown_host_writes_one_request_and_replaces_it_on_retry() {
    let home = tempfile::tempdir().unwrap();
    let t = tool(home.path(), SshEntitlement::default());
    let first = t
        .execute(serde_json::json!({
            "host": "db.example.com", "user": "ops", "reason": "read the logs"
        }))
        .await
        .unwrap();
    assert!(
        first
            .text
            .contains("Request recorded for db.example.com:22.")
    );
    assert!(!first.text.contains("replaces"));
    assert!(first.text.contains("Do not ask them to run commands"));
    let again = t
        .execute(serde_json::json!({"host": "db.example.com", "reason": "still need it"}))
        .await
        .unwrap();
    assert!(again.text.contains("replaces your earlier request"));
    let reqs = mur_ssh_request::read_pending(home.path(), chrono::Utc::now());
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].reason, "still need it");
}

#[tokio::test]
async fn bad_input_is_refused_before_anything_is_written() {
    let home = tempfile::tempdir().unwrap();
    for ssh in [SshEntitlement::default(), granted_ssh("h.example")] {
        bad_input_is_refused(home.path(), ssh).await;
    }
}

/// The same contract whether or not `h.example` is already granted.
async fn bad_input_is_refused(home: &std::path::Path, ssh: SshEntitlement) {
    let t = tool(home, ssh);
    for input in [
        serde_json::json!({"reason": "x"}),
        serde_json::json!({"host": "a/b", "reason": "x"}),
        serde_json::json!({"host": "h.example", "port": 70000, "reason": "x"}),
        serde_json::json!({"host": "h.example", "port": 0, "reason": "x"}),
        serde_json::json!({"host": "h.example", "user": "root; rm", "reason": "x"}),
        serde_json::json!({"host": "h.example", "reason": "   "}),
        serde_json::json!({"host": "h.example"}),
    ] {
        let r = t.execute(input.clone()).await;
        assert!(
            matches!(r, Err(ToolError::InvalidInput(_))),
            "{input} → {:?}",
            r.map(|o| o.text)
        );
    }
    assert_eq!(pending(home), 0);
}

#[test]
fn prompt_section_names_the_route_and_never_a_cli_command_to_run() {
    let none = SshFacts::new(&SshEntitlement::default(), &processes(SpawnMode::None));
    let s = prompt_section(&none, None);
    assert!(s.contains("Allowed hosts: none."));
    assert!(s.contains("can be exec'd: no"));
    assert!(s.contains("call `ssh_request`"));
    assert!(s.contains("Do not ask the user to run commands"));
    assert!(
        !s.contains("ssh-agent proxy:"),
        "no proxy line with no keys"
    );

    let some = SshFacts::new(&granted_ssh("[h.example]:2222"), &processes(SpawnMode::Any));
    let s = prompt_section(&some, None);
    assert!(s.contains("- [h.example]:2222 (key deploy)"), "{s}");
    assert_no_fingerprint(&s);
    assert!(s.contains("ssh-agent proxy: not recorded"));
    assert!(s.contains("can be exec'd: yes"));
}

#[test]
fn hints_fire_only_for_ssh() {
    assert!(
        spawn_denied_hint("/usr/bin/ssh")
            .unwrap()
            .contains("ssh_request")
    );
    assert!(spawn_denied_hint("ssh").is_some());
    assert!(spawn_denied_hint("/usr/bin/ssh-keygen").is_none());
    assert!(spawn_denied_hint("/usr/bin/git").is_none());
    assert!(
        proxy_refused_hint("sign_and_send_pubkey: signing failed for ED25519 \"k\" from agent: agent refused operation")
            .unwrap()
            .contains("ssh_request")
    );
    assert!(proxy_refused_hint("Permission denied (publickey).").is_none());
}

/// Neither fixture fingerprint reaches the agent (plan: S:Goal 5).
fn assert_no_fingerprint(text: &str) {
    assert!(
        !text.contains("SHA256:"),
        "fingerprint shown to the agent: {text}"
    );
}

#[test]
fn an_unlabeled_key_and_an_unnamed_host_are_never_shown_by_fingerprint() {
    let mut ssh = granted_ssh("h.example");
    ssh.keys[0].label = None;
    ssh.keys[0].allow_hosts.push(SshHostRule {
        host_key: "SHA256:unnamedhostkey".into(),
        label: None,
        known_hosts: vec![],
    });
    let facts = SshFacts::new(&ssh, &processes(SpawnMode::Any));
    let s = prompt_section(&facts, None);
    assert!(s.contains("- h.example (key (unlabeled))"), "{s}");
    assert!(
        s.contains("1 more host rule(s) recorded without a host name"),
        "{s}"
    );
    assert_no_fingerprint(&s);

    let target = RequestHost::parse("h.example", None).unwrap();
    for line in facts.granted(&target) {
        assert_no_fingerprint(&line);
    }
}
