use super::*;
use mur_common::model::BillingMode;

fn ev(agent: &str, kind: StepEventKind, tokens: u64) -> StepEvent {
    StepEvent {
        id: "s".into(),
        agent: Some(agent.into()),
        kind,
        usage: TokenUsage {
            input: tokens,
            output: 0,
            model_ref: None,
            by_model: vec![],
        },
        error: None,
    }
}

/// A share answered by registry entry `key` (`model` = its provider name).
fn share(key: Option<&str>, model: Option<&str>, input: u64, output: u64) -> ModelShare {
    ModelShare {
        registry_key: key.map(str::to_string),
        model: model.map(str::to_string),
        input,
        output,
        cache_read: 0,
        cache_write: 0,
    }
}

/// A turn made of `shares`, as a current runtime reports it.
fn turn(shares: Vec<ModelShare>) -> TokenUsage {
    TokenUsage {
        input: shares.iter().map(|s| s.input).sum(),
        output: shares.iter().map(|s| s.output).sum(),
        model_ref: shares.last().and_then(|s| s.model.clone()),
        by_model: shares,
    }
}

fn model(billing: BillingMode, input: Option<f64>, output: Option<f64>) -> ModelEntry {
    ModelEntry {
        billing: Some(billing),
        input_cost_per_1k: input,
        output_cost_per_1k: output,
        ..Default::default()
    }
}

fn agent(bills: bool, model_ref: Option<&str>) -> AgentBilling {
    AgentBilling {
        bills,
        model_ref: model_ref.map(str::to_string),
    }
}

/// $5 cap, $1/1k fallback. `api` runs an unpriced metered model (so it bills
/// at the fallback), `local` and `sub` are not billed, `router` is the
/// fleet's planner on a metered model priced $1/1k in, $1/1k out.
fn budget() -> Arc<OneShotBudget> {
    let agents = HashMap::from([
        ("api".to_string(), agent(true, Some("unpriced"))),
        ("local".to_string(), agent(false, Some("ollama"))),
        ("sub".to_string(), agent(false, Some("codex"))),
        ("router".to_string(), agent(true, Some("router-model"))),
    ]);
    let models = BTreeMap::from([
        (
            "unpriced".to_string(),
            model(BillingMode::UsageBilled, None, None),
        ),
        ("ollama".to_string(), model(BillingMode::Local, None, None)),
        (
            "codex".to_string(),
            model(BillingMode::Subscription, None, None),
        ),
        (
            "router-model".to_string(),
            model(BillingMode::UsageBilled, Some(1.0), Some(1.0)),
        ),
    ]);
    Arc::new(OneShotBudget::new(
        5.0,
        1.0,
        "router".into(),
        agents,
        models,
    ))
}

#[test]
fn trips_on_actual_billable_spend_and_halts_the_run() {
    let b = budget();
    let stop = b.stop_signal(Arc::new(|| false));
    b.record(&ev("api", StepEventKind::Done, 3_000));
    assert!(!b.tripped() && !stop(), "$3 of $5 is under the cap");
    b.record(&ev("api", StepEventKind::Failed, 2_000));
    assert!(
        b.tripped() && stop(),
        "$5 reaches the cap and halts the DAG"
    );
    assert!(b.stop_reason().starts_with(BUDGET_STOP_REASON));
}

#[test]
fn local_and_subscription_members_never_count() {
    let b = budget();
    b.record(&ev("local", StepEventKind::Done, 1_000_000));
    b.record(&ev("sub", StepEventKind::Done, 1_000_000));
    assert_eq!(b.spent_usd(), 0.0);
    assert!(!b.tripped(), "tokens nobody pays for must not stop the run");
}

#[test]
fn an_unresolvable_agent_counts_as_metered() {
    let b = budget();
    b.record(&ev("stranger", StepEventKind::Done, 5_000));
    assert!(b.tripped());
}

#[test]
fn non_terminal_events_carry_no_spend() {
    let b = budget();
    b.record(&ev("api", StepEventKind::Started, 9_000));
    b.record(&ev("api", StepEventKind::Blocked, 9_000));
    assert_eq!(b.spent_usd(), 0.0);
}

#[test]
fn the_kill_switch_still_stops_an_untripped_budget() {
    let b = budget();
    let stop = b.stop_signal(Arc::new(|| true));
    assert!(
        stop() && !b.tripped(),
        "the stop is the operator's, not the cap's"
    );
}

/// End to end through the real executor: the cap trips when rank 0 reports
/// its spend, rank 1 never starts, and the run ends `Stopped` — the same
/// terminal path as `mur fleet stop` (#2036). Command steps report no usage,
/// so the observer stands in for a metered member's delegate turn.
#[tokio::test]
async fn reaching_the_cap_stops_the_dag_before_the_next_rank() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("rank1-ran");
    let step = |id: &str, deps: &[&str], cmd: String| ProcedureStep {
        id: Some(id.to_string()),
        command: Some(cmd),
        description: format!("{id} step"),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    };
    let proc = Procedure {
        variables: vec![],
        steps: vec![
            step("s0", &[], "true".into()),
            step("s1", &["s0"], format!("touch {}", marker.display())),
        ],
    };
    let b = budget();
    let feed = {
        let b = Arc::clone(&b);
        Arc::new(move |e: StepEvent| {
            if e.id == "s0" && e.kind == StepEventKind::Done {
                b.record(&ev("api", StepEventKind::Done, 6_000));
            }
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(feed),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(b.tripped());
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "no step may start past the cap");
}

/// Negative control: the same run on local members spends nothing and
/// finishes — the cap must not stop work nobody is billed for.
#[tokio::test]
async fn the_same_run_on_local_members_finishes() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s0".into()),
            command: Some("true".into()),
            description: "s0".into(),
            ..Default::default()
        }],
    };
    let b = budget();
    let feed = {
        let b = Arc::clone(&b);
        Arc::new(move |e: StepEvent| {
            if e.kind == StepEventKind::Done {
                b.record(&ev("local", StepEventKind::Done, 6_000));
            }
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(feed),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(!b.tripped());
    assert_eq!(out.status, PipelineStatus::Success);
}

/// Review of #2086: each turn is billed at its own model's input and output
/// rates. The old guard billed every token at the dearest output rate in the
/// whole registry, so cheap input tokens counted as expensive output ones.
#[test]
fn a_turn_is_priced_at_its_own_models_input_and_output_rates() {
    let agents = HashMap::from([("api".to_string(), agent(true, Some("cheap")))]);
    let models = BTreeMap::from([
        (
            "cheap".to_string(),
            model(BillingMode::UsageBilled, Some(0.001), Some(0.004)),
        ),
        // Never used by this fleet: it must not set the rate.
        (
            "opus".to_string(),
            model(BillingMode::UsageBilled, Some(0.015), Some(0.075)),
        ),
    ]);
    let b = OneShotBudget::new(1.0, 0.075, "router".into(), agents, models);
    b.record_turn(
        Some("api"),
        &turn(vec![share(Some("cheap"), None, 100_000, 10_000)]),
    );
    // 100k in at $0.001 + 10k out at $0.004 = $0.14, not 110k at $0.075.
    assert!((b.spent_usd() - 0.14).abs() < 1e-9, "{}", b.spent_usd());
    assert!(
        !b.tripped(),
        "an unrelated dear model must not trip the cap"
    );
}

/// The model the runtime says answered the turn beats the profile's: a
/// fallback chain can route a "local" agent to a metered model.
#[test]
fn the_reported_model_beats_the_profile_model() {
    let b = budget();
    b.record_turn(
        Some("local"),
        &turn(vec![share(Some("router-model"), None, 3_000, 2_000)]),
    );
    assert!((b.spent_usd() - 5.0).abs() < 1e-9);
    assert!(b.tripped());
}

/// Review of #2086: the router's planning turn is billable work of the run.
/// A metered router over local members must still be able to hit the cap.
#[test]
fn a_metered_router_counts_even_when_every_member_is_local() {
    let b = budget();
    b.record_router(&TokenUsage {
        input: 4_000,
        output: 1_000,
        model_ref: None,
        by_model: vec![],
    });
    assert!((b.spent_usd() - 5.0).abs() < 1e-9);
    assert!(b.tripped(), "the router alone reached the cap");
    b.record(&ev("local", StepEventKind::Done, 1_000_000));
    assert!((b.spent_usd() - 5.0).abs() < 1e-9, "members stay free");
}

/// End to end: the router's turn reaches the cap during planning, so the DAG
/// halts at its first step boundary — no member step starts at all.
#[tokio::test]
async fn a_router_that_reached_the_cap_stops_the_run_before_any_step() {
    use crate::executor::dag::{DagExecOptions, execute_dag};
    use mur_common::pipeline::PipelineStatus;
    use mur_common::skill::manifest::{Procedure, ProcedureStep};

    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("member-ran");
    let proc = Procedure {
        variables: vec![],
        steps: vec![ProcedureStep {
            id: Some("s0".into()),
            command: Some(format!("touch {}", marker.display())),
            description: "s0".into(),
            ..Default::default()
        }],
    };
    let b = budget();
    b.record_router(&TokenUsage {
        input: 5_000,
        output: 1_000,
        model_ref: None,
        by_model: vec![],
    });
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            stop: Some(b.stop_signal(Arc::new(|| false))),
            on_step: Some(b.observer()),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "no member step may start past the cap");
    assert!(b.stop_reason().starts_with(BUDGET_STOP_REASON));
}

/// Two aliases serving one provider name at different billing — the shape a
/// subscription agent with a metered API fallback has.
fn sonnet_budget() -> OneShotBudget {
    let sonnet = |billing| ModelEntry {
        model: "claude-sonnet-4".into(),
        ..model(billing, Some(1.0), Some(1.0))
    };
    let agents = HashMap::from([("sub".to_string(), agent(false, Some("sonnet-sub")))]);
    let models = BTreeMap::from([
        ("sonnet-sub".to_string(), sonnet(BillingMode::Subscription)),
        ("sonnet-api".to_string(), sonnet(BillingMode::UsageBilled)),
    ]);
    OneShotBudget::new(5.0, 1.0, "router".into(), agents, models)
}

/// Re-review of #2086: the runtime's registry key, not the provider name,
/// names the model. A subscription agent that fell back to the metered alias
/// is billed, even though both aliases report the same provider name.
#[test]
fn the_registry_key_decides_when_it_differs_from_the_provider_name() {
    let b = sonnet_budget();
    b.record_turn(
        Some("sub"),
        &turn(vec![share(
            Some("sonnet-api"),
            Some("claude-sonnet-4"),
            2_000,
            1_000,
        )]),
    );
    assert!((b.spent_usd() - 3.0).abs() < 1e-9, "{}", b.spent_usd());
    b.record_turn(
        Some("sub"),
        &turn(vec![share(
            Some("sonnet-sub"),
            Some("claude-sonnet-4"),
            9_000,
            9_000,
        )]),
    );
    assert!(
        (b.spent_usd() - 3.0).abs() < 1e-9,
        "the subscription alias stays free"
    );
}

/// Re-review of #2086: a provider name the guard cannot pin to one entry is
/// never given the profile's classification. Here the profile is a
/// subscription — substituting it would discard metered spend.
#[test]
fn an_ambiguous_provider_name_counts_as_metered_not_as_the_profile_model() {
    let b = sonnet_budget();
    // An older runtime: no split, no key, only the provider name.
    b.record_turn(
        Some("sub"),
        &TokenUsage {
            input: 4_000,
            output: 1_000,
            model_ref: Some("claude-sonnet-4".into()),
            by_model: vec![],
        },
    );
    assert!((b.spent_usd() - 5.0).abs() < 1e-9, "{}", b.spent_usd());
    assert!(b.tripped());
}

/// A provider name only one entry serves does identify it.
#[test]
fn a_provider_name_served_by_exactly_one_entry_resolves() {
    let agents = HashMap::from([("api".to_string(), agent(true, None))]);
    let models = BTreeMap::from([(
        "opus".to_string(),
        ModelEntry {
            model: "claude-opus-4".into(),
            ..model(BillingMode::UsageBilled, Some(0.01), Some(0.05))
        },
    )]);
    let b = OneShotBudget::new(5.0, 1.0, "router".into(), agents, models);
    b.record_turn(
        Some("api"),
        &turn(vec![share(None, Some("claude-opus-4"), 10_000, 2_000)]),
    );
    assert!((b.spent_usd() - 0.2).abs() < 1e-9, "{}", b.spent_usd());
}

/// Re-review of #2086: one delegated turn answered by two differently billed
/// models. The metered share counts at its own rate even though the turn's
/// last call was local — pricing the sum at the last model would discard it.
#[test]
fn a_turn_split_across_models_bills_each_share_at_its_own_model() {
    let b = budget();
    b.record_turn(
        Some("local"),
        &turn(vec![
            share(Some("router-model"), Some("gpt-x"), 3_000, 1_000),
            share(Some("ollama"), Some("qwen3"), 500_000, 50_000),
        ]),
    );
    assert!((b.spent_usd() - 4.0).abs() < 1e-9, "{}", b.spent_usd());
    b.record_turn(
        Some("local"),
        &turn(vec![
            share(Some("ollama"), Some("qwen3"), 1_000, 0),
            share(Some("router-model"), Some("gpt-x"), 500, 500),
        ]),
    );
    assert!((b.spent_usd() - 5.0).abs() < 1e-9, "{}", b.spent_usd());
    assert!(b.tripped());
}

/// `extract_usage` reads the runtime's split off the wire.
#[test]
fn the_runtime_split_survives_extraction() {
    let task = serde_json::json!({"usage": {
        "input_tokens": 3, "output_tokens": 2, "model_ref": "qwen3",
        "by_model": [
            {"registry_key": "router-model", "model": "gpt-x", "input_tokens": 2, "output_tokens": 1},
            {"registry_key": null, "model": "qwen3", "input_tokens": 1, "output_tokens": 1}
        ]
    }});
    let u = crate::executor::dag::extract_usage(&task);
    assert_eq!(
        u.shares(),
        vec![
            share(Some("router-model"), Some("gpt-x"), 2, 1),
            share(None, Some("qwen3"), 1, 1),
        ]
    );
}

/// The shipped agent template with `name` and `model_ref` swapped in (a
/// hand-trimmed profile fails to parse, and the billing lookup's `.ok()`
/// would turn that into "unknown" — metered — passing for the wrong reason).
/// Line-by-line so a CRLF checkout still matches.
fn write_agent(home: &std::path::Path, name: &str, model_ref: &str, chain: &[&str]) {
    let tmpl = std::fs::read_to_string(
        env!("CARGO_MANIFEST_DIR").to_string()
            + "/../mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml",
    )
    .expect("shipped agent template");
    let mut replaced = false;
    let chain = if chain.is_empty() {
        String::new()
    } else {
        format!("\nfallback_chain: [{}]", chain.join(", "))
    };
    let yaml: Vec<String> = tmpl
        .lines()
        .map(|line| {
            if !replaced && line == "name: mur" {
                replaced = true;
                format!("name: {name}\nmodel_ref: {model_ref}{chain}")
            } else {
                line.to_string()
            }
        })
        .collect();
    assert!(replaced, "template's `name: mur` line not found");
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), yaml.join("\n") + "\n").unwrap();
}

/// Re-review of #2086: every primary is free, one member falls back to a
/// metered API. The guard must still be armed — judged by primaries alone it
/// was `None`, and the fallback's spend had nobody to count it.
#[test]
fn a_cap_arms_the_guard_even_when_every_primary_is_free() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::write(
        home.join("models.yaml"),
        "schema_version: 1\nmodels:\n  \
         local:\n    provider: ollama\n    model: qwen3\n    billing: local\n  \
         api:\n    provider: openai\n    model: gpt-x\n    billing: usage_billed\n    \
         input_cost_per_1k: 1.0\n    output_cost_per_1k: 1.0\n",
    )
    .unwrap();
    write_agent(home, "router", "local", &[]);
    write_agent(home, "worker", "local", &["api"]);
    let fleet: mur_common::fleet::Fleet = serde_yaml_ng::from_str(
        "name: dev\ngoal: g\nrouter: router\nmembers: [worker]\nchannel_id: fleet-dev\n",
    )
    .unwrap();
    assert!(
        !super::super::billing::fleet_billing(home, &fleet).billable,
        "precondition: judged by primaries, this fleet cannot spend"
    );

    let b =
        OneShotBudget::for_fleet(home, &fleet, Some(5.0)).expect("a positive cap arms the guard");
    // The worker's local turns are free…
    b.record(&StepEvent {
        usage: turn(vec![share(Some("local"), Some("qwen3"), 900_000, 90_000)]),
        ..ev("worker", StepEventKind::Done, 0)
    });
    assert_eq!(b.spent_usd(), 0.0);
    // …its fallback to the metered API is not.
    b.record(&StepEvent {
        usage: turn(vec![share(Some("api"), Some("gpt-x"), 4_000, 1_000)]),
        ..ev("worker", StepEventKind::Done, 0)
    });
    assert!((b.spent_usd() - 5.0).abs() < 1e-9, "{}", b.spent_usd());
    assert!(b.tripped(), "the fallback's spend reached the cap");

    assert!(
        OneShotBudget::for_fleet(home, &fleet, None).is_none()
            && OneShotBudget::for_fleet(home, &fleet, Some(0.0)).is_none(),
        "no positive cap, no guard"
    );
}

/// #2083 P3: a share's cached prompt tokens are priced at the model's cache
/// rates, not the full input rate — and a model with no cache rate recorded
/// keeps the input rate, the upper bound.
#[test]
fn cached_prompt_tokens_are_priced_at_the_cache_rates() {
    let opus = |read, write| ModelEntry {
        model: "claude-opus-4".into(),
        cache_read_cost_per_1k: read,
        cache_write_cost_per_1k: write,
        ..model(BillingMode::UsageBilled, Some(0.005), Some(0.025))
    };
    let agents = HashMap::from([("api".to_string(), agent(true, None))]);
    // 100k prompt: 90k read from cache, 5k written, 5k uncached; 1k out.
    let cached_share = || ModelShare {
        cache_read: 90_000,
        cache_write: 5_000,
        ..share(Some("opus"), Some("claude-opus-4"), 100_000, 1_000)
    };

    let priced = OneShotBudget::new(
        100.0,
        1.0,
        "router".into(),
        agents.clone(),
        BTreeMap::from([("opus".to_string(), opus(Some(0.0005), Some(0.00625)))]),
    );
    priced.record_turn(Some("api"), &turn(vec![cached_share()]));
    // 5k*0.005 + 90k*0.0005 + 5k*0.00625 + 1k*0.025 (per 1k)
    let want = 0.025 + 0.045 + 0.03125 + 0.025;
    assert!(
        (priced.spent_usd() - want).abs() < 1e-9,
        "{}",
        priced.spent_usd()
    );

    let unpriced = OneShotBudget::new(
        100.0,
        1.0,
        "router".into(),
        agents,
        BTreeMap::from([("opus".to_string(), opus(None, None))]),
    );
    unpriced.record_turn(Some("api"), &turn(vec![cached_share()]));
    let full = 100.0 * 0.005 + 0.025;
    assert!(
        (unpriced.spent_usd() - full).abs() < 1e-9,
        "{}",
        unpriced.spent_usd()
    );
}
