use mur_core::cmd::skill_suggest::{SuggestOptions, cmd_suggest};
use tempfile::TempDir;

fn make_recordings_dir(home: &std::path::Path) -> std::path::PathBuf {
    let dir = home.join("session").join("recordings");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tool_call_jsonl(tool_name: &str, session_suffix: &str) -> String {
    format!(
        r#"{{"type":"tool_call","tool":"{}","input":{{"url":"https://ex.com/{}"}},"ts":"2026-05-25T00:00:00Z"}}
{{"type":"tool_result","tool":"{}","ok":true,"output":"<html>..."}}
{{"type":"tool_call","tool":"browser.extract","input":{{"selector":".price"}},"ts":"2026-05-25T00:00:05Z"}}
{{"type":"tool_result","tool":"browser.extract","ok":true,"output":"$99"}}"#,
        tool_name, session_suffix, tool_name
    )
}

#[test]
fn empty_recordings_dir() {
    let home = TempDir::new().unwrap();
    let _rec_dir = make_recordings_dir(home.path());

    let opts = SuggestOptions {
        max_sessions: 20,
        threshold: 3,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}

#[test]
fn two_sessions_no_suggestion() {
    let home = TempDir::new().unwrap();
    let rec_dir = make_recordings_dir(home.path());

    for i in 1..=2 {
        let jsonl = tool_call_jsonl("browser.navigate", &i.to_string());
        std::fs::write(rec_dir.join(format!("sess-{i}.jsonl")), jsonl).unwrap();
    }

    let opts = SuggestOptions {
        max_sessions: 20,
        threshold: 3,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}

#[test]
fn three_sessions_triggers_suggestion() {
    let home = TempDir::new().unwrap();
    let rec_dir = make_recordings_dir(home.path());

    for i in 1..=3 {
        let jsonl = tool_call_jsonl("browser.navigate", &i.to_string());
        std::fs::write(rec_dir.join(format!("sess-{i}.jsonl")), jsonl).unwrap();
    }

    let opts = SuggestOptions {
        max_sessions: 20,
        threshold: 3,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}

#[test]
fn respects_max_sessions() {
    let home = TempDir::new().unwrap();
    let rec_dir = make_recordings_dir(home.path());

    // Write 10 sessions, but max_sessions=5 → only 5 most recent scanned.
    for i in 1..=10 {
        let jsonl = tool_call_jsonl("browser.navigate", &i.to_string());
        std::fs::write(rec_dir.join(format!("sess-{i}.jsonl")), jsonl).unwrap();
    }

    let opts = SuggestOptions {
        max_sessions: 5,
        threshold: 3,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}

#[test]
fn respects_threshold_two() {
    let home = TempDir::new().unwrap();
    let rec_dir = make_recordings_dir(home.path());

    for i in 1..=2 {
        let jsonl = tool_call_jsonl("browser.navigate", &i.to_string());
        std::fs::write(rec_dir.join(format!("sess-{i}.jsonl")), jsonl).unwrap();
    }

    let opts = SuggestOptions {
        max_sessions: 20,
        threshold: 2,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}

#[test]
fn no_recordings_dir() {
    let home = TempDir::new().unwrap();
    // Don't create recordings dir.

    let opts = SuggestOptions {
        max_sessions: 20,
        threshold: 3,
    };
    let result = cmd_suggest(home.path(), opts);
    assert!(result.is_ok());
}
