//! §3.1/§3.2: the A2A wire shape of a review turn.
//!
//! The agent runtime's `message/send` reads `params.message` as an A2A
//! `Message` (`role` + `parts`), so every turn the driver sends is a single
//! text part carrying a rendered prompt. The reviewer's verdict comes back
//! as free text; [`extract_verdict_json`] pulls the machine-validated JSON
//! out of it (§3.2: "the wire encoding ... is left to the builder, but it
//! must be machine-validated").

use super::constants::{
    REVIEW_BINDING_RULINGS_HEADER, REVIEW_HUMAN_NOTES_HEADER, REVIEW_MAIN_PROMPT,
    REVIEW_NO_OPEN_FINDINGS, REVIEW_REVIEWER_PROMPT,
};
use super::ledger::Ledger;
use super::schema::{HumanNote, Role};

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

/// P3a-§6.2: notes for `to` — the ledger's unseen ones, then the pending
/// ones aimed at `to` or broadcast, in that order. Empty when there are
/// none, so the slot vanishes and the message is unchanged.
fn render_human_notes(ledger: &Ledger, pending: &[HumanNote], to: Role) -> String {
    let notes: Vec<&HumanNote> = ledger
        .unseen_notes(to)
        .iter()
        .chain(pending.iter().filter(|n| n.target.is_none_or(|t| t == to)))
        .collect();
    if notes.is_empty() {
        return String::new();
    }
    let mut out = format!("{REVIEW_HUMAN_NOTES_HEADER}\n");
    for n in notes {
        out.push_str(&format!("- {}\n", n.text));
    }
    out.push('\n');
    out
}

/// Fill `{name}` slots in `template` in ONE left-to-right pass. Substituted
/// values are never rescanned, so human and model text (task, rulings, main's
/// reply) reaches the agent byte-for-byte even when it contains a literal
/// placeholder token. An unknown `{...}` is copied through untouched.
fn render_template(template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let hit = slots.iter().find_map(|(name, value)| {
            let token_len = name.len() + 2;
            (tail.len() >= token_len
                && tail[1..].starts_with(name)
                && tail.as_bytes()[token_len - 1] == b'}')
                .then_some((token_len, *value))
        });
        match hit {
            Some((token_len, value)) => {
                out.push_str(value);
                rest = &tail[token_len..];
            }
            None => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Main's turn (§3.1, §3.4): the task plus every finding still open.
/// `pending` are notes typed at this prompt and not yet flushed (P3a-§6.2).
pub fn main_turn_params(
    task: &str,
    round: u32,
    ledger: &Ledger,
    pending: &[HumanNote],
) -> serde_json::Value {
    let text = render_template(
        REVIEW_MAIN_PROMPT,
        &[
            ("round", &round.to_string()),
            ("open_findings", &render_open_findings(ledger)),
            (
                "binding_rulings",
                &render_binding_rulings(ledger, Role::Main),
            ),
            (
                "human_notes",
                &render_human_notes(ledger, pending, Role::Main),
            ),
            ("task", task),
        ],
    );
    text_message_params(&text)
}

/// The reviewer's turn (§3.2): the task, main's reply, and the open set the
/// reviewer must give a status for.
pub fn reviewer_turn_params(
    task: &str,
    round: u32,
    main_reply: &str,
    ledger: &Ledger,
    pending: &[HumanNote],
) -> serde_json::Value {
    let text = render_template(
        REVIEW_REVIEWER_PROMPT,
        &[
            ("round", &round.to_string()),
            ("open_findings", &render_open_findings(ledger)),
            (
                "binding_rulings",
                &render_binding_rulings(ledger, Role::Reviewer),
            ),
            (
                "human_notes",
                &render_human_notes(ledger, pending, Role::Reviewer),
            ),
            ("task", task),
            ("main_reply", main_reply),
        ],
    );
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
        let p = main_turn_params("fix the bug", 1, &Ledger::default(), &[]);
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
            main_turn_params("t", 4, &l, &[]),
            reviewer_turn_params("t", 4, "reply", &l, &[]),
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
            main_turn_params("t", 1, &Ledger::default(), &[]),
            reviewer_turn_params("t", 1, "reply", &Ledger::default(), &[]),
        ] {
            let text = message_text(&p).unwrap();
            assert!(!text.contains(REVIEW_BINDING_RULINGS_HEADER), "{text}");
            assert!(!text.contains("{binding_rulings}"), "{text}");
        }
    }

    #[test]
    fn reviewer_prompt_does_not_expand_placeholders_inside_main_reply() {
        let p = reviewer_turn_params("t", 2, "literal {task} here", &Ledger::default(), &[]);
        assert!(message_text(&p).unwrap().contains("literal {task} here"));
    }

    /// QA S1: a human ruling is binding text and must reach the agent
    /// byte-for-byte, even when it names a template placeholder.
    fn ledger_with_ruling_text(text: &str) -> Ledger {
        use super::super::schema::{ReviewPayload, RulingDecision};
        let mut l = ruled_ledger();
        // A proactive ruling on the same finding replaces the earlier one.
        l.apply(&ReviewPayload::Ruling {
            finding: "F1".into(),
            decision: RulingDecision::Fix,
            text: text.into(),
        })
        .unwrap();
        l
    }

    #[test]
    fn ruling_text_placeholders_survive_both_roles() {
        let ruling = "keep literal {task} and {main_reply} and {round} and {open_findings}";
        let l = ledger_with_ruling_text(ruling);
        for p in [
            main_turn_params("THE-TASK", 4, &l, &[]),
            reviewer_turn_params("THE-TASK", 4, "THE-REPLY", &l, &[]),
        ] {
            let text = message_text(&p).unwrap();
            assert!(text.contains(&format!("- F1 fix: {ruling}")), "{text}");
        }
    }

    #[test]
    fn task_placeholders_survive_reviewer_prompt() {
        let task = "document the {main_reply} and {binding_rulings} tokens";
        let p = reviewer_turn_params(task, 1, "THE-REPLY", &Ledger::default(), &[]);
        assert!(message_text(&p).unwrap().contains(task));
    }

    #[test]
    fn render_template_is_single_pass_and_keeps_unknown_braces() {
        assert_eq!(
            render_template("{a}|{b}|{c}|{", &[("a", "{b}"), ("b", "x")]),
            "{b}|x|{c}|{"
        );
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

    // ---- P3a-§6.2: human notes ----

    fn hn(text: &str, target: Option<Role>) -> HumanNote {
        HumanNote {
            text: text.into(),
            target,
        }
    }

    /// Messages rendered by `main` @ d60f20a7, before the note slot existed.
    ///
    /// Maintenance: these files are the frozen "no notes" output. A red
    /// golden test means the empty-note path changed the message — fix the
    /// template, not the file. Regenerate only when a template or constant
    /// change is intentional:
    /// `MUR_BLESS_WIRE_GOLDEN=1 cargo test -p mur-core --lib -- --ignored bless_wire_golden`
    /// then review the `.txt` diff in the PR. The files are versioned.
    const GOLDEN: [(&str, &str); 4] = [
        (
            "main_empty",
            include_str!("testdata/wire_golden/main_empty.txt"),
        ),
        (
            "main_ruled",
            include_str!("testdata/wire_golden/main_ruled.txt"),
        ),
        (
            "reviewer_empty",
            include_str!("testdata/wire_golden/reviewer_empty.txt"),
        ),
        (
            "reviewer_ruled",
            include_str!("testdata/wire_golden/reviewer_ruled.txt"),
        ),
    ];

    const GOLDEN_TASK: &str = "fix the {main_reply} bug";
    const GOLDEN_REPLY: &str = "done; see {open_findings}";

    /// The four no-note renders, in `GOLDEN` order.
    fn golden_cases() -> [serde_json::Value; 4] {
        let (task, reply) = (GOLDEN_TASK, GOLDEN_REPLY);
        let (empty, ruled) = (Ledger::default(), ruled_ledger());
        [
            main_turn_params(task, 1, &empty, &[]),
            main_turn_params(task, 4, &ruled, &[]),
            reviewer_turn_params(task, 1, reply, &empty, &[]),
            reviewer_turn_params(task, 4, reply, &ruled, &[]),
        ]
    }

    /// Rewrites `testdata/wire_golden/*.txt` from the current templates.
    /// Ignored, and a no-op unless `MUR_BLESS_WIRE_GOLDEN=1`, so a plain
    /// `--ignored` run cannot silently re-bless. See `GOLDEN` for when.
    #[test]
    #[ignore]
    fn bless_wire_golden() {
        if std::env::var("MUR_BLESS_WIRE_GOLDEN").as_deref() != Ok("1") {
            return;
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/cmd/fleet/review/testdata/wire_golden");
        for ((name, _), p) in GOLDEN.iter().zip(&golden_cases()) {
            std::fs::write(dir.join(format!("{name}.txt")), message_text(p).unwrap()).unwrap();
        }
    }

    /// With no unseen and no pending notes the message is byte-identical to
    /// the pre-3a one: no header, no blank line, no shifted separator.
    #[test]
    fn no_notes_message_is_byte_identical_to_before() {
        let (task, reply) = (GOLDEN_TASK, GOLDEN_REPLY);
        let empty = Ledger::default();
        let rendered = golden_cases();
        for ((name, golden), p) in GOLDEN.iter().zip(&rendered) {
            assert_eq!(message_text(p).unwrap(), *golden, "{name}");
        }
        // Pending notes for the other side only must not produce a block.
        let main_only = [hn("m", Some(Role::Main))];
        let rev_only = [hn("r", Some(Role::Reviewer))];
        assert_eq!(
            message_text(&main_turn_params(task, 1, &empty, &rev_only)).unwrap(),
            GOLDEN[0].1
        );
        assert_eq!(
            message_text(&reviewer_turn_params(task, 1, reply, &empty, &main_only)).unwrap(),
            GOLDEN[2].1
        );
    }

    /// Unseen first, then pending for `to` (broadcast or targeted), in
    /// order; other-side-only notes absent.
    #[test]
    fn notes_render_unseen_then_pending_for_the_recipient() {
        let mut l = Ledger::default();
        l.apply(&hn("u-both", None).into()).unwrap();
        l.apply(&hn("u-rev", Some(Role::Reviewer)).into()).unwrap();
        let pending = [
            hn("p-main", Some(Role::Main)),
            hn("p-both", None),
            hn("p-rev", Some(Role::Reviewer)),
        ];
        let main = main_turn_params("t", 1, &l, &pending);
        let main = message_text(&main).unwrap();
        let rev = reviewer_turn_params("t", 1, "reply", &l, &pending);
        let rev = message_text(&rev).unwrap();
        for (text, want, absent) in [
            (main, ["u-both", "p-main", "p-both"], "rev"),
            (rev, ["u-both", "u-rev", "p-both"], "main"),
        ] {
            let at: Vec<usize> = want
                .iter()
                .map(|w| text.find(&format!("- {w}\n")).expect(text))
                .collect();
            assert!(at.windows(2).all(|w| w[0] < w[1]), "{text}");
            assert!(!text.contains(&format!("p-{absent}")), "{text}");
            assert!(!text.contains(&format!("u-{absent}")), "{text}");
            assert!(text.contains(REVIEW_HUMAN_NOTES_HEADER), "{text}");
        }
    }

    /// The note block sits after the binding rulings, before the findings.
    #[test]
    fn note_block_sits_between_rulings_and_findings() {
        let l = ruled_ledger();
        let pending = [hn("keep it small", None)];
        for p in [
            main_turn_params("t", 4, &l, &pending),
            reviewer_turn_params("t", 4, "reply", &l, &pending),
        ] {
            let text = message_text(&p).unwrap();
            let ruling = text.find("- F1 fix: use the cache").expect(text);
            let header = text.find(REVIEW_HUMAN_NOTES_HEADER).expect(text);
            let note = text.find("- keep it small").expect(text);
            let finding = text.find("- F1 [high, open]").expect(text);
            assert!(ruling < header && header < note && note < finding, "{text}");
        }
    }

    /// AC-P3a-18: neither turn message asks the agent to justify not
    /// adopting a note (§0).
    #[test]
    fn no_justify_non_adoption_instruction() {
        let pending = [hn("n", None)];
        for p in [
            main_turn_params("t", 1, &Ledger::default(), &pending),
            reviewer_turn_params("t", 1, "r", &Ledger::default(), &pending),
        ] {
            let text = message_text(&p).unwrap().to_lowercase();
            for banned in [
                "justify",
                "explain why",
                "if you do not adopt",
                "if you don't",
            ] {
                assert!(!text.contains(banned), "{banned}: {text}");
            }
        }
    }

    /// Note text reaches the agent byte-for-byte, placeholder tokens too.
    #[test]
    fn note_text_placeholders_survive() {
        let pending = [hn("see {open_findings} and {task}", None)];
        let p = reviewer_turn_params("t", 1, "r", &Ledger::default(), &pending);
        assert!(
            message_text(&p)
                .unwrap()
                .contains("- see {open_findings} and {task}\n")
        );
    }
}
