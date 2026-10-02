use super::*;

#[test]
fn test_attachment_format_is_text_based() {
    assert!(AttachmentFormat::Mermaid.is_text_based());
    assert!(AttachmentFormat::PlantUml.is_text_based());
    assert!(!AttachmentFormat::Png.is_text_based());
    assert!(!AttachmentFormat::Svg.is_text_based());
}

#[test]
fn test_attachment_format_from_extension() {
    assert_eq!(
        AttachmentFormat::from_extension("mmd"),
        Some(AttachmentFormat::Mermaid)
    );
    assert_eq!(
        AttachmentFormat::from_extension("mermaid"),
        Some(AttachmentFormat::Mermaid)
    );
    assert_eq!(
        AttachmentFormat::from_extension("puml"),
        Some(AttachmentFormat::PlantUml)
    );
    assert_eq!(
        AttachmentFormat::from_extension("plantuml"),
        Some(AttachmentFormat::PlantUml)
    );
    assert_eq!(
        AttachmentFormat::from_extension("png"),
        Some(AttachmentFormat::Png)
    );
    assert_eq!(
        AttachmentFormat::from_extension("svg"),
        Some(AttachmentFormat::Svg)
    );
    assert_eq!(AttachmentFormat::from_extension("jpg"), None);
    assert_eq!(AttachmentFormat::from_extension(""), None);
    // Case insensitive
    assert_eq!(
        AttachmentFormat::from_extension("MMD"),
        Some(AttachmentFormat::Mermaid)
    );
}

#[test]
fn test_attachment_format_fence_lang() {
    assert_eq!(AttachmentFormat::Mermaid.fence_lang(), "mermaid");
    assert_eq!(AttachmentFormat::PlantUml.fence_lang(), "plantuml");
    assert_eq!(AttachmentFormat::Png.fence_lang(), "");
}

#[test]
fn test_attachment_type_from_format() {
    assert_eq!(
        AttachmentType::from_format(&AttachmentFormat::Mermaid),
        AttachmentType::Diagram
    );
    assert_eq!(
        AttachmentType::from_format(&AttachmentFormat::PlantUml),
        AttachmentType::Diagram
    );
    assert_eq!(
        AttachmentType::from_format(&AttachmentFormat::Png),
        AttachmentType::Image
    );
    assert_eq!(
        AttachmentType::from_format(&AttachmentFormat::Svg),
        AttachmentType::Image
    );
}

#[test]
fn test_attachment_serde() {
    let att = Attachment {
        att_type: AttachmentType::Diagram,
        format: AttachmentFormat::Mermaid,
        path: "my-pattern/arch.mermaid".to_string(),
        description: "Architecture diagram".to_string(),
    };

    let yaml = serde_yaml::to_string(&att).unwrap();
    assert!(yaml.contains("type: diagram"));
    assert!(yaml.contains("format: mermaid"));
    assert!(yaml.contains("path: my-pattern/arch.mermaid"));
    assert!(yaml.contains("description: Architecture diagram"));

    let deserialized: Attachment = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.att_type, AttachmentType::Diagram);
    assert_eq!(deserialized.format, AttachmentFormat::Mermaid);
}

#[test]
fn test_attachment_svg_serde() {
    let att = Attachment {
        att_type: AttachmentType::Image,
        format: AttachmentFormat::Svg,
        path: "my-pattern/logo.svg".to_string(),
        description: "Logo".to_string(),
    };

    let yaml = serde_yaml::to_string(&att).unwrap();
    let deserialized: Attachment = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.format, AttachmentFormat::Svg);
    assert_eq!(deserialized.att_type, AttachmentType::Image);
}

#[test]
fn test_pattern_kind_serde_roundtrip() {
    // All variants serialize to lowercase
    let cases = vec![
        (PatternKind::Technical, "technical"),
        (PatternKind::Preference, "preference"),
        (PatternKind::Fact, "fact"),
        (PatternKind::Procedure, "procedure"),
        (PatternKind::Behavioral, "behavioral"),
    ];
    for (kind, expected_str) in cases {
        let yaml = serde_yaml::to_string(&kind).unwrap();
        assert!(
            yaml.contains(expected_str),
            "Expected '{}' in '{}'",
            expected_str,
            yaml
        );
        let deserialized: PatternKind = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized, kind);
    }
}

#[test]
fn test_origin_trigger_serde_roundtrip() {
    let cases = vec![
        (OriginTrigger::UserExplicit, "user_explicit"),
        (OriginTrigger::UserCorrection, "user_correction"),
        (OriginTrigger::AgentInferred, "agent_inferred"),
        (OriginTrigger::CommunityShared, "community_shared"),
        (OriginTrigger::AutoConsolidated, "auto_consolidated"),
    ];
    for (trigger, expected_str) in cases {
        let yaml = serde_yaml::to_string(&trigger).unwrap();
        assert!(yaml.contains(expected_str));
        let deserialized: OriginTrigger = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized, trigger);
    }
}

#[test]
fn test_origin_serde_roundtrip() {
    #[allow(deprecated)] // intentional: testing legacy field serialization
    let origin = Origin {
        source: "commander".to_string(),
        trigger: OriginTrigger::UserExplicit,
        actor: None,
        user: Some("david".to_string()),
        platform: Some("slack".to_string()),
        confidence: 0.95,
    };
    let yaml = serde_yaml::to_string(&origin).unwrap();
    assert!(yaml.contains("source: commander"));
    assert!(yaml.contains("trigger: user_explicit"));
    assert!(yaml.contains("user: david"));
    assert!(yaml.contains("platform: slack"));

    let deserialized: Origin = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.source, "commander");
    assert_eq!(deserialized.trigger, OriginTrigger::UserExplicit);
    #[allow(deprecated)]
    {
        assert_eq!(deserialized.user, Some("david".to_string()));
        assert_eq!(deserialized.platform, Some("slack".to_string()));
    }
    assert!((deserialized.confidence - 0.95).abs() < 0.001);
}

#[test]
fn test_origin_optional_fields_omitted() {
    #[allow(deprecated)] // intentional: testing legacy field omission
    let origin = Origin {
        source: "cli".to_string(),
        trigger: OriginTrigger::AgentInferred,
        actor: None,
        user: None,
        platform: None,
        confidence: 1.0,
    };
    let yaml = serde_yaml::to_string(&origin).unwrap();
    assert!(!yaml.contains("user:"));
    assert!(!yaml.contains("platform:"));
}

#[test]
fn test_pattern_with_kind_and_origin_roundtrip() {
    use crate::knowledge::KnowledgeBase;
    let pattern = Pattern {
            base: KnowledgeBase {
                name: "test-pref".into(),
                description: "A preference".into(),
                content: Content::Plain("Use Chinese".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            #[allow(deprecated)] // intentional: testing legacy field in pattern roundtrip
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                actor: None,
                user: Some("david".into()),
                platform: None,
                confidence: 0.9,
            }),
            attachments: vec![],
        };

    let yaml = serde_yaml::to_string(&pattern).unwrap();
    assert!(yaml.contains("kind: preference"));
    assert!(yaml.contains("source: commander"));

    let deserialized: Pattern = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.kind, Some(PatternKind::Preference));
    assert_eq!(deserialized.effective_kind(), PatternKind::Preference);
    assert!(deserialized.origin.is_some());
    assert_eq!(deserialized.origin.unwrap().source, "commander");
}

#[test]
fn origin_with_actor_roundtrip() {
    use crate::{Actor, ActorSource};
    #[allow(deprecated)]
    let o = Origin {
        source: "commander".into(),
        trigger: OriginTrigger::AgentInferred,
        actor: Some(Actor {
            source: ActorSource::Slack,
            native_id: "U999".into(),
            display_name: Some("bob".into()),
            resolved_user_id: None,
        }),
        user: None,
        platform: None,
        confidence: 0.8,
    };
    let y = serde_yaml::to_string(&o).unwrap();
    let back: Origin = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back.actor.as_ref().unwrap().native_id, "U999");
}

#[test]
fn origin_backward_compat_no_actor_field() {
    // 舊 YAML (pre-sync feature) lacks `actor`, `user`, `platform` fields
    let old_yaml = r#"
source: starter
trigger: automatic
confidence: 0.5
"#;
    let o: Origin = serde_yaml::from_str(old_yaml).unwrap();
    assert!(o.actor.is_none());
    #[allow(deprecated)]
    {
        assert!(o.user.is_none());
        assert!(o.platform.is_none());
    }
}

#[test]
fn origin_reads_legacy_user_platform_yaml() {
    // YAML written by pre-sync code that populated user/platform (not actor)
    let legacy_yaml = r#"
source: import
trigger: automatic
user: alice
platform: "CLAUDE.md"
confidence: 0.7
"#;
    let o: Origin = serde_yaml::from_str(legacy_yaml).unwrap();
    #[allow(deprecated)]
    {
        assert_eq!(o.user.as_deref(), Some("alice"));
        assert_eq!(o.platform.as_deref(), Some("CLAUDE.md"));
    }
    assert!(o.actor.is_none());
}

#[test]
fn test_pattern_backward_compat_no_kind_no_origin() {
    // Existing YAML without kind/origin fields should deserialize fine
    let yaml = "name: old-pattern\ndescription: Old\ncontent: Some content\n";
    let pattern: Pattern = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(pattern.name, "old-pattern");
    assert!(pattern.kind.is_none());
    assert!(pattern.origin.is_none());
    assert_eq!(pattern.effective_kind(), PatternKind::Technical);
}

#[test]
fn test_pattern_kind_default() {
    assert_eq!(PatternKind::default(), PatternKind::Technical);
}

#[test]
fn evidence_contributions_default_empty() {
    let e = Evidence::default();
    assert!(e.contributions.is_empty());
}

#[test]
fn evidence_effectiveness_by_actor_splits_signals() {
    use crate::{Actor, ActorSource};

    let alice = Actor {
        source: ActorSource::Slack,
        native_id: "alice".into(),
        display_name: None,
        resolved_user_id: None,
    };
    let bob = Actor {
        source: ActorSource::Slack,
        native_id: "bob".into(),
        display_name: None,
        resolved_user_id: None,
    };

    let mut contribs = HashMap::new();
    contribs.insert(
        alice.key(),
        Contribution {
            success_signals: 8,
            override_signals: 2,
            last_seen: Utc::now(),
        },
    );
    contribs.insert(
        bob.key(),
        Contribution {
            success_signals: 1,
            override_signals: 4,
            last_seen: Utc::now(),
        },
    );

    let e = Evidence {
        source_sessions: vec![],
        first_seen: None,
        last_validated: None,
        injection_count: 15,
        success_signals: 9,
        failure_signals: 0,
        override_signals: 6,
        contributions: contribs,
    };

    assert!((e.effectiveness_by_actor(&alice) - 0.8).abs() < 0.001);
    assert!((e.effectiveness_by_actor(&bob) - 0.2).abs() < 0.001);
    // Global effectiveness is unchanged by per-actor accessor:
    // effectiveness() = success/(success+override) = 9/15 = 0.6
    assert!((e.effectiveness() - 0.6).abs() < 0.001);
}

#[test]
fn evidence_effectiveness_by_unknown_actor_returns_neutral_prior() {
    use crate::{Actor, ActorSource};

    let unknown = Actor {
        source: ActorSource::MurCli,
        native_id: "never-seen".into(),
        display_name: None,
        resolved_user_id: None,
    };
    let e = Evidence::default();
    // No contribution exists for this actor → neutral 0.5 prior
    assert!((e.effectiveness_by_actor(&unknown) - 0.5).abs() < 0.001);
}

#[test]
fn evidence_yaml_roundtrip_with_contributions() {
    use crate::{Actor, ActorSource};

    let actor = Actor {
        source: ActorSource::CommanderDaemon,
        native_id: "svc-1".into(),
        display_name: None,
        resolved_user_id: None,
    };
    let mut contribs = HashMap::new();
    contribs.insert(
        actor.key(),
        Contribution {
            success_signals: 3,
            override_signals: 1,
            last_seen: DateTime::parse_from_rfc3339("2026-04-18T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        },
    );
    let e = Evidence {
        contributions: contribs,
        ..Evidence::default()
    };
    let y = serde_yaml::to_string(&e).unwrap();
    let back: Evidence = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back.contributions.len(), 1);
    assert_eq!(
        back.contributions
            .get("CommanderDaemon:svc-1")
            .unwrap()
            .success_signals,
        3
    );
}

#[test]
fn evidence_yaml_backward_compat_no_contributions_field() {
    // Old YAML lacks the new field
    let old_yaml = r#"
source_sessions: []
first_seen: null
last_validated: null
injection_count: 5
success_signals: 3
override_signals: 1
"#;
    let e: Evidence = serde_yaml::from_str(old_yaml).unwrap();
    assert!(e.contributions.is_empty());
    assert_eq!(e.success_signals, 3);
}

#[test]
fn pattern_scope_defaults_personal() {
    // Pre-sync YAML has no `scope:` field
    let old_yaml = r#"
schema: 2
name: legacy-pattern
description: legacy
content: old content
tier: session
"#;
    let p: Pattern = serde_yaml::from_str(old_yaml).unwrap();
    assert_eq!(p.scope, crate::Scope::Personal);
}

#[test]
fn pattern_scope_team_roundtrip() {
    let y = r#"
schema: 2
name: team-pat
description: team pattern
content: team content
tier: project
scope:
  kind: team
  team_id: ops
"#;
    let p: Pattern = serde_yaml::from_str(y).unwrap();
    assert_eq!(
        p.scope,
        crate::Scope::Team {
            team_id: "ops".into()
        }
    );
    // Roundtrip verify
    let y2 = serde_yaml::to_string(&p).unwrap();
    let p2: Pattern = serde_yaml::from_str(&y2).unwrap();
    assert_eq!(p2.scope, p.scope);
}

#[test]
fn pattern_scope_community_roundtrip() {
    let y = r#"
schema: 2
name: comm-pat
description: community pattern
content: community content
tier: core
scope:
  kind: community
  pack_id: rust-best-practices
"#;
    let p: Pattern = serde_yaml::from_str(y).unwrap();
    assert_eq!(
        p.scope,
        crate::Scope::Community {
            pack_id: Some("rust-best-practices".into())
        }
    );
}
