//! Tell a list of sites from a sentence typed after `/browser live` (#1905).
//!
//! A research question split on spaces used to be written token by token
//! into the allowlist. When the arguments read like a task instead of hosts,
//! the caller offers a chooser of runnable commands and writes nothing.

use mur_common::host_pattern::normalize_host_pattern;

use super::browser_live_cmd::{split_authority, strip_url};
use super::suggest::Suggestion;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LiveIntent {
    Hosts,
    Task,
}

pub(super) const TASK_NOTICE: &str =
    "This looks like a research task, not a list of sites. Pick how to run it:";

/// Punctuation that ends or joins clauses; no hostname contains it.
const SENTENCE_PUNCT: [char; 8] = ['?', '？', '，', '。', '、', '!', '！', '；'];

/// Drop `--profile <x>`, `--yes` and `--research` so only host candidates remain.
fn host_tokens(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                it.next();
            }
            "--yes" | "--research" => {}
            other => out.push(other),
        }
    }
    out
}

/// The part of a token a host check looks at: URL scheme/path and port removed.
fn authority(tok: &str) -> &str {
    let a = strip_url(tok).unwrap_or(tok);
    split_authority(a).0
}

fn is_host(tok: &str) -> bool {
    normalize_host_pattern(authority(tok)).is_ok()
}

/// `Task` when the arguments read as a sentence; `Hosts` otherwise. A lone
/// junk word or a mix of hosts and junk stays `Hosts`, so `parse_hosts` can
/// name exactly which argument is wrong.
pub(super) fn classify_live_args(args: &[String]) -> LiveIntent {
    let toks = host_tokens(args);
    if toks
        .iter()
        .any(|t| authority(t).contains(SENTENCE_PUNCT.as_slice()))
    {
        return LiveIntent::Task;
    }
    let non_ascii_word = |t: &str| t.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
    if toks.iter().any(|t| non_ascii_word(t) && !is_host(t)) {
        return LiveIntent::Task;
    }
    if toks.len() >= 3 && !toks.iter().any(|t| is_host(t)) {
        return LiveIntent::Task;
    }
    LiveIntent::Hosts
}

/// Chooser items; `text` is the command inserted into the composer, so the
/// user still reviews it and presses Enter.
pub(super) fn task_choices(args: &[String]) -> Vec<Suggestion> {
    let item = |text: String, desc: &str| Suggestion {
        text,
        desc: Some(desc.to_owned()),
    };
    vec![
        item(
            "/browser live --research".to_owned(),
            "allow all hosts, log every connection",
        ),
        item(
            format!("/browser {}", host_tokens(args).join(" ")),
            "plain browser turn, current network policy",
        ),
        item("/browser live ".to_owned(), "name the sites instead"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn sentences_are_tasks() {
        for a in [
            &[
                "pro360",
                "與家電維修垂直市場兩條三角驗證，市場規模、用戶數、app",
                "評分、591／tasker／鄰家師傅",
            ][..],
            &["research", "the", "home", "repair", "market"][..],
            &["what", "is", "pro360?"][..],
            &["查台灣居家修繕市場規模"][..],
        ] {
            assert_eq!(classify_live_args(&s(a)), LiveIntent::Task, "{a:?}");
        }
    }

    #[test]
    fn host_lists_are_hosts() {
        for a in [
            &["shop.example.com"][..],
            &["例子.台灣"][..],
            &["https://x.com/a?b"][..],
            &["a.com", "b.com", "c.com"][..],
            &["pro360"][..],
            &["ok.com", "pro360"][..],
        ] {
            assert_eq!(classify_live_args(&s(a)), LiveIntent::Hosts, "{a:?}");
        }
    }

    #[test]
    fn flags_are_not_counted_as_words() {
        let a = s(&["--profile", "my-site", "--yes", "shop.example.com"]);
        assert_eq!(classify_live_args(&a), LiveIntent::Hosts);
    }

    #[test]
    fn choices_insert_runnable_commands() {
        let c = task_choices(&s(&["查台灣", "市場"]));
        assert_eq!(
            c.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            vec![
                "/browser live --research",
                "/browser 查台灣 市場",
                "/browser live "
            ]
        );
    }
}
