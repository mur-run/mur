//! Tests for `setup.rs` (#1762): the review-setup wizard and reconcile plan.

use std::io::Cursor;

use super::*;

fn choices() -> Vec<String> {
    vec!["alpha".into(), "beta".into(), "gamma".into()]
}

fn no_current(_: &str) -> Option<String> {
    None
}

fn wizard(script: &str, args: &SetupArgs) -> (Result<WizardAnswers>, String) {
    let mut input = Cursor::new(script.as_bytes().to_vec());
    let mut out = Vec::new();
    let r = run_wizard(&mut input, &mut out, args, &choices(), &no_current);
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn enter_everywhere_takes_defaults_and_diversifies_the_reviewer() {
    let (r, _) = wizard("\n\n\n\n", &SetupArgs::default());
    let a = r.unwrap();
    assert_eq!(a.main.name, DEFAULT_REVIEW_MAIN_AGENT);
    assert_eq!(a.reviewer.name, DEFAULT_REVIEW_REVIEWER_AGENT);
    assert_eq!(a.main.model, "alpha");
    assert_eq!(
        a.reviewer.model, "beta",
        "reviewer default avoids main's model"
    );
}

#[test]
fn models_by_number_or_name_and_bad_picks_reask() {
    let (r, out) = wizard("m\nr\n9\nnope\n3\nalpha\n", &SetupArgs::default());
    let a = r.unwrap();
    assert_eq!((a.main.name.as_str(), a.reviewer.name.as_str()), ("m", "r"));
    assert_eq!(a.main.model, "gamma");
    assert_eq!(a.reviewer.model, "alpha");
    assert!(out.contains("no model #9"));
    assert!(out.contains("'nope' is not in models.yaml"));
}

#[test]
fn flags_skip_their_questions() {
    let args = SetupArgs {
        main: Some("m".into()),
        reviewer: Some("r".into()),
        main_model: Some("beta".into()),
        ..Default::default()
    };
    // Only the reviewer model is asked.
    let (r, out) = wizard("\n", &args);
    let a = r.unwrap();
    assert_eq!(a.main.model, "beta");
    assert_eq!(a.reviewer.model, "alpha");
    assert!(!out.contains("agent name"));
}

#[test]
fn same_name_for_both_is_refused() {
    let (r, _) = wizard("x\nx\n", &SetupArgs::default());
    assert!(r.unwrap_err().to_string().contains("different agents"));
}

#[test]
fn empty_registry_is_refused_with_the_fix() {
    let mut input = Cursor::new(Vec::new());
    let mut out = Vec::new();
    let e = run_wizard(
        &mut input,
        &mut out,
        &SetupArgs::default(),
        &[],
        &no_current,
    )
    .unwrap_err();
    assert!(e.to_string().contains("mur model add"));
}

#[test]
fn eof_aborts_instead_of_guessing() {
    let (r, _) = wizard("", &SetupArgs::default());
    assert!(r.unwrap_err().to_string().contains("end of input"));
}

#[test]
fn existing_model_ref_is_the_default() {
    let current = |n: &str| (n == "coach").then(|| "gamma".to_string());
    let a = answers_from_flags(&SetupArgs::default(), &choices(), &current).unwrap();
    assert_eq!(a.main.model, "gamma");
    assert_eq!(a.reviewer.model, "alpha");
}

#[test]
fn non_interactive_without_models_names_the_flags() {
    let e = answers_from_flags(&SetupArgs::default(), &[], &no_current).unwrap_err();
    assert!(e.to_string().contains("--main-model"));
}

#[test]
fn unknown_flag_model_is_refused() {
    let args = SetupArgs {
        main_model: Some("typo".into()),
        ..Default::default()
    };
    let a = answers_from_flags(&args, &choices(), &no_current).unwrap();
    assert!(check_models(&a, &choices()).is_err());
}

fn existing(prompt: Option<&str>, model: &str, timeout: u32) -> MemberState {
    MemberState {
        exists: true,
        model_ref: Some(model.into()),
        hitl_timeout_secs: timeout,
        sys_prompt: prompt.map(str::to_string),
        recorded_prompt_hash: None,
    }
}

#[test]
fn absent_agent_is_created_with_timeout_and_prompt() {
    let p = plan_member("c", Role::Main, "alpha", &MemberState::default(), Some(600));
    assert_eq!(
        p,
        MemberPlan {
            create: true,
            set_model: None,
            raise_timeout: Some(600),
            prompt: PromptAction::Write,
        }
    );
}

#[test]
fn timeout_is_raised_never_lowered() {
    let low = plan_member(
        "c",
        Role::Main,
        "alpha",
        &existing(None, "alpha", 300),
        Some(600),
    );
    assert_eq!(low.raise_timeout, Some(600));
    let high = plan_member(
        "c",
        Role::Main,
        "alpha",
        &existing(None, "alpha", 900),
        Some(600),
    );
    assert_eq!(high.raise_timeout, None);
    let off = plan_member("c", Role::Main, "alpha", &existing(None, "alpha", 1), None);
    assert_eq!(off.raise_timeout, None, "stuck off: nothing to outlast");
}

#[test]
fn model_is_written_only_when_it_changes() {
    let same = plan_member(
        "c",
        Role::Main,
        "alpha",
        &existing(None, "alpha", 600),
        None,
    );
    assert_eq!(same.set_model, None);
    let other = plan_member("c", Role::Main, "beta", &existing(None, "alpha", 600), None);
    assert_eq!(other.set_model.as_deref(), Some("beta"));
}

#[test]
fn prompt_rules_placeholder_ours_current_and_user_edited() {
    let placeholder = crate::cmd::agent::lifecycle::default_sys_prompt("c");
    let p = plan_member(
        "c",
        Role::Main,
        "a",
        &existing(Some(&placeholder), "a", 600),
        None,
    );
    assert_eq!(p.prompt, PromptAction::Write);

    let current = role_prompt(Role::Reviewer);
    let p = plan_member(
        "c",
        Role::Reviewer,
        "a",
        &existing(Some(current), "a", 600),
        None,
    );
    assert_eq!(p.prompt, PromptAction::Keep);

    // An older prompt this command wrote: its hash was recorded.
    let mut ours = existing(Some("old role prompt"), "a", 600);
    ours.recorded_prompt_hash = Some(prompt_hash("old role prompt"));
    assert_eq!(
        plan_member("c", Role::Main, "a", &ours, None).prompt,
        PromptAction::Write
    );

    // Edited after we wrote it: hash no longer matches.
    let mut edited = existing(Some("my own words"), "a", 600);
    edited.recorded_prompt_hash = Some(prompt_hash("old role prompt"));
    assert_eq!(
        plan_member("c", Role::Main, "a", &edited, None).prompt,
        PromptAction::UserEdited
    );
}

#[test]
fn rerun_of_a_reconciled_agent_is_a_no_op() {
    let mut s = existing(Some(role_prompt(Role::Main)), "alpha", 600);
    s.recorded_prompt_hash = Some(prompt_hash(role_prompt(Role::Main)));
    let p = plan_member("c", Role::Main, "alpha", &s, Some(600));
    assert!(!p.create && p.set_model.is_none() && p.raise_timeout.is_none());
    assert_eq!(p.prompt, PromptAction::Keep);
    assert!(!p.needs_restart(false));
}

#[test]
fn reads_state_from_an_agent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!read_member_state(tmp.path(), "none").unwrap().exists);
    let dir = tmp.path().join("agents").join("c");
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = include_str!("testdata/setup_profile.yaml");
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
    std::fs::write(dir.join("sys_prompt.md"), "hello").unwrap();
    std::fs::write(dir.join(REVIEW_SETUP_PROMPT_HASH_FILE), "abc\n").unwrap();
    let s = read_member_state(tmp.path(), "c").unwrap();
    assert!(s.exists);
    assert_eq!(s.model_ref.as_deref(), Some("alpha"));
    assert_eq!(s.hitl_timeout_secs, 300);
    assert_eq!(s.sys_prompt.as_deref(), Some("hello"));
    assert_eq!(s.recorded_prompt_hash.as_deref(), Some("abc"));
}

#[test]
fn role_prompts_defer_the_wire_format_to_the_per_turn_message() {
    // The per-turn prompts own the exact JSON; the role prompts must not
    // restate it, or a wire change would leave them describing a stale one.
    for p in [REVIEW_MAIN_ROLE_PROMPT, REVIEW_REVIEWER_ROLE_PROMPT] {
        assert!(!p.contains("\"responses\""));
        assert!(!p.contains("\"verdict\""));
    }
    assert!(super::super::constants::REVIEW_MAIN_PROMPT.contains("\"responses\""));
}
