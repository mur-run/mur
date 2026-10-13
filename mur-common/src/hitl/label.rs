//! The human-readable label stored with a remembered gate-B decision (#1806).
//!
//! The label goes into a signed, append-only channel that cannot be scrubbed
//! later, so it is safe by construction rather than by best effort:
//!
//! 1. **Structure, not payload.** Only an allowlisted field per tool is read —
//!    a file tool's path, a shell tool's leading command words. Any other tool
//!    is labelled with its name alone. The input is never serialised whole.
//! 2. **Redact with the shared chokepoint**, plus the credential-field pass a
//!    short password needs (`crate::redact`).
//! 3. **Redact, then truncate.** Cutting first can split a secret so its
//!    pattern no longer matches and leave its prefix in the log.

use crate::redact::{redact_credentials, redact_home_path, redact_secrets};

/// Longest label, in `char`s. Long enough to tell two paths apart, short
/// enough that a label is never a payload.
pub const HITL_LABEL_MAX_CHARS: usize = 80;

/// How many leading words of a shell command are kept: program plus a
/// subcommand or two (`git push origin`), never its arguments at large.
pub const SHELL_LABEL_MAX_WORDS: usize = 3;

/// Tools whose `path` input names the file the decision was about.
const FILE_TOOLS: &[&str] = &["read_file", "write_file", "edit_file"];
const FILE_PATH_KEY: &str = "path";
/// Tools whose `command` input is a shell command line.
const SHELL_TOOLS: &[&str] = &["bash"];
const SHELL_COMMAND_KEY: &str = "command";

const ELLIPSIS: char = '…';

/// `tool` or `tool: summary`, redacted and at most [`HITL_LABEL_MAX_CHARS`].
pub fn decision_label(tool_name: &str, input: &serde_json::Value) -> String {
    let raw = match summary(tool_name, input) {
        Some(s) if !s.is_empty() => format!("{tool_name}: {s}"),
        _ => tool_name.to_string(),
    };
    truncate_chars(&redact(&raw), HITL_LABEL_MAX_CHARS)
}

fn summary(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let field = |k: &str| input.get(k).and_then(serde_json::Value::as_str);
    if FILE_TOOLS.contains(&tool_name) {
        return field(FILE_PATH_KEY).map(str::to_string);
    }
    if SHELL_TOOLS.contains(&tool_name) {
        return field(SHELL_COMMAND_KEY).map(shell_head);
    }
    None
}

/// The leading command words: skip `NAME=value` env prefixes, then keep
/// words while they look like a program or subcommand name, at most
/// [`SHELL_LABEL_MAX_WORDS`]. The first word may be a path (`./x`,
/// `/usr/bin/x`); later words must be lowercase letters and `-`/`_` only, so
/// an argument with a digit, capital, quote, flag, or operator ends the
/// label. A secret that is a plain lowercase word can still pass this
/// heuristic — the redactors behind it are the backstop, not this filter.
fn shell_head(command: &str) -> String {
    let mut words = command.split_whitespace().skip_while(|w| w.contains('='));
    let Some(program) = words.next().filter(|w| is_program(w)) else {
        return String::new();
    };
    std::iter::once(program)
        .chain(words.take_while(|w| is_subcommand(w)))
        .take(SHELL_LABEL_MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_program(w: &str) -> bool {
    w.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~'))
}

fn is_subcommand(w: &str) -> bool {
    !w.starts_with('-')
        && w.chars()
            .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '_'))
}

fn redact(s: &str) -> String {
    let s = redact_secrets(s);
    let s = redact_credentials(&s);
    redact_home_path(&s).into_owned()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[track_caller]
    fn clean(label: &str, secrets: &[&str]) {
        for s in secrets {
            assert!(!label.contains(s), "{s:?} leaked into {label:?}");
        }
        assert!(label.chars().count() <= HITL_LABEL_MAX_CHARS, "{label:?}");
    }

    #[test]
    fn file_tools_keep_the_path_only() {
        let l = decision_label(
            "edit_file",
            &json!({"path": "src/Cargo.toml", "old_string": "a", "new_string": "password=hunter2"}),
        );
        assert_eq!(l, "edit_file: src/Cargo.toml");
    }

    #[test]
    fn home_directory_is_collapsed() {
        let l = decision_label("read_file", &json!({"path": "/Users/alice/x/.env"}));
        assert_eq!(l, "read_file: ~/x/.env");
    }

    #[test]
    fn shell_keeps_leading_words_only() {
        let l = decision_label("bash", &json!({"command": "git push origin main --force"}));
        assert_eq!(l, "bash: git push origin");
        let l = decision_label("bash", &json!({"command": "cargo test -p mur-core"}));
        assert_eq!(l, "bash: cargo test");
        let l = decision_label("bash", &json!({"command": "\"$(cat x)\" go"}));
        assert_eq!(l, "bash");
    }

    #[test]
    fn other_tools_are_named_only() {
        let l = decision_label("mcp__x__send", &json!({"to": "a@b.c", "body": "token=abc"}));
        assert_eq!(l, "mcp__x__send");
    }

    /// The corpus that matters more than the feature: secret-looking inputs
    /// in every slot a label reads, none of which may reach the label.
    #[test]
    fn secret_corpus_never_reaches_a_label() {
        let openai = format!("sk-{}", "a".repeat(40));
        let ghp = format!("ghp_{}", "b".repeat(36));
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl";
        let cases: Vec<(&str, String, Vec<&str>)> = vec![
            ("bash", format!("{openai} run"), vec![openai.as_str()]),
            (
                "bash",
                format!("TOKEN={ghp} gh pr list"),
                vec![ghp.as_str()],
            ),
            ("bash", "PASSWORD=hunter2 psql".into(), vec!["hunter2"]),
            (
                "bash",
                "mysql --password hunter2 db".into(),
                vec!["hunter2"],
            ),
            ("bash", "mysql -phunter2 db".into(), vec!["hunter2"]),
            (
                "bash",
                "curl -H 'Authorization: Bearer abc.def' x".into(),
                vec!["abc.def"],
            ),
            (
                "bash",
                "curl https://bob:pa55@example.com".into(),
                vec!["pa55"],
            ),
            ("bash", format!("echo {jwt}"), vec![jwt]),
            (
                "bash",
                "echo Hunter2x | sudo -S ls".into(),
                vec!["Hunter2x", "sudo"],
            ),
            ("bash", "sshpass -p s3cret ssh h".into(), vec!["s3cret"]),
            (
                "read_file",
                "-----BEGIN RSA PRIVATE KEY-----".into(),
                vec!["BEGIN RSA PRIVATE"],
            ),
            (
                "read_file",
                format!("/tmp/{openai}.txt"),
                vec![openai.as_str()],
            ),
            (
                "write_file",
                "https://u:hunter2@h/p".into(),
                vec!["hunter2"],
            ),
            ("edit_file", "token=abc/x".into(), vec!["abc"]),
        ];
        for (tool, value, secrets) in cases {
            let key = if tool == "bash" { "command" } else { "path" };
            let label = decision_label(tool, &json!({ key: value }));
            clean(&label, &secrets);
        }
    }

    /// A secret straddling the cut: truncating first would leave a prefix
    /// the pattern no longer recognises.
    #[test]
    fn redaction_runs_before_the_length_cut() {
        let pad = "d/".repeat(30);
        let key = format!("sk-{}", "Z".repeat(40));
        let path = format!("{pad}{key}");
        let label = decision_label("read_file", &json!({ "path": path }));
        clean(&label, &["sk-ZZ", "ZZZZ"]);
    }

    #[test]
    fn long_labels_are_cut_on_char_boundaries() {
        let path = "目錄/".repeat(60);
        let label = decision_label("read_file", &json!({ "path": path }));
        assert_eq!(label.chars().count(), HITL_LABEL_MAX_CHARS);
        assert!(label.ends_with(ELLIPSIS));
    }
}
