use super::*;

fn tool(td: &tempfile::TempDir) -> EditFileTool {
    let root = td.path().to_str().unwrap();
    EditFileTool::new_for_test(
        SessionCwd::new(td.path().into()),
        FilesystemEntitlement {
            read: vec![],
            write: vec![root.to_string()],
            deny: vec![],
        },
    )
}

fn seed(td: &tempfile::TempDir, content: &str) {
    std::fs::write(td.path().join("f.txt"), content).unwrap();
}

#[tokio::test]
async fn single_match_replaces() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "aaa X bbb");
    tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(td.path().join("f.txt")).unwrap(),
        "aaa Y bbb"
    );
}

#[tokio::test]
async fn zero_matches_is_invalid_input() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "nothing here");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn missing_file_error_names_session_cwd() {
    let td = tempfile::tempdir().unwrap();
    let err = tool(&td)
        .execute(serde_json::json!({"path": "nope.txt", "old_string": "X", "new_string": "Y"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cannot edit"));
    assert!(err.to_string().contains("session cwd"));
}

#[tokio::test]
async fn multi_match_without_count_is_ambiguous() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X and X");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
}

#[tokio::test]
async fn expected_count_replaces_all_n() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X and X");
    let r = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y", "expected_count": 2}))
        .await
        .unwrap();
    assert!(r.text.contains("replaced 2"));
    assert_eq!(
        std::fs::read_to_string(td.path().join("f.txt")).unwrap(),
        "Y and Y"
    );
}

#[tokio::test]
async fn expected_count_mismatch_rejected() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X and X");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y", "expected_count": 3}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expected 3"));
}

fn read(td: &tempfile::TempDir) -> String {
    std::fs::read_to_string(td.path().join("f.txt")).unwrap()
}

#[tokio::test]
async fn batch_edits_apply_in_order() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "alpha beta gamma");
    let r = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "alpha", "new_string": "ONE"},
            {"old_string": "gamma", "new_string": "THREE"}
        ]}))
        .await
        .unwrap();
    assert!(r.text.contains("applied 2 edits"), "{}", r.text);
    assert_eq!(read(&td), "ONE beta THREE");
}

#[tokio::test]
async fn later_edit_sees_earlier_result() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "a");
    tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "a", "new_string": "bb"},
            {"old_string": "bb", "new_string": "c"}
        ]}))
        .await
        .unwrap();
    assert_eq!(read(&td), "c");
}

#[tokio::test]
async fn one_failing_edit_writes_nothing() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "alpha beta");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "alpha", "new_string": "ONE"},
            {"old_string": "missing", "new_string": "X"}
        ]}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("edit #2 of 2"), "{msg}");
    assert!(msg.contains("no edits were written"), "{msg}");
    assert_eq!(read(&td), "alpha beta", "file must be untouched");
}

#[tokio::test]
async fn batch_honours_per_edit_expected_count() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X X Y");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "Y", "new_string": "Z"},
            {"old_string": "X", "new_string": "W"}
        ]}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
    assert_eq!(read(&td), "X X Y");

    tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "Y", "new_string": "Z"},
            {"old_string": "X", "new_string": "W", "expected_count": 2}
        ]}))
        .await
        .unwrap();
    assert_eq!(read(&td), "W W Z");
}

#[tokio::test]
async fn mixing_edits_and_top_level_pair_is_refused() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X");
    let err = tool(&td)
        .execute(
            serde_json::json!({"path": "f.txt", "old_string": "X", "new_string": "Y",
            "edits": [{"old_string": "X", "new_string": "Z"}]}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not both"));
    assert_eq!(read(&td), "X");
}

#[tokio::test]
async fn empty_or_malformed_edits_rejected() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "X");
    for bad in [
        serde_json::json!([]),
        serde_json::json!("nope"),
        serde_json::json!([{"old_string": "X"}]),
        serde_json::json!([{"old_string": "", "new_string": "Y"}]),
    ] {
        let err = tool(&td)
            .execute(serde_json::json!({"path": "f.txt", "edits": bad}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)), "{bad}: {err}");
    }
    assert_eq!(read(&td), "X");
}

#[tokio::test]
async fn not_found_points_at_whitespace_drift() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "fn a() {}\nfn main() {\n    let x = 1;\n}\n");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt",
            "old_string": "fn main() {\n  let x = 1;\n}", "new_string": "Z"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("old_string not found"), "{err}");
    assert!(err.contains("lines 2-4"), "{err}");
    assert!(err.contains("whitespace"), "{err}");
    assert!(err.contains("    let x = 1;"), "{err}");
    assert_eq!(read(&td), "fn a() {}\nfn main() {\n    let x = 1;\n}\n");
}

#[tokio::test]
async fn not_found_names_the_first_differing_line() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "one\nlet total = count + 1;\nthree\n");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt",
            "old_string": "let total = count + 2;", "new_string": "Z"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("file has \"let total = count + 1;\""), "{err}");
}

#[tokio::test]
async fn unrelated_text_gets_no_snippet() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "alpha beta gamma\n");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt",
            "old_string": "zzzzqqqqxxxx", "new_string": "Z"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no similar text"), "{err}");
    assert!(!err.contains("alpha"), "{err}");
}

#[tokio::test]
async fn batch_hint_matches_against_prior_edits() {
    let td = tempfile::tempdir().unwrap();
    seed(&td, "hello world\n");
    let err = tool(&td)
        .execute(serde_json::json!({"path": "f.txt", "edits": [
            {"old_string": "world", "new_string": "there"},
            {"old_string": "hello worle", "new_string": "Z"}
        ]}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("edit #2 of 2"), "{err}");
    assert!(err.contains("hello there"), "{err}");
    assert_eq!(read(&td), "hello world\n");
}
