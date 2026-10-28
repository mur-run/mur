//! #2147 review: the boot-time case lint must see every tool a server
//! listed, not only the ones left after `deny` rules filtered them out.
//! Otherwise an exact deny on one of two case-variant tools looks like a
//! typo of the surviving variant.

use mur_agent_runtime::mcp::pool::McpPool;
use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::tools::registry::{McpBackend, build_tools};
use mur_common::agent::{McpServerEntry, ToolPolicy, ToolRule};
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn deny(pattern: &str) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy: ToolPolicy::Deny,
        risk: None,
        bypass_builtin_tier_floor: Default::default(),
    }
}

/// Boot the mock server (which lists `echo`, `connect`, plus `extra`) under
/// `rules`; return the registered names and everything logged.
async fn boot(extra: &str, rules: &[ToolRule]) -> (Vec<String>, String) {
    let entry = McpServerEntry {
        name: "mock".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp").into(),
        args: vec!["--extra-tool".into(), extra.into()],
        ..Default::default()
    };
    let pool = McpPool::new(vec![entry.clone()], SandboxPolicy::default(), None);
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let (_, map) = build_tools(
        None,
        None,
        None,
        None,
        &[entry],
        rules,
        McpBackend::unrouted(pool.clone()),
    )
    .await;
    pool.shutdown().await;
    let mut names: Vec<String> = map.into_keys().filter(|n| n.starts_with("mcp__")).collect();
    names.sort();
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    (names, text)
}

#[tokio::test]
async fn an_exact_deny_on_one_case_variant_is_not_called_a_typo() {
    let (names, log) = boot("Echo", &[deny("mcp__mock__Echo")]).await;
    assert_eq!(
        names,
        ["mcp__mock__connect", "mcp__mock__echo"],
        "the deny is enforced"
    );
    assert!(!log.contains("NOT IN EFFECT"), "{log}");
    assert!(!log.contains("did you mean"), "{log}");
}

#[tokio::test]
async fn a_miscased_deny_is_still_reported() {
    let (names, log) = boot("snap", &[deny("mcp__mock__Snap")]).await;
    assert!(names.contains(&"mcp__mock__snap".to_string()), "{names:?}");
    assert!(log.contains("did you mean `mcp__mock__snap`"), "{log}");
    assert!(log.contains("NOT IN EFFECT"), "{log}");
}
