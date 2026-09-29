#[test]
fn a_disabled_backend_produces_a_turn_that_explains_itself() {
    use crate::task_runner::RunnerBackend;
    use mur_common::cli_backend::{Activation, CLAUDE, CliBackend};
    static DISABLED: CliBackend = CliBackend {
        activation: Activation::Disabled {
            reason: "probe pending",
        },
        ..CLAUDE
    };
    let r = cli_track_runner(&DISABLED, "unix:///tmp/a.sock");
    // The stub's message is the turn's whole reply, so asserting it is
    // asserting what the user sees.
    assert!(
        matches!(r.backend_for_test(), RunnerBackend::Misconfigured(m)
            if m.contains("disabled") && m.contains("probe pending"))
    );
}

#[test]
fn no_socket_refuses_rather_than_spawning_blind() {
    use crate::task_runner::RunnerBackend;
    use mur_common::cli_backend::CLAUDE;
    let r = cli_track_runner(&CLAUDE, "");
    assert!(
        matches!(r.backend_for_test(), RunnerBackend::Misconfigured(m)
            if m.contains("no unix socket"))
    );
}

#[test]
fn a_usable_backend_gets_the_socket_it_will_dial() {
    use mur_common::cli_backend::CLAUDE;
    let r = cli_track_runner(&CLAUDE, "unix:///tmp/a.sock");
    // The `unix://` prefix must be stripped: it is a config spelling, not
    // a filesystem path, and `UnixStream::connect` takes the latter.
    assert_eq!(
        r.socket_path_for_test(),
        Some(std::path::Path::new("/tmp/a.sock"))
    );
}

use super::{UNRESOLVED_PROVIDER, no_model_notice};

/// The reply is the whole fix: it is the only surface the user is looking
/// at when an agent stops answering, so it must carry the cause and name the
/// commands — not point at a log.
#[test]
fn the_no_model_notice_carries_the_cause_and_a_way_out() {
    let m = no_model_notice(
        "pm",
        r#"model_ref "chatgpt_gpt_5_4_min" not in the registry"#,
    );
    assert!(
        m.contains("chatgpt_gpt_5_4_min"),
        "the cause must survive: {m}"
    );
    assert!(m.contains("mur agent doctor pm"), "{m}");
    assert!(m.contains("mur agent restart pm"), "{m}");
    // The internal marker is our bookkeeping. A user should not have to
    // learn a provider slug we invented to understand why nothing answers.
    assert!(!m.contains(UNRESOLVED_PROVIDER), "{m}");
}

use super::provider::{RefClientBuilder, chain_switch_handle};
use super::*;
use crate::llm::LlmClient;
use crate::llm::{LlmError, LlmRequest, LlmResponse, RequestIntent, StopReason};
use async_trait::async_trait;
use mur_common::config::{ModelSwitchConfig, RetryConfig};

/// Answers with its own name, or refuses to connect.
struct Fixed {
    name: &'static str,
    down: bool,
}

#[async_trait]
impl LlmClient for Fixed {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        if self.down {
            return Err(LlmError::Connect(format!("{} is down", self.name)));
        }
        Ok(LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: self.name.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model: self.name.to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }

    fn model_name(&self) -> &str {
        self.name
    }
}

fn req() -> LlmRequest {
    LlmRequest {
        messages: vec![],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        intent: RequestIntent::Interactive,
        pin_model_ref: None,
        task_id: None,
        effort: None,
    }
}

/// `/model` on a chain agent: the primary changes, the chain stays, and a
/// ref the registry does not know is refused before anything is swapped.
/// Chain agents used to get no `model/set` at all — and the global
/// fallback chain makes every agent one.
#[tokio::test]
async fn chain_switch_swaps_the_primary_and_keeps_the_chain() {
    let mut profile = inline_profile("ollama", "m");
    profile.model_ref = Some("boot".into());
    profile.fallback_chain = vec!["net".into()];
    let cfg = ModelSwitchConfig {
        retry: RetryConfig {
            max_retries: 0,
            backoff_base_ms: 0,
            cooldown_secs: 0,
        },
        ..ModelSwitchConfig::default()
    };
    let build_one: RefClientBuilder = Arc::new(|r: &str| match r {
        "boot" => Ok(Arc::new(Fixed {
            name: "boot",
            down: false,
        }) as Arc<dyn LlmClient>),
        "next" => Ok(Arc::new(Fixed {
            name: "next",
            down: true,
        })),
        "net" => Ok(Arc::new(Fixed {
            name: "net",
            down: false,
        })),
        other => anyhow::bail!("model_ref {other:?} not in registry"),
    });
    let (sw, build) = chain_switch_handle(profile, cfg, None, build_one);
    assert_eq!(sw.generate(req()).await.unwrap().text, "boot");

    assert!(
        build("nope").is_err(),
        "unknown ref must not become a primary"
    );

    sw.swap(build("next").unwrap());
    // The new primary is down, so the reply comes from the chain: the
    // switch changed the primary and nothing else.
    assert_eq!(sw.generate(req()).await.unwrap().text, "net");
}

/// An agent with one model ref and no chain still needs the routing-aware
/// client when Smart is on for it — the boot gate used to consult only the
/// chain length and difficulty routing, so Smart was dead for exactly the
/// agents that never configured anything else.
#[test]
fn single_ref_agent_with_smart_on_still_needs_the_routing_client() {
    assert!(!needs_routing_client(1, false, false));
    assert!(needs_routing_client(1, false, true));
    assert!(needs_routing_client(1, true, false));
    assert!(needs_routing_client(2, false, false));
}

#[test]
fn provider_host_extracts_external_host_only() {
    let mk = |b: Option<&str>| ModelEntry {
        base_url: b.map(Into::into),
        ..Default::default()
    };
    assert_eq!(
        provider_host(&mk(Some("https://api.deepseek.com"))).as_deref(),
        Some("api.deepseek.com")
    );
    // path on the base_url doesn't change the host
    assert_eq!(
        provider_host(&mk(Some("https://api.deepseek.com/v1"))).as_deref(),
        Some("api.deepseek.com")
    );
    // loopback endpoints are handled by local_llm_port, not allow_hosts
    assert_eq!(provider_host(&mk(Some("http://127.0.0.1:8088"))), None);
    assert_eq!(provider_host(&mk(Some("http://localhost:8000/v1"))), None);
    // no base_url => nothing to auto-allow
    assert_eq!(provider_host(&mk(None)), None);
}

#[test]
fn local_base_url_prefers_entry_then_env_then_file_then_default() {
    use std::path::Path;
    // entry wins
    assert_eq!(
        resolve_local_base_url(Some("http://e/v1"), None, Path::new("/nonexistent")),
        "http://e/v1"
    );
    // env wins when entry absent
    assert_eq!(
        resolve_local_base_url(
            None,
            Some("http://env/v1".into()),
            Path::new("/nonexistent")
        ),
        "http://env/v1"
    );
    // default when nothing available
    assert_eq!(
        resolve_local_base_url(None, None, Path::new("/nonexistent")),
        LOCAL_LLM_DEFAULT_BASE_URL
    );
}

/// Build a minimal inline-model `AgentProfile` (no `model_ref`, so
/// `resolve_model_entry` never touches the registry) for the given provider.
fn inline_profile(provider: &str, name: &str) -> mur_common::agent::AgentProfile {
    const MINIMAL: &str = r#"
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000002
name: agent_a
display_name: "Agent A"
version: "0.1.0"
persona:
  category: research
  description: "Minimal test agent"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport: { stdio: true, socket: { enabled: true, bind: "unix:///tmp/a.sock" } }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: ["~/.ssh"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#;
    let mut p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(MINIMAL).expect("minimal profile parses");
    p.model.provider = provider.to_string();
    p.model.name = name.to_string();
    p.model_ref = None;
    p
}

/// A cloud provider routed through a loopback bridge via `ANTHROPIC_BASE_URL`
/// must have its local proxy port allowlisted; a remote endpoint must not.
/// Env-var mutations make this test order-sensitive, so it owns the variable
/// for its full duration and clears it before exercising the negative cases.
#[test]
fn anthropic_via_local_bridge_grants_loopback_port() {
    let profile = inline_profile("anthropic", "claude-sonnet-5");
    let mur_home = std::path::Path::new("/nonexistent");

    // Local bridge → port is granted.
    let mut env =
        mur_common::test_env::EnvGuard::set([("ANTHROPIC_BASE_URL", "http://127.0.0.1:8088")]);
    assert_eq!(local_llm_port(&profile, mur_home), Some(8088));

    // Remote cloud endpoint → not loopback → no port granted.
    env.set_var("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
    assert_eq!(local_llm_port(&profile, mur_home), None);

    // Unset → no port granted.
    env.unset_var("ANTHROPIC_BASE_URL");
    assert_eq!(local_llm_port(&profile, mur_home), None);
}

#[test]
fn profile_needs_egress_matches_scoped_modes() {
    use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
    fn entry(mode: Option<McpNetMode>) -> McpServerEntry {
        let mut e = McpServerEntry {
            name: "s".into(),
            command: "cmd".into(),
            ..Default::default()
        };
        e.network = mode.map(|m| McpServerNetwork {
            mode: m,
            ..Default::default()
        });
        e
    }
    assert!(!profile_needs_egress(&[entry(None)]));
    assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Inherit))]));
    assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Off))]));
    assert!(profile_needs_egress(&[entry(Some(McpNetMode::Restricted))]));
    assert!(profile_needs_egress(&[
        entry(None),
        entry(Some(McpNetMode::BroadAudited))
    ]));
}
