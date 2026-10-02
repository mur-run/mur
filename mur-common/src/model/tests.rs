#[test]
fn vendor_candidates_prefer_the_recorded_vendor_then_the_host_then_provider() {
    // Recorded vendor wins — this is what new entries carry.
    let e = ModelEntry {
        provider: "openai".into(),
        vendor: Some("deepseek".into()),
        base_url: Some("https://api.deepseek.com/v1".into()),
        ..Default::default()
    };
    assert_eq!(e.vendor_candidates(), vec!["deepseek", "openai"]);

    // Legacy entry with no vendor: the endpoint host still identifies it,
    // which is how registries written before the field keep working.
    let legacy = ModelEntry {
        provider: "openai".into(),
        base_url: Some("https://api.deepseek.com/v1".into()),
        ..Default::default()
    };
    assert_eq!(legacy.vendor_candidates(), vec!["deepseek", "openai"]);

    // Nothing to infer: provider is all there is.
    let bare = ModelEntry {
        provider: "anthropic".into(),
        ..Default::default()
    };
    assert_eq!(bare.vendor_candidates(), vec!["anthropic"]);

    // No duplicate when host and provider agree.
    let same = ModelEntry {
        provider: "openai".into(),
        base_url: Some("https://api.openai.com/v1".into()),
        ..Default::default()
    };
    assert_eq!(same.vendor_candidates(), vec!["openai"]);
}

#[test]
fn vendor_is_omitted_from_yaml_when_absent_and_round_trips_when_set() {
    let bare = ModelEntry {
        provider: "anthropic".into(),
        model: "claude-opus-5".into(),
        ..Default::default()
    };
    let y = serde_yaml_ng::to_string(&bare).unwrap();
    assert!(!y.contains("vendor"), "{y}");

    let tagged = ModelEntry {
        provider: "openai".into(),
        vendor: Some("groq".into()),
        model: "llama-3.3".into(),
        ..Default::default()
    };
    let y = serde_yaml_ng::to_string(&tagged).unwrap();
    let back: ModelEntry = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(back.vendor.as_deref(), Some("groq"));
}
use super::*;

#[test]
fn parses_full_registry() {
    let yaml = r#"
schema_version: 1
models:
  anthropic_opus_4_7:
    provider: anthropic
    model: claude-opus-4-7
    secret: env:ANTHROPIC_API_KEY
    capabilities: [chat, tools]
  ollama_llama3:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(r.schema_version, 1);
    assert_eq!(r.models.len(), 2);
    let opus = r.models.get("anthropic_opus_4_7").unwrap();
    assert_eq!(opus.provider, "anthropic");
    assert_eq!(
        opus.secret,
        Some(SecretRef::Env("ANTHROPIC_API_KEY".into()))
    );
    assert!(r.models["ollama_llama3"].secret.is_none());
}

#[test]
fn round_trip_preserves_shape() {
    let mut r = ModelRegistry::default();
    r.models.insert(
        "foo".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            base_url: None,
            secret: Some(SecretRef::Keychain {
                service: "mur".into(),
                account: "anthropic".into(),
            }),
            capabilities: vec!["chat".into()],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            input_cost_per_1k: None,
            output_cost_per_1k: None,
            context_window: None,
            priced_at: None,
            ..Default::default()
        },
    );
    let s = serde_yaml_ng::to_string(&r).unwrap();
    let parsed: ModelRegistry = serde_yaml_ng::from_str(&s).unwrap();
    assert_eq!(r, parsed);
}

#[test]
fn rejects_unknown_secret_scheme() {
    let yaml = r#"
schema_version: 1
models:
  bad:
    provider: x
    model: y
    secret: bogus:value
"#;
    let r: Result<ModelRegistry, _> = serde_yaml_ng::from_str(yaml);
    assert!(r.is_err(), "should reject unknown scheme");
}

#[test]
fn test_registry_roundtrip_with_roles() {
    let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
roles:
  reflector:
    primary: haiku
    fallback: null
    cost_budget_per_day_usd: 0.5
"#;
    let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(reg.roles["reflector"].primary, "haiku");
    let back = serde_yaml_ng::to_string(&reg).unwrap();
    let reg2: ModelRegistry = serde_yaml_ng::from_str(&back).unwrap();
    assert_eq!(reg, reg2);
}

#[test]
fn test_resolve_role_primary() {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "haiku".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            input_cost_per_1k: None,
            output_cost_per_1k: None,
            context_window: None,
            priced_at: None,
            ..Default::default()
        },
    );
    reg.roles.insert(
        "reflector".into(),
        RoleEntry {
            primary: "haiku".into(),
            fallback: None,
            ..Default::default()
        },
    );
    assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
}

#[test]
fn test_resolve_role_fallback() {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "haiku".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            input_cost_per_1k: None,
            output_cost_per_1k: None,
            context_window: None,
            priced_at: None,
            ..Default::default()
        },
    );
    reg.roles.insert(
        "reflector".into(),
        RoleEntry {
            primary: "nonexistent".into(),
            fallback: Some("haiku".into()),
            ..Default::default()
        },
    );
    assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
}

#[test]
fn test_resolve_role_none() {
    let reg = ModelRegistry::default();
    assert_eq!(reg.resolve_role("reflector"), None);
}

#[test]
fn model_entry_parses_tier_field() {
    let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
    tier: local
  opus:
    provider: anthropic
    model: claude-opus-4-7
    tier: frontier
    cost_per_1k_tokens: 0.015
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(r.models["haiku"].tier, Some(RouteTier::Local));
    assert_eq!(r.models["opus"].tier, Some(RouteTier::Frontier));
    assert_eq!(r.models["opus"].cost_per_1k_tokens, Some(0.015));
    // Missing tier is None.
    let mut r2 = ModelRegistry::default();
    r2.models.insert(
        "x".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3".into(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            input_cost_per_1k: None,
            output_cost_per_1k: None,
            context_window: None,
            priced_at: None,
            ..Default::default()
        },
    );
    let yaml = serde_yaml_ng::to_string(&r2).unwrap();
    assert!(
        !yaml.contains("tier:"),
        "absent tier should not be serialized: {yaml}"
    );
}

#[test]
fn role_entry_parses_route_policy() {
    let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
  opus:
    provider: anthropic
    model: claude-opus-4-7
roles:
  dev:
    primary: opus
    route_policy: !force_frontier
      model_id: opus
  reflector:
    primary: haiku
    route_policy: prefer_local
  curator:
    primary: haiku
    route_policy: force_local
  chat:
    primary: haiku
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        r.roles["dev"].route_policy,
        Some(RoutePolicy::ForceFrontier {
            model_id: "opus".into()
        })
    );
    assert_eq!(
        r.roles["reflector"].route_policy,
        Some(RoutePolicy::PreferLocal)
    );
    assert_eq!(
        r.roles["curator"].route_policy,
        Some(RoutePolicy::ForceLocal)
    );
    assert_eq!(r.roles["chat"].route_policy, None);
}

#[test]
fn parses_split_cost_fields() {
    let yaml = r#"
schema_version: 1
models:
  opus:
    provider: anthropic
    model: claude-opus-4-8
    input_cost_per_1k: 0.005
    output_cost_per_1k: 0.025
    context_window: 200000
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    let e = r.models.get("opus").unwrap();
    assert_eq!(e.input_cost_per_1k, Some(0.005));
    assert_eq!(e.output_cost_per_1k, Some(0.025));
    assert_eq!(e.context_window, Some(200_000));
}

#[test]
fn max_tokens_parses_and_is_omitted_when_unset() {
    let yaml = r#"
schema_version: 1
models:
  opus:
    provider: claude
    model: claude-opus-5-5
    max_tokens: 128000
  plain:
    provider: anthropic
    model: claude-opus-5-5
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(r.models["opus"].max_tokens, Some(128_000));
    assert_eq!(r.models["plain"].max_tokens, None);
    // Unset stays out of the file, so saving an untouched registry does
    // not grow a `max_tokens: null` line on every entry.
    let out = serde_yaml_ng::to_string(&r.models["plain"]).unwrap();
    assert!(!out.contains("max_tokens"), "{out}");
}

#[test]
fn default_model_entry_is_empty() {
    let e = ModelEntry::default();
    assert!(e.provider.is_empty());
    assert_eq!(e.input_cost_per_1k, None);
    assert_eq!(e.output_cost_per_1k, None);
    assert_eq!(e.context_window, None);
}

#[test]
fn effective_costs_fallback_matrix() {
    // legacy only → both fall back to the blended rate
    let mut e = ModelEntry {
        cost_per_1k_tokens: Some(0.01),
        ..Default::default()
    };
    assert_eq!(e.effective_costs(), (Some(0.01), Some(0.01)));

    // split only → split wins, legacy ignored
    e = ModelEntry {
        input_cost_per_1k: Some(0.005),
        output_cost_per_1k: Some(0.025),
        ..Default::default()
    };
    assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

    // both → split wins
    e = ModelEntry {
        cost_per_1k_tokens: Some(0.01),
        input_cost_per_1k: Some(0.005),
        output_cost_per_1k: Some(0.025),
        ..Default::default()
    };
    assert_eq!(e.effective_costs(), (Some(0.005), Some(0.025)));

    // none → none
    e = ModelEntry::default();
    assert_eq!(e.effective_costs(), (None, None));
}
