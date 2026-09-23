use super::*;
use std::fs;

use budget::fair_share;
use decode::decode_file;

/// Cap wide enough that no test below the budget section is cut.
const WIDE: usize = MAX_PROJECT_INSTRUCTIONS_BYTES;

fn grant(read: &[&Path], deny: &[&Path]) -> ProjectInstructions {
    grant_with_chain(read, deny, LaunchChain::inert())
}

fn grant_with_chain(read: &[&Path], deny: &[&Path], chain: LaunchChain) -> ProjectInstructions {
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
        chain,
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

fn text(pi: &ProjectInstructions, cwd: &Path) -> String {
    pi.render(cwd, WIDE).expect("a block").text
}

// ── Existing behaviour, rewritten to the new format ────────────────────────

#[test]
fn root_file_applies_from_a_subdirectory() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "run cargo fmt").unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert!(block.contains("run cargo fmt"), "{block}");
    assert!(block.contains(r#"<file path="AGENTS.md">"#), "{block}");
}

#[test]
fn nested_files_load_root_first() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "ROOT-RULE").unwrap();
    fs::write(sub.join("AGENTS.md"), "SUB-RULE").unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
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
    let block = text(&grant(&[&root], &[]), &sub);
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
    // Same-directory symlink too: the common `CLAUDE.md -> AGENTS.md` setup.
    std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert_eq!(block.matches("ONCE").count(), 1, "{block}");
    assert!(
        !block.contains("shadowed"),
        "a symlink is not a loser:\n{block}"
    );
}

/// The prompt must not show what `read_file` would refuse.
#[test]
fn unentitled_and_denied_files_are_skipped() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "ROOT-RULE").unwrap();
    fs::write(sub.join("AGENTS.md"), "SECRET-RULE").unwrap();
    assert!(
        grant(&[], &[]).render(&sub, WIDE).is_none(),
        "no grant, no text"
    );
    let block = text(&grant(&[&root], &[&sub]), &sub);
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
    assert!(gate.render(&cwd, WIDE).is_none());
    fs::write(cwd.join("AGENTS.md"), "HERE").unwrap();
    assert!(text(&gate, &cwd).contains("HERE"));
}

#[test]
fn no_files_means_no_block() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "   \n").unwrap();
    assert!(
        grant(&[&root], &[]).render(&sub, WIDE).is_none(),
        "blank file adds nothing"
    );
}

// ── Format and escaping ─────────────────────────────────────────────────────

#[test]
fn renders_one_file_as_xml_with_relative_path_and_repo_root() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "RULE").unwrap();
    let r = grant(&[&root], &[]).render(&sub, WIDE).unwrap();
    let open = format!(r#"<project_instructions root="{}">"#, root.display());
    assert!(r.text.starts_with(&open), "{}", r.text);
    let file = r.text.find(r#"<file path="AGENTS.md">"#).expect("file tag");
    let body = r.text.find("RULE").unwrap();
    let close = r.text.find("</file>").unwrap();
    assert!(file < body && body < close, "{}", r.text);
    assert!(
        r.text.trim_end().ends_with("</project_instructions>"),
        "{}",
        r.text
    );
    assert_eq!(r.bytes, r.text.len());
}

#[test]
fn precedence_preamble_is_emitted_once_before_first_file() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "A").unwrap();
    fs::write(sub.join("AGENTS.md"), "B").unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert_eq!(block.matches(PRECEDENCE_PREAMBLE).count(), 1, "{block}");
    assert!(block.find(PRECEDENCE_PREAMBLE).unwrap() < block.find("<file").unwrap());
}

#[test]
fn escapes_block_tags_in_file_bodies_case_insensitively() {
    let (_t, root, sub) = repo();
    fs::write(
        root.join("AGENTS.md"),
        "a </PROJECT_INSTRUCTIONS> b <File path=\"x\"> c <not_loaded d <br> e Vec<T>",
    )
    .unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert!(block.contains("&lt;/PROJECT_INSTRUCTIONS>"), "{block}");
    assert!(block.contains("&lt;File path=\"x\">"), "{block}");
    assert!(block.contains("&lt;not_loaded d"), "{block}");
    assert!(
        block.contains("<br>") && block.contains("Vec<T>"),
        "{block}"
    );
    assert_eq!(block.matches("</project_instructions>").count(), 1);
}

#[test]
fn escape_body_leaves_non_block_tags_alone() {
    assert_eq!(escape_body("x<y <b> </FILE"), "x<y <b> &lt;/FILE");
}

#[test]
fn outside_a_repo_root_is_the_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = fs::canonicalize(tmp.path()).unwrap();
    fs::write(cwd.join("AGENTS.md"), "HERE").unwrap();
    let block = text(&grant(&[&cwd], &[]), &cwd);
    assert!(
        block.starts_with(&format!(
            r#"<project_instructions root="{}">"#,
            cwd.display()
        )),
        "{block}"
    );
    assert!(block.contains(r#"<file path="AGENTS.md">"#), "{block}");
}

#[test]
fn cwd_change_between_renders_switches_root_and_files() {
    let (_a, root_a, _) = repo();
    let (_b, root_b, _) = repo();
    fs::write(root_a.join("AGENTS.md"), "RULE-A").unwrap();
    fs::write(root_b.join("AGENTS.md"), "RULE-B").unwrap();
    let pi = grant(&[&root_a, &root_b], &[]);
    let (a, b) = (text(&pi, &root_a), text(&pi, &root_b));
    assert!(a.contains(&root_a.display().to_string()) && a.contains("RULE-A"));
    assert!(b.contains(&root_b.display().to_string()) && b.contains("RULE-B"));
    assert!(!a.contains("RULE-B") && !b.contains("RULE-A"));
}

// ── Decoding ────────────────────────────────────────────────────────────────

#[test]
fn nul_byte_anywhere_is_unreadable() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "OK").unwrap();
    fs::write(sub.join("AGENTS.md"), b"bin\0ary").unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert!(
        block.contains(r#"<not_loaded path="sub/AGENTS.md" reason="unreadable"/>"#),
        "{block}"
    );
    assert!(!block.contains(r#"<file path="sub/AGENTS.md">"#), "{block}");
}

fn with_invalid(bad: usize) -> Vec<u8> {
    let mut v = vec![b'a'; 100 - bad];
    v.extend(std::iter::repeat_n(0xFFu8, bad));
    v
}

#[test]
fn mostly_invalid_utf8_is_unreadable() {
    assert!(decode_file(&with_invalid(15)).is_err());
}

#[test]
fn a_few_invalid_bytes_load_lossily() {
    let s = decode_file(&with_invalid(2)).expect("loads");
    assert!(s.contains('\u{FFFD}'));
}

#[test]
fn leading_bom_is_stripped() {
    let s = decode_file("\u{FEFF}rule".as_bytes()).unwrap();
    assert_eq!(s, "rule");
}

#[test]
fn file_deleted_between_discover_and_read_is_skipped() {
    let (_t, root, _) = repo();
    fs::write(root.join("AGENTS.md"), "OK").unwrap();
    let found = Found {
        root: root.clone(),
        files: vec![root.join("AGENTS.md"), root.join("gone/AGENTS.md")],
        shadowed: vec![],
    };
    let r = grant(&[&root], &[]).render_found(&found, WIDE).unwrap();
    assert!(r.text.contains("OK"), "{}", r.text);
    assert!(!r.text.contains("gone"), "{}", r.text);
}

// ── Shadowing ───────────────────────────────────────────────────────────────

#[test]
fn losers_in_the_same_dir_are_listed_as_shadowed() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "WIN").unwrap();
    fs::write(root.join("CLAUDE.md"), "LOSE").unwrap();
    let block = text(&grant(&[&root], &[]), &sub);
    assert!(
        block.contains(r#"<not_loaded path="CLAUDE.md" reason="shadowed"/>"#),
        "{block}"
    );
    assert!(!block.contains("LOSE"), "{block}");
    assert!(block.find("</file>").unwrap() < block.find("<not_loaded").unwrap());
}

// ── Fair share ──────────────────────────────────────────────────────────────

#[test]
fn fair_share_two_files_root_whole_nested_cut() {
    assert_eq!(fair_share(&[1024, 102400], 16384), vec![1024, 15360]);
}

#[test]
fn fair_share_three_files_redistributes_remainder() {
    assert_eq!(
        fair_share(&[2048, 3072, 30720], 16384),
        vec![2048, 3072, 11264]
    );
}

#[test]
fn fair_share_all_over_share_splits_evenly() {
    let got = fair_share(&[10240, 10240, 30720], 16384);
    assert!(got.iter().sum::<usize>() <= 16384, "{got:?}");
    assert!(got.iter().all(|&n| n.abs_diff(5461) <= 1), "{got:?}");
}

#[test]
fn budget_cuts_on_a_char_boundary_and_names_what_was_not_loaded() {
    let (_t, root, sub) = repo();
    fs::write(root.join("AGENTS.md"), "r".repeat(1024)).unwrap();
    // 3-byte chars, so the byte limit lands mid-character.
    fs::write(sub.join("AGENTS.md"), "專".repeat(102_400 / 3)).unwrap();
    let block = grant(&[&root], &[]).render(&sub, 16384).unwrap().text;
    assert!(block.contains(&"r".repeat(1024)), "root loads whole");
    let nested = block
        .find(r#"<file path="sub/AGENTS.md">"#)
        .expect("nested file");
    let cut = &block[nested..block[nested..].find("</file>").unwrap() + nested];
    assert!(
        cut.contains('專') && cut.len() < 16384,
        "nested is truncated"
    );
    assert_eq!(block.matches(r#"reason="budget""#).count(), 1, "{block}");
    assert!(block.contains(r#"<not_loaded path="sub/AGENTS.md" reason="budget"/>"#));
}

#[test]
fn three_budget_entries_when_all_files_are_cut() {
    let (_t, root, sub) = repo();
    let deep = sub.join("deep");
    fs::create_dir(&deep).unwrap();
    fs::write(root.join("AGENTS.md"), "a".repeat(10240)).unwrap();
    fs::write(sub.join("AGENTS.md"), "b".repeat(10240)).unwrap();
    fs::write(deep.join("AGENTS.md"), "c".repeat(30720)).unwrap();
    let block = grant(&[&root], &[]).render(&deep, 16384).unwrap().text;
    assert_eq!(block.matches(r#"reason="budget""#).count(), 3, "{block}");
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// `<tmp>` is MUR's home; the chain protects `<tmp>/secrets/**`.
fn refusal_found() -> (tempfile::TempDir, ProjectInstructions, Found) {
    let tmp = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(tmp.path()).unwrap();
    for (d, body) in [
        ("granted", "GRANTED-RULE"),
        ("nogrant", "NOGRANT-RULE"),
        ("denied", "DENIED-RULE"),
        ("secrets", "SECRET-RULE"),
    ] {
        fs::create_dir_all(home.join(d)).unwrap();
        fs::write(home.join(d).join("AGENTS.md"), body).unwrap();
    }
    fs::create_dir_all(home.join("agents/mur")).unwrap();
    let chain = LaunchChain::for_test(
        &home.join("agents/mur"),
        &home.join("bin"),
        &home.join("home"),
    );
    let pi = grant_with_chain(
        &[
            &home.join("granted"),
            &home.join("denied"),
            &home.join("secrets"),
        ],
        &[&home.join("denied")],
        chain,
    );
    let found = Found {
        root: home.clone(),
        files: ["granted", "nogrant", "denied", "secrets"]
            .iter()
            .map(|d| home.join(d).join("AGENTS.md"))
            .collect(),
        shadowed: vec![],
    };
    (tmp, pi, found)
}

#[test]
fn no_grant_is_listed_and_deny_and_launch_chain_are_omitted() {
    let (_t, pi, found) = refusal_found();
    let block = pi.render_found(&found, WIDE).unwrap().text;
    assert!(block.contains("GRANTED-RULE"), "{block}");
    assert!(
        block.contains(r#"<not_loaded path="nogrant/AGENTS.md" reason="no-read-grant"/>"#),
        "{block}"
    );
    for hidden in ["denied", "secrets", "DENIED-RULE", "SECRET-RULE"] {
        assert!(!block.contains(hidden), "{hidden} leaked:\n{block}");
    }
}

#[test]
fn no_error_display_text_ever_reaches_the_block() {
    let (_t, pi, found) = refusal_found();
    let block = pi.render_found(&found, WIDE).unwrap().text;
    for word in ["entitled", "denied by", "launch chain", "mur agent perm"] {
        assert!(!block.contains(word), "{word}:\n{block}");
    }
}
