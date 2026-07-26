//! Phase 3 router planning. Instead of broadcasting the goal to every member,
//! the router agent emits a structured plan (steps with `delegate_to` +
//! `depends_on`) that becomes the `Procedure` the DAG executor runs. LLM output
//! is fragile, so parsing is defensive and **falls back to broadcast** on any
//! problem (unknown member, bad JSON, unresolved dep, cycle, oversize). The
//! parse/validate path is pure + unit-tested; the router dial is live.

use std::collections::HashSet;
use std::path::Path;

use mur_common::agent_facts::{AgentFacts, ExecFacts, agent_facts};
use mur_common::channel::ChannelEvent;
use mur_common::fleet::Fleet;
use mur_common::skill::manifest::{Procedure, ProcedureStep};
use serde::Deserialize;
use serde_json::json;

/// Upper bound on plan size — a runaway plan falls back to broadcast.
const MAX_PLAN_STEPS: usize = 32;

#[derive(Deserialize)]
struct RouterPlan {
    #[serde(default)]
    steps: Vec<PlanStep>,
}

#[derive(Deserialize)]
struct PlanStep {
    #[serde(default)]
    id: Option<String>,
    member: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    depends_on: Vec<String>,
}

/// End byte index of the balanced `{...}` beginning at `start` (a `{`),
/// ignoring braces inside double-quoted JSON strings. `None` if never balanced.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// First balanced `{...}` span in `text` that parses into a non-empty router
/// plan. Trying *every* `{` (not just the first, not first-to-last) means stray
/// prose braces — `use {member}` — or a leading unrelated JSON object can't
/// defeat extraction, and trailing prose braces aren't swallowed.
fn extract_plan(text: &str) -> Option<RouterPlan> {
    let bytes = text.as_bytes();
    (0..bytes.len())
        .filter(|&i| bytes[i] == b'{')
        .find_map(|start| {
            let end = balanced_end(bytes, start)?;
            let plan: RouterPlan = serde_json::from_str(&text[start..=end]).ok()?;
            (!plan.steps.is_empty()).then_some(plan)
        })
}

/// Parse a router reply into a validated `Procedure`, or `None` to fall back to
/// broadcast. Validates: parseable JSON, non-empty and ≤ `MAX_PLAN_STEPS`, every
/// `member` is a real fleet member, and (via the executor's own validator)
/// resolvable `depends_on` with no cycles.
pub fn parse_router_plan(text: &str, members: &[String], goal: &str) -> Option<Procedure> {
    let plan = extract_plan(text)?;
    if plan.steps.len() > MAX_PLAN_STEPS {
        return None;
    }
    let member_set: HashSet<&str> = members.iter().map(String::as_str).collect();
    let mut steps = Vec::with_capacity(plan.steps.len());
    for (i, ps) in plan.steps.iter().enumerate() {
        if !member_set.contains(ps.member.as_str()) {
            return None; // unknown member → fall back to broadcast
        }
        let task = if ps.task.trim().is_empty() {
            ps.member.clone()
        } else {
            ps.task.clone()
        };
        steps.push(ProcedureStep {
            description: format!("{}: {task}", ps.member),
            intent: Some(format!(
                "{goal}\n\n[Router assignment for {}]: {task}",
                ps.member
            )),
            delegate_to: Some(ps.member.clone()),
            id: Some(ps.id.clone().unwrap_or_else(|| format!("s{i}"))),
            depends_on: ps.depends_on.clone(),
            ..Default::default()
        });
    }
    // Reject duplicate step ids: build_dag's id→index map would silently
    // overwrite, producing wrong execution rather than a clean fallback.
    let mut seen: HashSet<&str> = HashSet::new();
    for s in &steps {
        if !seen.insert(s.id.as_deref().unwrap_or("")) {
            return None;
        }
    }
    // Reuse the executor's validator: resolvable deps + acyclic.
    crate::executor::dag::validate_steps(&steps).ok()?;
    Some(Procedure {
        variables: vec![],
        steps,
    })
}

/// Ask the router agent to produce a plan; `None` on any failure (caller falls
/// back to broadcast). Requires the router agent to be running.
///
/// A fleet with one member short-circuits to broadcast (`None`). This is a
/// deliberate trade, not a pure optimisation: the router's only remaining
/// freedom with one executor is to pre-split the goal into several steps for
/// that same agent — something the member agent does for itself anyway — and
/// rediscovering the one possible assignee costs a full LLM round trip on
/// EVERY iteration. It matters because solo fleets are the adapter for
/// agent-granularity dispatch (`mur agent who` routes a denied binary to the
/// narrowest capable fleet, usually a one-member one); an adapter that doubles
/// the cost of every mechanical command pushes people toward the ungoverned
/// `mur agent send` path instead.
pub fn plan_via_router(
    mur_home: &Path,
    fleet: &Fleet,
    goal: &str,
    events: &[ChannelEvent],
) -> Option<Procedure> {
    if fleet.members.len() <= 1 {
        return None;
    }
    let prompt = build_plan_prompt(mur_home, fleet, events);
    let params = json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] }
    });
    let mut out = String::new();
    crate::a2a_dial::dial_message_streaming(
        mur_home,
        fleet.router_or_concierge(),
        params,
        |delta, _thinking, _id| out.push_str(delta),
        |_hitl| {},
        |_step| {},
    )
    .ok()?;
    parse_router_plan(&out, &fleet.members, goal)
}

/// How many of a member's skills to name before truncating. A router prompt
/// gains nothing from a member's twentieth skill, and one agent here really
/// does have nineteen.
const ROSTER_MAX_SKILLS: usize = 6;
/// Same idea for writable roots: enough to tell repos apart, not an inventory.
const ROSTER_MAX_WRITES: usize = 2;

/// One line per member: what it is, what it knows, and — the part no
/// self-description can fake — what the kernel actually lets it run and write.
///
/// Without this the router sees `Members: frontend, rustsmith, pm, qa` and
/// routes on what the NAMES suggest. Facts, not names, decide who can do the
/// work; a good name is a coincidence.
///
/// Pure so it can be tested without a `~/.mur`. Returns `None` when no member
/// resolved, so the caller keeps today's plain-names line rather than sending a
/// worse prompt.
fn member_roster(facts: &[AgentFacts]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    let trunc = |items: &[String], max: usize| -> String {
        let shown: Vec<String> = items.iter().take(max).cloned().collect();
        match items.len().checked_sub(max) {
            Some(n) if n > 0 => format!("{}, +{n} more", shown.join(", ")),
            _ => shown.join(", "),
        }
    };
    let lines: Vec<String> = facts
        .iter()
        .map(|f| {
            let mut parts = vec![format!("- {}", f.name)];
            if !f.role.is_empty() {
                parts.push(format!("({})", f.role));
            }
            let exec = match &f.exec {
                ExecFacts::Unrestricted => "any".to_string(),
                ExecFacts::Nothing => "none".to_string(),
                ExecFacts::Allowlist(l) => trunc(l, ROSTER_MAX_SKILLS),
            };
            parts.push(format!("| can run: {exec}"));
            if !f.writes.is_empty() {
                let w: Vec<String> = f.writes.iter().map(|p| p.display().to_string()).collect();
                parts.push(format!("| writes: {}", trunc(&w, ROSTER_MAX_WRITES)));
            }
            if !f.skills.is_empty() {
                parts.push(format!("| skills: {}", trunc(&f.skills, ROSTER_MAX_SKILLS)));
            }
            parts.join(" ")
        })
        .collect();
    Some(lines.join("\n"))
}

fn build_plan_prompt(mur_home: &Path, fleet: &Fleet, events: &[ChannelEvent]) -> String {
    let recent: String = events
        .iter()
        .rev()
        .take(8)
        .rev()
        .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let facts: Vec<AgentFacts> = fleet
        .members
        .iter()
        .filter_map(|m| agent_facts(mur_home, m))
        .collect();
    let members = match member_roster(&facts) {
        Some(roster) => format!("\n{roster}"),
        None => format!(" {}", fleet.members.join(", ")),
    };
    format!(
        "You are the router for fleet '{}'. Goal: {}\nMembers (delegate ONLY to these):{}\nRecent channel activity:\n{}\n\nProduce a minimal plan that assigns work to the RIGHT members (not everyone) with dependencies. Match each step to a member that can actually run it — `can run` and `writes` are what the sandbox enforces, so a member without the binary or the directory WILL fail the step. Reply with ONLY JSON:\n{{\"steps\":[{{\"id\":\"s1\",\"member\":\"<one of the members>\",\"task\":\"<what to do>\",\"depends_on\":[]}}]}}\nUse `depends_on` (referencing earlier step ids) for steps that must wait. Keep it small.",
        fleet.name,
        fleet.goal,
        members,
        if recent.is_empty() {
            "(none yet)"
        } else {
            &recent
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<String> {
        vec!["pm".into(), "qa".into()]
    }

    #[test]
    fn parses_valid_plan_with_deps() {
        let txt = r#"{"steps":[
            {"id":"a","member":"pm","task":"triage"},
            {"id":"b","member":"qa","task":"test","depends_on":["a"]}
        ]}"#;
        let p = parse_router_plan(txt, &members(), "G").unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
        assert_eq!(p.steps[1].depends_on, vec!["a".to_string()]);
        assert_eq!(
            p.steps[0].intent.as_deref(),
            Some("G\n\n[Router assignment for pm]: triage")
        );
    }

    #[test]
    fn extracts_json_from_prose_and_fences() {
        let txt = "Sure! Here is the plan:\n```json\n{\"steps\":[{\"member\":\"pm\",\"task\":\"go\"}]}\n```\nHope that helps.";
        let p = parse_router_plan(txt, &members(), "G").unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
    }

    #[test]
    fn falls_back_on_unknown_member() {
        let txt = r#"{"steps":[{"member":"intruder","task":"x"}]}"#;
        assert!(parse_router_plan(txt, &members(), "G").is_none());
    }

    #[test]
    fn falls_back_on_malformed_or_empty() {
        assert!(parse_router_plan("not json at all", &members(), "G").is_none());
        assert!(parse_router_plan(r#"{"steps":[]}"#, &members(), "G").is_none());
        assert!(parse_router_plan("{ totally broken", &members(), "G").is_none());
    }

    #[test]
    fn falls_back_on_cycle_and_unknown_dep() {
        // cycle a<->b
        let cyc = r#"{"steps":[
            {"id":"a","member":"pm","task":"x","depends_on":["b"]},
            {"id":"b","member":"qa","task":"y","depends_on":["a"]}
        ]}"#;
        assert!(parse_router_plan(cyc, &members(), "G").is_none());
        // depends_on an id that doesn't exist
        let bad = r#"{"steps":[{"id":"a","member":"pm","task":"x","depends_on":["ghost"]}]}"#;
        assert!(parse_router_plan(bad, &members(), "G").is_none());
    }

    #[test]
    fn falls_back_on_duplicate_ids() {
        let txt = r#"{"steps":[
            {"id":"s1","member":"pm","task":"a"},
            {"id":"s1","member":"qa","task":"b"}
        ]}"#;
        assert!(parse_router_plan(txt, &members(), "G").is_none());
    }

    #[test]
    fn extracts_json_despite_prose_braces() {
        // A stray brace before the real JSON (and trailing prose braces) must
        // not defeat extraction — the greedy first-{ to last-} version failed.
        let txt = "Plan: give work to {member} thus: {\"steps\":[{\"member\":\"pm\",\"task\":\"go\"}]} ok}";
        let p = parse_router_plan(txt, &members(), "G").unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
    }

    #[test]
    fn ignores_braces_inside_strings() {
        // `}{` inside a task string must not be read as object boundaries.
        let txt = r#"{"steps":[{"member":"pm","task":"emit }{ verbatim"}]}"#;
        let p = parse_router_plan(txt, &members(), "G").unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(
            p.steps[0].intent.as_deref(),
            Some("G\n\n[Router assignment for pm]: emit }{ verbatim")
        );
    }

    #[test]
    fn plan_intent_carries_original_goal_verbatim() {
        let goal = "Implement the FULL spec at /abs/path/spec.md — do not paraphrase.";
        let text = r#"{"steps":[{"member":"pm","task":"do part one"}]}"#;
        let p = parse_router_plan(text, &["pm".to_string()], goal).unwrap();
        let intent = p.steps[0].intent.as_deref().unwrap();
        assert!(
            intent.starts_with(goal),
            "original goal must lead the intent"
        );
        assert!(intent.contains("do part one"));
    }

    fn roster_facts(name: &str, exec: ExecFacts, writes: &[&str], skills: &[&str]) -> AgentFacts {
        AgentFacts {
            name: name.into(),
            role: String::new(),
            exec,
            writes: writes.iter().map(std::path::PathBuf::from).collect(),
            net: mur_common::agent::NetworkOutboundMode::Restricted,
            skills: skills.iter().map(|s| s.to_string()).collect(),
            model_ref: String::new(),
            effort: None,
            running: true,
            drift: false,
        }
    }

    #[test]
    fn roster_states_the_enforced_facts_and_truncates() {
        let long: Vec<&str> = vec!["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"];
        let r = member_roster(&[roster_facts(
            "rustsmith",
            ExecFacts::Allowlist(vec!["git".into(), "cargo".into()]),
            &["/repo/mur", "/home/rs", "/cache"],
            &long,
        )])
        .unwrap();
        assert!(r.contains("can run: git, cargo"), "{r}");
        // Writes are capped, and the overflow is stated rather than hidden.
        assert!(r.contains("writes: /repo/mur, /home/rs, +1 more"), "{r}");
        assert!(r.contains("+2 more"), "skills not truncated: {r}");
    }

    #[test]
    fn roster_is_none_when_no_member_resolved() {
        // Caller must fall back to the plain names line, never to an empty
        // "Members:" section.
        assert!(member_roster(&[]).is_none());
    }
}
