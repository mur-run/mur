//! P3b-§3.1: the `/review` line grammar. Pure syntax — agent names are
//! canonicalised by the caller, which has the MUR home.

use crate::cmd::fleet::review::constants::{
    REVIEW_FLAG_AUTO, REVIEW_FLAG_BUDGET, REVIEW_FLAG_DEADLINE, REVIEW_FLAG_MAIN,
    REVIEW_FLAG_REVIEWER, REVIEW_FLAGS_END, REVIEW_SYNTAX_BAD_BUDGET, REVIEW_SYNTAX_MISSING,
    REVIEW_SYNTAX_NEEDS_VALUE, REVIEW_SYNTAX_RESUME_ARGS, REVIEW_SYNTAX_UNKNOWN_FLAG,
    REVIEW_USAGE_MURMUR, REVIEW_WORD_RESUME,
};
use crate::cmd::fleet::review::session::ReviewArgs;

/// What the typed `/review …` line asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewLine {
    Bare,
    Start(ReviewArgs),
    Resume(String),
    AutoRefused,
}

/// The next whitespace-delimited word and what follows it.
fn next_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

fn syntax(reason: String) -> String {
    format!("{reason}\n{REVIEW_USAGE_MURMUR}")
}

fn missing(what: &str) -> String {
    syntax(REVIEW_SYNTAX_MISSING.replace("{what}", what))
}

/// The value after `flag`, which may not itself look like a flag.
fn flag_value<'a>(flag: &str, rest: &'a str) -> Result<(&'a str, &'a str), String> {
    match next_word(rest) {
        Some((v, after)) if !v.starts_with('-') => Ok((v, after)),
        _ => Err(syntax(REVIEW_SYNTAX_NEEDS_VALUE.replace("{flag}", flag))),
    }
}

/// `Err` is the text to show: the reason, then the usage constant.
pub fn parse_review_line(rest: &str) -> Result<ReviewLine, String> {
    let Some((first, after)) = next_word(rest) else {
        return Ok(ReviewLine::Bare);
    };
    if first == REVIEW_WORD_RESUME {
        return match (
            next_word(after),
            next_word(after).and_then(|(_, r)| next_word(r)),
        ) {
            (Some((name, _)), None) => Ok(ReviewLine::Resume(name.to_string())),
            _ => Err(syntax(REVIEW_SYNTAX_RESUME_ARGS.to_string())),
        };
    }

    let (mut main, mut reviewer, mut deadline, mut budget_usd) = (None, None, None, None);
    let mut cursor = rest;
    let task = loop {
        let Some((word, after)) = next_word(cursor) else {
            break "";
        };
        if word == REVIEW_FLAGS_END {
            // Everything after `--` is the task, `--auto` included.
            break after.trim();
        }
        if !word.starts_with('-') {
            let task = cursor.trim();
            if task.split_whitespace().any(|w| w == REVIEW_FLAG_AUTO) {
                return Ok(ReviewLine::AutoRefused);
            }
            break task;
        }
        match word {
            REVIEW_FLAG_AUTO => return Ok(ReviewLine::AutoRefused),
            REVIEW_FLAG_MAIN | REVIEW_FLAG_REVIEWER | REVIEW_FLAG_DEADLINE | REVIEW_FLAG_BUDGET => {
                let (value, next) = flag_value(word, after)?;
                match word {
                    REVIEW_FLAG_MAIN => main = Some(value.to_string()),
                    REVIEW_FLAG_REVIEWER => reviewer = Some(value.to_string()),
                    REVIEW_FLAG_DEADLINE => deadline = Some(value.to_string()),
                    _ => {
                        let n = value.parse::<f64>().ok().filter(|n| n.is_finite());
                        budget_usd = Some(n.ok_or_else(|| {
                            syntax(REVIEW_SYNTAX_BAD_BUDGET.replace("{value}", value))
                        })?);
                    }
                }
                cursor = next;
            }
            _ => return Err(syntax(REVIEW_SYNTAX_UNKNOWN_FLAG.replace("{flag}", word))),
        }
    };

    let main = main.ok_or_else(|| missing(REVIEW_FLAG_MAIN))?;
    let reviewer = reviewer.ok_or_else(|| missing(REVIEW_FLAG_REVIEWER))?;
    if task.is_empty() {
        return Err(missing("task"));
    }
    Ok(ReviewLine::Start(ReviewArgs {
        main,
        reviewer,
        task: task.to_string(),
        deadline,
        budget_usd,
    }))
}
