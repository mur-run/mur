//! Named constants for the review loop (CLAUDE.md rule 2 / spec AC22: no
//! hardcoded values — timings, retry delay, and countdown values are config
//! or constants, not literals).

use std::time::Duration;

/// §7.1, Q6, P3: the fixed prefix that identifies a review-session fleet.
/// User-created fleets may not use this prefix (§7.1), so the prefix alone
/// reliably distinguishes a review session from an ordinary one.
pub const REVIEW_FLEET_PREFIX: &str = "review-";

/// §5: default countdown before an auto-mode send goes out. Any key during
/// the countdown drops back to semi-auto.
#[allow(dead_code)] // not wired yet: §5 auto mode (MURMUR)
pub const AUTO_COUNTDOWN_DEFAULT: Duration = Duration::from_secs(3);
/// §5: the countdown is never shorter than this, even if configured lower.
/// A configured value below this is clamped to it with a warning (§5).
#[allow(dead_code)] // not wired yet: §5 auto mode (MURMUR)
pub const AUTO_COUNTDOWN_MIN: Duration = Duration::from_millis(1500);

/// §8.1: one retry after this delay on A2A send failure / peer offline,
/// before the session pauses and reverts to semi-auto.
pub const TRANSPORT_RETRY_DELAY: Duration = Duration::from_secs(5);

/// §3.3: round-stuck fires when the open finding set (IDs + statuses) is
/// unchanged across this many consecutive rounds.
pub const ROUND_STUCK_AFTER_UNCHANGED_ROUNDS: u32 = 2;

/// §3.4: a finding rejected this many times by the main agent triggers
/// automatic escalation to the human.
#[allow(dead_code)] // not wired yet: §3.4 rebuttal/escalation
pub const REJECT_ESCALATION_THRESHOLD: u32 = 2;

/// §3.2 / §3.4: a malformed verdict or rebuttal gets this many retries
/// (with a validation hint) before the system treats it as `blocked`.
pub const MALFORMED_RESPONSE_RETRIES: u32 = 1;

/// §3.2 / §3.4: the validation hint appended to a re-sent turn after a
/// malformed reply. Placeholder: `{problem}` (the validator's one-line reason).
pub const REVIEW_VALIDATION_HINT: &str = "

Your previous reply could not be accepted: {problem}. Reply again, ending with exactly one fenced ```json block of the required shape.";

/// §8.2 "Restarting round N+1" — the fixed plain-text note prepended to
/// every A2A request of the restarted round, verbatim (AC11e: a grep must
/// find this text in exactly one non-test source location). Do NOT
/// reformat this string; AC11e pins it byte-for-byte.
#[allow(dead_code)] // not wired yet: §8.2 resume
pub const REVIEW_ROUND_RESTART_NOTE: &str = "This round was interrupted and restarted. A previous attempt may have reached you. Re-read the current workspace state before responding; do not assume your last-seen state is current.";

/// §8.2 fatal case — the fixed marker filename inside a channel's own
/// directory (`<MUR_HOME>/channels/<channel id>/corrupted.json`), next to
/// `events.jsonl`. The builder does not choose this path.
pub const REVIEW_CORRUPTED_MARKER_FILE: &str = "corrupted.json";

/// §8.2 — the fixed reason string stamped into the corrupted marker and
/// into the `session_stopped` event when a session can never be resumed.
#[allow(dead_code)] // not wired yet: §8.2 resume
pub const REVIEW_STOP_REASON_CORRUPTED: &str = "corrupted";

/// §8.2 Continue path — the `session_stopped` reason when a human chooses
/// Abandon instead of Continue after a partial-damage replay.
#[allow(dead_code)] // not wired yet: §8.2 resume
pub const REVIEW_STOP_REASON_REPLAY_FAILED: &str = "replay_failed";

/// §3.1: main's turn prompt. Placeholders: `{task}`, `{round}`,
/// `{open_findings}` (a rendered list, or [`REVIEW_NO_OPEN_FINDINGS`]).
pub const REVIEW_MAIN_PROMPT: &str = "You are the main agent in a review loop (round {round}).

Task:
{task}

Open review findings from the reviewer:
{open_findings}

Do the task, or revise your previous work to address the open findings, then summarise what you changed. If any findings are listed above, end your reply with exactly one fenced ```json block answering every one of them:
{\"responses\": [{\"id\": \"F1\", \"answer\": \"accept\" | \"reject\" | \"partial\", \"reason\": \"...\"}]}
A reason is required for reject and partial.";

/// §3.2: the reviewer's turn prompt. Placeholders: `{task}`, `{round}`,
/// `{main_reply}`, `{open_findings}`. It pins the verdict wire shape and
/// tells the reviewer to return `blocked` when uncertain (§3.2).
pub const REVIEW_REVIEWER_PROMPT: &str = "You are the reviewer in a review loop (round {round}).

Task the main agent is working on:
{task}

The main agent's latest reply:
{main_reply}

Previously issued findings that are still open:
{open_findings}

Review the work. End your reply with exactly one fenced ```json block of this shape:
{\"verdict\": \"approve\" | \"revise\" | \"blocked\", \"findings\": [{\"severity\": \"high\" | \"medium\" | \"low\", \"issue\": \"...\"}], \"prior\": [{\"id\": \"F1\", \"status\": \"open\" | \"withdrawn\" | \"resolved\" | \"disputed\", \"reason\": \"...\"}]}

Rules: `findings` lists NEW findings only; never invent IDs, the system assigns them. `prior` must give a status for every finding listed above. If you are uncertain, return \"blocked\" rather than guess.";

/// Rendered in place of `{open_findings}` when the open set is empty.
pub const REVIEW_NO_OPEN_FINDINGS: &str = "(none)";

/// The file whose presence `a2a_dial` treats as "the agent is up". The
/// session preflight checks the same file so it agrees with the first send.
pub const RUNNING_LOCK: &str = "running.lock";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_note_matches_the_spec_text_byte_for_byte() {
        // §8.2 "Restart note text (normative; must match byte-for-byte)".
        assert_eq!(
            REVIEW_ROUND_RESTART_NOTE,
            "This round was interrupted and restarted. A previous attempt may have reached you. Re-read the current workspace state before responding; do not assume your last-seen state is current."
        );
    }

    #[test]
    fn countdown_min_is_never_zero_and_default_is_above_min() {
        assert!(AUTO_COUNTDOWN_MIN > Duration::ZERO);
        assert!(AUTO_COUNTDOWN_DEFAULT >= AUTO_COUNTDOWN_MIN);
    }

    #[test]
    fn review_prefix_is_fixed() {
        assert_eq!(REVIEW_FLEET_PREFIX, "review-");
    }
}
