use super::*;

#[test]
fn resolve_summarize_no_flag_uses_config_enabled_and_model() {
    let (enabled, model) = resolve_summarize(
        /* no_summarize */ false,
        /* cli_model    */ None,
        /* cfg_enabled  */ true,
        /* cfg_model    */ Some("qwen3:14b"),
    );
    assert!(enabled);
    assert_eq!(model.as_deref(), Some("qwen3:14b"));
}

#[test]
fn resolve_summarize_no_summarize_flag_forces_disabled_regardless_of_config() {
    let (enabled, model) = resolve_summarize(
        /* no_summarize */ true,
        /* cli_model    */ None,
        /* cfg_enabled  */ true,
        /* cfg_model    */ Some("qwen3:14b"),
    );
    assert!(!enabled, "--no-summarize must override enabled config");
    // model still bubbles up (CLI didn't set one) — the disabled flag is what matters.
    assert_eq!(model.as_deref(), Some("qwen3:14b"));
}

#[test]
fn resolve_summarize_cli_model_overrides_config_model() {
    let (enabled, model) = resolve_summarize(
        /* no_summarize */ false,
        /* cli_model    */ Some("qwen3:4b"),
        /* cfg_enabled  */ true,
        /* cfg_model    */ Some("qwen3:14b"),
    );
    assert!(enabled);
    assert_eq!(
        model.as_deref(),
        Some("qwen3:4b"),
        "CLI model wins over config"
    );
}

#[test]
fn resolve_summarize_cli_model_falls_back_to_config_when_none() {
    let (enabled, model) = resolve_summarize(
        /* no_summarize */ false,
        /* cli_model    */ None,
        /* cfg_enabled  */ false,
        /* cfg_model    */ Some("qwen3:14b"),
    );
    assert!(
        !enabled,
        "config-disabled stays disabled without CLI override"
    );
    assert_eq!(model.as_deref(), Some("qwen3:14b"));
}

// ── Fix round 1, finding 1 pinning tests ──────────────────────────────
//
// Before this fix, doctor/preflight probed `ollama_backends[0].endpoint`
// (one arbitrary endpoint) and validated every Ollama-routed model
// against it. A model actually routed to a second endpoint was reported
// "missing" against a host that was never queried.

#[test]
fn group_ollama_backends_by_endpoint_keeps_endpoints_separate() {
    let backends = vec![
        mur_common::config::BackendConfig {
            provider: "ollama".into(),
            model: "llama3:70b".into(),
            endpoint: Some("http://localhost:11434".into()),
            ..Default::default()
        },
        mur_common::config::BackendConfig {
            provider: "ollama".into(),
            model: "qwen3:4b".into(),
            endpoint: Some("http://box.local:11434".into()),
            ..Default::default()
        },
    ];
    let groups = group_ollama_backends_by_endpoint(&backends);
    assert_eq!(
        groups.len(),
        2,
        "two distinct endpoints must stay distinct: {groups:?}"
    );

    let local = groups
        .iter()
        .find(|g| g.endpoint == "http://localhost:11434")
        .expect("localhost group present");
    assert_eq!(local.models, vec!["llama3:70b".to_string()]);

    let boxed = groups
        .iter()
        .find(|g| g.endpoint == "http://box.local:11434")
        .expect("box.local group present");
    assert_eq!(boxed.models, vec!["qwen3:4b".to_string()]);

    // The bug this pins: model B must never appear under endpoint A's group.
    assert!(!local.models.contains(&"qwen3:4b".to_string()));
    assert!(!boxed.models.contains(&"llama3:70b".to_string()));
}

#[test]
fn collect_backend_configs_keeps_same_model_on_different_endpoints_distinct() {
    // Two stages route the SAME model name to two DIFFERENT Ollama hosts.
    // Deduping by (provider, model) alone — the pre-fix behavior — would
    // collapse these into one entry and silently drop an endpoint from
    // the probe set.
    let shared_model = "llama3:70b";
    let cfg = mur_common::config::Config {
        conversations: mur_common::config::ConversationsConfig {
            ask: mur_common::config::AskConfig {
                backend: Some(mur_common::config::BackendConfig {
                    provider: "ollama".into(),
                    model: shared_model.into(),
                    endpoint: Some("http://localhost:11434".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            compact: mur_common::config::CompactConfig {
                extractive_backend: Some(mur_common::config::BackendConfig {
                    provider: "ollama".into(),
                    model: shared_model.into(),
                    endpoint: Some("http://box.local:11434".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let backends = collect_backend_configs(&cfg);
    let groups = group_ollama_backends_by_endpoint(&backends);
    assert_eq!(
        groups.len(),
        2,
        "same model on two different endpoints must survive as two groups: {groups:?}"
    );

    // The bug this pins: the model routed to endpoint B (box.local) must
    // not be reported missing when only endpoint A (localhost) was
    // probed — it must show up in box.local's own group, not be
    // silently dropped by dedup.
    let box_group = groups
        .iter()
        .find(|g| g.endpoint == "http://box.local:11434")
        .expect("box.local endpoint must survive dedup, not be silently dropped");
    assert!(box_group.models.contains(&shared_model.to_string()));

    let local_group = groups
        .iter()
        .find(|g| g.endpoint == "http://localhost:11434")
        .expect("localhost endpoint must survive dedup too");
    assert!(local_group.models.contains(&shared_model.to_string()));
}
