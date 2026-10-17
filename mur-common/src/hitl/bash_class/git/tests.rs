//! PR #1932 review: every case here was checked against a real repo with a
//! textconv (`diff.<drv>.textconv`) and an external diff (`diff.external`)
//! configured — "prompt" rows ran a driver, "read" rows did not.

use crate::hitl::bash_class::is_readonly_bash;

#[test]
fn default_textconv_or_ext_diff_paths_prompt() {
    for c in [
        // a patch with no flag at all — textconv / external diff run
        "git diff",
        "git diff HEAD~1",
        "git show",
        "git show HEAD",
        "git show --oneline", // still prints the patch
        "git log -p",
        "git log -S needle", // pickaxe textconvs every blob
        "git log -G re",
        "git log --word-diff -p",
        "git diff --check",
        "git diff --exit-code",
        "git whatchanged -p",
        "git blame f.x",
        "git status -v",
        "git status -sv",
        "git status --verb", // parse-options prefix
        // explicit driver flags, including re-enabling after a disable
        "git log -p --ext-diff",
        "git diff --textconv",
        "git log -p --no-ext-diff --no-textconv --textconv",
        "git diff --no-textconv --no-ext-diff --ext-diff",
        "git blame --no-textconv --textconv f",
        "git cat-file --textconv HEAD:f",
        "git cat-file --te HEAD:f",
        "git cat-file --filters HEAD:f",
        "git cat-file --filt --path=f HEAD:x",
        "git grep --textconv pat",
        // half a disable is not a disable
        "git diff --no-textconv",
        "git log -p --no-ext-diff",
        // signature verification runs gpg.program
        "git log --show-signature",
        "git log --format=%G?",
        "git log --format %GS",
        "git log --pretty=%h%GK",
        // an unknown flag on a patch-capable command fails closed
        "git log --some-new-flag",
    ] {
        assert!(!is_readonly_bash(c), "should prompt: {c}");
    }
}

#[test]
fn no_patch_or_drivers_off_stays_read() {
    for c in [
        "git log",
        "git log --oneline -10",
        "git log -n5 --oneline",
        "git log -n 5",
        "git log --stat",
        "git log --name-status --follow -- f.x",
        "git log --format=%h%d --decorate --all",
        "git log --graph --oneline --all",
        "git log --author=x --since=1.day --grep=y -i",
        "git log -p --no-ext-diff --no-textconv",
        "git log -p --no-textconv --no-ext-diff --word-diff",
        "git log --oneline -- path/with-p",
        "git diff --stat HEAD~1 HEAD --",
        "git diff HEAD~1..HEAD --name-only --",
        "git diff --no-textconv --no-ext-diff HEAD~1 HEAD --",
        "git show -s",
        "git show --stat HEAD",
        "git show --no-patch --format=%H",
        "git show HEAD:Cargo.toml",
        "git show HEAD~1:src/",
        "git whatchanged --oneline",
        "git blame --no-textconv HEAD -- f.x",
        "git cat-file -p HEAD:f",
        "git grep -n pat HEAD --",
        "git shortlog -sn",
    ] {
        assert!(is_readonly_bash(c), "should stay read-only: {c}");
    }
}

#[test]
fn rg_and_file_exec_or_write_flags_prompt() {
    for c in [
        "rg --pre=touch pattern victim.txt",
        "rg --pre touch pattern",
        "rg -i --pre ./p x",
        "rg --hostname-bin=./h x",
        "file -C -m ./magic",
        "file -zC -m ./magic", // clustered short option
        "file --compile -m ./magic",
        "file --co -m ./magic", // getopt_long prefix
    ] {
        assert!(!is_readonly_bash(c), "should prompt: {c}");
    }
    for c in [
        "rg TODO",
        "rg -n --pre-glob '*.gz' x",
        "rg --no-pre x",
        "file Cargo.toml",
        "file -b --mime-type f",
    ] {
        assert!(is_readonly_bash(c), "should stay read-only: {c}");
    }
}

/// #1937: `core.fsmonitor` runs on any index refresh and `filter.<drv>.clean`
/// on a stat-dirty file, with no flag that can switch the second one off (the
/// driver name is the repo's). Checked on git 2.53.0 with both vectors set and
/// a stat-only change: "prompt" rows ran one, "read" rows ran neither.
#[test]
fn worktree_or_index_reads_prompt() {
    for c in [
        "git status",
        "git status -sb",
        "git status --porcelain=v2",
        "git ls-files",
        "git ls-files -s",
        "git ls-files --cached",
        "git diff --stat",
        "git diff --name-only",
        "git diff --no-textconv --no-ext-diff",
        "git diff --cached --stat",
        "git diff --staged --name-only HEAD~1 HEAD --",
        "git diff HEAD~1 --name-only", // one rev: the other side is the worktree
        "git diff HEAD~1 --stat --",
        "git diff HEAD~1 HEAD --stat", // no `--`: an operand may be a path
        "git diff HEAD..f.x --stat",   // `HEAD..f.x` can be a file name
        "git diff --no-index --stat a b",
        "git diff --stat :f HEAD:f --",
        "git grep -n pat",
        "git grep pat HEAD", // no `--`
        "git grep --cached pat HEAD --",
        "git grep -e pat --",
        "git grep -A 3 pat --", // `3` is a value, not a rev
        "git grep -n pat -- f.x",
        "git blame --no-textconv f.x",
        "git blame --no-textconv -- f.x",
        "git blame --no-textconv --contents f HEAD -- f.x",
        "git describe --dirty",
        "git describe --always --broken",
        "git describe --dir",
        "git show :f.x",
        "git show :0:f.x",
        "git rev-parse :f.x",
        "git cat-file -p :f.x",
        "git cat-file --batch",
        "git cat-file --batch-check",
    ] {
        assert!(!is_readonly_bash(c), "should prompt: {c}");
    }
}

#[test]
fn tree_to_tree_reads_stay_read() {
    for c in [
        "git diff --stat HEAD~1 HEAD --",
        "git diff --name-only HEAD~1..HEAD --",
        "git diff --stat HEAD~3...HEAD -- src/",
        "git diff --no-textconv --no-ext-diff HEAD~1 HEAD --",
        "git diff --stat HEAD:a HEAD~1:a --",
        "git grep -n pat HEAD --",
        "git grep -n -e pat HEAD --",
        "git grep -in pat HEAD~1 -- src/",
        "git blame --no-textconv HEAD -- f.x",
        "git blame --no-textconv -L 1,5 HEAD -- f.x",
        "git blame --no-textconv -L1,5 HEAD~1 -- f.x",
        "git describe",
        "git describe --always --tags",
        "git show HEAD:f.x",
        "git show :/fix",
        "git rev-parse HEAD",
        "git rev-parse --show-toplevel",
        "git cat-file -p HEAD:f.x",
        "git ls-tree -r HEAD",
        "git log --oneline -1 -- f.x",
    ] {
        assert!(is_readonly_bash(c), "should stay read-only: {c}");
    }
}
