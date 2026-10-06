use super::*;

#[test]
fn test_extract_file_paths_backtick() {
    let paths =
        extract_file_paths("Check `src/main.rs` and `mur-common/src/config.rs` for details.");
    assert!(paths.contains(&"src/main.rs".to_string()));
    assert!(paths.contains(&"mur-common/src/config.rs".to_string()));
}

#[test]
fn test_extract_file_paths_home() {
    let paths = extract_file_paths("Data at `~/.mur/patterns/*.yaml`");
    assert!(paths.contains(&"~/.mur/patterns/*.yaml".to_string()));
}

#[test]
fn test_extract_file_paths_ignores_urls() {
    let paths = extract_file_paths("Visit `https://example.com/path/to/file.html`");
    assert!(paths.is_empty());
}

#[test]
fn test_extract_commands() {
    // Initialize known subcommand words so the parser can recognize them
    set_known_commands(
        ["mur learn", "mur learn extract", "mur stats", "mur verify"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    let cmds = extract_commands("Run `mur learn extract --llm` to start");
    assert!(cmds.contains(&"mur learn extract".to_string()));
}

#[test]
fn test_extract_commands_cargo_run() {
    let cmds = extract_commands("e.g. cargo run -- search \"swift testing\"");
    assert!(cmds.contains(&"mur search".to_string()));
}

#[test]
fn test_extract_code_refs_method() {
    let refs = extract_code_refs("`Pattern::deref()` forwards to `KnowledgeBase`");
    assert!(refs.contains(&"Pattern::deref".to_string()));
    assert!(refs.contains(&"KnowledgeBase".to_string()));
}

#[test]
fn test_extract_code_refs_fn() {
    let refs = extract_code_refs("calls `fn score_and_rank_generic()` for ranking");
    assert!(refs.contains(&"fn score_and_rank_generic".to_string()));
}

#[test]
fn test_extract_code_refs_struct() {
    let refs = extract_code_refs("`struct ResearchReport` holds the output");
    assert!(refs.contains(&"struct ResearchReport".to_string()));
}

#[test]
fn test_verify_command_known() {
    set_known_commands(
        ["mur stats", "mur verify", "mur learn", "mur learn extract"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    assert_eq!(verify_command("mur stats"), VerifyResult::Valid);
    assert_eq!(verify_command("mur verify"), VerifyResult::Valid);
}

#[test]
fn test_verify_command_unknown() {
    assert!(matches!(
        verify_command("mur foobar"),
        VerifyResult::Invalid(_)
    ));
}

#[test]
fn test_verify_command_parent() {
    // "mur learn" is a valid parent of "mur learn extract"
    set_known_commands(
        ["mur learn", "mur learn extract", "mur learn cross"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    assert_eq!(verify_command("mur learn"), VerifyResult::Valid);
}

#[test]
fn test_verify_command_non_mur() {
    assert!(matches!(
        verify_command("cargo build"),
        VerifyResult::Skipped(_)
    ));
}

#[test]
fn test_verify_home_path_missing_on_host_is_skipped() {
    // `~/` paths describe the reader's machine, not the repo: a Linux-only
    // dir like `~/.config/autostart` must not fail on a macOS host.
    let root = std::env::temp_dir();
    let result = verify_file_path("~/.mur-verify-test-nonexistent/autostart", &root);
    assert!(
        matches!(result, VerifyResult::Skipped(_)),
        "expected Skipped, got {result:?}"
    );
}

#[test]
fn test_looks_like_path() {
    assert!(looks_like_path("src/main.rs"));
    assert!(looks_like_path("mur-core/src/lib.rs"));
    assert!(looks_like_path("~/.mur/config.yaml"));
    assert!(looks_like_path("internal/api/handlers/"));
    assert!(!looks_like_path("https://example.com/foo"));
    assert!(!looks_like_path("--flag-name"));
}

#[test]
fn test_parse_claims_dedup() {
    let content = "See `src/main.rs` and also `src/main.rs` again.";
    let claims = parse_claims(content, "test.md");
    let path_claims: Vec<_> = claims
        .iter()
        .filter(|c| matches!(c.kind, ClaimKind::FilePath(_)))
        .collect();
    // Should only appear once
    assert_eq!(path_claims.len(), 1);
}

#[test]
fn test_summary() {
    let results = vec![
        VerifiedClaim {
            claim: Claim {
                kind: ClaimKind::FilePath("a.rs".into()),
                source_file: "t.md".into(),
                line_number: 1,
                raw: "a.rs".into(),
            },
            result: VerifyResult::Valid,
        },
        VerifiedClaim {
            claim: Claim {
                kind: ClaimKind::FilePath("b.rs".into()),
                source_file: "t.md".into(),
                line_number: 2,
                raw: "b.rs".into(),
            },
            result: VerifyResult::Invalid("not found".into()),
        },
    ];
    let summary = VerifySummary::from_results(&results);
    assert_eq!(summary.total, 2);
    assert_eq!(summary.valid, 1);
    assert_eq!(summary.invalid, 1);
}

#[test]
fn test_extract_file_paths_strips_line_suffix() {
    let paths =
        extract_file_paths("See `mur-core/src/verify.rs:446`, `a/b.rs:318-322` and `c/d.rs:12:5`.");
    assert_eq!(
        paths,
        vec![
            "mur-core/src/verify.rs".to_string(),
            "a/b.rs".to_string(),
            "c/d.rs".to_string(),
        ]
    );
}

#[test]
fn test_verify_path_with_line_suffix_is_valid() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let claims = parse_claims("See `mur-core/src/verify.rs:446-530`.", "x.md");
    let path_claims: Vec<_> = claims
        .iter()
        .filter(|c| matches!(c.kind, ClaimKind::FilePath(_)))
        .collect();
    assert_eq!(path_claims.len(), 1);
    assert!(matches!(
        verify_claim(path_claims[0], root),
        VerifyResult::Valid
    ));
}
