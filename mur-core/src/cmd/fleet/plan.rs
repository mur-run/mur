//! Phase 3 router planning. Instead of broadcasting the goal to every member,
//! the router agent emits a structured plan (steps with `delegate_to` +
//! `depends_on`) that becomes the `Procedure` the DAG executor runs. LLM output
//! is fragile, so parsing is defensive and **falls back to broadcast** on any
//! problem (unknown member, bad JSON, unresolved dep, cycle, oversize). The
//! parse/validate path is pure + unit-tested; the router dial is live.

use std::collections::HashSet;
use std::path::Path;

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
pub fn plan_via_router(
    mur_home: &Path,
    fleet: &Fleet,
    goal: &str,
    events: &[ChannelEvent],
) -> Option<Procedure> {
    let prompt = build_plan_prompt(fleet, events);
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

fn build_plan_prompt(fleet: &Fleet, events: &[ChannelEvent]) -> String {
    let recent: String = events
        .iter()
        .rev()
        .take(8)
        .rev()
        .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are the router for fleet '{}'. Goal: {}\nMembers (delegate ONLY to these): {}\nRecent channel activity:\n{}\n\nProduce a minimal plan that assigns work to the RIGHT members (not everyone) with dependencies. Reply with ONLY JSON:\n{{\"steps\":[{{\"id\":\"s1\",\"member\":\"<one of the members>\",\"task\":\"<what to do>\",\"depends_on\":[]}}]}}\nUse `depends_on` (referencing earlier step ids) for steps that must wait. Keep it small.",
        fleet.name,
        fleet.goal,
        fleet.members.join(", "),
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
}
