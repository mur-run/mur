//! P3a-§3: the send prompt's note lines — `/note <text>`, `@<agent>
//! <text>`, and the unknown-slash hint (N10). `/rule` is not handled here;
//! the session checks it first and this parser reports it as
//! [`NoteLine::NotNote`].
//!
//! `@<agent>` is resolved here, in the session (N11): the alias map first,
//! then the caller's resolver, whose answer must equal one of the two
//! session members — a resolver returns its input unchanged when nothing
//! matches, so its answer alone is not proof of membership.

use super::constants::{
    COMMAND_PREFIX, NOTE_COMMAND, NOTE_USAGE_HINT, RULE_COMMAND, TARGET_ALIAS_MAIN,
    TARGET_ALIAS_REVIEWER, TARGET_NOT_FOUND_HINT, TARGET_NOTE_PREFIX, TARGET_NOTE_USAGE_HINT,
    UNKNOWN_COMMAND_HINT,
};
use super::schema::{HumanNote, Role};

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
    let line = line.trim();
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    };
    if let Some(name) = head.strip_prefix(TARGET_NOTE_PREFIX) {
        return target_note(name, rest, members, resolve);
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
