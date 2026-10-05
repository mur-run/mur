//! P2-§5: the `/rule` input grammar, shared by the ruling prompt (§5.1)
//! and the send prompt (§5.3). Pure: no I/O, no ledger mutation.

use std::collections::BTreeSet;

use super::constants::{
    ABANDON_COMMAND, LEAVE_PAUSED_KEY, RULE_COMMAND, RULE_DECISION_DROP, RULE_DECISION_FIX,
    RULE_NOT_OPEN_HINT, RULE_USAGE_HINT, RULING_PROMPT_HINT,
};
use super::schema::RulingDecision;

/// A parsed `/rule` line, validated against the open set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulingInput {
    pub finding: String,
    pub decision: RulingDecision,
    pub text: String,
}

/// What one line typed at the ruling prompt means (P2-§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptLine {
    Rule(RulingInput),
    Abandon,
    /// `q` or EOF: leave the session paused (R8).
    Leave,
    /// Anything else: print the hint and re-prompt.
    Other(String),
}

/// Classify one line read at the ruling prompt. `raw.is_empty()` is EOF
/// (no newline was read) and leaves the session paused, like `q`; a bare
/// Enter is `"\n"` and is not leave. `open` = the ledger's open-set IDs.
pub fn classify_ruling_line(raw: &str, open: &BTreeSet<String>) -> PromptLine {
    if raw.is_empty() {
        return PromptLine::Leave;
    }
    let line = raw.trim();
    if line.eq_ignore_ascii_case(LEAVE_PAUSED_KEY) {
        return PromptLine::Leave;
    }
    if line == ABANDON_COMMAND {
        return PromptLine::Abandon;
    }
    if is_rule_command(line) {
        return match parse_rule_command(line, open) {
            Ok(input) => PromptLine::Rule(input),
            Err(hint) => PromptLine::Other(hint),
        };
    }
    PromptLine::Other(RULING_PROMPT_HINT.to_string())
}

/// True when `line` is a `/rule` command (well-formed or not), so a caller
/// such as the send prompt knows to route it here rather than treat it as
/// a plain answer.
pub fn is_rule_command(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some(RULE_COMMAND)
}

/// Parse `/rule drop|fix F<n> <text>`. `Err` carries the inline hint: the
/// usage line for a malformed command, or the not-open hint naming the
/// finding when it is not in `open` (P2-§5.4).
pub fn parse_rule_command(line: &str, open: &BTreeSet<String>) -> Result<RulingInput, String> {
    let usage = || RULE_USAGE_HINT.to_string();
    let rest = line
        .trim()
        .strip_prefix(RULE_COMMAND)
        .filter(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        .ok_or_else(usage)?;
    let (decision, rest) = split_word(rest).ok_or_else(usage)?;
    let decision = match decision {
        RULE_DECISION_DROP => RulingDecision::Drop,
        RULE_DECISION_FIX => RulingDecision::Fix,
        _ => return Err(usage()),
    };
    let (finding, text) = split_word(rest).ok_or_else(usage)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(usage());
    }
    if !open.contains(finding) {
        return Err(RULE_NOT_OPEN_HINT.replace("{id}", finding));
    }
    Ok(RulingInput {
        finding: finding.to_string(),
        decision,
        text: text.to_string(),
    })
}

/// Split off the first whitespace-delimited word; `None` when there is none.
fn split_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    Some(s.split_once(char::is_whitespace).unwrap_or((s, "")))
}
