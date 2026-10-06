use super::*;

fn act(tool: &str, target: &str, outcome: Outcome) -> Action {
    Action {
        tool: tool.into(),
        target: target.into(),
        outcome,
        excerpt: None,
    }
}

#[test]
fn edits_are_changed_and_commands_are_evidence() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    l.record(act("write_file", "src/b.rs", Outcome::Ok));
    l.record(act("bash", "cargo test", Outcome::Ok));
    // The whole point: nine edits are not a passing build. Editing lands
    // in `changed`; only something that RAN counts as evidence.
    assert_eq!(l.changed().len(), 2);
    assert_eq!(l.verified().len(), 1);
    assert_eq!(l.verified()[0].target, "cargo test");
}

#[test]
fn a_read_with_nothing_run_is_not_evidence() {
    let mut l = TurnLedger::default();
    l.record(act("read_file", "src/a.rs", Outcome::Ok));
    // A successful read (or a sed/grep/cat, same shape) is not evidence
    // that anything works — it must report as nothing having run.
    assert!(l.verified().is_empty());
    let card = render(&l);
    assert!(card.contains("nothing ran"), "{card}");
}

#[test]
fn a_passing_test_suite_is_evidence() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    l.record(act("bash", "cargo test --workspace", Outcome::Ok));
    assert_eq!(l.verified().len(), 1);
    assert_eq!(l.verified()[0].target, "cargo test --workspace");
}

#[test]
fn a_runner_name_inside_a_grep_pattern_is_not_evidence() {
    let mut l = TurnLedger::default();
    // Investigating this very bug looks like this: a grep for the
    // runner string must not itself be credited as having run it.
    l.record(act("bash", "grep -rn \"cargo test\" src/", Outcome::Ok));
    assert!(l.verified().is_empty());
    let card = render(&l);
    assert!(card.contains("nothing ran"), "{card}");
}

#[test]
fn a_runner_after_a_cd_prefix_is_evidence() {
    let mut l = TurnLedger::default();
    l.record(act(
        "bash",
        "cd /repo && cargo nextest run -p x",
        Outcome::Ok,
    ));
    assert_eq!(l.verified().len(), 1);
}

#[test]
fn a_failed_command_is_never_evidence() {
    let mut l = TurnLedger::default();
    l.record(act(
        "bash",
        "cargo test",
        Outcome::Failed("2 failed".into()),
    ));
    assert!(l.verified().is_empty());
    assert_eq!(l.blocked().len(), 1);
}

#[test]
fn a_zero_tool_turn_that_names_external_state_warrants_settlement() {
    let claimed = TurnLedger {
        claims_external_state: true,
        ..Default::default()
    };
    assert!(claimed.unverified_claim());
    assert!(claimed.warrants_settlement());

    // Same claim, but a tool ran: the other clauses decide, and a single
    // successful read decides "no card" exactly as before.
    let mut read = TurnLedger {
        claims_external_state: true,
        ..Default::default()
    };
    read.record(act("read_file", "README.md", Outcome::Ok));
    assert!(!read.unverified_claim());
    assert!(!read.warrants_settlement());

    // No claim, nothing ran: still a pure question, still no card.
    assert!(!TurnLedger::default().unverified_claim());
    assert!(!TurnLedger::default().warrants_settlement());
}

#[test]
fn render_prints_the_unverified_row_for_a_zero_tool_claim() {
    let l = TurnLedger {
        claims_external_state: true,
        ..Default::default()
    };
    let card = render(&l);
    assert!(card.contains(UNVERIFIED_ROW.trim_end()), "{card}");
    assert!(!card.contains("nothing ran"), "{card}");
    assert!(card.contains("⚠ unverified"), "{card}");
    assert!(!card.contains("✔"), "{card}");
}

#[test]
fn the_claim_flag_is_additive_on_the_wire() {
    // A ledger written before the field existed.
    let old =
        r#"{"actions":[],"stop":"end_turn","iterations":0,"input_tokens":0,"output_tokens":0}"#;
    let l: TurnLedger = serde_json::from_str(old).unwrap();
    assert!(!l.claims_external_state);
    // `false` is not written; `true` round-trips.
    let s = serde_json::to_string(&TurnLedger::default()).unwrap();
    assert!(!s.contains("claims_external_state"), "{s}");
    let flagged = TurnLedger {
        claims_external_state: true,
        ..Default::default()
    };
    let s = serde_json::to_string(&flagged).unwrap();
    assert!(s.contains(r#""claims_external_state":true"#), "{s}");
    let back: TurnLedger = serde_json::from_str(&s).unwrap();
    assert!(back.claims_external_state);
}

#[test]
fn settlement_triggers_on_change_failure_or_dirty_stop() {
    // A read-only turn that ended cleanly: no table.
    let mut quiet = TurnLedger::default();
    quiet.record(act("read_file", "README.md", Outcome::Ok));
    assert!(!quiet.warrants_settlement());

    // One edit is enough.
    let mut edited = TurnLedger::default();
    edited.record(act("edit_file", "a.rs", Outcome::Ok));
    assert!(edited.warrants_settlement());

    // So is one failure, with nothing changed.
    let mut failed = TurnLedger::default();
    failed.record(act("bash", "ls", Outcome::Failed("nope".into())));
    assert!(failed.warrants_settlement());

    // And so is a truncated turn that did nothing at all — the user needs
    // to know the output is partial even when the transcript looks calm.
    let truncated = TurnLedger {
        stop: StopKind::MaxIterations,
        ..Default::default()
    };
    assert!(truncated.warrants_settlement());
}

#[test]
fn classify_separates_denial_from_ordinary_failure() {
    let denied = classify(
        "ignored",
        false,
        &crate::tools::ToolStatus::Denied {
            detail: "`cargo` is not in agent 'mur''s spawn allowlist".to_string(),
            scope: crate::tools::DenialScope::Action,
        },
    );
    match denied {
        Outcome::Denied(d) => assert!(d.contains("cargo"), "{d}"),
        other => panic!("expected Denied, got {other:?}"),
    }
    // A denial is not merely a failure: the remedy is to route or grant,
    // not to retry, so it must not be folded into Failed.
    assert!(matches!(
        classify("error: no such file", true, &crate::tools::ToolStatus::Ok),
        Outcome::Failed(_)
    ));
    assert_eq!(
        classify("all good", false, &crate::tools::ToolStatus::Ok),
        Outcome::Ok
    );
}

/// Spec §3.7: a yield is its own outcome — before `is_error`, and never
/// folded into `Ok`.
#[test]
fn classify_running_is_neither_ok_nor_failed() {
    let running = classify(
        "cargo test …\n[still running after 30s — job_id: j-1]",
        false,
        &crate::tools::ToolStatus::Running {
            job_id: "j-1".into(),
            bytes_seen: 512,
        },
    );
    assert_eq!(running, Outcome::Running("j-1".into()));
    let mut l = TurnLedger::default();
    l.record(act("bash", "cargo test", running));
    assert!(l.verified().is_empty(), "a yield is not evidence");
    assert!(l.blocked().is_empty(), "a yield is not a failure");
    assert_eq!(l.running().len(), 1);
    assert!(l.warrants_settlement());
    let card = render(&l);
    assert!(card.contains("⏳ bash · still running (j-1)"), "{card}");
    assert!(!card.contains("✘"), "{card}");
}

#[test]
fn render_states_an_empty_verified_column_instead_of_hiding_it() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    let card = render(&l);
    // The whole feature in one assertion: nine edits and no test run must
    // not read as success.
    assert!(card.contains("nothing ran"), "{card}");
    assert!(card.contains("1 file(s)"), "{card}");
    // The bug was the GLYPH, not the words: `✔` is painted with
    // `theme.success`, so the row rendered green while denying it works.
    assert!(
        card.contains("⚠ verified"),
        "an unverified turn must not carry the success glyph: {card}"
    );
    assert!(!card.contains("✔ verified"), "{card}");
}

#[test]
fn render_counts_files_not_edits_and_survives_a_markdown_renderer() {
    let mut l = TurnLedger::default();
    for _ in 0..4 {
        l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    }
    l.record(act("edit_file", "src/b.rs", Outcome::Ok));
    let card = render(&l);
    // Four edits to one file is one changed file. The old count said 5 and
    // then listed src/a.rs four times underneath itself.
    assert!(card.contains("2 file(s)"), "{card}");
    assert_eq!(card.matches("src/a.rs").count(), 1, "{card}");
    // Fenced, or every consumer's Markdown pass reflows the rows into one
    // paragraph (a single newline is a soft break in CommonMark).
    assert!(card.starts_with("\n\n```\n"), "{card}");
    assert!(card.ends_with("```"), "{card}");
}

#[test]
fn render_names_the_denial_and_the_truncation() {
    let mut l = TurnLedger {
        stop: StopKind::MaxIterations,
        iterations: 25,
        ..Default::default()
    };
    l.record(act(
        "bash",
        "cargo test",
        Outcome::Denied("`cargo` is not in the spawn allowlist".into()),
    ));
    let card = render(&l);
    assert!(card.contains("sandbox:"), "{card}");
    // The truncation notice belongs IN the settlement, not appended after
    // the model's own claim where the two can contradict each other.
    assert!(card.contains("iteration ceiling"), "{card}");
    assert!(card.contains("output may be incomplete"), "{card}");
}

#[test]
fn describe_target_picks_the_identifying_argument() {
    let bash = serde_json::json!({"command": "cargo test", "timeout_secs": 600});
    assert_eq!(describe_target("bash", &bash), "cargo test");
    let edit = serde_json::json!({"path": "src/lib.rs", "old_string": "x"});
    assert_eq!(describe_target("edit_file", &edit), "src/lib.rs");
    // Unknown tool: fall back to the whole input rather than an empty cell.
    assert!(!describe_target("mcp__media__x", &serde_json::json!({"q": 1})).is_empty());
    // Long commands are cut so the table stays a table.
    let long = serde_json::json!({"command": "x".repeat(200)});
    assert!(describe_target("bash", &long).chars().count() <= RUNAWAY_BACKSTOP);
    // Newlines would break the row.
    let multi = serde_json::json!({"command": "a\nb"});
    assert_eq!(describe_target("bash", &multi), "a b");
}

#[test]
fn dispatched_fleet_target_keeps_its_recoverable_run_id() {
    let input = serde_json::json!({"fleet": "deep-research", "goal": "research it"});
    let output = serde_json::json!({
        "run_id": "fleet-deep-research-01a0bd40",
        "status": "dispatched"
    })
    .to_string();

    assert_eq!(
        describe_target_with_result("fleet_run", &input, &output),
        "deep-research → fleet-deep-research-01a0bd40"
    );
}

#[test]
fn fleet_target_ignores_missing_or_non_dispatched_handles() {
    let input = serde_json::json!({"fleet": "deep-research", "goal": "research it"});
    assert_eq!(
        describe_target_with_result("fleet_run", &input, "not json"),
        "deep-research"
    );
    assert_eq!(
        describe_target_with_result(
            "fleet_run",
            &input,
            &serde_json::json!({"run_id": "old-run", "status": "failed"}).to_string()
        ),
        "deep-research"
    );
}

#[test]
fn long_detail_survives_to_the_renderer() {
    // The runtime cannot know the terminal width, so it must not decide
    // what fits. Only a runaway-dump backstop remains.
    let mut l = TurnLedger::default();
    let long = "e".repeat(300);
    l.record(act("bash", "cargo test", Outcome::Failed(long.clone())));
    let card = render(&l);
    assert!(card.contains(&long), "detail was truncated: {card}");
    assert!(!card.contains('…'), "nothing should be elided here: {card}");
}

#[test]
fn the_settlement_header_carries_no_fixed_width_rule() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/lib.rs", Outcome::Ok));
    let card = render(&l);
    assert!(card.contains("─ settlement ─\n"), "{card}");
    // A hard-coded rule cannot match a pane it never measured.
    assert!(!card.contains("──────────"), "{card}");
}

#[test]
fn describe_target_drops_empty_args() {
    // `{}` is punctuation, not a target — the card line reads better bare.
    assert_eq!(
        describe_target("mcp__media__stats", &serde_json::json!({})),
        ""
    );
    assert_eq!(
        describe_target("mcp__media__stats", &serde_json::Value::Null),
        ""
    );
}

/// The exact shapes from the field report: an MCP tool with empty args and
/// a bash failure wrapped twice by the transport.
#[test]
fn render_compacts_mcp_names_and_transport_noise_in_blocked() {
    let mut l = TurnLedger::default();
    // mur_compress_stats is a query, not evidence — it does not belong in
    // `verified` (that's covered elsewhere). Here it's denied, so it's the
    // vehicle for exercising short_tool()/noise-stripping on the blocked
    // row instead of the verified one.
    l.record(act(
        "mcp__media__mur_compress_stats",
        "",
        Outcome::Denied("not permitted".into()),
    ));
    l.record(act(
        "bash",
        "ls; grep -rn \"compress-today\" --include=* -l . 2>/dev/null | head",
        Outcome::Failed("tool error: tool execution failed: command timed out after 30s".into()),
    ));
    let card = render(&l);
    assert!(
        card.contains("  ✘ mur_compress_stats · sandbox: not permitted\n"),
        "{card}"
    );
    assert!(!card.contains("mcp__media"), "{card}");
    assert!(!card.contains("{}"), "{card}");
    assert!(
        card.contains("  ✘ bash · command timed out after 30s\n"),
        "{card}"
    );
    assert!(!card.contains("tool execution failed"), "{card}");
    assert!(card.contains("      ls; grep"), "{card}");
}

#[test]
fn a_reason_that_already_names_the_target_does_not_repeat_it() {
    // write_file quotes the offending path in its own message, so the card
    // was printing that path on the reason line and again as the target.
    let path = "/tmp/scratchpad/settlement-test.txt";
    let mut l = TurnLedger::default();
    l.record(act(
            "write_file",
            path,
            Outcome::Failed(format!(
                "tool execution failed: path not write-entitled: {path} (grant it via `mur agent perm allow-write`)"
            )),
        ));
    let card = render(&l);
    assert_eq!(
        card.matches(path).count(),
        1,
        "target printed more than once:\n{card}"
    );
}

#[test]
fn a_reason_that_omits_the_target_still_prints_it() {
    // Negative control. Without this, "never print the target" would look
    // like it fixed the duplication while quietly dropping the one fact
    // that says WHICH file the failure was about.
    let mut l = TurnLedger::default();
    l.record(act(
        "write_file",
        "/tmp/some/file.txt",
        Outcome::Failed("disk quota exceeded".into()),
    ));
    let card = render(&l);
    assert!(card.contains("/tmp/some/file.txt"), "target lost:\n{card}");
}

/// Every unclean stop says what to do about it, next to the fact, and
/// names the command that exists now.
#[test]
fn every_unclean_stop_names_its_remedy() {
    assert_eq!(StopKind::EndTurn.remedy("dev"), None);
    for k in [
        StopKind::MaxIterations,
        StopKind::TokenBudget,
        StopKind::LoopDetected,
        StopKind::MaxTokens,
        StopKind::Deadline,
        StopKind::Stuck {
            last_calls: "bash".into(),
        },
    ] {
        assert!(k.remedy("dev").is_some(), "{k:?}");
    }
    assert!(
        ITERATION_CEILING_NOTE.contains(&crate::task_runner::ITERATION_CEILING.to_string()),
        "the card must name the ceiling the loop uses"
    );
    assert!(
        StopKind::Deadline
            .remedy("dev")
            .unwrap()
            .contains("mur limits dev --deadline")
    );
    let s = StopKind::Stuck {
        last_calls: "bash, bash, bash".into(),
    }
    .remedy("dev")
    .unwrap();
    assert!(
        s.contains("bash, bash, bash") && s.contains("--stuck"),
        "{s}"
    );

    let ledger = TurnLedger {
        stop: StopKind::Deadline,
        iterations: 17,
        agent: "dev".into(),
        ..Default::default()
    };
    let card = render(&ledger);
    assert!(
            card.contains("⚠ stopped at deadline (17 iterations) — output may be incomplete · raise it: mur limits dev --deadline"),
            "{card}"
        );
}

/// `TokenBudget` is only ever DESERIALISED — no code path produces it since
/// 2.79 (the variant's own doc comment says so). So a card carrying it is
/// being rendered by a current runtime reading an OLD ledger, and telling
/// that reader to "upgrade the agent runtime" sends them to fix the one
/// thing that is already correct. Worse, it names a bound that no longer
/// exists without saying what replaced it.
#[test]
fn the_retired_token_budget_remedy_does_not_send_the_user_to_upgrade() {
    let r = StopKind::TokenBudget.remedy("dev").unwrap();
    assert!(
        !r.contains("upgrade"),
        "the runtime reading this ledger is already new; the LEDGER is old: {r}"
    );
    assert!(
        r.contains("2.79"),
        "a retired bound must say when it was retired, or the reader \
             cannot tell it apart from a live one: {r}"
    );
    assert!(
        r.contains("mur limits dev"),
        "naming a dead bound without naming the live ones leaves the \
             reader with nothing to do: {r}"
    );
}

/// A turn that hit a wall must hand back enough to RESUME it, and that
/// handoff has to be derived from the ledger — same reason the verified /
/// changed split is derived. `graceful_exit` asks the model to "summarize
/// … the remaining steps", which is exactly the narration this module
/// exists to replace: the one artefact a reader needs in order to continue
/// is the one nothing checks.
#[test]
fn a_wall_stop_hands_back_what_is_needed_to_resume() {
    let mut l = TurnLedger {
        stop: StopKind::Deadline,
        iterations: 12,
        agent: "dev".into(),
        ..Default::default()
    };
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    l.record(act("write_file", "src/b.rs", Outcome::Ok));
    l.record(act(
        "bash",
        "cargo test -p x",
        Outcome::Failed("1 failed".into()),
    ));

    let h = l.handoff().expect("a deadline stop must produce a handoff");
    // The three facts a resumer cannot reconstruct from prose: which files
    // are dirty, what the last gate run actually said, and that nothing
    // was proven.
    assert!(h.contains("src/a.rs") && h.contains("src/b.rs"), "{h}");
    assert!(h.contains("cargo test -p x"), "{h}");
    assert!(h.contains("1 failed"), "{h}");
    // It must not claim the work is done.
    assert!(!h.contains("complete"), "{h}");
}

/// A handoff nothing renders is a handoff nobody gets. The card is the
/// only place the stop reason is already shown, so it is where the resume
/// state belongs — right under the line that says the output is partial.
#[test]
fn the_card_carries_the_handoff_on_a_wall_stop() {
    let mut l = TurnLedger {
        stop: StopKind::Stuck {
            last_calls: "bash, bash, bash".into(),
        },
        iterations: 9,
        agent: "dev".into(),
        ..Default::default()
    };
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    let card = render(&l);
    assert!(card.contains("resume from here:"), "{card}");
    assert!(card.contains("src/a.rs"), "{card}");
    assert!(
        card.contains("no gate run succeeded"),
        "the handoff must say the edits are unproven: {card}"
    );
    // Still fenced — the handoff is inside the code block, not loose
    // Markdown that a renderer will reflow into one paragraph.
    assert!(card.ends_with("```"), "{card}");
}

/// Negative control: a clean turn's card must stay free of resume noise.
#[test]
fn a_clean_card_carries_no_handoff() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    l.record(act("bash", "cargo test", Outcome::Ok));
    let card = render(&l);
    assert!(!card.contains("resume from here"), "{card}");
}

/// The negative control: a turn that ended on its own terms has nothing to
/// resume, and emitting a handoff there would train the reader to skip it.
#[test]
fn a_clean_stop_has_no_handoff() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    l.record(act("bash", "cargo test", Outcome::Ok));
    assert!(l.handoff().is_none());
}

/// Negative control for the test above. Without it, "make every remedy
/// mention `mur limits`" would satisfy the assertion while flattening the
/// live stops — whose remedies are specific and must stay that way.
#[test]
fn a_live_stop_keeps_its_own_remedy_and_not_the_retired_boilerplate() {
    let loop_detected = StopKind::LoopDetected.remedy("dev").unwrap();
    assert!(
        loop_detected.contains("identical arguments") && !loop_detected.contains("2.79"),
        "{loop_detected}"
    );
    let max_tokens = StopKind::MaxTokens.remedy("dev").unwrap();
    assert!(
        max_tokens.contains("continue from where it stopped") && !max_tokens.contains("2.79"),
        "{max_tokens}"
    );
}

/// Only a track-backed count is a measured count. A turn that got no track
/// (cwd not at a repo root, tracks opted out) falls back to what the tools
/// said about themselves — a `bash` redirect is invisible there — and the
/// card must say so on the line itself, or every card looks equally
/// trustworthy and the reader cannot tell which turns the diff vouched for.
#[test]
fn render_marks_a_changed_count_that_no_diff_backs() {
    let mut l = TurnLedger::default();
    l.record(act("edit_file", "src/a.rs", Outcome::Ok));
    let card = render(&l);
    assert!(
        card.contains("~ changed    1 file(s) (tool-reported)"),
        "{card}"
    );

    let backed = TurnLedger {
        files_changed: Some(vec!["src/a.rs".into()]),
        ..TurnLedger::default()
    };
    let card = render(&backed);
    assert!(card.contains("~ changed    1 file(s)\n"), "{card}");
    assert!(!card.contains("tool-reported"), "{card}");
}
