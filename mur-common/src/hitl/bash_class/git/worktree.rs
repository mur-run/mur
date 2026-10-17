//! #1937: does a git read stay off the worktree and the index?
//!
//! Two repo-config vectors run a program on a plain read with no flag at all:
//! `core.fsmonitor` on any index refresh, and `filter.<drv>.clean` when a
//! stat-dirty file has to be re-hashed. The driver name is the repo's, so no
//! `-c` can switch every clean filter off. The only sound rule is the shape of
//! the command: Read only when every object it names is a tree-ish, never the
//! worktree or the index. Fail-closed — any operand git could take as a path
//! prompts, which is why `diff`/`grep`/`blame` need an explicit `--`.

/// Always compare the worktree or index — no flag makes them tree-only.
const ALWAYS_TOUCHES: &[&str] = &["status", "ls-files"];

/// `diff` flags that put the index or worktree on one side.
const DIFF_INDEX_FLAGS: &[&str] = &["--cached", "--staged", "--no-index", "--merge-base"];

/// `diff` options proven to take no separate value, matched on the name
/// before any `=`. Anything else (`-O f`, `-S str`, `-G re`, an abbreviated
/// `--cach`) prompts: a value word would be miscounted as the second rev and
/// git would diff the one real rev against the worktree.
const DIFF_NOVALUE: &[&str] = &[
    "--stat",
    "--numstat",
    "--shortstat",
    "--summary",
    "--name-only",
    "--name-status",
    "--raw",
    "--patch",
    "-p",
    "-u",
    "--no-patch",
    "-s",
    "--no-textconv",
    "--no-ext-diff",
    "--no-color",
    "--no-renames",
    "--quiet",
    "--exit-code",
    "--ignore-all-space",
    "-w",
    "--ignore-space-change",
    "-b",
    "--ignore-blank-lines",
    "-R",
    "--full-index",
];

/// `grep` flags that search the index or worktree instead of the named tree.
/// `--untracked` / `--no-index` read the worktree; `--cached` the index.
const GREP_INDEX_FLAGS: &[&str] = &["--cached", "--untracked", "--no-index"];

/// `grep` options proven to take no value, so every other word is pattern or
/// rev. Anything else (`-A 3`, `-f file`, `--max-depth 2`) prompts: a value
/// word would be misread as a rev.
const GREP_NOVALUE_SHORT: &str = "nilcwvhHEFPoqLGz";
const GREP_NOVALUE_LONG: &[&str] = &[
    "--line-number",
    "--ignore-case",
    "--files-with-matches",
    "--files-without-match",
    "--count",
    "--word-regexp",
    "--invert-match",
    "--extended-regexp",
    "--basic-regexp",
    "--fixed-strings",
    "--perl-regexp",
    "--only-matching",
    "--quiet",
    "--heading",
    "--break",
    "--column",
    "--full-name",
    "--no-color",
];
/// The explicit-pattern option; its value is the NEXT word.
const GREP_PATTERN_FLAGS: &[&str] = &["-e", "--regexp"];

/// `blame` options proven to take no value. `-L` takes one (separate or
/// attached). `--contents` reads a file in place of the blob — absent.
const BLAME_NOVALUE: &[&str] = &[
    "-w",
    "-s",
    "-e",
    "-l",
    "-t",
    "-f",
    "-n",
    "-p",
    "-M",
    "--root",
    "--porcelain",
    "--line-porcelain",
    "--show-email",
    "--show-name",
    "--show-number",
    "--no-textconv",
];
const BLAME_RANGE: &str = "-L";

/// `describe` flags that compare the worktree; parse-options takes prefixes.
const DESCRIBE_WORKTREE_LONG: &[&str] = &["--dirty", "--broken"];

/// A revision written `:path` / `:N:path` names an index entry (an index
/// refresh runs fsmonitor). `:/text` searches commit messages — no index.
const INDEX_PATH_PREFIX: char = ':';
const MESSAGE_SEARCH_PREFIX: &str = ":/";
/// Options that read object names from stdin, which may name `:path`.
const STDIN_OBJECT_FLAGS: &[&str] = &["--stdin", "--batch"];

const PATHSPEC_SEP: &str = "--";
const RANGE_SEP: &str = "..";

/// True when the command provably reads only trees, commits and blobs.
pub(super) fn stays_off_worktree(sub: &str, args: &[&str]) -> bool {
    if ALWAYS_TOUCHES.contains(&sub) {
        return false;
    }
    let opts = before_sep(args);
    if opts
        .iter()
        .any(|a| names_index_entry(a) || reads_stdin_objects(a))
    {
        return false;
    }
    match sub {
        "diff" => diff_is_tree_to_tree(args, &opts),
        "grep" => grep_names_a_tree(args, &opts),
        "blame" => blame_names_a_rev(args, &opts),
        "describe" => !opts
            .iter()
            .any(|a| super::long_prefix_of(a, DESCRIBE_WORKTREE_LONG)),
        _ => true,
    }
}

/// Words before a bare `--`; the rest is pathspec.
fn before_sep<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter()
        .copied()
        .take_while(|a| *a != PATHSPEC_SEP)
        .collect()
}

fn has_sep(args: &[&str]) -> bool {
    args.contains(&PATHSPEC_SEP)
}

fn names_index_entry(arg: &str) -> bool {
    arg.starts_with(INDEX_PATH_PREFIX) && !arg.starts_with(MESSAGE_SEARCH_PREFIX)
}

fn reads_stdin_objects(arg: &str) -> bool {
    STDIN_OBJECT_FLAGS
        .iter()
        .any(|f| super::flag_name(arg).starts_with(f))
}

/// Two revs, or one range — and `--` so neither can be a file name.
fn diff_is_tree_to_tree(args: &[&str], opts: &[&str]) -> bool {
    if !has_sep(args) {
        return false;
    }
    let (flags, revs): (Vec<&str>, Vec<&str>) = opts.iter().partition(|a| a.starts_with('-'));
    if flags.iter().any(|f| {
        let name = super::flag_name(f);
        DIFF_INDEX_FLAGS.contains(&name) || !DIFF_NOVALUE.contains(&name)
    }) {
        return false;
    }
    match revs.as_slice() {
        [a, b] => !a.contains(RANGE_SEP) && !b.contains(RANGE_SEP),
        [r] => r.contains(RANGE_SEP),
        _ => false,
    }
}

/// `grep [flags] <pattern> <rev>… --` with at least one rev.
fn grep_names_a_tree(args: &[&str], opts: &[&str]) -> bool {
    if !has_sep(args) {
        return false;
    }
    let mut positionals = 0usize;
    let mut explicit_pattern = false;
    let mut it = opts.iter();
    while let Some(a) = it.next() {
        if GREP_PATTERN_FLAGS.contains(a) {
            explicit_pattern = true;
            if it.next().is_none() {
                return false;
            }
        } else if a.starts_with("--") {
            if GREP_INDEX_FLAGS.contains(&super::flag_name(a)) || !GREP_NOVALUE_LONG.contains(a) {
                return false;
            }
        } else if let Some(cluster) = a.strip_prefix('-') {
            if cluster.is_empty() || !cluster.chars().all(|c| GREP_NOVALUE_SHORT.contains(c)) {
                return false;
            }
        } else {
            positionals += 1;
        }
    }
    let revs = if explicit_pattern {
        positionals
    } else {
        positionals.saturating_sub(1)
    };
    revs >= 1
}

/// `blame [flags] <rev> -- <file>`: exactly one rev before `--`.
fn blame_names_a_rev(args: &[&str], opts: &[&str]) -> bool {
    if !has_sep(args) {
        return false;
    }
    let mut revs = 0usize;
    let mut it = opts.iter();
    while let Some(a) = it.next() {
        if *a == BLAME_RANGE {
            if it.next().is_none() {
                return false;
            }
        } else if a.starts_with(BLAME_RANGE) && a.len() > BLAME_RANGE.len() {
            // attached `-L1,5`
        } else if a.starts_with('-') {
            if !BLAME_NOVALUE.contains(a) {
                return false;
            }
        } else {
            revs += 1;
        }
    }
    revs == 1
}
