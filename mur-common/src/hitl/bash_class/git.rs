//! Read-only classification of a `git …` command line.
//!
//! A "read" git subcommand is not read-only under every flag or every repo:
//! `--output=F` overwrites a file, `grep -O` runs a command, and anything that
//! renders a diff can run a driver the REPO configured — an external diff
//! (`diff.external`, `diff.<drv>.command`) or a textconv filter
//! (`diff.<drv>.textconv`). textconv is ON by default for `log -p`, `show`,
//! `diff`, `blame`, pickaxe (`log -S/-G`) and `status -v`, so blocking the
//! explicit `--textconv`/`--ext-diff` flags is not enough. The rule is
//! fail-closed: a diff-family command is Read only when it provably renders no
//! patch (every flag is on a no-patch allowlist) or it switches both drivers
//! off (`--no-ext-diff --no-textconv`).
//!
//! NOT covered, deliberately: config that runs a program on a plain read with
//! no flag at all (`core.fsmonitor` on `git status`). Closing that would take
//! `git status` out of the read lane; it is a repo-trust problem, not a flag.

/// `git` subcommands that only read, given the per-subcommand flag checks below.
/// Excludes anything with a write mode (`branch -D`, `tag X`, `remote add`,
/// `config`, `stash`, …).
///
/// REMOVED `reflog` — `git reflog expire/delete` is destructive (prunes the reflog).
const GIT_READONLY_SUBCMDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "describe",
    "shortlog",
    "whatchanged",
    "grep",
];

/// Flags under which `git branch` only LISTS. Any positional argument makes it
/// create a branch (`git branch x`), and `-d/-D/-m/-M/-c/-C/-f/-u` rename,
/// delete, copy or retarget — all absent. `--list` is here but its pattern
/// operand is not: a positional still fails the check, which costs one prompt
/// for `git branch --list 'feat*'` and buys a parser that never has to know
/// which flags take values.
const GIT_BRANCH_LIST_FLAGS: &[&str] = &[
    "--show-current",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--list",
    "--no-color",
];

/// Flags under which `git remote` only prints the configured remotes. Bare
/// `git remote` lists names. `show`/`update`/`prune` are subcommands, not
/// flags, so they fail the check — `show` and `update` contact the remote,
/// `prune` deletes refs.
const GIT_REMOTE_LIST_FLAGS: &[&str] = &["-v", "--verbose"];

/// The diff-family option that writes the output to a FILE instead of stdout
/// (`git log/show/diff/whatchanged --output=F`). Revision options are matched
/// exactly by git — `--outp=F` is rejected — so no abbreviation check here.
const GIT_OUTPUT_FLAG: &str = "--output";

/// `git grep -O[cmd]` / `--open-files-in-pager[=cmd]` RUNS a command. grep
/// parses with parse-options, which accepts any unique long prefix (`--open`),
/// so the long form is matched by prefix down to this minimum (`--op`).
const GIT_GREP_EXEC_LONG: &str = "--open-files-in-pager";
const GIT_GREP_EXEC_MIN_PREFIX: usize = 4;
const GIT_GREP_EXEC_SHORT: char = 'O';

/// Subcommands that render through the diff machinery, where the repo's
/// textconv / external-diff drivers apply.
const DIFF_FAMILY: &[&str] = &["log", "whatchanged", "show", "diff"];
/// Of those, the ones that print a patch with no flag at all.
const PATCH_BY_DEFAULT: &[&str] = &["show", "diff"];

/// The two flags that switch the repo's diff drivers off, and the two that
/// switch them back on (last one wins in git). Revision/diff options are
/// matched exactly by git (`--textc`, `--ext` are rejected), so exact match.
const NO_TEXTCONV: &str = "--no-textconv";
const NO_EXT_DIFF: &str = "--no-ext-diff";
const TEXTCONV: &str = "--textconv";
const EXT_DIFF: &str = "--ext-diff";

/// Verifying a signature runs `gpg.program`, which the repo config chooses.
const SHOW_SIGNATURE: &str = "--show-signature";
/// The format placeholder family (`%G?`, `%GS`, …) that verifies signatures.
const FORMAT_SIGNATURE_PLACEHOLDER: &str = "%G";

/// Flags that suppress the patch, or replace it with a summary that runs no
/// driver. `show`/`diff` need one of these (or both drivers off) to be Read.
const PATCH_SUPPRESSORS: &[&str] = &[
    "-s",
    "--no-patch",
    "--quiet",
    "--stat",
    "--numstat",
    "--shortstat",
    "--summary",
    "--dirstat",
    "--name-only",
    "--name-status",
    "--raw",
];

/// Diff-family flags that select or format commits and render no patch —
/// verified against a repo with a textconv and an external diff configured.
/// Matched by name, ignoring `=value`; a separate value word is treated as a
/// positional. Anything else (`-p`, `-S`, `-G`, `--word-diff`, `--check`,
/// `--exit-code`, a short cluster, …) falls off the list and prompts unless
/// both drivers are off.
const NO_PATCH_FLAGS: &[&str] = &[
    "--oneline",
    "-n",
    "--max-count",
    "--skip",
    "--since",
    "--until",
    "--after",
    "--before",
    "--author",
    "--committer",
    "--grep",
    "-i",
    "--regexp-ignore-case",
    "--all",
    "--branches",
    "--tags",
    "--remotes",
    "--decorate",
    "--no-decorate",
    "--graph",
    "--format",
    "--pretty",
    "--abbrev-commit",
    "--no-abbrev-commit",
    "--date",
    "--relative-date",
    "--reverse",
    "--first-parent",
    "--no-merges",
    "--merges",
    "--follow",
    "--topo-order",
    "--date-order",
    "--left-right",
    "--parents",
    "--no-walk",
    "--no-color",
    "--color",
    "--cached",
    "--staged",
    "-M",
    "--find-renames",
    NO_TEXTCONV,
    NO_EXT_DIFF,
];

/// `status` prints a diff (textconv on) when verbose. parse-options takes any
/// unique long prefix (`--v`, `--verb`); the short form can cluster (`-sv`).
const STATUS_VERBOSE_LONG: &str = "--verbose";
const STATUS_VERBOSE_SHORT: char = 'v';
/// `cat-file --textconv/--filters` and `grep --textconv` run the drivers; both
/// parse with parse-options, so long prefixes count down to `--t` / `--f`.
const CAT_FILE_EXEC_LONG: &[&str] = &[TEXTCONV, "--filters"];
const GREP_EXEC_LONG: &[&str] = &[TEXTCONV];
const LONG_PREFIX_MIN: usize = 3;

/// The decision for the words after `git`.
pub(super) fn is_readonly<'a>(mut toks: impl Iterator<Item = &'a str>) -> bool {
    match toks.next() {
        Some("branch") => only_flags(toks, GIT_BRANCH_LIST_FLAGS),
        Some("remote") => only_flags(toks, GIT_REMOTE_LIST_FLAGS),
        Some(sub) if GIT_READONLY_SUBCMDS.contains(&sub) => {
            let args: Vec<&str> = toks.collect();
            !args.iter().any(|t| flag_writes(sub, t)) && runs_no_driver(sub, &args)
        }
        _ => false,
    }
}

/// Is every remaining token one of `allowed`? No positional operands at all.
fn only_flags<'a>(mut rest: impl Iterator<Item = &'a str>, allowed: &[&str]) -> bool {
    rest.all(|t| allowed.contains(&t))
}

/// The flag's name, without an `=value`.
fn flag_name(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(n, _)| n)
}

/// Is `arg` a long option parse-options could expand to one of `targets`?
fn long_prefix_of(arg: &str, targets: &[&str]) -> bool {
    let name = flag_name(arg);
    name.starts_with("--")
        && name.len() >= LONG_PREFIX_MIN
        && targets.iter().any(|t| t.starts_with(name))
}

/// Does this argument turn a read-only git subcommand into a write or an exec?
/// Fail-safe on shape: any short-option cluster holding `O` counts for grep,
/// even where the `O` might be a value, because a false prompt is cheap.
fn flag_writes(sub: &str, arg: &str) -> bool {
    let name = flag_name(arg);
    if name == GIT_OUTPUT_FLAG {
        return true;
    }
    if sub != "grep" {
        return false;
    }
    if name.starts_with("--") {
        return name.len() >= GIT_GREP_EXEC_MIN_PREFIX && GIT_GREP_EXEC_LONG.starts_with(name);
    }
    name.starts_with('-') && name.contains(GIT_GREP_EXEC_SHORT)
}

/// Can this command run a program the repo config chose? Fail-closed per
/// subcommand; anything not shown safe answers "it can" (→ prompt).
fn runs_no_driver(sub: &str, args: &[&str]) -> bool {
    match sub {
        "status" => !args.iter().any(|a| status_verbose(a)),
        "cat-file" => !args.iter().any(|a| long_prefix_of(a, CAT_FILE_EXEC_LONG)),
        "grep" => !args.iter().any(|a| long_prefix_of(a, GREP_EXEC_LONG)),
        // blame runs textconv by default; it has no external-diff path. Its
        // `--textconv` would re-enable (last wins), so it must be absent.
        "blame" => {
            args.contains(&NO_TEXTCONV) && !args.iter().any(|a| long_prefix_of(a, &[TEXTCONV]))
        }
        s if DIFF_FAMILY.contains(&s) => diff_family_is_safe(s, args),
        _ => true,
    }
}

fn status_verbose(arg: &str) -> bool {
    if arg.starts_with("--") {
        return long_prefix_of(arg, &[STATUS_VERBOSE_LONG]);
    }
    arg.starts_with('-') && arg.contains(STATUS_VERBOSE_SHORT)
}

fn diff_family_is_safe(sub: &str, args: &[&str]) -> bool {
    // Options before a bare `--`; everything after it is a pathspec.
    let opts: Vec<&str> = args.iter().copied().take_while(|a| *a != "--").collect();
    if opts.iter().any(|a| verifies_signature(a)) {
        return false;
    }
    let drivers_off = opts.contains(&NO_TEXTCONV)
        && opts.contains(&NO_EXT_DIFF)
        && !opts.iter().any(|a| *a == TEXTCONV || *a == EXT_DIFF);
    drivers_off || renders_no_patch(sub, &opts)
}

/// The placeholder is checked on EVERY word, not only `--format=…`: the format
/// can also arrive as the next word (`--format '%G?'`). A grep pattern holding
/// `%G` prompts too — a false prompt is cheap.
fn verifies_signature(arg: &str) -> bool {
    arg == SHOW_SIGNATURE || arg.contains(FORMAT_SIGNATURE_PLACEHOLDER)
}

/// Every flag is on a no-patch list, and a subcommand that patches by default
/// carries an explicit suppressor — or, for `show`, names only `rev:path`
/// objects (a blob/tree print, no diff).
fn renders_no_patch(sub: &str, opts: &[&str]) -> bool {
    let (flags, positionals): (Vec<&str>, Vec<&str>) =
        opts.iter().partition(|a| a.starts_with('-'));
    let suppressor = |f: &&str| PATCH_SUPPRESSORS.contains(&flag_name(f));
    if !flags.iter().all(|f| suppressor(f) || is_no_patch_flag(f)) {
        return false;
    }
    if !PATCH_BY_DEFAULT.contains(&sub) {
        return true;
    }
    let objects_only =
        sub == "show" && !positionals.is_empty() && positionals.iter().all(|p| p.contains(':'));
    flags.iter().any(suppressor) || objects_only
}

/// `-5` / `-n5` count limits, or a listed flag (by name, ignoring `=value`).
fn is_no_patch_flag(arg: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = arg.strip_prefix('-')
        && (digits(rest) || rest.strip_prefix('n').is_some_and(digits))
    {
        return true;
    }
    NO_PATCH_FLAGS.contains(&flag_name(arg))
}

#[cfg(test)]
mod tests;
