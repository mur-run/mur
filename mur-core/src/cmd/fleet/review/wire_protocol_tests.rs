//! #1785: main's §3.4 responses block is review-loop protocol, not part of
//! the deliverable. The reviewer must see main's work without it, and the
//! parsed answers as a separate, clearly labelled metadata section.

use super::constants::{REVIEW_MAIN_PROMPT, REVIEW_MAIN_RESPONSES_HEADER, REVIEW_REVIEWER_PROMPT};
use super::ledger::Ledger;
use super::schema::RebuttalAnswer;
use super::wire::{message_text, reviewer_turn_params, split_responses_block};

const BLOCK: &str = "```json\n{\"responses\": [{\"id\": \"F1\", \"answer\": \"accept\"}, \
                     {\"id\": \"F2\", \"answer\": \"partial\", \"reason\": \"kept it short\"}]}\n```";

fn reviewer_text(main_reply: &str) -> String {
    message_text(&reviewer_turn_params(
        "t",
        2,
        main_reply,
        &Ledger::default(),
        &[],
    ))
    .unwrap()
    .to_string()
}

#[test]
fn trailing_responses_block_is_split_off() {
    let reply = format!("Hello there.\n\n{BLOCK}\n  \n");
    let (deliverable, responses) = split_responses_block(&reply);
    assert_eq!(deliverable, "Hello there.");
    let responses = responses.expect("responses parsed");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0].id, "F1");
    assert_eq!(responses[1].answer, RebuttalAnswer::Partial);
}

#[test]
fn no_block_passes_through_unchanged() {
    let (deliverable, responses) = split_responses_block("Hello there.");
    assert_eq!(deliverable, "Hello there.");
    assert!(responses.is_none());
}

#[test]
fn block_followed_by_prose_is_not_stripped() {
    let reply = format!("Hello.\n{BLOCK}\nand one more thing");
    let (deliverable, responses) = split_responses_block(&reply);
    assert_eq!(deliverable, reply);
    assert!(responses.is_none());
}

#[test]
fn unterminated_fence_passes_through_unchanged() {
    let reply = "Hello.\n```json\n{\"responses\": []}\n";
    let (deliverable, responses) = split_responses_block(reply);
    assert_eq!(deliverable, reply);
    assert!(responses.is_none());
}

#[test]
fn json_only_reply_falls_back_to_original() {
    let (deliverable, responses) = split_responses_block(BLOCK);
    assert_eq!(
        deliverable, BLOCK,
        "the reviewer must never get an empty reply"
    );
    assert!(responses.is_some(), "the answers are still metadata");
}

/// A task whose deliverable IS a trailing JSON block must keep it: only a
/// block shaped like `{"responses": [...]}` is protocol.
#[test]
fn trailing_non_responses_json_is_deliverable() {
    let reply = "Here is the config:\n```json\n{\"name\": \"x\"}\n```";
    let (deliverable, responses) = split_responses_block(reply);
    assert_eq!(deliverable, reply);
    assert!(responses.is_none());
}

#[test]
fn only_the_last_block_is_considered() {
    let reply = format!("Example:\n{BLOCK}\nthen:\n```json\n{{\"name\": \"x\"}}\n```");
    let (deliverable, responses) = split_responses_block(&reply);
    assert_eq!(deliverable, reply);
    assert!(responses.is_none());
}

#[test]
fn reviewer_does_not_see_the_fenced_block() {
    let text = reviewer_text(&format!("Hello there.\n{BLOCK}"));
    assert!(text.contains("Hello there."));
    assert!(!text.contains("\"responses\""), "{text}");
}

#[test]
fn reviewer_gets_answers_as_a_labelled_metadata_section() {
    let text = reviewer_text(&format!("Hello there.\n{BLOCK}"));
    let at = text
        .find(REVIEW_MAIN_RESPONSES_HEADER)
        .expect("metadata section present");
    let section = &text[at..];
    assert!(section.contains("F1: accept"), "{section}");
    assert!(section.contains("F2: partial — kept it short"), "{section}");
    assert!(
        text.find("Hello there.").unwrap() < at,
        "the deliverable comes first, the metadata after it"
    );
}

#[test]
fn no_responses_no_metadata_section() {
    let text = reviewer_text("Hello there.");
    assert!(!text.contains(REVIEW_MAIN_RESPONSES_HEADER));
}

#[test]
fn reviewer_prompt_says_the_section_is_metadata_not_work() {
    assert!(REVIEW_REVIEWER_PROMPT.contains("{main_responses}"));
    assert!(REVIEW_MAIN_RESPONSES_HEADER.contains("not part of the deliverable"));
    assert!(
        REVIEW_REVIEWER_PROMPT.contains("protocol, not the deliverable"),
        "the reviewer must be told where protocol ends and the work begins"
    );
}

#[test]
fn reviewer_prompt_requires_self_obtained_evidence_for_approve() {
    // #1782: a reviewer approved citing line numbers it had not checked.
    assert!(REVIEW_REVIEWER_PROMPT.contains("evidence you obtained yourself in this turn"));
    assert!(REVIEW_REVIEWER_PROMPT.contains("cannot reproduce a citation"));
}

#[test]
fn main_prompt_scopes_the_summary_as_protocol() {
    assert!(
        REVIEW_MAIN_PROMPT.contains("protocol, not the deliverable"),
        "main must be told the summary and JSON are loop protocol"
    );
}
