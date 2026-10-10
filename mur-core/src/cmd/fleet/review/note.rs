//! P3a-§3: the send prompt's note lines — `/note <text>`, `@<agent>
//! <text>`, and the unknown-slash hint (N10). `/rule` is not handled here;
//! the session checks it first and this parser reports it as
//! [`NoteLine::NotNote`].
//!
//! `@<agent>` is resolved here, in the session (N11): the alias map first,
//! then the caller's resolver, whose answer must equal one of the two
//! session members — a resolver returns its input unchanged when nothing
//! matches, so its answer alone is not proof of membership.

use std::collections::BTreeSet;

use super::constants::{
    COMMAND_PREFIX, MURMUR_STOP_COMMAND, NOTE_COMMAND, NOTE_USAGE_HINT, RULE_COMMAND,
    TARGET_ALIAS_MAIN, TARGET_ALIAS_REVIEWER, TARGET_NOT_FOUND_HINT, TARGET_NOTE_PREFIX,
    TARGET_NOTE_USAGE_HINT, UNKNOWN_COMMAND_HINT,
};
use super::driver::SendAnswer;
use super::ruling::{is_rule_command, parse_rule_command};
use super::schema::{HumanNote, Role};
use super::session::is_send_answer;

/// Where the send-prompt line came from (P3b-§5.3). `Stdin` is the terminal
/// session and is byte-for-byte what P3a shipped; `Murmur` is the in-app
/// review, whose input box has no way to "ask again" for an unknown name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMode {
    Stdin,
    Murmur,
}

/// What one send-prompt line means for notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteLine {
    /// A note to queue; the prompt asks again.
    Note(HumanNote),
    /// Print this and ask again; nothing is queued.
    Hint(String),
    /// Not a note line: `/rule`, Enter, `y`, `q`, plain text.
    NotNote,
}

/// Parse one send-prompt line. `members` is `[main, reviewer]`; `resolve`
/// is `canonicalize_agent_name(mur_home, _)` in production.
pub fn parse_note_line(
    line: &str,
    members: &[String; 2],
    resolve: impl Fn(&str) -> String,
) -> NoteLine {
    parse_note_line_mode(line, members, LineMode::Stdin, resolve)
}

/// [`parse_note_line`] for either input surface. The only difference:
/// `@<unknown> <text>` is an N3 hint on `Stdin` and a broadcast note of the
/// whole typed line on `Murmur` (P3b-§5.2, Q11).
pub fn parse_note_line_mode(
    line: &str,
    members: &[String; 2],
    mode: LineMode,
    resolve: impl Fn(&str) -> String,
) -> NoteLine {
    let line = line.trim();
    let (head, rest) = split_head(line);
    if let Some(name) = head.strip_prefix(TARGET_NOTE_PREFIX) {
        return match target_note(name, rest, members, resolve) {
            NoteLine::Hint(_)
                if mode == LineMode::Murmur && !name.is_empty() && !rest.is_empty() =>
            {
                NoteLine::Note(HumanNote {
                    text: line.to_string(),
                    target: None,
                })
            }
            other => other,
        };
    }
    if !head.starts_with(COMMAND_PREFIX) || head == RULE_COMMAND {
        return NoteLine::NotNote;
    }
    if head != NOTE_COMMAND {
        return NoteLine::Hint(UNKNOWN_COMMAND_HINT.replace("{command}", head));
    }
    if rest.is_empty() {
        return NoteLine::Hint(NOTE_USAGE_HINT.to_string());
    }
    NoteLine::Note(HumanNote {
        text: rest.to_string(),
        target: None,
    })
}

/// P3b-§5.3: the name in `@<name> <text>` that MURMUR will broadcast as a
/// general note — exactly the lines `parse_note_line_mode(Murmur)` turns from
/// a not-found hint into a broadcast. `None` for every other line, and the
/// resolver is not called for a line that is not `@<name> <text>`.
pub fn unknown_target(
    line: &str,
    members: &[String; 2],
    resolve: impl Fn(&str) -> String,
) -> Option<String> {
    let (head, rest) = split_head(line);
    let name = head.strip_prefix(TARGET_NOTE_PREFIX)?;
    if name.is_empty()
        || rest.is_empty()
        || name == TARGET_ALIAS_MAIN
        || name == TARGET_ALIAS_REVIEWER
    {
        return None;
    }
    member_role(&resolve(name), members)
        .is_none()
        .then(|| name.to_string())
}

/// `(first word, the rest trimmed)` of a trimmed send-prompt line.
fn split_head(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    }
}

fn target_note(
    name: &str,
    text: &str,
    members: &[String; 2],
    resolve: impl Fn(&str) -> String,
) -> NoteLine {
    if name.is_empty() || text.is_empty() {
        return NoteLine::Hint(TARGET_NOTE_USAGE_HINT.to_string());
    }
    let target = match name {
        TARGET_ALIAS_MAIN => Some(Role::Main),
        TARGET_ALIAS_REVIEWER => Some(Role::Reviewer),
        _ => member_role(&resolve(name), members),
    };
    match target {
        Some(role) => NoteLine::Note(HumanNote {
            text: text.to_string(),
            target: Some(role),
        }),
        None => NoteLine::Hint(TARGET_NOT_FOUND_HINT.replace("{name}", name)),
    }
}

/// The role whose member name is `canonical`. Exact match first; then
/// ASCII case-insensitively, the same rule `canonicalize_agent_name` uses —
/// on a case-insensitive filesystem (default macOS APFS) its exact-match
/// branch succeeds for `Reviewer` and returns the input as typed, so an
/// exact comparison alone would call a member `<unknown>`.
fn member_role(canonical: &str, members: &[String; 2]) -> Option<Role> {
    let roles = || [Role::Main, Role::Reviewer].into_iter().zip(members);
    if let Some(role) = roles().find_map(|(role, m)| (m == canonical).then_some(role)) {
        return Some(role);
    }
    // Case-insensitive fallback only when it is unambiguous: members that
    // differ only in case must not resolve by list order.
    let mut folded = roles().filter(|(_, m)| m.eq_ignore_ascii_case(canonical));
    match (folded.next(), folded.next()) {
        (Some((role, _)), None) => Some(role),
        _ => None,
    }
}

/// One send-prompt line → the driver's answer (P3b-§5.2, D2), or the hint to
/// print before asking again. The UI thread calls this with `Murmur`; `Stdin`
/// decides exactly as `TerminalGate::confirm_send` does, but that gate is not
/// refactored onto it in 3b (stdin stays byte-identical).
pub(crate) fn send_answer_for(
    line: &str,
    members: &[String; 2],
    open: &BTreeSet<String>,
    mode: LineMode,
    resolve: impl Fn(&str) -> String,
) -> Result<SendAnswer, String> {
    if is_rule_command(line.trim()) {
        return parse_rule_command(line, open).map(SendAnswer::SendWithRuling);
    }
    if mode == LineMode::Murmur && line.trim() == MURMUR_STOP_COMMAND {
        return Ok(SendAnswer::Stop);
    }
    match parse_note_line_mode(line, members, mode, resolve) {
        NoteLine::Note(note) => Ok(SendAnswer::Note(note)),
        NoteLine::Hint(hint) => Err(hint),
        NoteLine::NotNote => Ok(match mode {
            LineMode::Stdin if is_send_answer(line) => SendAnswer::Send,
            LineMode::Stdin => SendAnswer::Stop,
            LineMode::Murmur if is_murmur_send(line) => SendAnswer::Send,
            LineMode::Murmur => SendAnswer::Note(HumanNote {
                text: line.trim().to_string(),
                target: None,
            }),
        }),
    }
}

/// MURMUR's Enter / `y` / `yes`. There is no EOF in an input box, so an
/// empty line is consent here (it is the Enter key) and never on stdin.
pub(crate) fn is_murmur_send(line: &str) -> bool {
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}
