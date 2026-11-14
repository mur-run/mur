use super::*;

#[test]
fn model_switch_config_defaults_and_omitted_block() {
    // Omitted `models:` block deserializes to defaults.
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert_eq!(cfg.models.default, None);
    assert!(cfg.models.fallback_chain.is_empty());
    assert_eq!(cfg.models.retry.max_retries, DEFAULT_MAX_RETRIES);
    assert_eq!(cfg.models.retry.backoff_base_ms, DEFAULT_BACKOFF_BASE_MS);
    assert_eq!(cfg.models.retry.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    assert!(!cfg.models.routing.enabled);

    // A populated block round-trips.
    let yaml = "models:\n  default: claude_sonnet\n  fallback_chain: [claude_sonnet, deepseek_v4_pro]\n  routing:\n    enabled: true\n    cheap: deepseek_v4_flash\n    frontier: claude_opus\n    threshold_input_tokens: 1500\n";
    let cfg: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.models.default.as_deref(), Some("claude_sonnet"));
    assert_eq!(
        cfg.models.fallback_chain,
        vec!["claude_sonnet", "deepseek_v4_pro"]
    );
    assert!(cfg.models.routing.enabled);
    assert_eq!(cfg.models.routing.threshold_input_tokens, Some(1500));
}

#[test]
fn smart_config_defaults_off_with_autopick() {
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert!(
        !cfg.models.smart.enabled,
        "Smart background routing is opt-in (capability-gate spec §2)"
    );
    assert_eq!(cfg.models.smart.cheap, None); // auto-pick
    assert_eq!(
        cfg.models.smart.max_escalations,
        DEFAULT_SMART_MAX_ESCALATIONS
    );
}

#[test]
fn smart_override_inherits_field_by_field() {
    let global = SmartConfig {
        enabled: true,
        cheap: Some("g".into()),
        max_escalations: 3,
    };
    assert_eq!(global.merged(None), global);
    assert_eq!(global.merged(Some(&SmartOverride::default())), global);
    // Overriding one field must not reset the others — the whole point.
    let only_cheap = SmartOverride {
        cheap: Some("a".into()),
        ..Default::default()
    };
    let m = global.merged(Some(&only_cheap));
    assert!(m.enabled, "overriding cheap must not disable Smart");
    assert_eq!(m.cheap.as_deref(), Some("a"));
    assert_eq!(m.max_escalations, 3);
    // An explicit false still beats a global true.
    let off = SmartOverride {
        enabled: Some(false),
        ..Default::default()
    };
    assert!(!global.merged(Some(&off)).enabled);
}

#[test]
fn routing_override_inherits_field_by_field() {
    let global = RoutingConfig {
        enabled: true,
        cheap: Some("c".into()),
        frontier: Some("f".into()),
        threshold_input_tokens: Some(9),
    };
    let only_cheap = RoutingOverride {
        cheap: Some("a".into()),
        ..Default::default()
    };
    let m = global.merged(Some(&only_cheap));
    assert!(m.enabled, "overriding cheap must not disable routing");
    assert_eq!(m.cheap.as_deref(), Some("a"));
    assert_eq!(m.frontier.as_deref(), Some("f"));
    assert_eq!(m.threshold_input_tokens, Some(9));
}
