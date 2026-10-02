use super::*;
use crate::agent::AgentProfile;
use crate::config::{ModelSwitchConfig, RoutingConfig};

fn profile(model_ref: Option<&str>, chain: &[&str]) -> AgentProfile {
    let mut p = AgentProfile::default_for_tests();
    p.model_ref = model_ref.map(|s| s.to_string());
    p.fallback_chain = chain.iter().map(|s| s.to_string()).collect();
    p
}

#[test]
fn per_agent_primary_and_chain_win_over_global() {
    let cfg = ModelSwitchConfig {
        default: Some("global_default".into()),
        fallback_chain: vec!["g1".into(), "g2".into()],
        ..Default::default()
    };
    let p = profile(Some("agent_primary"), &["agent_primary", "agent_fb"]);
    // per-agent model_ref is primary; per-agent chain used; primary de-duped.
    assert_eq!(
        resolve_model_refs(&p, &cfg, None),
        vec!["agent_primary", "agent_fb"]
    );
}

#[test]
fn falls_back_to_global_default_and_chain() {
    let cfg = ModelSwitchConfig {
        default: Some("global_default".into()),
        fallback_chain: vec!["g1".into(), "global_default".into()],
        ..Default::default()
    };
    let p = profile(None, &[]); // no per-agent model_ref or chain
    // primary = global default; global chain used; primary de-duped out.
    assert_eq!(
        resolve_model_refs(&p, &cfg, None),
        vec!["global_default", "g1"]
    );
}

#[test]
fn routed_primary_overrides_model_ref() {
    let cfg = ModelSwitchConfig {
        fallback_chain: vec!["g1".into()],
        ..Default::default()
    };
    let p = profile(Some("agent_primary"), &[]);
    assert_eq!(
        resolve_model_refs(&p, &cfg, Some("frontier".into())),
        vec!["frontier", "g1"]
    );
}

#[test]
fn no_config_no_agent_yields_empty() {
    // Nothing configured → empty vec (caller falls back to inline model).
    let cfg = ModelSwitchConfig::default();
    assert!(resolve_model_refs(&profile(None, &[]), &cfg, None).is_empty());
}

#[test]
fn difficulty_picks_frontier_over_threshold() {
    let r = RoutingConfig {
        enabled: true,
        cheap: Some("cheap".into()),
        frontier: Some("frontier".into()),
        threshold_input_tokens: Some(1000),
    };
    assert_eq!(choose_by_difficulty(1500, &r), Some("frontier".into()));
    assert_eq!(choose_by_difficulty(500, &r), Some("cheap".into()));
    // Misconfigured (missing frontier) → None (fall through).
    let bad = RoutingConfig {
        enabled: true,
        cheap: Some("c".into()),
        frontier: None,
        threshold_input_tokens: None,
    };
    assert_eq!(choose_by_difficulty(9999, &bad), None);
}

#[test]
fn pick_cheap_model_lowest_cost_chat_excluding_primary() {
    let mut reg = ModelRegistry::default();
    let mk = |cost: f64, caps: &[&str]| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        cost_per_1k_tokens: Some(cost),
        ..Default::default()
    };
    reg.models.insert("frontier".into(), mk(0.01, &["chat"]));
    reg.models.insert("cheap".into(), mk(0.0001, &["chat"]));
    reg.models
        .insert("embed".into(), mk(0.00001, &["embedding"])); // not chat → skip
    // cheapest chat-capable, excluding the agent's own primary:
    assert_eq!(
        pick_cheap_model(&reg, Some("cheap"), &[]),
        Some("frontier".into())
    ); // cheap excluded
    assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap".into()));
    // no chat entries → None (Smart inert)
    let mut empty = ModelRegistry::default();
    empty.models.insert("e".into(), mk(0.0, &["embedding"]));
    assert_eq!(pick_cheap_model(&empty, None, &[]), None);
}

#[test]
fn satisfies_is_permissive_at_baseline_and_fail_closed_above_it() {
    let mk = |caps: &[&str]| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    // Baseline: an entry written before the field existed is still chat.
    assert!(satisfies(&mk(&[]), &[]));
    assert!(satisfies(&mk(&["chat"]), &[]));
    assert!(!satisfies(&mk(&["embedding"]), &[]));
    // Above baseline: unstated is not permission.
    assert!(!satisfies(&mk(&[]), &[Requirement::Vision]));
    assert!(!satisfies(&mk(&["chat"]), &[Requirement::Vision]));
    assert!(satisfies(&mk(&["chat", "vision"]), &[Requirement::Vision]));
    assert!(!satisfies(&mk(&["chat", "vision"]), &[Requirement::Tools]));
    assert!(satisfies(
        &mk(&["chat", "vision", "tools"]),
        &[Requirement::Vision, Requirement::Tools]
    ));
}

/// Tools and Vision disagree about silence on purpose. A tool-incapable
/// model fails loudly and the chain advances; a blind one answers with
/// confident nonsense. So an entry that declares nothing keeps its place in
/// the chain for a tool turn — otherwise every pre-`capabilities` entry
/// (most of a real registry) would drop out of every tool-carrying request
/// — while the same silence disqualifies it for an image.
#[test]
fn undeclared_capabilities_pass_tools_but_never_vision() {
    let mk = |caps: &[&str]| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    // Silence: permitted for tools, never for vision.
    assert!(satisfies(&mk(&[]), &[Requirement::Tools]));
    assert!(!satisfies(&mk(&[]), &[Requirement::Vision]));
    assert!(!satisfies(
        &mk(&[]),
        &[Requirement::Vision, Requirement::Tools]
    ));
    // A declaration is taken at its word in both directions.
    assert!(!satisfies(&mk(&["chat"]), &[Requirement::Tools]));
    assert!(satisfies(&mk(&["chat", "tools"]), &[Requirement::Tools]));
}

/// The incident, as a regression test: an image request against a registry
/// where nothing declares vision must find no cheap candidate at all.
#[test]
fn pick_cheap_model_declines_when_no_entry_declares_the_requirement() {
    let mk = |cost: f64, caps: &[&str]| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        cost_per_1k_tokens: Some(cost),
        ..Default::default()
    };
    let mut reg = ModelRegistry::default();
    reg.models
        .insert("cheap_text".into(), mk(0.0001, &["chat"]));
    reg.models.insert("legacy".into(), mk(0.0002, &[]));
    reg.models
        .insert("frontier".into(), mk(0.01, &["chat", "vision"]));
    // No requirement -> cheapest wins (today's behaviour, unchanged).
    assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap_text".into()));
    // Vision required -> only the declaring entry qualifies, cost be damned.
    assert_eq!(
        pick_cheap_model(&reg, None, &[Requirement::Vision]),
        Some("frontier".into())
    );
    // Nothing declares vision -> None, so Smart goes inert.
    let mut blind = ModelRegistry::default();
    blind
        .models
        .insert("cheap_text".into(), mk(0.0001, &["chat"]));
    blind.models.insert("legacy".into(), mk(0.0002, &[]));
    assert_eq!(pick_cheap_model(&blind, None, &[Requirement::Vision]), None);
}

/// A price with no date is a guess wearing the costume of a fact. But a
/// date on an entry that carries no price would be the same lie in the
/// other direction, so the stamp is conditional on there being a rate.
#[test]
fn priced_at_stamps_only_priced_entries() {
    let now = chrono::Utc::now();

    let mut unpriced = ModelEntry {
        provider: "openai".into(),
        model: "local-thing".into(),
        ..Default::default()
    };
    unpriced.stamp_priced_at(now);
    assert_eq!(unpriced.priced_at, None);
    assert_eq!(unpriced.price_age(now), None);

    let mut priced = ModelEntry {
        output_cost_per_1k: Some(0.025),
        ..unpriced.clone()
    };
    priced.stamp_priced_at(now);
    assert_eq!(priced.priced_at, Some(now));

    // A legacy single-rate entry counts as priced.
    let mut legacy = ModelEntry {
        cost_per_1k_tokens: Some(0.01),
        ..unpriced.clone()
    };
    legacy.stamp_priced_at(now);
    assert!(legacy.priced_at.is_some());

    // Re-stamping never moves the date backwards.
    let earlier = now - chrono::TimeDelta::days(30);
    priced.stamp_priced_at(earlier);
    assert_eq!(priced.priced_at, Some(now));
}

/// Entries written before this field existed must keep loading, and must
/// report an unknown age rather than inheriting today's date.
#[test]
fn registry_without_priced_at_still_loads_and_reports_unknown_age() {
    let yaml = r#"
schema_version: 1
models:
  opus:
    provider: anthropic
    model: claude-opus-5
    input_cost_per_1k: 0.005
    output_cost_per_1k: 0.025
"#;
    let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let e = &reg.models["opus"];
    assert_eq!(e.priced_at, None);
    assert_eq!(e.price_age(chrono::Utc::now()), None);
    // Round-trips without inventing the field.
    let out = serde_yaml_ng::to_string(&reg).unwrap();
    assert!(!out.contains("priced_at"), "{out}");
}

/// `mur model add --input-cost/--output-cost` leaves `cost_per_1k_tokens`
/// unset, so an entry priced the current way must still be rankable.
#[test]
fn pick_cheap_model_sees_split_cost_entries() {
    let mut reg = ModelRegistry::default();
    let split = |input: f64, output: f64| ModelEntry {
        provider: "x".into(),
        model: "m".into(),
        capabilities: vec!["chat".into()],
        input_cost_per_1k: Some(input),
        output_cost_per_1k: Some(output),
        ..Default::default()
    };
    reg.models.insert("dear".into(), split(0.005, 0.025));
    reg.models.insert("cheap".into(), split(0.0001, 0.0004));
    assert_eq!(pick_cheap_model(&reg, None, &[]), Some("cheap".into()));

    // Input-only entries are priced too, rather than silently skipped.
    let mut input_only = ModelRegistry::default();
    input_only.models.insert(
        "in".into(),
        ModelEntry {
            provider: "x".into(),
            model: "m".into(),
            capabilities: vec!["chat".into()],
            input_cost_per_1k: Some(0.002),
            ..Default::default()
        },
    );
    assert_eq!(pick_cheap_model(&input_only, None, &[]), Some("in".into()));
}

#[test]
fn subscription_metadata_round_trips_without_a_secret() {
    let yaml = r#"schema_version: 1
models:
  chatgpt_sol:
    provider: codex
    model: gpt-5.6-sol
    base_url: http://127.0.0.1:8088/codex/v1
    tier: frontier
    billing: subscription
    catalog_verified: true
"#;
    let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let entry = &reg.models["chatgpt_sol"];
    assert_eq!(entry.billing, Some(BillingMode::Subscription));
    assert_eq!(entry.catalog_verified, Some(true));
    assert!(entry.secret.is_none());
    let out = serde_yaml_ng::to_string(&reg).unwrap();
    assert!(out.contains("billing: subscription"), "{out}");
    assert!(out.contains("catalog_verified: true"), "{out}");
}

/// Entries written before billing metadata existed keep loading and
/// stay unknown — never inheriting a billing mode on reserialize.
/// Explicit `billing:` wins. Without it, the provider decides what can be
/// decided — ollama runs here, codex/claude ride a subscription — and
/// everything else is treated as metered, because guessing "free" is the
/// one mistake a cost gate must not make.
#[test]
fn provider_inference_and_route_tier_cover_existing_aliases() {
    for provider in [
        "ollama",
        "mlx",
        "llamacpp",
        "llama_cpp",
        "localai",
        "lmstudio",
        "local",
    ] {
        assert_eq!(
            inferred_billing_for_provider(provider),
            BillingMode::Local,
            "{provider}"
        );
        let entry = ModelEntry {
            provider: provider.into(),
            ..Default::default()
        };
        assert_eq!(entry.effective_route_tier(), RouteTier::Local, "{provider}");
    }
    for provider in ["claude", "codex"] {
        assert_eq!(
            inferred_billing_for_provider(provider),
            BillingMode::Subscription
        );
    }
    assert_eq!(
        inferred_billing_for_provider("unknown"),
        BillingMode::UsageBilled
    );
}

#[test]
fn projected_cost_uses_four_to_one_workload_and_legacy_rates() {
    let split = ModelEntry {
        input_cost_per_1k: Some(1.0),
        output_cost_per_1k: Some(10.0),
        ..Default::default()
    };
    assert_eq!(split.projected_cost(4_000, 1_000), Some(14.0));
    let legacy = ModelEntry {
        cost_per_1k_tokens: Some(2.0),
        ..Default::default()
    };
    assert_eq!(legacy.projected_cost(4_000, 1_000), Some(10.0));
    assert_eq!(ModelEntry::default().projected_cost(4_000, 1_000), None);
}

#[test]
fn pick_cheap_model_uses_projected_four_to_one_cost() {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "cheap_input".into(),
        ModelEntry {
            provider: "openai".into(),
            model: "a".into(),
            input_cost_per_1k: Some(0.1),
            output_cost_per_1k: Some(2.0),
            ..Default::default()
        },
    );
    reg.models.insert(
        "cheap_output".into(),
        ModelEntry {
            provider: "openai".into(),
            model: "b".into(),
            input_cost_per_1k: Some(1.0),
            output_cost_per_1k: Some(0.1),
            ..Default::default()
        },
    );
    assert_eq!(
        pick_cheap_model(&reg, None, &[]),
        Some("cheap_input".into())
    );
}

#[test]
fn billing_is_inferred_from_the_provider_when_not_declared() {
    let mut e = ModelEntry {
        provider: "ollama".into(),
        model: "llama3.2:3b".into(),
        ..Default::default()
    };
    assert_eq!(e.billing_or_inferred(), BillingMode::Local);
    e.provider = "codex".into();
    assert_eq!(e.billing_or_inferred(), BillingMode::Subscription);
    e.provider = "claude".into();
    assert_eq!(e.billing_or_inferred(), BillingMode::Subscription);
    e.provider = "openai".into();
    assert_eq!(
        e.billing_or_inferred(),
        BillingMode::UsageBilled,
        "unknown is metered"
    );
    e.provider = "anthropic".into();
    assert_eq!(e.billing_or_inferred(), BillingMode::UsageBilled);
    // A declaration overrides every inference — an LM Studio entry is
    // `provider: openai` and the user marks it local.
    e.billing = Some(BillingMode::Local);
    assert_eq!(e.billing_or_inferred(), BillingMode::Local);
}

#[test]
fn entry_without_billing_metadata_stays_unknown() {
    let yaml = r#"schema_version: 1
models:
  gpt:
    provider: openai
    model: gpt-4o
    secret: env:OPENAI_API_KEY
"#;
    let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let entry = &reg.models["gpt"];
    assert_eq!(entry.billing, None);
    assert_eq!(entry.catalog_verified, None);
    let out = serde_yaml_ng::to_string(&reg).unwrap();
    assert!(!out.contains("billing"), "{out}");
    assert!(!out.contains("catalog_verified"), "{out}");
    for (raw, mode) in [
        ("subscription", BillingMode::Subscription),
        ("usage_billed", BillingMode::UsageBilled),
        ("local", BillingMode::Local),
    ] {
        let m: BillingMode = serde_yaml_ng::from_str(raw).unwrap();
        assert_eq!(m, mode);
    }
}
