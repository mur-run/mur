//! Write-back tests for `finish` (plan 5.6 CLI half + 5.7).

use super::*;
use mur_browser::{
    heal::{BudgetExceeded, HealEvent, HealStatus},
    recorder::{Action, Mode, Step},
    replay::{StepOutcome, StepStatus},
};

const RUN: &str = "checkout";

fn step(n: u32, locators: &[&str]) -> Step {
    Step {
        step: n,
        intent: format!("step {n}"),
        intent_auto: false,
        action: Action::Click,
        value: None,
        locators: locators.iter().map(|l| l.to_string()).collect(),
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

fn recorded() -> Run {
    Run {
        name: RUN.into(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        steps: vec![
            step(1, &["role:button[name=\"Old\"]"]),
            step(2, &["role:link[name=\"Next\"]"]),
            step(3, &["role:button[name=\"Pay\"]"]),
        ],
    }
}

fn event(step: u32, status: HealStatus) -> HealEvent {
    HealEvent {
        step,
        from: vec!["role:button[name=\"Old\"]".into()],
        to: vec!["role:button[name=\"New\"]".into(), "text:New".into()],
        node: "button \"New\"".into(),
        score: 0.9,
        reason: "no locator matched".into(),
        status,
    }
}

fn report(heals: Vec<HealEvent>, failed: bool) -> ReplayReport {
    let steps = (1..=3)
        .map(|n| StepOutcome {
            step: n,
            status: if failed && n == 3 {
                StepStatus::Failed
            } else if heals.iter().any(|h| h.step == n) {
                StepStatus::Healed
            } else {
                StepStatus::Passed
            },
            locator_used: None,
            message: None,
        })
        .collect::<Vec<_>>();
    let count = |s: StepStatus| steps.iter().filter(|o| o.status == s).count() as u32;
    ReplayReport {
        run: RUN.into(),
        total: 3,
        passed: count(StepStatus::Passed),
        failed: count(StepStatus::Failed),
        healed: count(StepStatus::Healed),
        steps,
        heals,
        budget_exceeded: None,
        written_back: 0,
    }
}

/// A MUR home holding `recorded()`; returns (tempdir, actions.yaml bytes).
fn home() -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    let path = paths::run_actions(temp.path(), RUN);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let yaml = to_yaml(&recorded()).unwrap();
    fs::write(&path, &yaml).unwrap();
    (temp, yaml)
}

fn actions(home: &Path) -> String {
    fs::read_to_string(paths::run_actions(home, RUN)).unwrap()
}

fn saved_report(home: &Path) -> ReplayReport {
    serde_yaml::from_str(&fs::read_to_string(paths::run_report(home, RUN)).unwrap()).unwrap()
}

#[test]
fn verified_heal_is_prepended_whole_and_counted() {
    let (temp, _) = home();
    let heals = vec![event(1, HealStatus::Verified)];
    finish(temp.path(), RUN, recorded(), report(heals, false)).unwrap();

    let run = from_yaml(&actions(temp.path())).unwrap();
    assert_eq!(
        run.steps[0].locators,
        vec![
            "role:button[name=\"New\"]",
            "text:New",
            "role:button[name=\"Old\"]"
        ]
    );
    assert!(run.steps[0].healed);
    assert_eq!(run.steps[0].last_hit, 0);
    assert_eq!(run.steps[1], recorded().steps[1], "other steps untouched");
    let saved = saved_report(temp.path());
    assert_eq!(saved.written_back, 1);
    assert!(
        saved.summary().contains("written back 1"),
        "{}",
        saved.summary()
    );
}

#[test]
fn only_verified_heals_are_written_back() {
    let (temp, _) = home();
    let heals = vec![
        event(1, HealStatus::Verified),
        event(3, HealStatus::Unverified),
    ];
    finish(temp.path(), RUN, recorded(), report(heals, false)).unwrap();

    let run = from_yaml(&actions(temp.path())).unwrap();
    assert!(run.steps[0].healed);
    assert_eq!(run.steps[2], recorded().steps[2], "unverified not applied");
    assert_eq!(saved_report(temp.path()).written_back, 1);
}

#[test]
fn unverified_only_leaves_actions_yaml_byte_identical() {
    let (temp, before) = home();
    let heals = vec![event(3, HealStatus::Unverified)];
    finish(temp.path(), RUN, recorded(), report(heals, false)).unwrap();
    assert_eq!(actions(temp.path()), before);
    assert_eq!(saved_report(temp.path()).written_back, 0);
}

#[test]
fn any_failed_step_blocks_write_back_but_keeps_the_report() {
    let (temp, before) = home();
    let heals = vec![event(1, HealStatus::Verified)];
    let err = finish(temp.path(), RUN, recorded(), report(heals, true)).unwrap_err();

    assert!(err.to_string().contains("failed"), "{err}");
    assert_eq!(actions(temp.path()), before);
    let saved = saved_report(temp.path());
    assert_eq!(saved.written_back, 0);
    assert_eq!(saved.heals.len(), 1);
}

#[test]
fn over_budget_writes_report_but_not_actions_and_exits_non_zero() {
    let (temp, before) = home();
    let mut rep = report(vec![event(1, HealStatus::Verified)], false);
    let over = BudgetExceeded {
        healed: 2,
        total: 4,
        allowed: 1,
        max_ratio: 0.2,
    };
    rep.budget_exceeded = Some(over);
    let err = finish(temp.path(), RUN, recorded(), rep).unwrap_err();

    assert!(err.to_string().contains("heal rate too high"), "{err}");
    assert_eq!(actions(temp.path()), before, "actions.yaml unchanged");
    let saved = saved_report(temp.path());
    assert_eq!(saved.written_back, 0, "Verified but over budget → 0");
    assert_eq!(saved.budget_exceeded, Some(over));
    assert_eq!(saved.heals.len(), 1);
}

#[test]
fn failed_write_back_still_writes_report_and_errors() {
    let (temp, _) = home();
    // A directory where the rename target should be makes persist fail.
    let target = paths::run_actions(temp.path(), RUN);
    fs::remove_file(&target).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep"), "x").unwrap();

    let heals = vec![event(1, HealStatus::Verified)];
    let err = finish(temp.path(), RUN, recorded(), report(heals, false)).unwrap_err();

    assert!(
        format!("{err:#}").contains("writing verified heals back"),
        "{err:#}"
    );
    assert_eq!(saved_report(temp.path()).written_back, 0);
    let leftovers: Vec<_> = fs::read_dir(target.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp file cleaned up");
}

#[test]
fn heal_ratio_parser_bounds() {
    assert_eq!(parse_heal_ratio("0"), Ok(0.0));
    assert_eq!(parse_heal_ratio("1"), Ok(1.0));
    assert_eq!(parse_heal_ratio("0.2"), Ok(0.2));
    assert!(parse_heal_ratio("1.01").is_err());
    assert!(parse_heal_ratio("-0.1").is_err());
    assert!(parse_heal_ratio("NaN").is_err());
    assert!(parse_heal_ratio("abc").is_err());
}
