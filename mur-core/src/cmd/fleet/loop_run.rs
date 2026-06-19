//! Phase 2a: the fleet loop. Wraps Phase 1's single iteration in a guarded loop
//! (iteration cap, deadline, stuck-detection, router convergence). The guards
//! live HERE — outside any agent — so the daemon `fleet_tick` (Phase 2b) can
//! reuse the same logic. The live orchestration needs running member agents;
//! the pure guard helpers below are unit-tested.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent};
use mur_common::fleet::Fleet;

use super::run::build_fleet_procedure;
use super::store;

/// Iterations with no new agent activity before the loop gives up.
const STUCK_LIMIT: u32 = 2;
/// Default iteration cap when neither the CLI flag nor fleet.yaml sets one.
const DEFAULT_MAX_ITERATIONS: u32 = 8;
/// Conservative per-turn token estimate for budget *projection*. Real per-token
/// cost isn't available real-time (OTel telemetry is input-only + async), so we
/// project high and stop early — a safety ceiling, not an invoice.
const EST_TOKENS_PER_TURN: u64 = 8000;
/// Fallback per-1k-token USD rate when models.yaml has no priced entry and no
/// `MUR_FLEET_COST_PER_1K` override. Deliberately dear (frontier-ish output rate)
/// so the projection errs high → stops early.
const DEFAULT_PRICE_PER_1K: f64 = 0.05;

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStop {
    /// Router judged the goal complete.
    Converged,
    /// Hit the iteration cap.
    MaxIterations,
    /// Hit the wall-clock deadline.
    Deadline,
    /// `STUCK_LIMIT` consecutive iterations produced no new agent activity.
    Stuck,
    /// Projected cumulative cost would exceed the fleet's budget.
    Budget,
    /// Kill-switch engaged via `mur fleet stop`.
    Stopped,
}

/// Parse a humantime-ish duration: `30s`, `5m`, `2h`, `1d`, or a bare integer
/// (= seconds). Returns None on anything else. (No `humantime` dependency.)
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().ok()?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// Does the router's reply signal completion? True iff a standalone `done`
/// token appears AND no `continue`/negation token does. This fails SAFE: an
/// ambiguous, negated ("not done"), or empty reply returns false ("keep
/// going"), and the cap/deadline/stuck guards still bound the loop. Stopping
/// early on a false positive is worse than one extra iteration.
pub fn is_converged(reply: &str) -> bool {
    let tokens: Vec<String> = reply
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let has = |t: &str| tokens.iter().any(|w| w == t);
    has("done") && !has("continue") && !has("not") && !has("incomplete")
}

/// Pure pre-iteration guard check. `iteration` = number of iterations already
/// completed. Returns Some(stop) to halt before running another.
pub fn check_guards(
    iteration: u32,
    max_iterations: u32,
    elapsed: Duration,
    deadline: Option<Duration>,
    stuck_count: u32,
) -> Option<LoopStop> {
    if iteration >= max_iterations {
        return Some(LoopStop::MaxIterations);
    }
    if let Some(d) = deadline
        && elapsed >= d
    {
        return Some(LoopStop::Deadline);
    }
    if stuck_count >= STUCK_LIMIT {
        return Some(LoopStop::Stuck);
    }
    None
}

/// Resolve the effective iteration cap: CLI flag > fleet.yaml loop.max_iterations > default.
fn effective_max_iterations(flag: Option<u32>, fleet: &Fleet) -> u32 {
    flag.or_else(|| fleet.loop_cfg.as_ref().map(|l| l.max_iterations))
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_ITERATIONS)
}

/// Resolve the effective deadline: CLI flag > fleet.yaml loop.deadline > none.
fn effective_deadline(flag: Option<&str>, fleet: &Fleet) -> Option<Duration> {
    flag.and_then(parse_duration).or_else(|| {
        fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.deadline.as_str())
            .filter(|s| !s.is_empty())
            .and_then(parse_duration)
    })
}

/// Projected USD for one iteration: every member takes a ~`EST_TOKENS_PER_TURN`
/// turn at `price_per_1k`. Conservative so real spend stays BELOW the projection.
pub fn estimate_iteration_cost_usd(members: usize, price_per_1k: f64) -> f64 {
    members as f64 * (EST_TOKENS_PER_TURN as f64 / 1000.0) * price_per_1k
}

/// Would another iteration (projected `next_cost`) exceed `budget`? Enforced only
/// when budget is `Some(>0)`; stops BEFORE the unaffordable iteration (fail-safe).
pub fn budget_exceeded(spent: f64, next_cost: f64, budget: Option<f64>) -> bool {
    matches!(budget, Some(b) if b > 0.0 && spent + next_cost > b)
}

/// Conservative per-1k-token USD rate for projection: `MUR_FLEET_COST_PER_1K`
/// env → else the dearest output rate in `models.yaml` → else `DEFAULT_PRICE_PER_1K`.
fn fleet_price_per_1k(mur_home: &Path) -> f64 {
    if let Ok(v) = std::env::var("MUR_FLEET_COST_PER_1K")
        && let Ok(p) = v.parse::<f64>()
        && p > 0.0
    {
        return p;
    }
    if let Ok(reg) = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")) {
        let max = reg
            .models
            .values()
            .filter_map(|m| m.effective_costs().1)
            .fold(0.0_f64, f64::max);
        if max > 0.0 {
            return max;
        }
    }
    DEFAULT_PRICE_PER_1K
}

/// Resolve the effective budget USD: CLI flag > fleet.yaml `loop.budget_usd` > none.
fn effective_budget(flag: Option<f64>, fleet: &Fleet) -> Option<f64> {
    flag.or_else(|| fleet.loop_cfg.as_ref().map(|l| l.budget_usd))
        .filter(|&b| b > 0.0)
}

/// `mur fleet run --loop`: run guarded iterations until the router converges or
/// a guard trips. Requires the member + router agents to be running.
pub async fn cmd_fleet_run_loop(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        anyhow::bail!("fleet '{name}' has no members");
    }
    let max_iter = effective_max_iterations(max_iterations, &fleet);
    let deadline = effective_deadline(deadline.as_deref(), &fleet);
    let budget = effective_budget(budget_usd, &fleet);
    let per_iter_cost =
        estimate_iteration_cost_usd(fleet.members.len(), fleet_price_per_1k(mur_home));
    let mut spent = 0.0_f64;
    let start = Instant::now();
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let mut last_seq = svc
        .load_events(&fleet.channel_id)?
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    let mut iteration = 0u32;
    let mut stuck = 0u32;

    let stop = loop {
        // Kill-switch (highest priority): a `mur fleet stop` between iterations halts here.
        if super::control::is_stopped(mur_home, name) {
            break LoopStop::Stopped;
        }
        if let Some(stop) = check_guards(iteration, max_iter, start.elapsed(), deadline, stuck) {
            break stop;
        }
        // Budget projection (fail-safe: stop before an iteration we can't afford).
        if budget_exceeded(spent, per_iter_cost, budget) {
            break LoopStop::Budget;
        }
        println!("── fleet '{}' iteration {} ──", name, iteration + 1);

        // Router plans this iteration (seeing prior state); falls back to broadcast.
        let pre_events = svc.load_events(&fleet.channel_id).unwrap_or_default();
        let proc = super::plan::plan_via_router(mur_home, &fleet, &pre_events)
            .unwrap_or_else(|| build_fleet_procedure(&fleet.goal, &fleet.members));
        let opts = crate::executor::dag::DagExecOptions {
            // Fail-closed on the unattended loop path: never blanket-approve.
            // (No risk tier on fan-out steps today; this guards future
            // router-emitted risk steps. Best-practice audit / OWASP ASI06.)
            yes: false,
            channel_id: Some(fleet.channel_id.clone()),
            // uuid nonce so concurrent `--loop` runs don't collide on the
            // channel's idempotency-key dedup (the iteration stays for readability).
            run_id: format!("loop-{}-{}-{}", name, uuid::Uuid::now_v7(), iteration),
            ..Default::default()
        };
        let _ = crate::executor::dag::execute_dag(mur_home, &format!("fleet:{name}"), &proc, &opts)
            .await?;
        iteration += 1;
        spent += per_iter_cost;

        // Stuck-detection: did this iteration add any new agent-authored event?
        let events = svc.load_events(&fleet.channel_id)?;
        let progressed = events
            .iter()
            .any(|e| e.seq > last_seq && matches!(e.actor, ChannelActor::Agent { .. }));
        last_seq = events.last().map(|e| e.seq).unwrap_or(last_seq);
        if progressed {
            stuck = 0;
        } else {
            stuck += 1;
        }

        // Convergence: ask the router. A failed ask (e.g. router down) is treated
        // as "continue" — the cap/deadline/stuck guards still bound the loop.
        if ask_router_done(mur_home, &fleet, &events).unwrap_or(false) {
            break LoopStop::Converged;
        }
    };

    println!(
        "fleet '{}' loop stopped after {iteration} iteration(s) (~${spent:.2} projected): {stop:?}",
        name
    );
    Ok(())
}

/// Ask the router agent whether the goal is complete. Streams a one-word reply.
fn ask_router_done(mur_home: &Path, fleet: &Fleet, events: &[ChannelEvent]) -> Result<bool> {
    let recent: String = events
        .iter()
        .rev()
        .take(8)
        .rev()
        .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let done_when = fleet
        .loop_cfg
        .as_ref()
        .map(|l| l.done_when.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(none given — judge from the goal)");
    let prompt = format!(
        "You are the router for fleet '{}'.\nGoal: {}\nDone-criterion: {}\nRecent channel activity:\n{}\n\nIs the goal complete? Reply with exactly one word: DONE or CONTINUE.",
        fleet.name,
        fleet.goal,
        done_when,
        if recent.is_empty() {
            "(none yet)"
        } else {
            &recent
        },
    );
    let params = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] }
    });
    let mut out = String::new();
    crate::a2a_dial::dial_message_streaming(
        mur_home,
        fleet.router_or_concierge(),
        params,
        |delta, _thinking, _id| out.push_str(delta),
        |_hitl| {},
    )?;
    Ok(is_converged(&out))
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn check_guards_precedence_and_trips() {
        // under all limits → keep going
        assert_eq!(check_guards(0, 8, Duration::from_secs(0), None, 0), None);
        // iteration cap
        assert_eq!(
            check_guards(8, 8, Duration::from_secs(0), None, 0),
            Some(LoopStop::MaxIterations)
        );
        // deadline
        assert_eq!(
            check_guards(
                1,
                8,
                Duration::from_secs(10),
                Some(Duration::from_secs(5)),
                0
            ),
            Some(LoopStop::Deadline)
        );
        // not yet past deadline
        assert_eq!(
            check_guards(
                1,
                8,
                Duration::from_secs(3),
                Some(Duration::from_secs(5)),
                0
            ),
            None
        );
        // stuck
        assert_eq!(
            check_guards(1, 8, Duration::from_secs(0), None, STUCK_LIMIT),
            Some(LoopStop::Stuck)
        );
        // cap takes precedence over stuck
        assert_eq!(
            check_guards(8, 8, Duration::from_secs(0), None, STUCK_LIMIT),
            Some(LoopStop::MaxIterations)
        );
    }

    #[test]
    fn estimate_and_budget_guard() {
        // 2 members × 8000/1000 × 0.05 = 0.8 / iteration
        assert!((estimate_iteration_cost_usd(2, 0.05) - 0.8).abs() < 1e-9);
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
    fn fleet_price_per_1k_env_then_default() {
        let tmp = tempfile::tempdir().unwrap();
        // env override wins (nextest isolates per-process, so this is safe)
        unsafe { std::env::set_var("MUR_FLEET_COST_PER_1K", "0.123") };
        assert!((fleet_price_per_1k(tmp.path()) - 0.123).abs() < 1e-9);
        // no env + no models.yaml → documented default
        unsafe { std::env::remove_var("MUR_FLEET_COST_PER_1K") };
        assert!((fleet_price_per_1k(tmp.path()) - DEFAULT_PRICE_PER_1K).abs() < 1e-9);
    }

    #[test]
    fn effective_max_iterations_precedence() {
        let mut f = Fleet {
            name: "x".into(),
            display_name: String::new(),
            goal: String::new(),
            router: None,
            members: vec![],
            channel_id: "fleet-x".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
        };
        // default when nothing set
        assert_eq!(effective_max_iterations(None, &f), DEFAULT_MAX_ITERATIONS);
        // fleet.yaml value
        f.loop_cfg = Some(mur_common::fleet::FleetLoop {
            trigger: "manual".into(),
            max_iterations: 3,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: String::new(),
        });
        assert_eq!(effective_max_iterations(None, &f), 3);
        // CLI flag wins
        assert_eq!(effective_max_iterations(Some(5), &f), 5);
        // a 0 in fleet.yaml is ignored → default
        if let Some(l) = f.loop_cfg.as_mut() {
            l.max_iterations = 0;
        }
        assert_eq!(effective_max_iterations(None, &f), DEFAULT_MAX_ITERATIONS);
    }
}
