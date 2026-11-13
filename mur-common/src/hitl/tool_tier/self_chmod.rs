//! #2362: `chmod` on the agent's own files is a `Write`, not a privilege
//! change.
//!
//! `chmod` is on the privileged deny-list because a mode change can open a
//! file to everyone or set setuid. `chmod +x build/run.sh` does neither, yet
//! it prompted exactly like `sudo` — and `--no-hitl` never waives a
//! privileged call (#2313), so a self-scoped `chmod +x` was the one prompt an
//! unattended boot could not get past.
//!
//! This only ever LOWERS a tier, so every doubt leaves the text untouched. A
//! segment is dropped from the privileged scan only when ALL of:
//! - the original command has no heredoc, no `cd`/`pushd`/`popd` (relative
//!   paths resolve in the session directory) and names `TMPDIR` only as
//!   `$TMPDIR/` or `${TMPDIR}/` (no reassignment);
//! - the segment is exactly `chmod MODE PATH…`: no flags (`-R`), no wrapper;
//! - `MODE` sets no setuid/setgid/sticky bit and grants write to nobody but
//!   the owner (octal `[0]NNN` with group/other digits in `0 1 4 5`, or
//!   symbolic `[ugoa]*[+-=][rwxX]+` where `w` is only added for `u`);
//! - every path is a literal relative path (no `..`, no glob, no leading `-`)
//!   or a literal path under `$TMPDIR/`.
//!
//! The remaining segments are rebuilt as words for the privileged head scan
//! only; every other tier check still sees the whole command.

use std::borrow::Cow;

use crate::hitl::dest::{Word, segments, tokenize};

/// Prefixes that name the agent's scratch dir (the runtime exports it as
/// `TMPDIR`).
const SCRATCH_PREFIXES: &[&str] = &["$TMPDIR/", "${TMPDIR}/"];

/// Shell words that move the working directory, after which a relative path
/// no longer means the session directory.
const DIR_CHANGERS: &[&str] = &["cd", "pushd", "popd"];

/// The variable [`SCRATCH_PREFIXES`] expand.
const SCRATCH_VAR: &str = "TMPDIR";

/// Octal digits a group/other position may hold: no write bit.
const NO_WRITE_DIGITS: &[char] = &['0', '1', '4', '5'];

/// `scanned` without its self-scoped `chmod` segments, judged against the
/// whole original `cmd`. Untouched (borrowed) on any doubt.
pub(super) fn without_self_scoped<'a>(cmd: &str, scanned: &'a str) -> Cow<'a, str> {
    if !command_is_eligible(cmd) || scanned.contains("<<") {
        return Cow::Borrowed(scanned);
    }
    let Some(segs) = tokenize(scanned).map(segments) else {
        return Cow::Borrowed(scanned);
    };
    if !segs.iter().any(|s| is_self_scoped_chmod(s)) {
        return Cow::Borrowed(scanned);
    }
    let rest: Vec<String> = segs
        .iter()
        .filter(|s| !is_self_scoped_chmod(s))
        .map(|s| {
            s.iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    Cow::Owned(rest.join(" ; "))
}

/// Whole-command preconditions: no heredoc, no directory change, and
/// `TMPDIR` never named except as a path prefix.
fn command_is_eligible(cmd: &str) -> bool {
    if cmd.contains("<<") {
        return false;
    }
    let moves_dir = cmd
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`'))
        .any(|t| DIR_CHANGERS.contains(&t.trim_matches(|c| c == '"' || c == '\'')));
    let named = cmd.matches(SCRATCH_VAR).count();
    let as_prefix: usize = SCRATCH_PREFIXES
        .iter()
        .map(|p| cmd.matches(p).count())
        .sum();
    !moves_dir && named == as_prefix
}

fn is_self_scoped_chmod(seg: &[Word]) -> bool {
    let [head, mode, paths @ ..] = seg else {
        return false;
    };
    head.text == "chmod"
        && !head.quoted
        && mode_is_safe(&mode.text)
        && !paths.is_empty()
        && paths.iter().all(|p| path_is_own(&p.text))
}

fn mode_is_safe(mode: &str) -> bool {
    if mode.chars().all(|c| c.is_ascii_digit()) {
        return octal_is_safe(mode);
    }
    !mode.is_empty() && mode.split(',').all(symbolic_clause_is_safe)
}

/// `NNN` or `0NNN`: a leading special digit other than `0` is setuid/setgid/
/// sticky; a group or other digit with the write bit opens the file.
fn octal_is_safe(mode: &str) -> bool {
    let digits = match mode.len() {
        3 => mode,
        4 if mode.starts_with('0') => &mode[1..],
        _ => return false,
    };
    digits.chars().all(|c| ('0'..='7').contains(&c))
        && digits[1..].chars().all(|c| NO_WRITE_DIGITS.contains(&c))
}

/// `[ugoa]*[+-=][rwxX]+`, adding `w` only for `u`.
fn symbolic_clause_is_safe(clause: &str) -> bool {
    let Some(op_at) = clause.find(['+', '-', '=']) else {
        return false;
    };
    let (who, rest) = clause.split_at(op_at);
    let (op, perms) = rest.split_at(1);
    let who_ok = who.chars().all(|c| matches!(c, 'u' | 'g' | 'o' | 'a'));
    let perms_ok = !perms.is_empty() && perms.chars().all(|c| matches!(c, 'r' | 'w' | 'x' | 'X'));
    let adds_write = op != "-" && perms.contains('w');
    who_ok && perms_ok && (!adds_write || who == "u")
}

/// A literal relative path, or a literal path under the scratch dir.
fn path_is_own(path: &str) -> bool {
    let rel = SCRATCH_PREFIXES
        .iter()
        .find_map(|p| path.strip_prefix(p))
        .unwrap_or(path);
    !rel.is_empty()
        && !rel.starts_with('-')
        && !rel.starts_with('/')
        && rel.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '+' | '@' | '%' | '-')
        })
        && !rel.split('/').any(|c| c == "..")
}

#[cfg(test)]
mod tests;
