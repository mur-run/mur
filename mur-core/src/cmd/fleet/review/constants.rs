//! Named constants for the review loop (CLAUDE.md rule 2 / spec AC22: no
//! hardcoded values — timings, retry delay, and countdown values are config
//! or constants, not literals).

use std::time::Duration;

/// §7.1, Q6, P3: the fixed prefix that identifies a review-session fleet.
/// User-created fleets may not use this prefix (§7.1), so the prefix alone
/// reliably distinguishes a review session from an ordinary one.
pub const REVIEW_FLEET_PREFIX: &str = "review-";

/// Stop-screen warning prefix when `approve` ends a session while a high
/// finding is still `open` (#1721, option B: warn, do not block).
pub const OPEN_HIGH_APPROVE_WARNING: &str = "WARNING: approved with open high-severity findings:";

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

/// P2-§5 `/rule` grammar: `/rule drop|fix F<n> <text>`.
pub const RULE_COMMAND: &str = "/rule";
pub const RULE_DECISION_DROP: &str = "drop";
pub const RULE_DECISION_FIX: &str = "fix";
/// P2-§5.2 / R8: abandon is an explicit command, never a single key.
pub const ABANDON_COMMAND: &str = "/abandon";
/// P2-§5.2: `q` (case-insensitive) leaves the session paused.
pub const LEAVE_PAUSED_KEY: &str = "q";
pub const RULE_USAGE_HINT: &str = "usage: /rule drop|fix F<n> <text>";
/// P2-§5.4; `{id}` is replaced with the finding the human named.
pub const RULE_NOT_OPEN_HINT: &str = "{id} is not an open finding";
/// Shown at the ruling prompt for any other input (including bare Enter).
pub const RULING_PROMPT_HINT: &str =
    "type /rule drop|fix F<n> <text>, /abandon, or q to leave the session paused";
/// P3a-§3 `/note <text>`: a note to both sides.
pub const NOTE_COMMAND: &str = "/note";
pub const NOTE_USAGE_HINT: &str = "usage: /note <text>";
/// P3a-§3 `@<agent> <text>`: a note to one side.
pub const TARGET_NOTE_PREFIX: char = '@';
pub const TARGET_NOTE_USAGE_HINT: &str = "usage: @<agent> <text>";
/// P3a N11: fixed aliases, checked before any name lookup.
pub const TARGET_ALIAS_MAIN: &str = "主";
pub const TARGET_ALIAS_REVIEWER: &str = "審查";
/// P3a N3; `{name}` is the agent as typed. Never broadcast instead.
pub const TARGET_NOT_FOUND_HINT: &str =
    "agent {name} not found; use /note <text> to send it to both sides";
/// P3a N10; `{command}` is the slash word as typed. Never a stop.
pub const UNKNOWN_COMMAND_HINT: &str = "unknown command: {command}";
/// P3a-§5.3: `paused { kind: other }` reason when appending pending note
/// `recorded + 1` of `total` fails. `{recorded}`, `{total}`, `{cause}`.
pub const NOTE_FLUSH_FAILED_REASON: &str = "note flush failed after {recorded} of {total} notes; the {recorded} already recorded will be sent on resume, the rest were not recorded: {cause}";
/// Every send-prompt command starts with this (P3a N10).
pub const COMMAND_PREFIX: char = '/';
/// P2-§5.3 send prompt; `{member}` is replaced with the recipient.
/// The `/rule` hint says the line also sends this turn, so the human knows
/// before typing it (the line is the send consent).
pub const SEND_PROMPT: &str = "Send to {member}? [Enter = send, q = stop, /rule drop|fix F<n> <text> = record a ruling and send this turn, /note <text> = note to both, @<agent> <text> = note to one] ";
/// P2-§5.3: printed before main's message rebuilt after a `/rule` at its
/// send prompt; the full rebuilt message follows.
pub const RULING_REGENERATED_BANNER: &str = "[ruling applied; message regenerated]";
/// P2-§5.3 / AC-P2-18: a held ruling whose finding left the open set.
/// `{id}` = the finding, `{status}` = its status now.
pub const RULING_DISCARDED_CLOSED_NOTICE: &str =
    "Ruling on {id} discarded: finding is already {status}.";
/// P2-§5.3 / AC-P2-19: a held ruling with no round left to govern.
/// `{verdict}` ∈ [`RULING_SESSION_END_APPROVE`], [`RULING_SESSION_END_BLOCKED`].
pub const RULING_DISCARDED_SESSION_END_NOTICE: &str =
    "Ruling on {id} discarded: session ended with {verdict}.";
pub const RULING_SESSION_END_APPROVE: &str = "approve";
pub const RULING_SESSION_END_BLOCKED: &str = "blocked";
/// P2-§5.2: the `paused { kind: escalation }` reason.
pub const REVIEW_PAUSE_REASON_ESCALATION: &str = "awaiting a ruling";
/// P2-§5.1 `/abandon`: the `session_stopped` reason (P1 wire value).
pub const REVIEW_STOP_REASON_ESCALATION: &str = "escalation";
/// P2-§5.1 step 3: the ruling prompt; `{id}` is replaced with the finding.
pub const RULING_PROMPT: &str =
    "Awaiting ruling on {id} — /rule drop|fix {id} <text>, /abandon, q = leave paused ";
/// P2-§5.1 step 3: both sides' last positions, shown above
/// [`RULING_PROMPT`]. `{id}`, `{reason}` (why it escalated), `{issue}` (the
/// reviewer's latest reason, falling back to its finding) and `{main}`
/// (main's last reject reason).
pub const RULING_POSITIONS: &str =
    "\n--- {id} awaits your ruling ({reason}) ---\n  reviewer: {issue}\n  main: {main}\n";
/// Stands in for `{main}` when main's reject carried no reason.
pub const RULING_NO_MAIN_REASON: &str = "(no reason given)";
/// P2-§5.3: rebuttal retry hint when main rejects a finding ruled `fix`.
/// `{id}` is replaced with the finding ID.
pub const REVIEW_FIX_RULED_REJECT_HINT: &str =
    "finding {id} was ruled `fix` by the human; answer accept or partial, not reject";

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
/// `{open_findings}` (a rendered list, or [`REVIEW_NO_OPEN_FINDINGS`]),
/// `{binding_rulings}` (P2-§5.3; a [`REVIEW_BINDING_RULINGS_HEADER`] block
/// ending in a blank line, or empty), `{human_notes}` (P3a-§6.2; a
/// [`REVIEW_HUMAN_NOTES_HEADER`] block ending in a blank line, or empty).
pub const REVIEW_MAIN_PROMPT: &str = "You are the main agent in a review loop (round {round}).

Task:
{task}

{binding_rulings}{human_notes}Open review findings from the reviewer:
{open_findings}

Do the task, or revise your previous work to address the open findings, then summarise what you changed. If any findings are listed above, end your reply with exactly one fenced ```json block answering every one of them:
{\"responses\": [{\"id\": \"F1\", \"answer\": \"accept\" | \"reject\" | \"partial\", \"reason\": \"...\"}]}
A reason is required for reject and partial.";

/// §3.2: the reviewer's turn prompt. Placeholders: `{task}`, `{round}`,
/// `{main_reply}`, `{open_findings}`, `{binding_rulings}`, `{human_notes}` (as in
/// [`REVIEW_MAIN_PROMPT`]). It pins the verdict wire shape and
/// tells the reviewer to return `blocked` when uncertain (§3.2).
pub const REVIEW_REVIEWER_PROMPT: &str = "You are the reviewer in a review loop (round {round}).

Task the main agent is working on:
{task}

The main agent's latest reply:
{main_reply}

{binding_rulings}{human_notes}Previously issued findings that are still open:
{open_findings}

Review the work. End your reply with exactly one fenced ```json block of this shape:
{\"verdict\": \"approve\" | \"revise\" | \"blocked\", \"findings\": [{\"severity\": \"high\" | \"medium\" | \"low\", \"issue\": \"...\"}], \"prior\": [{\"id\": \"F1\", \"status\": \"open\" | \"withdrawn\" | \"resolved\" | \"disputed\", \"reason\": \"...\"}]}

Rules: `findings` lists NEW findings only; never invent IDs, the system assigns them. `prior` must give a status for every finding listed above. `approve` is refused while any high-severity finding is `disputed`. If you are uncertain, return \"blocked\" rather than guess.";

/// P2-§5.3: heads the binding-ruling block, ranked above the findings.
pub const REVIEW_BINDING_RULINGS_HEADER: &str =
    "Binding rulings from the human (these override any finding below; do not argue them):";
/// P3a-§6.2: heads the human-note block. Notes outrank findings (P1-§6)
/// but are guidance, not rulings: no instruction to justify non-adoption.
pub const REVIEW_HUMAN_NOTES_HEADER: &str =
    "Notes from the human (these take priority over the findings below):";

/// Rendered in place of `{open_findings}` when the open set is empty.
pub const REVIEW_NO_OPEN_FINDINGS: &str = "(none)";

/// `ChannelService::create_for_fleet` names a fleet's channel
/// `fleet-<fleet name>`; review code maps a channel back to its session
/// through this prefix.
pub const FLEET_CHANNEL_PREFIX: &str = "fleet-";

/// §7.0 run lock: the file a review driver holds an exclusive advisory lock
/// on for its whole life, inside the session's channel directory. Distinct
/// from the per-agent [`RUNNING_LOCK`]. Liveness is "can the lock be
/// taken", never the pid written in it (display only).
pub const DRIVER_LOCK_FILE: &str = "driver.lock";

/// §7.0 — the lock holder's self-description (pid, start time, host), a
/// sibling of [`DRIVER_LOCK_FILE`]. Kept out of the lock file because
/// Windows `LockFileEx` is mandatory: while held, no other handle can read
/// the locked file's bytes. Display only, never consulted for liveness.
pub const DRIVER_OWNER_FILE: &str = "driver.owner";

/// §7.0 — the `paused` reason resume records before continuing a session
/// whose driver died without pausing.
pub const REVIEW_PAUSE_REASON_CRASHED: &str = "crashed";

/// `paused.reason` when the human aborts an in-flight turn with Esc×2
/// (P3b-§6.3).
pub const REVIEW_PAUSE_REASON_ABORTED: &str = "turn aborted by the human (Esc Esc)";

/// P2-§6 row 3 / P1 §7: the continue prompt of a plain paused session.
pub const REVIEW_PAUSED_CONTINUE_PROMPT: &str =
    "Paused — continue? [Enter = continue, q = leave paused] ";

/// P2-§6 row 2: a ruling is the last thing recorded and nothing is owed.
pub const RULING_RECORDED_CONTINUE_PROMPT: &str =
    "Ruling recorded — continue? [Enter = continue, q = leave paused] ";

/// Printed when the human leaves a session paused at a resume prompt.
pub const REVIEW_LEFT_PAUSED_NOTICE: &str = "Left paused.";

/// §7.1 — the `session_stopped` reason written by `mur fleet delete review-…`.
pub const REVIEW_STOP_REASON_DELETED: &str = "deleted";

/// §7.1 — the `session_stopped` reason prune writes before removing a paused
/// or crashed session.
pub const REVIEW_STOP_REASON_PRUNED: &str = "pruned";

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
