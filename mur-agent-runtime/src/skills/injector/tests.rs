use super::*;

/// Build a note exactly as the production note builder does, then apply
/// the Required tag the same way a `required` note on disk carries it.
///
/// Policy is expressed as a TAG, not as a new field, because
/// `lifecycle::injection_policy` is the single reader of that tag (plan
/// §3): a test that set a bespoke field would be testing a shape that
/// never reaches disk.
fn note(name: &str, body: &str, required: bool) -> LoadedSkill {
    use mur_common::skill::note::{NoteSpec, note_manifest};

    let mut manifest = note_manifest(&NoteSpec {
        name,
        description: body,
        body,
        kind: mur_common::skill::lifecycle::NoteKind::Rule,
        publisher: "agent:mur",
    });
    if required {
        manifest.tags.push("required".into());
    }
    LoadedSkill {
        name: name.to_string(),
        manifest,
        // A `remember`-written note is never in the trust store.
        trust: TrustLevel::Sandboxed,
        scope: SkillScope::Agent,
        content_hash: String::new(),
        dir: std::path::PathBuf::new(),
    }
}

/// REGRESSION TEST TO KEEP FOREVER (plan §14): the original bug.
///
/// A permanent instruction competed in the same name-sorted, top-K
/// truncated pool as incidental memories, so it survived or died by
/// alphabetical luck. Here the Required note sorts LAST alphabetically
/// (`zz-`) and there are far more BestEffort notes than `max_in_prompt`,
/// so under the old `sort().take(n)` it is guaranteed to be cut.
///
/// Required must bypass relevance, top-K, recency, and alphabetical
/// order (plan invariant 1).
#[test]
fn required_memory_survives_top_k_regardless_of_alphabetical_order() {
    let mut skills: Vec<LoadedSkill> = (0..50)
        .map(|i| {
            note(
                &format!("aa-incidental-{i:02}"),
                &format!("trivia {i}"),
                false,
            )
        })
        .collect();
    skills.push(note("zz-language", "永遠用中文回答我", true));

    let mem = MemoryConfig {
        max_in_prompt: 10,
        ..MemoryConfig::default()
    };
    let r = inject_layer2(
        &skills,
        &SkillsConfig::default(),
        &mem,
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );

    assert!(
        r.system_addendum.contains("永遠用中文回答我"),
        "the permanent instruction must reach the prompt even though it \
         sorts last and top-K is 10; got:\n{}",
        r.system_addendum
    );
    assert!(
        r.injected_names.contains(&"zz-language".to_string()),
        "Required must be reported as injected, got: {:?}",
        r.injected_names
    );
}

/// Plan §14 test 2: "20 Required, top-K 10 → all 20 considered."
///
/// Distinct from the regression test above, which has a single Required
/// note: this one proves the Required set is not itself capped at
/// `max_in_prompt`. It also pins the reporting invariant — the
/// "N more not shown" notice must count only BestEffort, since claiming a
/// Required memory was hidden would be a lie about a guarantee.
#[test]
fn required_set_is_never_capped_by_max_in_prompt() {
    let skills: Vec<LoadedSkill> = (0..20)
        .map(|i| note(&format!("instruction-{i:02}"), &format!("rule {i}"), true))
        .collect();

    let mem = MemoryConfig {
        max_in_prompt: 10,
        ..MemoryConfig::default()
    };
    let r = inject_layer2(
        &skills,
        &SkillsConfig::default(),
        &mem,
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );

    for i in 0..20 {
        let name = format!("instruction-{i:02}");
        assert!(
            r.injected_names.contains(&name),
            "all 20 Required must be injected despite top-K 10; {name} is \
             missing from {:?}",
            r.injected_names
        );
    }
    assert!(
        !r.system_addendum.contains("more memories not shown"),
        "no Required memory may be reported as hidden; got:\n{}",
        r.system_addendum
    );
}

/// Plan invariant 2 / §8: the character cap may never silently drop a
/// Required memory.
///
/// `max_chars` is set absurdly low (1) so that under the old shared-budget
/// loop every note would hit the `continue` and be counted as dropped.
/// Required must ignore the cap entirely; BestEffort must still yield.
#[test]
fn required_memory_ignores_the_character_cap_and_best_effort_yields() {
    let skills = vec![
        note("zz-permanent", "永遠用中文回答我", true),
        note("aa-trivia", "user likes tabs", false),
    ];

    let mem = MemoryConfig {
        max_in_prompt: 10,
        max_chars: 1,
        ..MemoryConfig::default()
    };
    let r = inject_layer2(
        &skills,
        &SkillsConfig::default(),
        &mem,
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );

    assert!(
        r.system_addendum.contains("永遠用中文回答我"),
        "Required must survive a max_chars that cannot fit it; got:\n{}",
        r.system_addendum
    );
    assert_eq!(
        r.injected_names,
        vec!["zz-permanent".to_string()],
        "BestEffort must yield to Required, never the reverse"
    );
    assert!(
        r.system_addendum.contains("1 more memories not shown"),
        "the dropped BestEffort note must still be disclosed; got:\n{}",
        r.system_addendum
    );
}

/// The exact artifact the `remember` tool writes — built by the production
/// note builder, not a hand-rolled yaml — must reach the prompt even though
/// notes carry no `SessionStart` trigger. Negative control: an ordinary
/// trigger-less skill still stays out.
#[test]
fn memory_notes_inject_without_a_session_start_trigger() {
    use mur_common::skill::note::{NoteSpec, note_manifest};

    let note = LoadedSkill {
        name: "reply-in-zh-tw".into(),
        manifest: note_manifest(&NoteSpec {
            name: "reply-in-zh-tw",
            description: "使用者要求一律以繁體中文回覆",
            body: "以後所有回覆都用繁體中文。",
            kind: mur_common::skill::lifecycle::NoteKind::Rule,
            publisher: "agent:mur",
        }),
        // Reality check: a `remember`-written note is never in the trust
        // store, so the loader gives it Sandboxed. Asserting on Verified
        // would have tested a state that never occurs.
        trust: TrustLevel::Sandboxed,
        scope: SkillScope::Agent,
        content_hash: String::new(),
        dir: std::path::PathBuf::new(),
    };
    assert!(
        note.manifest.triggers.is_empty(),
        "precondition: the real note artifact has no triggers"
    );
    let untriggered_skill = loaded("plain", "not a memory", TrustLevel::Verified, "");

    let r = inject_layer2(
        &[note, untriggered_skill],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(
        r.system_addendum.contains("以後所有回覆都用繁體中文"),
        "memory body must be injected, got: {}",
        r.system_addendum
    );
    assert!(
        r.injected_names.contains(&"reply-in-zh-tw".to_string()),
        "memory must be reported as injected"
    );
    assert!(
        !r.injected_names.contains(&"plain".to_string()),
        "negative control: a trigger-less non-note skill must stay out"
    );
}
use mur_common::skill::loader::SkillScope;
use mur_common::skill::parse_canonical;
use mur_common::skill::types::TrustLevel;

fn loaded(name: &str, abstract_: &str, trust: TrustLevel, triggers: &str) -> LoadedSkill {
    let yaml = format!(
        r#"name: {name}
version: 1.0.0
publisher: human:t
description: test
category: context
content:
  abstract: "{abstract_}"
  context: body
{triggers}
"#
    );
    let m = parse_canonical(&yaml).unwrap();
    LoadedSkill {
        name: name.to_string(),
        manifest: m,
        trust,
        scope: SkillScope::Global,
        content_hash: String::new(),
        dir: std::path::PathBuf::new(),
    }
}

#[test]
fn project_scoped_skill_injects_only_when_project_matches() {
    let mk = |name: &str, scope_yaml: &str| {
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: test\n\
             category: context\n{scope_yaml}content:\n  abstract: \"a\"\n  context: body\n\
             triggers:\n  - type: session_start\n"
        );
        LoadedSkill {
            name: name.to_string(),
            manifest: parse_canonical(&yaml).unwrap(),
            trust: TrustLevel::Verified,
            scope: SkillScope::Global,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    };
    let skills = vec![mk("u", ""), mk("p", "scope: project\nproject: /repo\n")];
    let names = |active: Option<&str>| {
        inject_layer2(
            &skills,
            &SkillsConfig::default(),
            &MemoryConfig::default(),
            0.0,
            &HashSet::new(),
            None,
            active,
            None,
        )
        .injected_names
    };
    // no active project → project skill fail-closed; user always injects
    let n0 = names(None);
    assert!(n0.contains(&"u".to_string()) && !n0.contains(&"p".to_string()));
    // matching active project → project skill injects
    assert!(names(Some("/repo")).contains(&"p".to_string()));
    // wrong project → fail-closed
    assert!(!names(Some("/other")).contains(&"p".to_string()));
}

#[test]
fn on_demand_skill_never_injects_layer2() {
    let s = loaded(
        "hidden-leaf",
        "should never appear",
        TrustLevel::Verified,
        "visibility: on_demand\ntriggers:\n  - type: session_start\n    pattern: \"\"",
    );
    let result = inject_layer2(
        &[s],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(result.injected_names.is_empty());
    assert!(result.system_addendum.is_empty());
}

#[test]
fn fleet_scoped_skill_injects_only_when_fleet_matches() {
    let mk = |name: &str, scope_yaml: &str| {
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: test\n\
             category: context\n{scope_yaml}content:\n  abstract: \"a\"\n  context: body\n\
             triggers:\n  - type: session_start\n"
        );
        LoadedSkill {
            name: name.to_string(),
            manifest: parse_canonical(&yaml).unwrap(),
            trust: TrustLevel::Verified,
            scope: SkillScope::Global,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    };
    let skills = vec![mk("u", ""), mk("f", "scope: fleet\nfleet: dev\n")];
    // active_fleet is the 5th arg; active_project stays None throughout.
    let names = |active_fleet: Option<&str>| {
        inject_layer2(
            &skills,
            &SkillsConfig::default(),
            &MemoryConfig::default(),
            0.0,
            &HashSet::new(),
            active_fleet,
            None,
            None,
        )
        .injected_names
    };
    // no active fleet → fleet skill fail-closed; user always injects
    let n0 = names(None);
    assert!(n0.contains(&"u".to_string()) && !n0.contains(&"f".to_string()));
    // matching active fleet → fleet skill injects
    assert!(names(Some("dev")).contains(&"f".to_string()));
    // wrong fleet → fail-closed
    assert!(!names(Some("other")).contains(&"f".to_string()));
}

#[test]
fn no_session_start_not_injected() {
    let s = loaded(
        "cmd-only",
        "Do stuff",
        TrustLevel::Verified,
        "triggers:\n  - type: command\n    pattern: /x\n",
    );
    let result = inject_layer2(
        &[s],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(result.system_addendum.is_empty());
    assert!(result.injected_names.is_empty());
}

#[test]
fn trusted_before_sandboxed() {
    let a = loaded(
        "sand",
        "low trust",
        TrustLevel::Sandboxed,
        "triggers:\n  - type: session_start\n",
    );
    let b = loaded(
        "trust",
        "high trust",
        TrustLevel::Trusted,
        "triggers:\n  - type: session_start\n",
    );
    let result = inject_layer2(
        &[a, b],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert_eq!(result.injected_names.len(), 2);
    assert_eq!(result.injected_names[0], "trust");
    assert_eq!(result.injected_names[1], "sand");
}

/// A Required note sitting in the same list as BestEffort trivia gave the
/// model no reason to prefer it over a conflicting persona default (the
/// concierge persona says "mirror the user's language"; a pinned note says
/// "always reply in zh-TW"). Required renders in its own block, marked as
/// outranking persona/style defaults, and LAST so it holds the recency end.
#[test]
fn required_renders_as_overriding_block_after_best_effort() {
    let skills = vec![
        note("zz-permanent", "回覆固定使用繁體中文", true),
        note("aa-trivia", "user likes tabs", false),
    ];
    let r = inject_layer2(
        &skills,
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    let a = &r.system_addendum;
    let header = a
        .find(REQUIRED_HEADER)
        .unwrap_or_else(|| panic!("Required block header missing:\n{a}"));
    let pinned = a.find("回覆固定使用繁體中文").unwrap();
    let trivia = a.find("user likes tabs").unwrap();
    assert!(
        header < pinned,
        "pinned note must sit under the header:\n{a}"
    );
    assert!(trivia < header, "Required must come after BestEffort:\n{a}");
}

/// Regression: the adaptive cutoff used to `return` before the
/// Required/BestEffort split, so once `cumulative_input_tokens` (runner
/// lifetime, never reset) crossed 80% of the window, every permanent
/// instruction silently vanished from the prompt. Required is a
/// guarantee of injection (plan invariant 2); the cutoff may only shed
/// trigger-gated skills and BestEffort memories.
#[test]
fn required_memory_survives_adaptive_cutoff() {
    let skills = vec![
        note("zz-permanent", "永遠用中文回答我", true),
        note("aa-trivia", "user likes tabs", false),
        loaded(
            "x",
            "hi",
            TrustLevel::Verified,
            "triggers:\n  - type: session_start\n",
        ),
    ];
    let cfg = SkillsConfig {
        adaptive: Some(mur_common::config::AdaptiveSkillsConfig {
            min_remaining_context_ratio: 0.5,
            ..mur_common::config::AdaptiveSkillsConfig::default()
        }),
        ..SkillsConfig::default()
    };
    let r = inject_layer2(
        &skills,
        &cfg,
        &MemoryConfig::default(),
        0.85,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(
        r.system_addendum.contains("永遠用中文回答我"),
        "Required must survive the adaptive cutoff; got:\n{}",
        r.system_addendum
    );
    assert_eq!(r.injected_names, vec!["zz-permanent".to_string()]);
    assert!(r.budget_skipped, "the cutoff itself must still be reported");
}

#[test]
fn adaptive_skips_when_context_too_full() {
    let s = loaded(
        "x",
        "hi",
        TrustLevel::Verified,
        "triggers:\n  - type: session_start\n",
    );
    let cfg = SkillsConfig {
        adaptive: Some(mur_common::config::AdaptiveSkillsConfig {
            min_remaining_context_ratio: 0.5,
            ..mur_common::config::AdaptiveSkillsConfig::default()
        }),
        ..SkillsConfig::default()
    };
    let result = inject_layer2(
        &[s],
        &cfg,
        &MemoryConfig::default(),
        0.85,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(result.budget_skipped);
    assert!(result.injected_names.is_empty());
}

#[test]
fn max_skills_capped() {
    let skills: Vec<_> = (0..5)
        .map(|i| {
            loaded(
                &format!("s{i}"),
                "hi",
                TrustLevel::Verified,
                "triggers:\n  - type: session_start\n",
            )
        })
        .collect();
    let cfg = SkillsConfig {
        max_skills_in_prompt: 2,
        ..SkillsConfig::default()
    };
    let result = inject_layer2(
        &skills,
        &cfg,
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert_eq!(result.injected_names.len(), 2);
}

#[test]
fn team_scoped_skill_injects_when_team_matches() {
    let mk = |name: &str, scope_yaml: &str| {
        let yaml = format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: test\n\
             category: context\n{scope_yaml}content:\n  abstract: \"a\"\n  context: body\n\
             triggers:\n  - type: session_start\n"
        );
        LoadedSkill {
            name: name.to_string(),
            manifest: parse_canonical(&yaml).unwrap(),
            trust: TrustLevel::Verified,
            scope: SkillScope::Global,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    };
    let skills = vec![mk("u", ""), mk("ts", "scope: team\nteam: org-x\n")];
    // active_team is the 7th arg; active_fleet and active_project stay None.
    let names = |active_team: Option<&str>| {
        inject_layer2(
            &skills,
            &SkillsConfig::default(),
            &MemoryConfig::default(),
            0.0,
            &HashSet::new(),
            None,
            None,
            active_team,
        )
        .injected_names
    };
    // no active team → team skill fail-closed; user always injects
    let n0 = names(None);
    assert!(n0.contains(&"u".to_string()) && !n0.contains(&"ts".to_string()));
    // matching active team → team skill injects
    assert!(names(Some("org-x")).contains(&"ts".to_string()));
    // wrong team → fail-closed
    assert!(!names(Some("org-y")).contains(&"ts".to_string()));
}

#[test]
fn team_scoped_skill_excluded_without_active_team() {
    let yaml = "name: ts\nversion: 1.0.0\npublisher: human:t\ndescription: test\n\
                category: context\nscope: team\nteam: org-x\n\
                content:\n  abstract: \"a\"\n  context: body\n\
                triggers:\n  - type: session_start\n";
    let s = LoadedSkill {
        name: "ts".to_string(),
        manifest: parse_canonical(yaml).unwrap(),
        trust: TrustLevel::Verified,
        scope: SkillScope::Global,
        content_hash: String::new(),
        dir: std::path::PathBuf::new(),
    };
    let result = inject_layer2(
        &[s],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &HashSet::new(),
        None,
        None,
        None,
    );
    assert!(
        result.injected_names.is_empty(),
        "team skill must not inject when active_team is None"
    );
}

#[test]
fn recently_fired_breaks_tie_within_same_trust() {
    let a = loaded(
        "a",
        "a",
        TrustLevel::Verified,
        "triggers:\n  - type: session_start\n",
    );
    let b = loaded(
        "b",
        "b",
        TrustLevel::Verified,
        "triggers:\n  - type: session_start\n",
    );
    let mut fired = HashSet::new();
    fired.insert("b".to_string());
    let result = inject_layer2(
        &[a, b],
        &SkillsConfig::default(),
        &MemoryConfig::default(),
        0.0,
        &fired,
        None,
        None,
        None,
    );
    assert_eq!(result.injected_names.len(), 2);
    assert_eq!(result.injected_names[0], "b");
}
