//! Pre-dispatch triage: before a fan-out spends a concurrency slot, a
//! deadline and real dollars, decide whether the task is the right SIZE for
//! the budget it is about to be given — and if it is not, say so while the
//! cheapest thing that has happened is one prompt.
//!
//! Deliberately NOT a turn estimator. Turns stopped being a boundary at 2.79
//! (`cmd/limits.rs:84`: "IGNORED since 2.79 — remove it; the bounds are
//! limits: deadline / stuck / cost_usd"), so a component that predicted "this
//! needs 25 turns" would be predicting a number nothing enforces. What this
//! predicts instead is shape: too big, too vague, too entangled, or fine.
//!
//! Three properties hold regardless of what the model says:
//!
//! 1. **The verdict cannot widen a limit.** `TriageOutcome` echoes the
//!    `ResolvedLimits` it was handed and owns no way to express a different
//!    one. A model that asks for more budget gets a decision, not a raise.
//! 2. **An unavailable model never blocks dispatch.** Timeout, transport
//!    error, unparseable JSON — all degrade to `Decision::Proceed` with the
//!    reason recorded. Making the triage model a hard dependency would mean
//!    killing it is how you stop the fleet.
//! 3. **Nothing is asked of the model unless a rule asked first.** The
//!    prefilter is pure and free; most jobs never reach the LLM at all.
//!
//! Calibration (logging verdict against the run's actual `cost_usd` /
//! terminal state) is NOT in this module yet — see the open item. Until it
//! exists, treat the thresholds here as declared guesses, not measurements.

use std::time::Duration;

use mur_common::limits::ResolvedLimits;

/// Coarse size band. Deliberately four buckets, not a number: the evidence a
/// model has at triage time does not support more precision than this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Complexity {
    S,
    M,
    L,
    Xl,
}

/// What the model recommends. Advisory — `decide` is what binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    Proceed,
    Split,
    AskHuman,
    Reject,
}

/// The structured verdict the triage model must return. Free prose is not
/// accepted: a verdict that cannot be parsed is treated as no verdict.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TriageVerdict {
    pub complexity: Complexity,
    /// 0-5. How much of the task is underspecified.
    pub ambiguity: u8,
    /// 0-5. How much of it reaches into code it does not own.
    pub dependency_risk: u8,
    pub recommended_action: RecommendedAction,
    #[serde(default)]
    pub proposed_splits: Vec<String>,
    /// 0.0-1.0. Below the floor, the model does not get to decide.
    pub confidence: f64,
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// What the dispatcher actually does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Proceed,
    Split,
    AskHuman,
    Reject,
}

/// Why the LLM was consulted, or why it was not.
#[derive(Debug, Clone, PartialEq)]
pub enum Prefilter {
    /// No rule fired — dispatch as-is, no model call, no cost.
    Skip,
    /// These rules fired; the task is worth one prompt.
    Consult(Vec<&'static str>),
}

/// Why a decision came out the way it did — the half that makes a triage
/// worth having run.
#[derive(Debug, Clone, PartialEq)]
pub enum Basis {
    /// No rule fired; the model was never asked.
    NoRuleFired,
    /// The model answered and `decide` bound its verdict.
    Verdict(TriageVerdict),
    /// The model was asked and could not answer. Carries the failure.
    Degraded(String),
}

/// The result of triage. Note what is absent: any field that could change a
/// limit. `limits` is the input, echoed for the caller's audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageOutcome {
    pub decision: Decision,
    pub basis: Basis,
    pub prefilter: Prefilter,
    pub limits: ResolvedLimits,
}

/// Below this confidence the model's recommendation is not binding and the
/// task goes to a human instead. A guess that knows it is a guess is not a
/// basis for spending a budget unattended.
pub const CONFIDENCE_FLOOR: f64 = 0.6;

/// A description shorter than this, paired with a sweeping verb, is the
/// classic underspecified-but-enormous request.
const TERSE_CHARS: usize = 120;

/// Budget under which even a modest task deserves a second look.
const THIN_BUDGET_USD: f64 = 0.50;

/// Verbs that describe work with no natural edge.
const SWEEPING: &[&str] = &[
    "refactor", "rewrite", "migrate", "redesign", "overhaul", "port ", "重構", "重寫", "遷移",
    "全部", "整個",
];

/// Phrases that hand the scope back to the agent to invent.
const VAGUE: &[&str] = &[
    "etc",
    "and so on",
    "as needed",
    "whatever",
    "and more",
    "之類",
    "等等",
    "自行",
];

/// Pure, free, and runs on every job. Decides only whether the far more
/// expensive thing is worth doing.
pub fn prefilter(description: &str, limits: &ResolvedLimits) -> Prefilter {
    let lower = description.to_lowercase();
    let mut fired: Vec<&'static str> = Vec::new();

    let sweeping = SWEEPING.iter().any(|v| lower.contains(v));
    if sweeping && description.chars().count() < TERSE_CHARS {
        fired.push("terse description with an unbounded verb");
    }
    if VAGUE.iter().any(|v| lower.contains(v)) {
        fired.push("scope handed back to the agent");
    }
    if distinct_crates(description) > 1 {
        fired.push("spans more than one crate");
    }
    if let Some(c) = limits.cost_usd.value
        && c < THIN_BUDGET_USD
    {
        fired.push("budget is thin");
    }
    if let Some(d) = limits.deadline.value
        && d < Duration::from_secs(5 * 60)
    {
        fired.push("deadline is short");
    }

    if fired.is_empty() {
        Prefilter::Skip
    } else {
        Prefilter::Consult(fired)
    }
}

/// Count distinct `mur-*` crate names named in the text — the cheapest
/// available proxy for blast radius in this workspace.
fn distinct_crates(description: &str) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for tok in description.split(|c: char| !(c.is_alphanumeric() || c == '-')) {
        let t = tok.trim_end_matches('/');
        if t.starts_with("mur-") && t.len() > 4 && !seen.contains(&t) {
            seen.push(t);
        }
    }
    seen.len()
}

/// Bind a verdict to a decision. The model recommends; this function is what
/// the dispatcher obeys, and it overrides the model in two directions.
pub fn decide(v: &TriageVerdict) -> Decision {
    // A model that is unsure does not get to spend the budget, whichever way
    // it leaned — including toward `Reject`, which is just as destructive to
    // get wrong as a runaway.
    if v.confidence < CONFIDENCE_FLOOR {
        return Decision::AskHuman;
    }
    match v.recommended_action {
        // XL never proceeds on the model's say-so. If it knows how to cut it
        // up, cut it up; if it does not, that is a human's call.
        RecommendedAction::Proceed if v.complexity == Complexity::Xl => {
            if v.proposed_splits.is_empty() {
                Decision::AskHuman
            } else {
                Decision::Split
            }
        }
        RecommendedAction::Proceed => Decision::Proceed,
        // "Split" with nothing to split into is not an instruction.
        RecommendedAction::Split => {
            if v.proposed_splits.is_empty() {
                Decision::AskHuman
            } else {
                Decision::Split
            }
        }
        RecommendedAction::AskHuman => Decision::AskHuman,
        RecommendedAction::Reject => Decision::Reject,
    }
}

/// The system prompt. States the 2.79 boundary out loud so the model does not
/// reach for the turn count that nothing enforces.
pub const TRIAGE_SYSTEM: &str = "\
You are a pre-dispatch triage step. Judge whether a task fits the budget it \
is about to be given. Do NOT estimate turns or iterations — they bound \
nothing. The only bounds are a wall-clock deadline, a stuck timeout and a \
dollar cap. Reply with ONE JSON object and no prose: \
{\"complexity\":\"S|M|L|XL\",\"ambiguity\":0-5,\"dependency_risk\":0-5,\
\"recommended_action\":\"proceed|split|ask_human|reject\",\
\"proposed_splits\":[\"...\"],\"confidence\":0.0-1.0,\"reasons\":[\"...\"]}";

/// Tolerate a model that wraps its JSON in prose or a fenced block. Anything
/// beyond that is a failed verdict, not something to guess at.
pub fn parse_verdict(raw: &str) -> Result<TriageVerdict, String> {
    let start = raw
        .find('{')
        .ok_or_else(|| "no JSON object in reply".to_string())?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| "no JSON object in reply".to_string())?;
    if end <= start {
        return Err("no JSON object in reply".into());
    }
    let v: TriageVerdict =
        serde_json::from_str(&raw[start..=end]).map_err(|e| format!("unparseable verdict: {e}"))?;
    if !(0.0..=1.0).contains(&v.confidence) {
        return Err(format!("confidence out of range: {}", v.confidence));
    }
    if v.ambiguity > 5 || v.dependency_risk > 5 {
        return Err("ambiguity/dependency_risk out of range".into());
    }
    Ok(v)
}

/// Run triage for one task. `ask` is the model call, injected so this is
/// testable without a network and so the caller owns which model pays.
///
/// Never returns `Err`: every failure path degrades to `Proceed`, because a
/// triage step that can block dispatch is a new way to take the fleet down.
pub async fn triage<F, Fut>(
    description: &str,
    limits: &ResolvedLimits,
    timeout: Duration,
    ask: F,
) -> TriageOutcome
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let pre = prefilter(description, limits);
    let reasons = match &pre {
        Prefilter::Skip => {
            return TriageOutcome {
                decision: Decision::Proceed,
                basis: Basis::NoRuleFired,
                prefilter: pre,
                limits: limits.clone(),
            };
        }
        Prefilter::Consult(r) => r.clone(),
    };

    let prompt = format!(
        "Task:\n{description}\n\nRules that flagged it: {}\n\nBudget: {}",
        reasons.join(", "),
        describe_budget(limits)
    );

    let degraded = |msg: String| TriageOutcome {
        decision: Decision::Proceed,
        basis: Basis::Degraded(msg),
        prefilter: pre.clone(),
        limits: limits.clone(),
    };

    let raw = match tokio::time::timeout(timeout, ask(prompt)).await {
        Err(_) => return degraded(format!("triage timed out after {timeout:?}")),
        Ok(Err(e)) => return degraded(format!("triage model unavailable: {e}")),
        Ok(Ok(r)) => r,
    };

    match parse_verdict(&raw) {
        Err(e) => degraded(e),
        Ok(v) => TriageOutcome {
            decision: decide(&v),
            basis: Basis::Verdict(v),
            prefilter: pre,
            limits: limits.clone(),
        },
    }
}

fn describe_budget(l: &ResolvedLimits) -> String {
    let d = l
        .deadline
        .value
        .map(|d| format!("{}s", d.as_secs()))
        .unwrap_or_else(|| "none".into());
    let c = l
        .cost_usd
        .value
        .map(|c| format!("${c:.2}"))
        .unwrap_or_else(|| "unmetered".into());
    format!("deadline {d}, cost cap {c}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::limits::{Resolved, Source, Stuck};

    fn limits(deadline_secs: Option<u64>, cost: Option<f64>) -> ResolvedLimits {
        ResolvedLimits {
            deadline: Resolved {
                value: deadline_secs.map(Duration::from_secs),
                source: Source::BuiltIn,
            },
            stuck: Resolved {
                value: Stuck::After(Duration::from_secs(600)),
                source: Source::BuiltIn,
            },
            cost_usd: Resolved {
                value: cost,
                source: Source::BuiltIn,
            },
        }
    }

    fn verdict(c: Complexity, a: RecommendedAction, conf: f64) -> TriageVerdict {
        TriageVerdict {
            complexity: c,
            ambiguity: 2,
            dependency_risk: 2,
            recommended_action: a,
            proposed_splits: vec![],
            confidence: conf,
            reasons: vec![],
        }
    }

    // ---- prefilter: the free half ----

    #[test]
    fn an_ordinary_bounded_task_never_reaches_the_model() {
        let p = prefilter(
            "add a unit test for parse_verdict covering the fenced-block case",
            &limits(Some(1800), Some(5.0)),
        );
        assert_eq!(p, Prefilter::Skip, "no rule should fire on a plain task");
    }

    #[test]
    fn terse_plus_sweeping_verb_is_consulted() {
        let p = prefilter("refactor the executor", &limits(Some(1800), Some(5.0)));
        assert!(matches!(p, Prefilter::Consult(_)), "{p:?}");
    }

    #[test]
    fn a_long_careful_description_with_a_sweeping_verb_is_not_flagged_for_terseness() {
        // The rule is terse AND sweeping. A detailed refactor brief is
        // exactly the case that should NOT cost an extra call.
        let long = format!("refactor {}", "the retry path in one file, ".repeat(10));
        let p = prefilter(&long, &limits(Some(1800), Some(5.0)));
        assert_eq!(p, Prefilter::Skip, "{p:?}");
    }

    #[test]
    fn spanning_two_crates_is_consulted() {
        let p = prefilter(
            "move the tier ceiling from mur-core into mur-agent-runtime so both gates share it",
            &limits(Some(1800), Some(5.0)),
        );
        match p {
            Prefilter::Consult(r) => assert!(r.contains(&"spans more than one crate"), "{r:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn naming_one_crate_twice_is_not_two_crates() {
        let p = prefilter(
            "add one test in mur-core, in the mur-core executor module",
            &limits(Some(1800), Some(5.0)),
        );
        assert_eq!(p, Prefilter::Skip, "{p:?}");
    }

    #[test]
    fn a_thin_budget_is_consulted() {
        match prefilter("add a unit test", &limits(Some(1800), Some(0.10))) {
            Prefilter::Consult(r) => assert!(r.contains(&"budget is thin"), "{r:?}"),
            other => panic!("{other:?}"),
        }
    }

    // ---- decide: the binding half ----

    #[test]
    fn low_confidence_goes_to_a_human_even_when_the_model_says_proceed() {
        let v = verdict(Complexity::S, RecommendedAction::Proceed, 0.3);
        assert_eq!(decide(&v), Decision::AskHuman);
    }

    #[test]
    fn low_confidence_goes_to_a_human_even_when_the_model_says_reject() {
        // A wrong reject is as costly as a runaway: it silently drops work.
        let v = verdict(Complexity::Xl, RecommendedAction::Reject, 0.2);
        assert_eq!(decide(&v), Decision::AskHuman);
    }

    #[test]
    fn xl_never_proceeds_on_the_models_say_so() {
        let mut v = verdict(Complexity::Xl, RecommendedAction::Proceed, 0.95);
        v.proposed_splits = vec!["extract the gate".into(), "wire the ceiling".into()];
        assert_eq!(decide(&v), Decision::Split);
    }

    #[test]
    fn xl_with_no_splits_to_offer_goes_to_a_human() {
        let v = verdict(Complexity::Xl, RecommendedAction::Proceed, 0.95);
        assert_eq!(decide(&v), Decision::AskHuman);
    }

    #[test]
    fn split_with_an_empty_list_is_not_an_instruction() {
        let v = verdict(Complexity::L, RecommendedAction::Split, 0.9);
        assert_eq!(decide(&v), Decision::AskHuman);
    }

    #[test]
    fn a_confident_small_task_proceeds() {
        let v = verdict(Complexity::S, RecommendedAction::Proceed, 0.9);
        assert_eq!(decide(&v), Decision::Proceed);
    }

    // ---- parsing ----

    #[test]
    fn a_fenced_verdict_parses() {
        let raw = "Sure, here you go:\n```json\n{\"complexity\":\"XL\",\"ambiguity\":4,\
                   \"dependency_risk\":3,\"recommended_action\":\"split\",\
                   \"proposed_splits\":[\"a\"],\"confidence\":0.8,\"reasons\":[\"r\"]}\n```";
        let v = parse_verdict(raw).expect("should parse");
        assert_eq!(v.complexity, Complexity::Xl);
        assert_eq!(v.recommended_action, RecommendedAction::Split);
    }

    #[test]
    fn prose_with_no_json_is_a_failed_verdict_not_a_guess() {
        assert!(parse_verdict("I think this is probably fine, go ahead").is_err());
    }

    #[test]
    fn a_confidence_outside_the_range_is_rejected() {
        let raw = "{\"complexity\":\"S\",\"ambiguity\":1,\"dependency_risk\":1,\
                   \"recommended_action\":\"proceed\",\"confidence\":9.9}";
        assert!(
            parse_verdict(raw).is_err(),
            "9.9 must not read as confident"
        );
    }

    // ---- the three invariants ----

    #[tokio::test]
    async fn a_skipped_prefilter_makes_no_model_call() {
        let outcome = triage(
            "add a unit test for parse_verdict",
            &limits(Some(1800), Some(5.0)),
            Duration::from_secs(5),
            |_| async { panic!("the model must not be called when no rule fired") },
        )
        .await;
        assert_eq!(outcome.decision, Decision::Proceed);
        assert_eq!(outcome.basis, Basis::NoRuleFired);
    }

    #[tokio::test]
    async fn an_unavailable_model_degrades_to_proceed_and_says_why() {
        let l = limits(Some(1800), Some(5.0));
        let outcome = triage(
            "refactor the executor",
            &l,
            Duration::from_secs(5),
            |_| async { Err("connection refused".to_string()) },
        )
        .await;
        assert_eq!(
            outcome.decision,
            Decision::Proceed,
            "a dead triage model must never block dispatch"
        );
        match outcome.basis {
            Basis::Degraded(m) => assert!(m.contains("connection refused"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hanging_model_degrades_instead_of_hanging_dispatch() {
        let l = limits(Some(1800), Some(5.0));
        let outcome = triage(
            "refactor the executor",
            &l,
            Duration::from_millis(50),
            |_| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(String::new())
            },
        )
        .await;
        assert_eq!(outcome.decision, Decision::Proceed);
        assert!(matches!(outcome.basis, Basis::Degraded(m) if m.contains("timed out")));
    }

    #[tokio::test]
    async fn the_verdict_cannot_change_the_budget_it_was_judged_against() {
        let l = limits(Some(1800), Some(5.0));
        let outcome = triage(
            "refactor the executor",
            &l,
            Duration::from_secs(5),
            |_| async {
                // A model doing its level best to ask for a raise.
                Ok(
                    "{\"complexity\":\"XL\",\"ambiguity\":5,\"dependency_risk\":5,\
                \"recommended_action\":\"proceed\",\"proposed_splits\":[\"a\",\"b\"],\
                \"confidence\":0.99,\"reasons\":[\"needs a 10x budget and 4 hours\"]}"
                        .to_string(),
                )
            },
        )
        .await;
        assert_eq!(
            outcome.limits, l,
            "triage echoes the limits it was given; it cannot widen them"
        );
        assert_eq!(outcome.decision, Decision::Split);
    }

    #[tokio::test]
    async fn the_prompt_carries_the_rules_that_fired_and_the_real_budget() {
        let l = limits(Some(1800), Some(0.10));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = seen.clone();
        let _ = triage(
            "refactor the executor",
            &l,
            Duration::from_secs(5),
            move |p| {
                *sink.lock().unwrap() = p;
                async { Err("stop here".to_string()) }
            },
        )
        .await;
        let p = seen.lock().unwrap().clone();
        assert!(p.contains("budget is thin"), "{p}");
        assert!(p.contains("$0.10"), "{p}");
    }

    #[test]
    fn the_system_prompt_forbids_the_retired_turn_estimate() {
        assert!(
            TRIAGE_SYSTEM.contains("Do NOT estimate turns"),
            "the 2.79 boundary must be stated to the model"
        );
    }
}
