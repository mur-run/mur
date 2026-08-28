use mur_common::config::{MemoryConfig, SkillsConfig};
use mur_common::skill::TriggerKind;
use mur_common::skill::loader::LoadedSkill;
use mur_common::skill::types::{Category, HostId, Priority};
use std::collections::HashSet;

fn priority_val(p: &Priority) -> u8 {
    match p {
        Priority::Low => 0,
        Priority::Normal => 1,
        Priority::High => 2,
        Priority::Critical => 3,
    }
}

#[derive(Debug, Clone, Default)]
pub struct InjectionResult {
    pub system_addendum: String,
    pub injected_names: Vec<String>,
    pub budget_skipped: bool,
}

// ponytail: 8 args, one over clippy's threshold. The honest fix is bundling
// `active_fleet`/`active_project`/`active_team` into one `Scope` struct — they
// always travel together — but that is mechanical churn across every call site
// and belongs in its own commit, not this bug fix.
#[allow(clippy::too_many_arguments)]
pub fn inject_layer2(
    skills: &[LoadedSkill],
    cfg: &SkillsConfig,
    mem: &MemoryConfig,
    context_fill_ratio: f64,
    recently_fired: &HashSet<String>,
    active_fleet: Option<&str>,
    active_project: Option<&str>,
    active_team: Option<&str>,
) -> InjectionResult {
    // Adaptive cutoff: skip entirely when remaining context is too small.
    if let Some(ad) = &cfg.adaptive {
        let remaining = 1.0 - context_fill_ratio;
        if remaining < ad.min_remaining_context_ratio {
            return InjectionResult {
                budget_skipped: true,
                ..Default::default()
            };
        }
    }

    // Host + scope + not-on-demand. Split below into memories (always-on) and
    // trigger-gated skills.
    let visible: Vec<&LoadedSkill> = skills
        .iter()
        .filter(|s| {
            s.manifest.hosts.is_empty()
                || s.manifest
                    .hosts
                    .iter()
                    .any(|h| matches!(h, HostId::All | HostId::MurAgent))
        })
        // Scope: fleet/project-scoped skills inject only when the active scope
        // matches (fail-closed); user/enterprise always pass. active_project is
        // the member's cwd repo root; active_fleet is the turn's `fleet-<name>`
        // channel (membership-verified by the channel/delegate handler).
        .filter(|s| {
            mur_common::skill::manifest::scope_visible(
                s.manifest.scope,
                s.manifest.fleet.as_deref(),
                s.manifest.project.as_deref(),
                s.manifest.team.as_deref(),
                active_fleet,
                active_project,
                active_team,
            )
        })
        .filter(|s| s.manifest.visibility != mur_common::skill::manifest::Visibility::OnDemand)
        .collect();

    // Memories (`Category::Note`, written by the `remember` tool, `/remember`
    // and `mur notes create`) are always-on: the user stated them outright, so
    // they carry no `SessionStart` trigger and must not compete for skill
    // slots. Without this split they were written to disk and never reached a
    // prompt — the agent ignored its own saved rules and reported an empty
    // memory when asked.
    // ponytail: capped by max_skills_in_prompt, same as skills; give notes
    // their own config knob only if a real memory list starves.
    let mut notes: Vec<&LoadedSkill> = visible
        .iter()
        .copied()
        .filter(|s| s.manifest.category == Category::Note)
        .collect();
    notes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut mem_dropped = notes.len().saturating_sub(mem.max_in_prompt);
    notes.truncate(mem.max_in_prompt);

    // Filter: must have at least one `SessionStart` trigger.
    let mut candidates: Vec<&LoadedSkill> = visible
        .into_iter()
        .filter(|s| s.manifest.category != Category::Note)
        .filter(|s| {
            s.manifest
                .triggers
                .iter()
                .any(|t| matches!(t.kind, TriggerKind::SessionStart))
        })
        .collect();

    // Sort: trust desc, recent-fired boost, then priority asc, then name for determinism.
    candidates.sort_by(|a, b| {
        let trust_cmp = b.trust.cmp(&a.trust);
        if trust_cmp != std::cmp::Ordering::Equal {
            return trust_cmp;
        }
        let a_recent = recently_fired.contains(&a.name);
        let b_recent = recently_fired.contains(&b.name);
        if a_recent != b_recent {
            return b_recent.cmp(&a_recent);
        }
        priority_val(&a.manifest.priority)
            .cmp(&priority_val(&b.manifest.priority))
            .then(a.name.cmp(&b.name))
    });

    candidates.truncate(cfg.max_skills_in_prompt);

    // Adaptive token budget (char-based proxy).
    let budget = cfg
        .adaptive
        .as_ref()
        .map(|ad| {
            let remaining = 1.0 - context_fill_ratio;
            ((cfg.max_total_tokens as f64) * remaining.powf(ad.context_fill_decay)) as usize
        })
        .unwrap_or(cfg.max_total_tokens)
        .max(100);

    let mut spent = 0usize;
    let mut names = Vec::new();

    // Memories draw on their OWN character budget. Sharing the skill budget
    // would mean saving a memory silently evicts a bound skill.
    let mut mem_spent = 0usize;
    let mut mem_lines = Vec::new();
    for s in notes {
        let body = s
            .manifest
            .content
            .note
            .as_deref()
            .unwrap_or(&s.manifest.content.r#abstract);
        let line = format!("- {}: {}", s.name, body.trim());
        if mem_spent + line.len() + 1 > mem.max_chars {
            mem_dropped += 1;
            continue;
        }
        mem_spent += line.len() + 1;
        mem_lines.push(line);
        names.push(s.name.clone());
    }
    // No silent caps: a memory the user saved and cannot see in the prompt is
    // indistinguishable from one that was never saved.
    if mem_dropped > 0 {
        mem_lines.push(format!(
            "- ({mem_dropped} more memories not shown here — the user can list \
             them all with /memories)"
        ));
    }

    let mut lines = Vec::new();
    for s in candidates {
        let line = format!(
            "[Skill: {} ({:?})] {}",
            s.name,
            s.trust,
            s.manifest.content.r#abstract.trim()
        );
        if spent + line.len() + 1 > budget {
            continue;
        }
        spent += line.len() + 1;
        lines.push(line);
        names.push(s.name.clone());
    }
    if lines.is_empty() && mem_lines.is_empty() {
        return InjectionResult::default();
    }
    let mut system_addendum = String::new();
    if !lines.is_empty() {
        system_addendum.push_str(&format!(
            "\n--- Bound Skills ---\n{}\n---\n",
            lines.join("\n")
        ));
    }
    // Memory goes LAST, for two independent reasons that agree: standing user
    // instructions win the recency end of a long prompt, and a block that
    // changes when a memory is saved invalidates less of the cached prefix
    // sitting in front of it.
    if !mem_lines.is_empty() {
        system_addendum.push_str(&format!(
            "\n--- Memory (durable facts and rules you saved for this user; \
             /memories lists them, /forget <name> removes one) ---\n{}\n---\n",
            mem_lines.join("\n")
        ));
    }
    InjectionResult {
        system_addendum,
        injected_names: names,
        budget_skipped: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
