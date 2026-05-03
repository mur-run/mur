//! Track C1 — M-c1.0.2: supervisor refuses to construct an LLM client when
//! `entitlements.llm.mode = off`.
//!
//! Bridges (LLM-less mur agents that relay chat-platform traffic to/from the
//! A2A bus) must not dial a provider. `mur_agent_runtime::llm::build_client`
//! is the gate the supervisor calls before its provider-specific
//! construction; this test exercises the gate in isolation.

use mur_common::{AgentProfile, LlmMode};

fn profile_with_llm_off() -> AgentProfile {
    let yaml = include_str!("fixtures/minimal_profile.yaml");
    let mut p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    p.entitlements.llm.mode = LlmMode::Off;
    p
}

#[test]
fn llm_off_blocks_construction() {
    let profile = profile_with_llm_off();
    let err = mur_agent_runtime::llm::build_client(&profile).expect_err("llm.mode=off must block");
    let msg = err.to_string();
    assert!(
        msg.contains("llm.mode = off"),
        "expected error to mention 'llm.mode = off', got: {msg}"
    );
}

#[test]
fn llm_allowed_passes_gate() {
    let yaml = include_str!("fixtures/minimal_profile.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    // Default mode is Allowed; gate must succeed.
    assert_eq!(p.entitlements.llm.mode, LlmMode::Allowed);
    mur_agent_runtime::llm::build_client(&p).expect("llm.mode=allowed must pass gate");
}
