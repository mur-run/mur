//! `@` mention parsing (at-mention design §3.5). Pure: no filesystem, no
//! resolution — see [`super::mention_resolve`] for turning a [`Mention`] into
//! a delivery target.

/// A parsed `@name[#N] rest`, `@agent:name[#N] rest`, or `@fleet:name rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// Explicit namespace prefix, `None` for the bare `@name` form.
    pub namespace: Option<Namespace>,
    /// Target name as typed (case preserved; resolution folds agent names).
    pub name: String,
    /// Conversation ordinal from `#N`; always `>= 1` when present.
    pub ordinal: Option<u64>,
    /// The message after the mention token, leading whitespace removed.
    pub rest: String,
}

/// The explicit `@<namespace>:` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Agent,
    Fleet,
}

/// Words that cannot name an agent, fleet, or host (§3.5). The `:` form is
/// shared with the Phase 5 `@host:agent` surface, and `@all` is reserved for
/// broadcast.
pub const RESERVED: &[&str] = &["agent", "fleet", "all"];

/// Why a line starting with `@` is not a usable mention.
#[derive(Debug, PartialEq, Eq)]
pub enum MentionError {
    /// The mention token is syntactically invalid; the string says why.
    Malformed(String),
    /// The target is a reserved word (`@agent`, `@fleet`, `@all`).
    ReservedName(String),
}

/// `@name[#N] rest`, `@agent:name[#N] rest`, `@fleet:name rest`. Returns None
/// when `line` does not start with `@`. `@fleet:x#3` is `Err(Malformed)`.
pub fn parse_mention(line: &str) -> Option<Result<Mention, MentionError>> {
    let body = line.strip_prefix('@')?;
    let (token, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    Some(
        parse_token(token).map(|(namespace, name, ordinal)| Mention {
            namespace,
            name: name.to_string(),
            ordinal,
            rest: rest.trim_start().to_string(),
        }),
    )
}

/// Whether `name` is one of [`RESERVED`] (ASCII case-insensitive, matching
/// how agent names resolve).
pub fn is_reserved(name: &str) -> bool {
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name))
}

type Parts<'a> = (Option<Namespace>, &'a str, Option<u64>);

/// Split `[ns:]name[#N]` (the text after `@`, up to the first whitespace).
fn parse_token(token: &str) -> Result<Parts<'_>, MentionError> {
    let malformed = |why: &str| Err(MentionError::Malformed(format!("@{token}: {why}")));
    let (head, ordinal) = match token.split_once('#') {
        None => (token, None),
        // `#0` parses: it is the index's "not numbered" sentinel and resolves
        // to UNKNOWN_CONVERSATION (spec §3.5), not a malformed mention.
        Some((head, n)) => match n.parse::<u64>() {
            Ok(n) => (head, Some(n)),
            Err(_) => return malformed("`#` must be followed by a conversation number (1, 2, …)"),
        },
    };
    let (namespace, name) = match head.split_once(':') {
        None => (None, head),
        Some((prefix, name)) => {
            let ns = if prefix.eq_ignore_ascii_case("agent") {
                Namespace::Agent
            } else if prefix.eq_ignore_ascii_case("fleet") {
                Namespace::Fleet
            } else {
                return malformed("only `@agent:` and `@fleet:` prefixes are supported");
            };
            (Some(ns), name)
        }
    };
    if name.is_empty() || name.contains(':') {
        return malformed("expected `@name`, `@agent:name`, or `@fleet:name`");
    }
    if namespace == Some(Namespace::Fleet) && ordinal.is_some() {
        return malformed("a fleet has no conversation number; drop the `#N`");
    }
    if is_reserved(name) {
        return Err(MentionError::ReservedName(name.to_string()));
    }
    Ok((namespace, name, ordinal))
}

#[cfg(test)]
#[path = "mention_tests.rs"]
mod tests;
