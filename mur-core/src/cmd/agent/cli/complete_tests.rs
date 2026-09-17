use super::*;

fn skill(name: &str) -> Candidate {
    Candidate {
        display: name.into(),
        insert: name.into(),
        desc: String::new(),
        has_children: false,
    }
}

fn ctx() -> MenuContext {
    MenuContext {
        effort: vec!["low".into(), "high".into(), "max".into()],
        models: vec![
            ("fast".into(), "deepseek-v4".into()),
            ("smart".into(), "claude-opus-5".into()),
        ],
        secrets: vec!["GITHUB_TOKEN".into()],
        notes: vec!["last".into(), "note-20260908-101500".into()],
        ..MenuContext::default()
    }
}

fn cur() -> Current {
    Current::default()
}

/// Every settings menu marks the value in force — from the profile for
/// the model, from the session first for effort, from `App` for the rest —
/// and every action menu marks nothing.
#[test]
fn settings_menus_mark_the_value_in_force() {
    use mur_common::llm::Effort;
    let c = MenuContext {
        model_ref: Some("smart".into()),
        model_id: "claude-opus-5".into(),
        profile_effort: Some(Effort::High),
        ..ctx()
    };
    let cur = Current {
        auto: true,
        verbose: false,
        skin: "mur",
        ..cur()
    };
    let at = |input: &str, cur: &Current| {
        let s = compute(input, &[], &c, cur).unwrap();
        s.current.map(|i| s.items[i].display.clone())
    };
    assert_eq!(at("/model ", &cur).as_deref(), Some("smart"));
    assert_eq!(at("/effort ", &cur).as_deref(), Some("high"), "profile");
    let overridden = Current {
        session_effort: Some(Effort::Low),
        ..cur
    };
    assert_eq!(
        at("/effort ", &overridden).as_deref(),
        Some("low"),
        "session wins"
    );
    assert_eq!(at("/skin ", &cur).as_deref(), Some("mur"));
    assert_eq!(at("/auto ", &cur).as_deref(), Some("on"));
    assert_eq!(at("/verbose ", &cur).as_deref(), Some("off"));
    // The index is into the filtered rows, not the full list.
    let s = compute("/skin m", &[], &c, &cur).unwrap();
    assert_eq!(s.current, Some(0));
    assert_eq!(s.items[0].display, "mur");
    // Action menus and an unset effort mark nothing.
    assert_eq!(compute("/mcp ", &[], &c, &cur).unwrap().current, None);
    let unset = MenuContext {
        profile_effort: None,
        ..c.clone()
    };
    assert_eq!(
        compute("/effort ", &[], &unset, &cur).unwrap().current,
        None
    );
    assert_eq!(compute("/model ", &[], &MenuContext::default(), &cur), None);
}

fn effort_ctx(model: &str) -> MenuContext {
    MenuContext {
        effort: mur_common::llm::effort_shape(model)
            .levels()
            .iter()
            .map(|e| e.as_str().to_string())
            .collect(),
        ..MenuContext::default()
    }
}

fn displays(state: &CompletionState) -> Vec<String> {
    state.items.iter().map(|c| c.display.clone()).collect()
}

#[test]
fn no_menu_without_leading_slash() {
    assert!(compute("hello", &[skill("create-pr")], &ctx(), &cur()).is_none());
}

#[test]
fn matched_skill_resolves_slash_form_and_args() {
    // Real skill candidates carry a leading slash (see load_agent_skills).
    let skills = [skill("/brainstorming"), skill("/create-pr")];
    assert_eq!(
        matched_skill("/brainstorming", &skills),
        Some(("brainstorming".into(), String::new()))
    );
    assert_eq!(
        matched_skill("/create-pr fix the bug", &skills),
        Some(("create-pr".into(), "fix the bug".into()))
    );
    // Non-skill slash words and plain text don't match.
    assert_eq!(matched_skill("/help", &skills), None);
    assert_eq!(matched_skill("hello", &skills), None);
}

#[test]
fn top_level_filters_commands_by_prefix_substring() {
    let s = compute("/sk", &[skill("create-pr")], &ctx(), &cur()).unwrap();
    let d = displays(&s);
    assert!(d.contains(&"/skill".to_string()));
    assert!(d.contains(&"/skin".to_string()));
    // "sk" does not match the skill "create-pr".
    assert!(!d.contains(&"create-pr".to_string()));
}

#[test]
fn top_level_includes_matching_skills() {
    let s = compute("/cre", &[skill("create-pr")], &ctx(), &cur()).unwrap();
    // Commands first, then skills (`build_top_level`). `/secret` is here
    // because the match is a substring one and "se<cre>t" contains "cre" —
    // matching how the menu really behaves, rather than asserting a list
    // that any new command with those letters would break.
    assert_eq!(
        displays(&s),
        vec!["/secret".to_string(), "create-pr".to_string()]
    );
}

#[test]
fn empty_slash_shows_commands_and_skills() {
    let s = compute("/", &[skill("create-pr")], &ctx(), &cur()).unwrap();
    let d = displays(&s);
    assert!(d.contains(&"/mcp".to_string()));
    assert!(d.contains(&"create-pr".to_string()));
}

#[test]
fn panel_subcommands() {
    let s = compute("/panel ", &[], &ctx(), &cur()).unwrap();
    assert!(s.items.iter().any(|c| c.insert == "/panel preview "));
    assert_eq!(s.items.len(), 6);
}

#[test]
fn bare_deep_research_is_a_sendable_leaf() {
    let s = compute("/deep-research", &[], &ctx(), &cur()).unwrap();
    let row = s
        .items
        .iter()
        .find(|c| c.display == "/deep-research")
        .unwrap();
    assert!(!row.has_children);
    assert_eq!(row.insert, "/deep-research");

    let subs = compute("/deep-research ", &[], &ctx(), &cur()).unwrap();
    assert!(subs.items.iter().any(|c| c.display == "status"));
}

/// The menu is where the verb is discovered. Bare text is also a question,
/// but a menu offering only status/stop/setup reads as "you cannot ask here".
#[test]
fn deep_research_offers_ask_with_room_for_the_question() {
    let subs = compute("/deep-research ", &[], &ctx(), &cur()).unwrap();
    let ask = subs
        .items
        .iter()
        .find(|c| c.display == "ask")
        .expect("ask must be offered");
    // Trailing space: accepting it leaves the caret ready for the question
    // instead of sending an empty ask.
    assert_eq!(ask.insert, "/deep-research ask ");
    assert!(!ask.has_children);
}

#[test]
fn descends_to_subcommands_after_space() {
    let s = compute("/mcp ", &[], &ctx(), &cur()).unwrap();
    let d = displays(&s);
    assert!(d.contains(&"list".to_string()));
    assert!(d.contains(&"add-remote".to_string()));
    let add = s.items.iter().find(|c| c.display == "list").unwrap();
    assert_eq!(add.insert, "/mcp list ");
    assert!(!add.has_children);
}

#[test]
fn subcommands_filter_by_query() {
    let s = compute("/mcp add", &[], &ctx(), &cur()).unwrap();
    let d = displays(&s);
    assert!(d.contains(&"add".to_string()));
    assert!(d.contains(&"add-remote".to_string()));
    assert!(!d.contains(&"list".to_string()));
}

#[test]
fn no_menu_past_layer_two() {
    assert!(compute("/mcp add foo", &[], &ctx(), &cur()).is_none());
}

#[test]
fn command_without_subcommands_has_no_layer_two() {
    assert!(compute("/help ", &[], &ctx(), &cur()).is_none());
}

#[test]
fn unknown_command_no_match_closes_menu() {
    assert!(compute("/zzz", &[], &ctx(), &cur()).is_none());
}

#[test]
fn top_level_command_marks_children() {
    let s = compute("/mc", &[], &ctx(), &cur()).unwrap();
    let mcp = s.items.iter().find(|c| c.display == "/mcp").unwrap();
    assert!(mcp.has_children);
    assert_eq!(mcp.insert, "/mcp ");
    let help = compute("/hel", &[], &ctx(), &cur()).unwrap();
    let h = help.items.iter().find(|c| c.display == "/help").unwrap();
    assert!(!h.has_children);
}

#[test]
fn skill_display_name_handles_paths_and_names() {
    assert_eq!(skill_display_name("/a/b/skills/foo/skill.yaml"), "foo");
    assert_eq!(skill_display_name("bar.yaml"), "bar");
    assert_eq!(skill_display_name("baz"), "baz");
}

#[test]
fn multiline_input_has_no_menu() {
    assert!(compute("/mcp\nlist", &[], &ctx(), &cur()).is_none());
}
/// A missing agent reads nothing and must not panic: the menu degrades to
/// its command layer rather than taking the session down.
#[test]
fn menu_context_is_fail_soft_on_a_missing_agent() {
    let home = tempfile::tempdir().unwrap();
    let ctx = MenuContext::load(home.path(), "nope");
    assert!(ctx.effort.is_empty());
    assert!(ctx.secrets.is_empty());
    assert!(ctx.notes.is_empty());
}

/// The levels are an arbitrary subset per model, never a prefix of one
/// scale. A hardcoded low/medium/high/xhigh/max would be wrong for every
/// row below except the first.
#[test]
fn effort_levels_follow_the_model_not_a_fixed_scale() {
    let levels = |model: &str| -> Vec<String> {
        compute("/effort ", &[], &effort_ctx(model), &cur())
            .map(|s| s.items.iter().map(|c| c.display.clone()).collect())
            .unwrap_or_default()
    };

    assert_eq!(levels("claude-opus-5").len(), 5);
    assert!(levels("claude-opus-5").contains(&"xhigh".to_string()));

    // 4.6 predates the xhigh step but keeps max.
    let opus46 = levels("claude-opus-4-6");
    assert!(!opus46.contains(&"xhigh".to_string()), "{opus46:?}");
    assert!(opus46.contains(&"max".to_string()), "{opus46:?}");

    // DeepSeek V4 publishes low/high/max — there is no medium.
    let ds = levels("deepseek-v4");
    assert!(!ds.contains(&"medium".to_string()), "{ds:?}");

    // A switch has two positions, not three that collapse to two.
    assert_eq!(levels("qwen3-32b").len(), 2);

    // gpt-5 and friends stop at high.
    assert_eq!(levels("gpt-5"), vec!["low", "medium", "high"]);
}

/// A model that rejects the parameter (Magistral, HTTP 422) or has no
/// reasoning control (gpt-4o) opens no menu at all — and `/effort` still
/// carries no marker promising one.
#[test]
fn a_model_without_effort_opens_no_menu_and_promises_none() {
    for model in ["magistral-medium-latest", "gpt-4o"] {
        let c = effort_ctx(model);
        assert!(c.effort.is_empty(), "{model}");
        assert!(compute("/effort ", &[], &c, &cur()).is_none(), "{model}");
        let top = compute("/effort", &[], &c, &cur()).unwrap();
        let row = top.items.iter().find(|i| i.display == "/effort").unwrap();
        assert!(!row.has_children, "{model} promised a layer it cannot open");
    }
}

/// `/secret` offers the KEYs already held plus the revoke flag; a new KEY
/// is typed freely and simply matches nothing, which closes the menu.
#[test]
fn secret_offers_held_keys_and_delete() {
    let s = compute("/secret ", &[], &ctx(), &cur()).unwrap();
    let d: Vec<String> = s.items.iter().map(|c| c.display.clone()).collect();
    assert!(d.contains(&"GITHUB_TOKEN".to_string()), "{d:?}");
    assert!(d.contains(&"--delete".to_string()), "{d:?}");
    assert!(compute("/secret NEW_KEY", &[], &ctx(), &cur()).is_none());
}

/// `/model` completes registry aliases, described by the id behind them.
#[test]
fn model_offers_registry_aliases() {
    let s = compute("/model ", &[], &ctx(), &cur()).unwrap();
    let row = s.items.iter().find(|c| c.display == "fast").unwrap();
    assert_eq!(row.insert, "/model fast ");
    assert_eq!(row.desc, "deepseek-v4");
}

/// `/forget` completes note names, `last` first.
#[test]
fn forget_offers_last_then_note_names() {
    let s = compute("/forget ", &[], &ctx(), &cur()).unwrap();
    assert_eq!(s.items[0].display, "last");
    assert_eq!(s.items.len(), 2);
}

/// After a `/model` switch the menu must offer the NEW model's levels.
/// Two shapes with different level counts, so a stale context cannot pass
/// by coincidence.
#[test]
fn switching_models_changes_the_levels_on_offer() {
    let five = effort_ctx("claude-opus-5");
    let three = effort_ctx("gpt-5");
    assert_ne!(five.effort, three.effort);
    assert_eq!(
        compute("/effort ", &[], &five, &cur()).unwrap().items.len(),
        5
    );
    assert_eq!(
        compute("/effort ", &[], &three, &cur())
            .unwrap()
            .items
            .len(),
        3
    );
}

/// `/monitor` sits alphabetically between its `model`/`open` neighbours
/// in the completion table, and `SlashCmd::Monitor` parses from both its
/// long and short spellings.
#[test]
fn monitor_is_offered_between_its_alphabetical_neighbours_and_parses() {
    let words: Vec<&str> = COMMANDS.iter().map(|(w, _, _)| *w).collect();
    let idx = words.iter().position(|w| *w == "monitor").unwrap();
    assert_eq!(words[idx - 1], "model");
    assert_eq!(words[idx + 1], "open");
    assert!(offers("monitor"));

    use super::super::app::{SlashCmd, parse_slash};
    assert!(matches!(
        parse_slash("/monitor"),
        Some(SlashCmd::Monitor(_))
    ));
    assert!(matches!(parse_slash("/mon"), Some(SlashCmd::Monitor(_))));
}
