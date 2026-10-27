use super::*;
use std::path::Path;

const CWD: &str = "/work";

fn targets(cmd: &str) -> Vec<PathBuf> {
    match plan(cmd, Path::new(CWD)) {
        RmPlan::Rewrite(t) => t.into_iter().map(|t| t.path).collect(),
        other => panic!("{cmd:?}: expected Rewrite, got {other:?}"),
    }
}

fn is_unrewritable(cmd: &str) -> bool {
    matches!(plan(cmd, Path::new(CWD)), RmPlan::Unrewritable { .. })
}

#[test]
fn plain_removes_are_rewritten_with_resolved_paths() {
    assert_eq!(targets("rm a"), [PathBuf::from("/work/a")]);
    assert_eq!(targets("rm -rf dir"), [PathBuf::from("/work/dir")]);
    assert_eq!(targets("rm -- -weird"), [PathBuf::from("/work/-weird")]);
    assert_eq!(
        targets("rm \"with space\""),
        [PathBuf::from("/work/with space")]
    );
    assert_eq!(targets("rm 'single q'"), [PathBuf::from("/work/single q")]);
    assert_eq!(targets("sudo rm a"), [PathBuf::from("/work/a")]);
    assert_eq!(
        targets("rm /abs/x b"),
        [PathBuf::from("/abs/x"), PathBuf::from("/work/b")]
    );
    assert_eq!(targets("/bin/rm a"), [PathBuf::from("/work/a")]);
}

#[test]
fn cd_prefix_moves_the_base() {
    assert_eq!(targets("cd sub && rm a"), [PathBuf::from("/work/sub/a")]);
    assert_eq!(targets("cd /tmp/x && rm -r y"), [PathBuf::from("/tmp/x/y")]);
    assert_eq!(
        targets("rm a && rm b"),
        [PathBuf::from("/work/a"), PathBuf::from("/work/b")]
    );
}

#[test]
fn flags_become_the_rule() {
    let RmPlan::Rewrite(t) = plan("rm -Rfv d", Path::new(CWD)) else {
        panic!()
    };
    assert_eq!(
        t[0].rule,
        Rule::Rm {
            recursive: true,
            force: true,
            empty_dirs: false
        }
    );
    let RmPlan::Rewrite(t) = plan("rmdir e", Path::new(CWD)) else {
        panic!()
    };
    assert_eq!(t[0].rule, Rule::Rmdir);
    let RmPlan::Rewrite(t) = plan("unlink f", Path::new(CWD)) else {
        panic!()
    };
    assert_eq!(t[0].rule, Rule::Unlink);
}

#[test]
fn anything_not_understood_is_unrewritable() {
    for cmd in [
        "rm *.log",
        "rm $X",
        "rm $(ls)",
        "rm `x`",
        "rm a | tee",
        "rm a && make",
        "make && rm a",
        "find . -delete",
        "rm --no-preserve-root /",
        "rm /",
        "rm .",
        "rm ..",
        "rm -i a",
        "rm a; ls",
        "rm a || true",
        "rm a > log",
        "rm ~/x",
        "rm {a,b}",
        "rm \"$HOME/x\"",
        "rm a\\ b",
        "X=1 rm a",
        "sudo -u bob rm a",
        "rm",
        "rm -f",
        "unlink a b",
        "rmdir -p a/b",
        "cd - && rm a",
        "(rm a)",
        "ls | xargs rm",
        "rm a &",
    ] {
        assert!(
            is_unrewritable(cmd),
            "{cmd:?} -> {:?}",
            plan(cmd, Path::new(CWD))
        );
    }
}

#[test]
fn unbalanced_quotes_are_unparseable() {
    assert_eq!(
        plan("rm \"unterminated", Path::new(CWD)),
        RmPlan::Unparseable
    );
    assert_eq!(plan("rm 'x", Path::new(CWD)), RmPlan::Unparseable);
}

#[test]
fn non_removers_are_left_alone() {
    for cmd in [
        "echo rm",
        "grep rm file",
        "ls -la",
        "cargo test rm",
        "git rm --cached x",
        "echo 'rm -rf /'",
        "rmate file",
    ] {
        assert_eq!(plan(cmd, Path::new(CWD)), RmPlan::NotRm, "{cmd:?}");
    }
}
