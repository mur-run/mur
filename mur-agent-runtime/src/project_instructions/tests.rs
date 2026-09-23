use super::*;
use std::fs;

fn grant(read: &[&Path], deny: &[&Path]) -> ProjectInstructions {
    let s = |ps: &[&Path]| {
        ps.iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    };
    ProjectInstructions::new(
        FilesystemEntitlement {
            read: s(read),
            write: vec![],
            deny: s(deny),
        },
        LaunchChain::inert(),
    )
}

/// A temp git repo with `sub/` inside, canonicalized (macOS `/var` → `/private/var`).
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    (tmp, root, sub)
}

#[test]
fn root_file_applies_from_a_subdirectory() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "run cargo fmt").unwrap();
    let block = grant(&[&root], &[])
        .render(&sub)
        .expect("root file applies");
    assert!(block.starts_with("\n\n## Project instructions"), "{block}");
    assert!(block.contains("run cargo fmt"), "{block}");
    assert!(
        block.contains(&root.join("AGENTS.md").display().to_string()),
        "{block}"
    );
}

#[test]
fn nested_files_load_root_first() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "ROOT-RULE").unwrap();
    fs::write(sub.join("AGENTS.md"), "SUB-RULE").unwrap();
    let block = grant(&[&root], &[]).render(&sub).unwrap();
    let (r, s) = (
        block.find("ROOT-RULE").unwrap(),
        block.find("SUB-RULE").unwrap(),
    );
    assert!(r < s, "the more specific file must come later:\n{block}");
}

#[test]
fn agents_md_wins_over_claude_md_and_claude_md_is_the_fallback() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "FROM-AGENTS").unwrap();
    fs::write(root.join("CLAUDE.md"), "FROM-CLAUDE").unwrap();
    fs::write(sub.join("CLAUDE.md"), "SUB-CLAUDE").unwrap();
    let block = grant(&[&root], &[]).render(&sub).unwrap();
    assert!(block.contains("FROM-AGENTS"), "{block}");
    assert!(
        !block.contains("FROM-CLAUDE"),
        "one file per directory:\n{block}"
    );
    assert!(
        block.contains("SUB-CLAUDE"),
        "CLAUDE.md alone still counts:\n{block}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_claude_md_is_not_loaded_twice() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "ONCE").unwrap();
    std::os::unix::fs::symlink(root.join("AGENTS.md"), sub.join("CLAUDE.md")).unwrap();
    let block = grant(&[&root], &[]).render(&sub).unwrap();
    assert_eq!(block.matches("ONCE").count(), 1, "{block}");
}

/// The prompt must not show what `read_file` would refuse.
#[test]
fn unentitled_and_denied_files_are_skipped() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "ROOT-RULE").unwrap();
    fs::write(sub.join("AGENTS.md"), "SECRET-RULE").unwrap();
    assert!(grant(&[], &[]).render(&sub).is_none(), "no grant, no text");
    let block = grant(&[&root], &[&sub]).render(&sub).unwrap();
    assert!(block.contains("ROOT-RULE"), "{block}");
    assert!(!block.contains("SECRET-RULE"), "deny wins:\n{block}");
}

/// Outside a repo, a file in a parent directory (think `$HOME/AGENTS.md`)
/// must not leak into every directory beneath it.
#[test]
fn outside_a_repo_only_the_cwd_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = fs::canonicalize(tmp.path()).unwrap();
    let cwd = parent.join("work");
    fs::create_dir(&cwd).unwrap();
    fs::write(parent.join("AGENTS.md"), "PARENT").unwrap();
    let gate = grant(&[&parent], &[]);
    assert!(gate.render(&cwd).is_none());
    fs::write(cwd.join("AGENTS.md"), "HERE").unwrap();
    assert!(gate.render(&cwd).unwrap().contains("HERE"));
}

#[test]
fn budget_cuts_on_a_char_boundary_and_names_what_was_not_loaded() {
    let (_t, root, sub) = repo();
    // 3-byte chars, so the byte limit lands mid-character.
    fs::write(
        root.join("AGENTS.md"),
        "專".repeat(MAX_PROJECT_INSTRUCTIONS_BYTES),
    )
    .unwrap();
    fs::write(sub.join("AGENTS.md"), "LATE").unwrap();
    let block = grant(&[&root], &[]).render(&sub).unwrap();
    assert!(block.contains("more bytes not shown"), "cut is announced");
    assert!(
        !block.contains("LATE"),
        "no budget left for the nested file"
    );
    assert!(
        block.contains(&sub.join("AGENTS.md").display().to_string()),
        "but it is named"
    );
}

#[test]
fn no_files_means_no_block() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "   \n").unwrap();
    assert!(
        grant(&[&root], &[]).render(&sub).is_none(),
        "blank file adds nothing"
    );
}
