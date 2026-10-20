//! Heads that read only in a narrow, checkable form: `sed` with a print-only
//! script and `tmux`'s listing / capture verbs.
//!
//! Both are allowlists of SHAPES, not of heads. `sed` writes through `-i`,
//! `w`/`W`, `s///w` and GNU's `e` command; telling those apart from `/x/p`
//! in general needs a sed parser, so only a script built entirely from
//! addresses plus `p`/`q`/`d`/`=` is accepted. `tmux` runs arbitrary commands
//! through `run-shell`, `send-keys`, a `-f` config file and more, so only the
//! listing verbs and the `-p` (print to stdout) form of `capture-pane` /
//! `display-message` are accepted, and the verb must come first.

use std::sync::LazyLock;

use regex_lite::Regex;

/// One sed command: an optional `addr` or `addr,addr`, then a print-only verb.
/// An address is a line number, `$`, or a `/regex/` with no `/` inside.
static SED_PRINT_CMD: LazyLock<Regex> = LazyLock::new(|| {
    let addr = r"(?:\d+|\$|/[^/]*/)";
    Regex::new(&format!(r"^\s*(?:{addr}(?:\s*,\s*{addr})?)?\s*[pqd=]\s*$")).expect("static regex")
});

/// sed flags that change nothing but parsing/output.
const SED_SAFE_FLAGS: &[&str] = &[
    "-n", "-E", "-r", "--quiet", "--silent", "-nE", "-En", "-nr", "-rn",
];

fn sed_script_is_print_only(script: &str) -> bool {
    !script.trim().is_empty() && script.split(';').all(|c| SED_PRINT_CMD.is_match(c))
}

/// `sed [-n|-E] [-e SCRIPT]... [SCRIPT] [FILE]...` where every script prints.
pub(super) fn sed_prints_only<'a>(mut args: impl Iterator<Item = &'a str>) -> bool {
    let mut saw_script = false;
    while let Some(a) = args.next() {
        if a == "-e" {
            let Some(s) = args.next() else { return false };
            if !sed_script_is_print_only(s) {
                return false;
            }
            saw_script = true;
        } else if a.starts_with('-') {
            if !SED_SAFE_FLAGS.contains(&a) {
                return false; // `-i`, `--in-place`, `-f FILE`, `-s`, …
            }
        } else if !saw_script {
            if !sed_script_is_print_only(a) {
                return false;
            }
            saw_script = true;
        }
        // Remaining positionals are input files.
    }
    saw_script
}

/// tmux verbs that only list or test.
const TMUX_LIST_VERBS: &[&str] = &[
    "ls",
    "list-sessions",
    "list-windows",
    "lsw",
    "list-panes",
    "lsp",
    "list-clients",
    "lsc",
    "has-session",
    "has",
];

/// tmux verbs that read only with `-p` (print to stdout instead of a buffer
/// or the status line).
const TMUX_PRINT_VERBS: &[&str] = &["capture-pane", "capturep", "display-message", "display"];

/// `-p` given on its own or inside a short cluster (`-pt`, `-pS`).
fn has_short_flag(args: &[&str], flag: char) -> bool {
    args.iter()
        .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains(flag))
}

pub(super) fn tmux_reads<'a>(mut args: impl Iterator<Item = &'a str>) -> bool {
    // The verb must be first: global flags before it include `-f FILE`
    // (a config that runs commands) and `-c` (a shell command).
    let Some(verb) = args.next() else {
        return false;
    };
    let rest: Vec<&str> = args.collect();
    if TMUX_LIST_VERBS.contains(&verb) {
        // `-F` formats can run `#(cmd)`.
        return !rest.iter().any(|a| a.contains("#("));
    }
    TMUX_PRINT_VERBS.contains(&verb)
        && has_short_flag(&rest, 'p')
        && !has_short_flag(&rest, 'b')
        && !rest.iter().any(|a| a.contains("#("))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sed(c: &str) -> bool {
        sed_prints_only(c.split_whitespace())
    }
    fn tmux(c: &str) -> bool {
        tmux_reads(c.split_whitespace())
    }

    #[test]
    fn print_only_sed_scripts_read() {
        for c in [
            "-n 1,5p f.rs",
            "-n 10p a b",
            "-n $p f",
            "-n /fn/p f",
            "-n -e 1p -e 3,4p f",
            "5q f",
            "-nE 2,$p f",
            "-n 1p;3p f",
        ] {
            assert!(sed(c), "sed {c}");
        }
    }

    #[test]
    fn sed_that_can_write_or_exec_is_refused() {
        for c in [
            "-i s/a/b/ f",
            "-n 1w out f",
            "s/a/b/w out f",
            "-n 1e f",
            "-f prog f",
            "--in-place=.bak 1p f",
            "-n",
            "s/a/b/ f",
        ] {
            assert!(!sed(c), "sed {c}");
        }
    }

    #[test]
    fn tmux_listing_and_print_capture_read() {
        for c in [
            "capture-pane -pt s",
            "capture-pane -p -t s -S -30",
            "ls",
            "list-panes -a",
            "has-session -t x",
            "display-message -p #{pane_id}",
        ] {
            assert!(tmux(c), "tmux {c}");
        }
    }

    #[test]
    fn tmux_that_acts_is_refused() {
        for c in [
            "send-keys -t s rm Enter",
            "capture-pane -t s",
            "capture-pane -p -b buf",
            "-f evil.conf ls",
            "run-shell ls",
            "kill-session -t s",
            "ls -F #(id)",
            "",
        ] {
            assert!(!tmux(c), "tmux {c}");
        }
    }
}
