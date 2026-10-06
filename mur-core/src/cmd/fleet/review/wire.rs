//! §3.1/§3.2: the A2A wire shape of a review turn.
//!
//! The agent runtime's `message/send` reads `params.message` as an A2A
//! `Message` (`role` + `parts`), so every turn the driver sends is a single
//! text part carrying a rendered prompt. The reviewer's verdict comes back
//! as free text; [`extract_verdict_json`] pulls the machine-validated JSON
//! out of it (§3.2: "the wire encoding ... is left to the builder, but it
//! must be machine-validated").

use super::constants::{
    REVIEW_BINDING_RULINGS_HEADER, REVIEW_MAIN_PROMPT, REVIEW_NO_OPEN_FINDINGS,
    REVIEW_REVIEWER_PROMPT,
};
use super::ledger::Ledger;
use super::schema::Role;

/// Wrap `text` as A2A `message/send` params: one user message, one text part.
pub fn text_message_params(text: &str) -> serde_json::Value {
    serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] }
    })
}

/// The text of the first text part in params built by
/// [`text_message_params`]. Used by tests and by the semi-auto preview.
pub fn message_text(params: &serde_json::Value) -> Option<&str> {
    params["message"]["parts"]
        .as_array()?
        .iter()
        .find(|p| p["kind"] == "text")?["text"]
        .as_str()
}

/// Render the open set (§3.3: `open` ∪ `disputed`) one finding per line.
fn render_open_findings(ledger: &Ledger) -> String {
    let lines: Vec<String> = ledger
        .stop_screen_findings(false)
        .into_iter()
        .map(|f| {
            let severity = serde_json::to_value(f.severity).unwrap_or_default();
            let status = serde_json::to_value(f.status).unwrap_or_default();
            format!(
                "- {} [{}, {}]: {}",
                f.id,
                severity.as_str().unwrap_or_default(),
                status.as_str().unwrap_or_default(),
                f.issue
            )
        })
        .collect();
    if lines.is_empty() {
        REVIEW_NO_OPEN_FINDINGS.to_string()
    } else {
        lines.join("\n")
    }
}

/// P2-§5.3: rulings `role` has not yet been sent, as a block that sits
/// above the findings; empty when there are none, so the slot vanishes.
fn render_binding_rulings(ledger: &Ledger, role: Role) -> String {
    let rulings = ledger.binding_rulings(role);
    if rulings.is_empty() {
        return String::new();
    }
    let mut out = format!("{REVIEW_BINDING_RULINGS_HEADER}\n");
    for r in rulings {
        let decision = serde_json::to_value(r.decision).unwrap_or_default();
        out.push_str(&format!(
            "- {} {}: {}\n",
            r.finding,
            decision.as_str().unwrap_or_default(),
            r.text
        ));
    }
    out.push('\n');
    out
}

/// Main's turn (§3.1, §3.4): the task plus every finding still open.
pub fn main_turn_params(task: &str, round: u32, ledger: &Ledger) -> serde_json::Value {
    // Rulings and `{task}` are human text: substituted after the fixed
    // placeholders so a literal `{...}` inside them is never expanded.
    let text = REVIEW_MAIN_PROMPT
        .replace("{round}", &round.to_string())
        .replace("{open_findings}", &render_open_findings(ledger))
        .replace(
            "{binding_rulings}",
            &render_binding_rulings(ledger, Role::Main),
        )
        .replace("{task}", task);
    text_message_params(&text)
}

/// The reviewer's turn (§3.2): the task, main's reply, and the open set the
/// reviewer must give a status for.
pub fn reviewer_turn_params(
    task: &str,
    round: u32,
    main_reply: &str,
    ledger: &Ledger,
) -> serde_json::Value {
    // Rulings, `{task}` and `{main_reply}` are substituted last: human and
    // model text may itself contain a literal `{...}` placeholder and must
    // not be expanded.
    let text = REVIEW_REVIEWER_PROMPT
        .replace("{round}", &round.to_string())
        .replace("{open_findings}", &render_open_findings(ledger))
        .replace(
            "{binding_rulings}",
            &render_binding_rulings(ledger, Role::Reviewer),
        )
        .replace("{task}", task)
        .replace("{main_reply}", main_reply);
    text_message_params(&text)
}

/// Pull the verdict JSON out of a reviewer reply. Accepts a bare JSON object
/// or a fenced block (```` ```json ```` or plain ```` ``` ````); when the
/// reply has several fenced blocks the LAST one wins, since the prompt asks
/// for the verdict at the end. Returns `None` when nothing looks like JSON;
/// the caller still validates the shape.
pub fn extract_verdict_json(reply: &str) -> Option<&str> {
    let trimmed = reply.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let mut last = None;
    let mut rest = reply;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        // An unterminated fence ends the scan; earlier blocks still count.
        let Some(body_start) = after.find('\n').map(|i| i + 1) else {
            break;
        };
        let body = &after[body_start..];
        let Some(close) = body.find("```") else {
            break;
        };
        let candidate = body[..close].trim();
        if candidate.starts_with('{') {
            last = Some(candidate);
        }
        rest = &body[close + 3..];
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_an_a2a_message_with_one_text_part() {
        let p = text_message_params("hi");
        let msg: mur_common::a2a::Message = serde_json::from_value(p["message"].clone()).unwrap();
        assert_eq!(msg.role, "user");
        assert_eq!(msg.parts.len(), 1);
        assert_eq!(message_text(&p), Some("hi"));
    }

    #[test]
    fn main_prompt_includes_task_and_no_findings_marker() {
        let p = main_turn_params("fix the bug", 1, &Ledger::default());
        let text = message_text(&p).unwrap();
        assert!(text.contains("fix the bug"));
        assert!(text.contains("round 1"));
        assert!(text.contains(REVIEW_NO_OPEN_FINDINGS));
    }

    /// P2-§5.3: F1 issued, disputed and rejected twice, then ruled `fix`.
    fn ruled_ledger() -> Ledger {
        use super::super::schema::{
            Cumulative, FindingStatus, RebuttalAnswer, RebuttalResponseDto, ReviewPayload,
            RulingDecision, Severity,
        };
        let mut l = Ledger::default();
        let events = [
            ReviewPayload::FindingIssued {
                round: 1,
                id: "F1".into(),
                severity: Severity::High,
                issue: "unchecked unwrap".into(),
            },
            ReviewPayload::FindingStatus {
                round: 1,
                id: "F1".into(),
                status: FindingStatus::Disputed,
                reason: None,
            },
        ];
        for e in &events {
            l.apply(e).unwrap();
        }
        for round in [1, 2] {
            l.apply(&ReviewPayload::Rebuttal {
                round,
                responses: vec![RebuttalResponseDto {
                    id: "F1".into(),
                    answer: RebuttalAnswer::Reject,
                    reason: Some("no".into()),
                }],
                cumulative: Cumulative {
                    exec_time_ms: 0,
                    cost_usd_micros: 0,
                },
            })
            .unwrap();
        }
        l.apply(&ReviewPayload::Ruling {
            finding: "F1".into(),
            decision: RulingDecision::Fix,
            text: "use the cache".into(),
        })
        .unwrap();
        l
    }

    #[test]
    fn both_prompts_carry_binding_rulings_above_the_findings() {
        let l = ruled_ledger();
        for p in [
            main_turn_params("t", 4, &l),
            reviewer_turn_params("t", 4, "reply", &l),
        ] {
            let text = message_text(&p).unwrap();
            let header = text.find(REVIEW_BINDING_RULINGS_HEADER).expect(text);
            let ruling = text.find("- F1 fix: use the cache").expect(text);
            let finding = text.find("- F1 [high, open]").expect(text);
            assert!(header < ruling && ruling < finding, "{text}");
        }
    }

    #[test]
    fn no_rulings_no_header() {
        for p in [
            main_turn_params("t", 1, &Ledger::default()),
            reviewer_turn_params("t", 1, "reply", &Ledger::default()),
        ] {
            let text = message_text(&p).unwrap();
            assert!(!text.contains(REVIEW_BINDING_RULINGS_HEADER), "{text}");
            assert!(!text.contains("{binding_rulings}"), "{text}");
        }
    }

    #[test]
    fn reviewer_prompt_does_not_expand_placeholders_inside_main_reply() {
        let p = reviewer_turn_params("t", 2, "literal {task} here", &Ledger::default());
        assert!(message_text(&p).unwrap().contains("literal {task} here"));
    }

    #[test]
    fn extracts_bare_json() {
        assert_eq!(
            extract_verdict_json("  {\"verdict\":\"approve\"} "),
            Some("{\"verdict\":\"approve\"}")
        );
    }

    #[test]
    fn extracts_last_fenced_json_block() {
        let reply =
            "notes\n```rust\nfn x() {}\n```\nmore\n```json\n{\"verdict\":\"revise\"}\n```\n";
        assert_eq!(
            extract_verdict_json(reply),
            Some("{\"verdict\":\"revise\"}")
        );
    }

    #[test]
    fn plain_fence_counts_and_prose_alone_does_not() {
        assert_eq!(
            extract_verdict_json("ok\n```\n{\"verdict\":\"approve\"}\n```"),
            Some("{\"verdict\":\"approve\"}")
        );
        assert_eq!(extract_verdict_json("looks good to me"), None);
    }
}
