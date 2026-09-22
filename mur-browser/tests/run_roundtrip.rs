//! Fixture-backed lock on the `Action` ↔ tool-name mapping (Task 0).
//!
//! `run-roundtrip.yaml` covers all 9 `Action` variants. This is the
//! single guard the rest of the replay work (Task 3/4/5/6/7) builds on:
//! if `recorder::call_for` or `Action::tool_name` ever drift from what
//! `RecordHook::action_for` / `value_for` expect, this test — not a hand
//! read of the source — is what catches it.

use mur_browser::recorder::{self, Action};

const FIXTURE: &str = include_str!("fixtures/run-roundtrip.yaml");

fn load() -> recorder::Run {
    recorder::from_yaml(FIXTURE).expect("fixture must parse as a valid Run")
}

#[test]
fn fixture_covers_all_nine_action_variants() {
    let run = load();
    let mut seen: Vec<Action> = run.steps.iter().map(|s| s.action).collect();
    seen.sort_by_key(|a| *a as u8);
    seen.dedup();
    assert_eq!(
        seen.len(),
        9,
        "fixture must cover every Action variant exactly once, got {seen:?}"
    );
}

#[test]
fn call_for_round_trips_every_step_tool_name() {
    let run = load();
    for step in &run.steps {
        let (tool_name, args) = recorder::call_for(step)
            .unwrap_or_else(|e| panic!("call_for(step {}): {e}", step.step));

        assert_eq!(
            tool_name,
            step.action.tool_name(),
            "step {} tool_name mismatch",
            step.step
        );

        // Goto must never carry a locator; every other action must.
        let has_locator = args.get("ref").is_some() || args.get("element").is_some();
        assert_eq!(
            has_locator,
            step.action.needs_locator(),
            "step {} ({:?}): locator presence {has_locator} != needs_locator() {}",
            step.step,
            step.action,
            step.action.needs_locator(),
        );

        // Value key names are pinned to the same contract recorder.rs's
        // value_for reads: Goto→url, Fill→text, Select→values, Press→key,
        // AssertText/AssertValue→text.
        let expected_key = match step.action {
            Action::Goto => Some("url"),
            Action::Fill => Some("text"),
            Action::Select => Some("values"),
            Action::Press => Some("key"),
            Action::AssertText | Action::AssertValue => Some("text"),
            Action::Click | Action::Hover | Action::AssertVisible => None,
        };
        match expected_key {
            Some(key) => assert_eq!(
                args.get(key).and_then(serde_json::Value::as_str),
                step.value.as_deref(),
                "step {} ({:?}): arguments[{key:?}] must equal the recorded value",
                step.step,
                step.action,
            ),
            None => assert!(
                step.value.is_none(),
                "step {} ({:?}) is not expected to carry a value in this fixture",
                step.step,
                step.action,
            ),
        }
    }
}

#[test]
fn fixture_is_stable_yaml() {
    // Round-tripping the parsed Run back to YAML and re-parsing must be a
    // no-op, so the fixture can't silently drift from what `to_yaml` emits
    // elsewhere in the crate.
    let run = load();
    let reserialized = recorder::to_yaml(&run).unwrap();
    let reparsed = recorder::from_yaml(&reserialized).unwrap();
    assert_eq!(run, reparsed);
}
