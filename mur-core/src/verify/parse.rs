//! Claim extraction — pulls file paths, `mur` commands, and code references
//! out of documentation lines.

use std::collections::HashSet;

use regex::Regex;

use super::{Claim, ClaimKind, known_mur_commands};

/// Extract all verifiable claims from document content.
pub fn parse_claims(content: &str, source_file: &str) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut seen = HashSet::new();

    for (line_idx, line) in content.lines().enumerate() {
        let line_number = line_idx + 1;

        // Skip pure comment / frontmatter lines
        if line.trim_start().starts_with('#') && !line.contains('`') {
            continue;
        }

        // 1. File path claims — from inline code or code blocks
        for path in extract_file_paths(line) {
            let key = format!("path:{}", path);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::FilePath(path.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: path,
                });
            }
        }

        // 2. CLI command claims — `mur <subcommand>` references
        for cmd in extract_commands(line) {
            let key = format!("cmd:{}", cmd);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::Command(cmd.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: cmd,
                });
            }
        }

        // 3. Code reference claims — `Struct::method`, `fn name`, etc.
        for code_ref in extract_code_refs(line) {
            let key = format!("code:{}", code_ref);
            if seen.insert(key) {
                claims.push(Claim {
                    kind: ClaimKind::CodeRef(code_ref.clone()),
                    source_file: source_file.to_string(),
                    line_number,
                    raw: code_ref,
                });
            }
        }
    }

    claims
}

/// Extract file/directory path references from a line.
pub(super) fn extract_file_paths(line: &str) -> Vec<String> {
    let mut paths = Vec::new();

    // Match paths inside backticks: `some/path.rs` or `~/.mur/config.yaml`
    let backtick_re = Regex::new(r"`([^`]+)`").unwrap();
    // A `file:line` reference (`a.rs:12`, `a.rs:12-30`, `a.rs:12:5`) names
    // the file; the location suffix is not part of the path.
    let line_suffix_re = Regex::new(r":\d+(?:[-:]\d+)?$").unwrap();
    for cap in backtick_re.captures_iter(line) {
        for token in backtick_path_tokens(&cap[1]) {
            let text = line_suffix_re.replace(token, "");
            if looks_like_path(&text) && !paths.iter().any(|p| p == text.as_ref()) {
                paths.push(text.into_owned());
            }
        }
    }

    // Match bare paths that look like project-relative file refs
    // e.g. src/main.rs, internal/api/handlers/
    let bare_re = Regex::new(
        r"(?:^|\s|[(\[])([a-zA-Z_][a-zA-Z0-9_\-.]*/[a-zA-Z0-9_\-./]*\.[a-zA-Z]{1,10})(?:\s|[)\].,;:]|$)",
    )
    .unwrap();
    // Backtick spans were judged whole above; scanning their insides again
    // would turn a command's arguments back into path claims.
    let outside_spans = backtick_re.replace_all(line, " ");
    for cap in bare_re.captures_iter(&outside_spans) {
        let text = cap[1].to_string();
        if looks_like_path(&text) && !paths.contains(&text) {
            paths.push(text);
        }
    }

    paths
}

/// Split one backtick span into the words that each name a path.
///
/// A path never contains whitespace in these docs, so a span with spaces is
/// one of three shapes:
/// - a path list — paths joined by separator symbols
///   (`capture/ → store/ → inject/`): every path is a claim;
/// - a path-led command or description (`scripts/run.sh --out gen/x.json`,
///   `run.sh [--flag]`, `src/main.rs — entry point`): only the leading path
///   is a claim, since later arguments may name files the command creates;
/// - any other command or prose (`cargo build -p x`): no claim.
fn backtick_path_tokens(span: &str) -> Vec<&str> {
    let words: Vec<&str> = span.split_whitespace().collect();
    let Some(&first) = words.first() else {
        return Vec::new();
    };
    if !looks_like_path(first) {
        return Vec::new();
    }
    if is_path_list(&words) {
        return words.into_iter().filter(|w| !is_separator(w)).collect();
    }
    vec![first]
}

/// Characters that make a symbol word a shell operator (`>`, `>>`, `|`,
/// `&&`, `;`, `<`) rather than a list separator: the path after a
/// redirection or pipe is the command's output or next stage, not a claim.
const SHELL_OPERATOR_CHARS: &[char] = &['<', '>', '|', '&', ';'];

/// A word made only of punctuation/symbols (`→`, `,`, `·`, `—`, `+`), and
/// not a shell operator.
fn is_separator(word: &str) -> bool {
    !word.chars().any(char::is_alphanumeric) && !word.contains(SHELL_OPERATOR_CHARS)
}

/// Paths alternating with separators, starting and ending on a path. Two
/// adjacent paths (`run.sh out/x.json`) read as a command with an argument.
fn is_path_list(words: &[&str]) -> bool {
    words.len() >= 3
        && words.len() % 2 == 1
        && words.iter().enumerate().all(|(i, w)| {
            if i % 2 == 0 {
                looks_like_path(w)
            } else {
                is_separator(w)
            }
        })
}

/// Heuristic: does this string look like a file path?
pub(super) fn looks_like_path(s: &str) -> bool {
    // Must contain a slash or start with ~/
    if !s.contains('/') && !s.starts_with("~/") {
        return false;
    }
    // Must not be a URL
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("git://") {
        return false;
    }
    // Must not be a plain CLI flag or env var
    if s.starts_with("--") || s.starts_with("$") {
        return false;
    }
    // Must not contain shell metacharacters (it's a command, not a path)
    if s.contains('|') || s.contains('>') || s.contains('<') || s.contains('&') {
        return false;
    }
    // Must not look like a CLI invocation with flags
    if s.contains(" --") || s.contains(" -") {
        return false;
    }
    // Must not contain template variables
    if s.contains('{') || s.contains('}') {
        return false;
    }
    // Should have a file extension or end with /
    let has_ext = s.rsplit('/').next().is_some_and(|last| last.contains('.'));
    let is_dir = s.ends_with('/');
    let has_known_prefix = s.starts_with("~/")
        || s.starts_with("src/")
        || s.starts_with("internal/")
        || s.starts_with("cmd/")
        || s.starts_with("mur-")
        || s.starts_with("./")
        || s.starts_with("plans/")
        || s.starts_with("openspec/")
        || s.starts_with("docs/openspec/")
        || s.starts_with("research/")
        || s.starts_with("dashboard/");

    has_ext || is_dir || has_known_prefix
}

/// Subcommand words extracted from known commands (auto-populated from clap tree).
/// Used as a heuristic during doc parsing to distinguish subcommands from arguments.
fn subcommand_words() -> HashSet<String> {
    known_mur_commands()
        .iter()
        .flat_map(|cmd| {
            // "mur workflow schedule list" → ["workflow", "schedule", "list"]
            cmd.split_whitespace()
                .skip(1)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Extract `mur <subcommand>` command references.
pub(super) fn extract_commands(line: &str) -> Vec<String> {
    let mut cmds = Vec::new();
    let known_words = subcommand_words();

    // Match: `mur <words>` or mur <words> in code blocks
    let re = Regex::new(r"(?:`|^|\s)(mur\s+[a-z][a-z0-9\- ]*)").unwrap();
    for cap in re.captures_iter(line) {
        let cmd = cap[1].trim().to_string();
        // Keep only known subcommand words after "mur"
        let parts: Vec<&str> = cmd
            .split_whitespace()
            .enumerate()
            .take_while(|(i, p)| {
                *i == 0 || (known_words.contains(*p) && !p.starts_with('-') && !p.starts_with('<'))
            })
            .map(|(_, p)| p)
            .collect();
        if parts.len() >= 2 {
            let normalized = parts.join(" ");
            if !cmds.contains(&normalized) {
                cmds.push(normalized);
            }
        }
    }

    // Also match: `cargo run -- <command>` references
    let cargo_re = Regex::new(r"cargo\s+run\s+--\s+(\w+)").unwrap();
    for cap in cargo_re.captures_iter(line) {
        let subcmd = format!("mur {}", &cap[1]);
        if !cmds.contains(&subcmd) {
            cmds.push(subcmd);
        }
    }

    cmds
}

/// Extract code-level references like `Struct::method` or `fn function_name`.
pub(super) fn extract_code_refs(line: &str) -> Vec<String> {
    let mut refs = Vec::new();

    // Match Type::method or Type::method() inside backticks
    let method_re = Regex::new(r"`([A-Z][a-zA-Z0-9_]*::[a-z_][a-zA-Z0-9_]*)\(?`?").unwrap();
    for cap in method_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `fn function_name` or fn function_name()
    let fn_re = Regex::new(r"`?fn\s+([a-z_][a-z0-9_]*)`?").unwrap();
    for cap in fn_re.captures_iter(line) {
        let r = format!("fn {}", &cap[1]);
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `struct Name` or `enum Name`
    let struct_re = Regex::new(r"`((?:struct|enum|trait)\s+[A-Z][a-zA-Z0-9_]*)`").unwrap();
    for cap in struct_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match `impl Trait for Type` patterns
    let impl_re = Regex::new(r"`(impl\s+\w+(?:<[^>]+>)?\s+for\s+\w+)`").unwrap();
    for cap in impl_re.captures_iter(line) {
        let r = cap[1].to_string();
        if !refs.contains(&r) {
            refs.push(r);
        }
    }

    // Match standalone `TypeName` that look like struct/enum refs (PascalCase, backtick-wrapped)
    let type_re = Regex::new(r"`([A-Z][a-zA-Z0-9]*(?:[A-Z][a-z]+)+)`").unwrap();
    for cap in type_re.captures_iter(line) {
        let name = &cap[1];
        // Skip common non-code words
        if !matches!(
            name,
            "README"
                | "CLAUDE"
                | "YAML"
                | "JSON"
                | "TOML"
                | "HTTP"
                | "HTTPS"
                | "UUID"
                | "UTC"
                // External crate types discussed in docs, not local code
                | "FromRequestParts"
                | "ConnectInfo"
        ) {
            let r = name.to_string();
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
    }

    refs
}
