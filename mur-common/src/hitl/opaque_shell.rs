//! #1832: shell invocations whose body cannot be read from the command text.
//!
//! `tool_tier` is a deny-list over command heads, so a command that *runs
//! text it cannot show* (`eval $X`, `bash -c "$X"`, `curl … | sh`) is
//! arbitrary code execution that no head list can see. Falling back to
//! `Write` for those is the documented default for *unknown* commands; these
//! shapes are not unknown, they are known-opaque, so they are asked about.
//!
//! This is a policy choice, not a bug fix, and it is deliberately narrow:
//!
//! - Only the three shapes above. Interpreter heredocs and `python -c` are
//!   NOT matched — the history scan in #1832 puts those at ~5% of all Write
//!   calls, which would turn the default into a prompt storm.
//! - Quote-aware. A shell word inside a quoted argument (`grep "| sh" f`,
//!   `pgrep -f "bash -c $X"`) is data, not a command, and must not prompt.
//! - A literal body (`bash -c 'cargo build'`) is not opaque; the unwrapping
//!   in `tool_tier::tokens` already classifies what it contains.

/// Shells whose `-c` body or stdin is executed as a script.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "ash"];

/// Is any command in `cmd` an opaque shell invocation?
pub(super) fn is_opaque_shell(cmd: &str) -> bool {
    segments(cmd)
        .iter()
        .any(|(seg, piped)| segment_is_opaque(seg, *piped))
}

/// Split on unquoted `; & | newline`, remembering whether the separator that
/// introduced each segment was a single pipe (stdin comes from the left).
fn segments(cmd: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut piped = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q == '"' {
                    cur.extend(chars.next());
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    cur.push(c);
                    cur.extend(chars.next());
                }
                ';' | '&' | '|' | '\n' => {
                    let mut run = String::from(c);
                    while let Some(&n) = chars.peek() {
                        if matches!(n, ';' | '&' | '|') {
                            run.push(n);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push((std::mem::take(&mut cur), piped));
                    piped = run == "|" || run == "|&";
                }
                _ => cur.push(c),
            },
        }
    }
    out.push((cur, piped));
    out
}

fn segment_is_opaque(seg: &str, piped: bool) -> bool {
    let words: Vec<&str> = seg
        .split_whitespace()
        .map(|w| w.trim_start_matches(['(', '{']))
        .filter(|w| !w.is_empty())
        .skip_while(|w| is_env_assignment(w))
        .collect();
    let Some(head) = words.first().map(|w| basename(w).to_ascii_lowercase()) else {
        return false;
    };
    if head == "eval" {
        return true;
    }
    if !SHELLS.contains(&head.as_str()) {
        return false;
    }
    let rest = &words[1..];
    // `… | sh` — a script arriving on stdin: no script file, no `-c`.
    if piped && reads_script_from_stdin(rest) {
        return true;
    }
    // `sh -c <body>` where the body is a substitution rather than literal text.
    rest.iter()
        .position(|w| is_c_flag(w))
        .and_then(|i| rest.get(i + 1))
        .is_some_and(|body| {
            // Single quotes make `$` literal; double quotes and bare words do not.
            !body.starts_with('\'') && {
                let b = body.trim_start_matches('"');
                b.starts_with('$') || b.starts_with('`')
            }
        })
}

/// Does a shell invoked with `args` read its script from stdin?
///
/// POSIX `sh [options] [file [args]]`: the first operand is a script file
/// unless `-s` is given, in which case every operand is a positional argument
/// and the script still comes from stdin. So `curl … | sh -s -- arg` is as
/// opaque as `curl … | sh` (#1848), while `… | sh script.sh` is not.
fn reads_script_from_stdin(args: &[&str]) -> bool {
    let (mut s_flag, mut operand) = (false, false);
    let mut it = args.iter();
    while let Some(w) = it.next() {
        if *w == "--" {
            operand = it.next().is_some();
            break;
        }
        if is_c_flag(w) {
            return false;
        }
        if w.starts_with('-') || w.starts_with('+') {
            // `-o name` / `-O name` take a separate argument; it is not a file.
            if matches!(*w, "-o" | "+o" | "-O" | "+O") {
                it.next();
            }
            s_flag |= w.starts_with('-') && !w.starts_with("--") && w[1..].contains('s');
        } else {
            operand = true;
            break;
        }
    }
    s_flag || !operand
}

/// `-c`, `-lc`, `-ec` … (a short-flag cluster containing `c`; `--long` excluded).
fn is_c_flag(w: &str) -> bool {
    w.starts_with('-') && !w.starts_with("--") && w[1..].contains('c')
}

fn is_env_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}
