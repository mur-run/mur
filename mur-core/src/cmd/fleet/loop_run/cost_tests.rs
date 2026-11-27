use super::*;

#[test]
fn parse_duration_units_and_bare_seconds() {
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
    assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
    assert_eq!(parse_duration(" 2h "), Some(Duration::from_secs(7200)));
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("2y"), None);
    assert_eq!(parse_duration("h"), None);
}

#[test]
fn is_converged_detects_done_word_only() {
    assert!(is_converged("DONE"));
    assert!(is_converged("done"));
    assert!(is_converged("The goal is DONE."));
    assert!(is_converged("Done — all issues closed"));
    assert!(!is_converged("CONTINUE"));
    assert!(!is_converged("not done yet")); // negation guard → continue
    assert!(!is_converged("done but CONTINUE")); // continue token wins
    assert!(!is_converged("undone")); // substring, not a token → false
    assert!(!is_converged(""));
}

/// The regression this constant exists for. A measured deep-research run —
/// three members on a $0.003/1k model — cost $21.52, and the old estimate
/// (8000 tokens per member) projected about $0.07, so a $10 ceiling waved
/// it through and reported the overrun only afterwards.
///
/// The projection must now exceed a $10 ceiling for that shape, so the loop
/// refuses BEFORE spending rather than apologising after.
#[test]
fn a_three_member_iteration_no_longer_projects_as_pocket_change() {
    let projected = estimate_iteration_cost_usd(3, 0.003);
    assert!(
        projected > 10.0,
        "three members must not project under a $10 ceiling: ${projected}"
    );
    // …and not so dear that the shipped default refuses a normal run.
    assert!(
        projected < crate::cmd::deep_research::setup::DEFAULT_RUN_BUDGET_USD,
        "the default budget must still admit one iteration: ${projected}"
    );
    assert!(budget_exceeded(0.0, projected, Some(10.0)));
    assert!(!budget_exceeded(
        0.0,
        projected,
        Some(crate::cmd::deep_research::setup::DEFAULT_RUN_BUDGET_USD)
    ));
}

#[test]
fn estimate_and_budget_guard() {
    // Expressed through the constant, not a literal: this pins the FORMULA
    // (linear in members and in price), and the constant is documented as
    // something to retune once there is more than one measured run.
    let per_member_k = EST_TOKENS_PER_MEMBER_ITERATION as f64 / 1000.0;
    assert!((estimate_iteration_cost_usd(2, 0.05) - 2.0 * per_member_k * 0.05).abs() < 1e-9);
    // Linear in both arguments.
    assert!(
        (estimate_iteration_cost_usd(4, 0.05) - 2.0 * estimate_iteration_cost_usd(2, 0.05)).abs()
            < 1e-9
    );
    assert!(
        (estimate_iteration_cost_usd(2, 0.10) - 2.0 * estimate_iteration_cost_usd(2, 0.05)).abs()
            < 1e-9
    );
    // no budget / zero budget → never blocks
    assert!(!budget_exceeded(100.0, 5.0, None));
    assert!(!budget_exceeded(100.0, 5.0, Some(0.0)));
    // stops BEFORE exceeding
    assert!(budget_exceeded(0.9, 0.2, Some(1.0))); // 1.1 > 1.0
    assert!(!budget_exceeded(0.7, 0.2, Some(1.0))); // 0.9 <= 1.0
    // even iteration 0 unaffordable → blocked immediately
    assert!(budget_exceeded(0.0, 2.0, Some(1.0)));
}

#[test]
fn real_iteration_cost_from_tokens() {
    // 10_000 tokens × $0.05/1k = $0.50
    assert!((iteration_cost_usd(10_000, 0.05) - 0.5).abs() < 1e-9);
    // zero tokens → zero (caller falls back to the projection to avoid
    // under-counting; the helper itself is exact).
    assert_eq!(iteration_cost_usd(0, 0.05), 0.0);
    // real cost is typically well under the 8000-tok/member projection:
    // a 1200-token iteration costs far less than the 1-member projection.
    assert!(iteration_cost_usd(1200, 0.05) < estimate_iteration_cost_usd(1, 0.05));
}

/// The mapping is worthless if it emits a name the calibration does not
/// recognise, so it is checked against the real constant rather than
/// against a copy of the strings.
#[test]
fn every_mapped_stop_reason_is_one_the_calibration_scores() {
    use crate::executor::triage_calibration::GUARD_STOPS;
    for stop in [
        LoopStop::Converged,
        LoopStop::MaxIterations,
        LoopStop::Deadline,
        LoopStop::Stuck,
        LoopStop::Budget,
        LoopStop::Stopped,
        LoopStop::CommanderKilled,
        LoopStop::QueueDrained,
        LoopStop::AwaitingApproval,
    ] {
        if let Some(r) = calibration_stop_reason(stop) {
            assert!(
                GUARD_STOPS.contains(&r),
                "{stop:?} maps to `{r}`, which GUARD_STOPS does not contain —                      the calibration would silently read it as no overrun"
            );
        }
    }
}

/// The rename trap: the loop says `max-iterations`, the runtime says
/// `iteration_ceiling`. Passing the human label straight through would
/// lose every runaway triage was meant to predict.
#[test]
fn a_runaway_maps_to_the_runtimes_name_not_the_human_label() {
    assert_eq!(
        calibration_stop_reason(LoopStop::MaxIterations),
        Some("iteration_ceiling")
    );
    assert_ne!(
        calibration_stop_reason(LoopStop::MaxIterations),
        Some(outcome_label(LoopStop::MaxIterations)),
        "the display label must not be what gets scored"
    );
}

/// A run that ended well must never be scored as an overrun — that would
/// blame triage for letting through work that was fine.
#[test]
fn a_clean_finish_is_not_an_overrun() {
    for stop in [
        LoopStop::Converged,
        LoopStop::QueueDrained,
        LoopStop::Stopped,
        LoopStop::CommanderKilled,
        LoopStop::AwaitingApproval,
    ] {
        assert_eq!(calibration_stop_reason(stop), None, "{stop:?}");
    }
}

/// Budget is detected from spend vs cap, not from a guard name the
/// runtime never emits.
#[test]
fn a_budget_stop_is_left_to_the_spend_comparison() {
    assert_eq!(calibration_stop_reason(LoopStop::Budget), None);
}

#[test]
fn fleet_price_per_1k_env_then_default() {
    let tmp = tempfile::tempdir().unwrap();
    // env override wins (nextest isolates per-process, so this is safe)
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_FLEET_COST_PER_1K", "0.123");
    let (rate, src) = fleet_price_per_1k(tmp.path());
    assert!((rate - 0.123).abs() < 1e-9);
    assert_eq!(src, GuardRate::Env);
    // no env + no models.yaml → documented default, and it says so
    envg.unset_var("MUR_FLEET_COST_PER_1K");
    let (rate, src) = fleet_price_per_1k(tmp.path());
    assert!((rate - DEFAULT_PRICE_PER_1K).abs() < 1e-9);
    assert_eq!(src, GuardRate::Default);
}

/// The budget guard bills one flat rate against an iteration's whole token
/// count, while the cost report prices each component separately. If the
/// guard's rate ever dips below what the report would charge for a token of
/// any model in the registry, the fleet overspends its budget by exactly
/// that ratio — and nothing on screen says so. Both bugs this branch fixed
/// were that failure: a rate in the wrong field made the guard 5x cheap,
/// and a stripped registry would have made it 57x cheap.
#[test]
fn guard_rate_is_never_below_what_the_report_charges() {
    use crate::cmd::conversations_cost_report::resolve_rates;
    use mur_common::model::{ModelEntry, ModelRegistry};

    let entry = |input: f64, output: f64| ModelEntry {
        provider: "anthropic".into(),
        model: "m".into(),
        input_cost_per_1k: Some(input),
        output_cost_per_1k: Some(output),
        ..Default::default()
    };
    let mut reg = ModelRegistry::default();
    for (alias, wire, i, o) in [
        ("opus", "claude-opus-5", 0.005, 0.025),
        ("sonnet", "claude-sonnet-5", 0.003, 0.015),
        ("haiku", "claude-haiku-4-5", 0.001, 0.005),
        ("ds", "deepseek-v4-pro", 0.000435, 0.00087),
    ] {
        reg.models.insert(
            alias.into(),
            ModelEntry {
                model: wire.into(),
                ..entry(i, o)
            },
        );
    }

    let guard = dearest_output_rate(&reg).expect("registry is priced");
    for e in reg.models.values() {
        // Per 1k, against the report's per-1M rates.
        let ((r_in, r_out, r_cw, r_cr), _) =
            resolve_rates(&e.model, Some(&reg)).expect("report prices it");
        for (label, report_rate) in [
            ("input", r_in),
            ("output", r_out),
            ("cache_write", r_cw),
            ("cache_read", r_cr),
        ] {
            assert!(
                guard >= report_rate / 1000.0,
                "guard {guard}/1k is under the {label} rate the report charges for {} \
                     ({}/1k) — a fleet on that model overspends its budget",
                e.model,
                report_rate / 1000.0
            );
        }
    }
}

/// A registry that prices nothing must not collapse the ceiling to zero —
/// the documented default is deliberately dearer than any real model.
#[test]
fn unpriced_registry_yields_no_ceiling_rather_than_a_free_one() {
    use mur_common::model::{ModelEntry, ModelRegistry};
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "local".into(),
        ModelEntry {
            provider: "openai".into(),
            model: "Qwen3.5-4B-MLX-4bit".into(),
            ..Default::default()
        },
    );
    assert_eq!(dearest_output_rate(&reg), None);
    // Const item: editing the default below a real model's rate fails the
    // BUILD, not just this test.
    const _: () = assert!(DEFAULT_PRICE_PER_1K > 0.025);
}

/// A metered member is priced at its own rate, and a subscription member adds
/// nothing, so a mixed fleet no longer projects every seat at the dearest rate.
#[test]
fn estimate_sums_per_member_rates_not_the_fleet_maximum() {
    let cheap = estimate_iteration_cost_usd(1, 0.0006);
    let dear = estimate_iteration_cost_usd(1, 0.025);
    let mixed = cheap + dear;
    assert!(mixed < estimate_iteration_cost_usd(2, 0.025));
}

/// Plant a profile whose `model_ref` names a `models.yaml` entry. Uses the
/// shipped template so the profile parses (a trimmed one would read as
/// "unknown", i.e. metered, passing for the wrong reason).
fn plant_member(home: &Path, name: &str, model_ref: &str) {
    let tmpl = std::fs::read_to_string(
        env!("CARGO_MANIFEST_DIR").to_string()
            + "/../mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml",
    )
    .expect("shipped agent template");
    let body: Vec<String> = tmpl
        .lines()
        .map(|l| {
            if l == "name: mur" {
                format!("name: {name}\nmodel_ref: {model_ref}")
            } else {
                l.to_string()
            }
        })
        .collect();
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), body.join("\n") + "\n").unwrap();
}

/// `member_projection` reads each member's `model_ref` → `models.yaml` rate,
/// skips subscription seats, falls back for an unpriced model, and lets the
/// env override pin one rate for everyone.
#[test]
fn member_projection_reads_models_yaml_per_member() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::write(
        home.join("models.yaml"),
        "schema_version: 1\nmodels:\n  \
         cheap:\n    provider: openai\n    model: c\n    billing: usage_billed\n    \
         input_cost_per_1k: 0.0001\n    output_cost_per_1k: 0.0006\n  \
         sub:\n    provider: openai\n    model: s\n    billing: subscription\n    \
         input_cost_per_1k: 0.005\n    output_cost_per_1k: 0.025\n  \
         unpriced:\n    provider: openai\n    model: u\n    billing: usage_billed\n",
    )
    .unwrap();
    for (a, m) in [("a", "cheap"), ("b", "sub"), ("c", "unpriced")] {
        plant_member(home, a, m);
    }
    let fleet: Fleet =
        serde_yaml_ng::from_str("name: dev\ngoal: g\nmembers: [a, b, c]\nchannel_id: fleet-dev\n")
            .unwrap();
    let fallback = 0.01;
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var("MUR_FLEET_COST_PER_1K");

    let got = preflight::member_projection(home, &fleet, fallback);
    // a: own rate; b: subscription → 0; c: no rate → fallback.
    let want = estimate_iteration_cost_usd(1, 0.0006) + estimate_iteration_cost_usd(1, fallback);
    assert!((got - want).abs() < 1e-9, "got {got}, want {want}");

    envg.set_var("MUR_FLEET_COST_PER_1K", "0.5");
    let pinned = preflight::member_projection(home, &fleet, 0.5);
    let want = 2.0 * estimate_iteration_cost_usd(1, 0.5);
    assert!((pinned - want).abs() < 1e-9, "got {pinned}, want {want}");
}
