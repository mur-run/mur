//! Decide whether a `bash` command is a delete MUR can perform itself (D2).
//!
//! The rule that shapes every branch: a false [`RmPlan::Rewrite`] is a bug
//! (MUR would move something the shell would not have touched, or skip
//! something it would have), while a false [`RmPlan::Unrewritable`] only
//! means one delete goes unprotected. So anything not fully understood is
//! `Unrewritable`, and the command runs as the model wrote it.

use std::path::{Path, PathBuf};

/// Programs that delete their path arguments. `trash` is already recoverable.
pub const REMOVERS: &[&str] = &["rm", "rmdir", "unlink"];
/// Prefixes that run the next word as the command.
const WRAPPERS: &[&str] = &["sudo", "command", "env", "exec", "nohup", "time", "nice"];
/// Prefixes that only matter for detection: they hand their arguments to a
/// remover in ways that cannot be rewritten (`xargs rm`, `find -exec rm`).
const DETECT_ONLY_WRAPPERS: &[&str] = &["xargs", "doas"];

/// What to do with one `bash` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmPlan {
    /// No remover in the command; run as-is.
    NotRm,
    /// Every segment is a remover fully understood; MUR performs the moves
    /// and the command is NOT run.
    Rewrite(Vec<TrashTarget>),
    /// A remover is present but cannot be rewritten safely. Run as-is, log.
    Unrewritable { reason: String },
    /// Unbalanced quotes or similar. Run as-is, log.
    Unparseable,
}

/// One path the command would delete, resolved against the command's cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashTarget {
    /// Absolute when the base cwd was absolute. Not normalized: `..` is left
    /// for the kernel, which resolves it the way the shell would.
    pub path: PathBuf,
    /// The argument as the model spelled it, for tool output.
    pub spelled: String,
    pub rule: Rule,
    /// Index of the `&&` segment it came from: a failure stops later ones.
    pub segment: usize,
}

/// The remover semantics a target must satisfy before it is moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Rm {
        /// `-r` / `-R`: directories are allowed.
        recursive: bool,
        /// `-f`: a missing path is not an error.
        force: bool,
        /// `-d`: empty directories are allowed.
        empty_dirs: bool,
    },
    /// Only an empty directory.
    Rmdir,
    /// Only a non-directory.
    Unlink,
}

/// Plan `command`, run in `cwd`.
pub fn plan(command: &str, cwd: &Path) -> RmPlan {
    if !mentions_remover(command) {
        return RmPlan::NotRm;
    }
    let segments = match scan(command) {
        Ok(s) => s,
        Err(ScanError::Unbalanced) => return RmPlan::Unparseable,
        Err(ScanError::Unsupported(what)) => return unrewritable(format!("contains {what}")),
    };
    let mut base = cwd.to_path_buf();
    let mut targets = Vec::new();
    let last = segments.len() - 1;
    for (i, seg) in segments.iter().enumerate() {
        let Ok(words) = shell_words::split(seg) else {
            return RmPlan::Unparseable;
        };
        let words = match strip_prefixes(&words) {
            Ok(w) => w,
            Err(why) => return unrewritable(why),
        };
        let Some((prog, args)) = words.split_first() else {
            return unrewritable("empty command segment".into());
        };
        let name = prog.rsplit('/').next().unwrap_or(prog);
        match name {
            "cd" if i != last && prog == "cd" => match args {
                [dir] if !dir.is_empty() && dir != "-" => base = base.join(dir),
                _ => return unrewritable("cd without a single plain directory".into()),
            },
            p if REMOVERS.contains(&p) => match parse_remover(p, args, &base, i) {
                Ok(mut t) => targets.append(&mut t),
                Err(why) => return unrewritable(why),
            },
            other => return unrewritable(format!("mixed with `{other}`")),
        }
    }
    if targets.is_empty() {
        return unrewritable("no remover with paths".into());
    }
    RmPlan::Rewrite(targets)
}

fn unrewritable(reason: String) -> RmPlan {
    RmPlan::Unrewritable { reason }
}

/// Leading `VAR=value` words and bare wrappers (`sudo rm`) are dropped. A
/// wrapper with options of its own (`sudo -u x`, `nice -n 5`) is not
/// understood, nor is an assignment, which changes the remover's environment.
fn strip_prefixes(words: &[String]) -> Result<&[String], String> {
    let mut rest = words;
    while let Some((w, tail)) = rest.split_first() {
        if is_assignment(w) {
            return Err("environment assignment before remover".into());
        }
        if !WRAPPERS.contains(&w.as_str()) {
            break;
        }
        if tail
            .first()
            .is_some_and(|n| n.starts_with('-') || is_assignment(n))
        {
            return Err(format!("`{w}` with options"));
        }
        rest = tail;
    }
    Ok(rest)
}

fn parse_remover(
    prog: &str,
    args: &[String],
    base: &Path,
    segment: usize,
) -> Result<Vec<TrashTarget>, String> {
    let (flags, paths) = split_options(args);
    let rule = match prog {
        "rm" => {
            let (mut recursive, mut force, mut empty_dirs) = (false, false, false);
            for f in flags {
                if f.starts_with("--") {
                    return Err(format!("rm option `{f}`"));
                }
                for c in f.chars().skip(1) {
                    match c {
                        'r' | 'R' => recursive = true,
                        'f' => force = true,
                        'd' => empty_dirs = true,
                        'v' => {}
                        // `-i` prompts; with stdin at EOF the shell deletes
                        // nothing, so moving would be more destructive.
                        _ => return Err(format!("rm option `-{c}`")),
                    }
                }
            }
            Rule::Rm {
                recursive,
                force,
                empty_dirs,
            }
        }
        "rmdir" if flags.is_empty() => Rule::Rmdir,
        "unlink" if flags.is_empty() && paths.len() == 1 => Rule::Unlink,
        _ => return Err(format!("{prog} with options or argument count it rejects")),
    };
    if paths.is_empty() {
        return Err(format!("{prog} without paths"));
    }
    paths
        .iter()
        .map(|p| {
            let leaf = Path::new(p).file_name();
            if p.is_empty() || leaf.is_none() || p.ends_with("/.") || p.ends_with("/..") {
                return Err(format!("path `{p}` the remover refuses"));
            }
            Ok(TrashTarget {
                path: base.join(p),
                spelled: p.clone(),
                rule,
                segment,
            })
        })
        .collect()
}

/// `(options, operands)`. Options are `-x` words before the first operand or
/// `--`; a long option stays in `options` so the caller rejects it.
fn split_options(args: &[String]) -> (Vec<&str>, Vec<String>) {
    let mut flags = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        if a.starts_with('-') && a.len() > 1 {
            flags.push(a.as_str());
            i += 1;
        } else {
            break;
        }
    }
    (flags, args[i..].to_vec())
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Cheap, over-eager check: does any command position look like a remover?
/// Over-eager is fine — it only decides whether [`scan`] runs at all.
fn mentions_remover(command: &str) -> bool {
    let flat: String = command
        .chars()
        .map(|c| if "|&;()`\n{}".contains(c) { '\n' } else { c })
        .collect();
    flat.lines().any(|line| {
        let words: Vec<String> = line
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c| c == '"' || c == '\'' || c == '$')
                    .to_string()
            })
            .collect();
        let mut i = 0;
        while i < words.len() && is_assignment(&words[i]) {
            i += 1;
        }
        let mut in_find = false;
        // After a wrapper, its own options and their arguments cannot be
        // told apart from the command, so any remover word counts.
        let mut after_wrapper = false;
        while let Some(w) = words.get(i) {
            let base = w.rsplit('/').next().unwrap_or(w);
            if REMOVERS.contains(&base) {
                return true;
            }
            if base == "find" {
                in_find = true;
            } else if in_find && (w == "-delete" || w == "-exec" || w == "-execdir") {
                if w == "-delete" {
                    return true;
                }
            } else if WRAPPERS.contains(&base) || DETECT_ONLY_WRAPPERS.contains(&base) {
                after_wrapper = true;
            } else if !in_find && !after_wrapper {
                return false;
            }
            i += 1;
        }
        false
    })
}

#[derive(Debug)]
enum ScanError {
    Unbalanced,
    Unsupported(&'static str),
}

/// Split on unquoted `&&`, refusing every other construct the shell would
/// expand or redirect. Inside double quotes, `$`, backtick, and `\` still
/// expand, so they are refused there too.
fn scan(command: &str) -> Result<Vec<String>, ScanError> {
    #[derive(PartialEq)]
    enum Q {
        None,
        Single,
        Double,
    }
    let mut q = Q::None;
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    let mut word_start = true;
    while let Some(c) = chars.next() {
        match q {
            Q::Single => {
                if c == '\'' {
                    q = Q::None;
                }
                cur.push(c);
                continue;
            }
            Q::Double => {
                match c {
                    '"' => q = Q::None,
                    '$' | '`' | '\\' => return Err(ScanError::Unsupported("expansion in quotes")),
                    _ => {}
                }
                cur.push(c);
                continue;
            }
            Q::None => {}
        }
        match c {
            '\'' => q = Q::Single,
            '"' => q = Q::Double,
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                segments.push(std::mem::take(&mut cur));
                word_start = true;
                continue;
            }
            '&' => return Err(ScanError::Unsupported("`&` or `||`")),
            '|' => return Err(ScanError::Unsupported("a pipe")),
            ';' | '\n' => return Err(ScanError::Unsupported("`;` or a newline")),
            '<' | '>' => return Err(ScanError::Unsupported("a redirection")),
            '$' | '`' => return Err(ScanError::Unsupported("an expansion")),
            '(' | ')' => return Err(ScanError::Unsupported("a subshell")),
            '*' | '?' | '[' => return Err(ScanError::Unsupported("a glob")),
            '{' | '}' => return Err(ScanError::Unsupported("a brace expansion")),
            '#' if word_start => return Err(ScanError::Unsupported("a comment")),
            '~' if word_start => return Err(ScanError::Unsupported("a `~` expansion")),
            '!' => return Err(ScanError::Unsupported("`!`")),
            '\\' => return Err(ScanError::Unsupported("a backslash escape")),
            _ => {}
        }
        word_start = c.is_whitespace();
        cur.push(c);
    }
    if q != Q::None {
        return Err(ScanError::Unbalanced);
    }
    segments.push(cur);
    Ok(segments)
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
