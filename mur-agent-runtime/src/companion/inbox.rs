//! Inbox markdown writer — Spec §3.6, R16 (O_CREAT | O_EXCL).
//!
//! Each message is written as a single `.md` file under `<inbox_dir>/<id>.md`.
//! The write is exclusive: a duplicate id returns `ErrorKind::AlreadyExists`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::SecondsFormat;

use crate::companion::notifier::CompanionMessage;

/// Write `msg` to `<inbox_dir>/<id>.md`.
///
/// Uses `OpenOptions::create_new(true)` which maps to `O_CREAT | O_EXCL`.
/// Returns `Err` whose `kind()` is `ErrorKind::AlreadyExists` on a duplicate id.
pub fn write_inbox_md(inbox_dir: &Path, msg: &CompanionMessage) -> Result<PathBuf> {
    std::fs::create_dir_all(inbox_dir)?;

    let path = inbox_dir.join(format!("{}.md", msg.id));
    let content = render(msg);

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    f.write_all(content.as_bytes())?;

    Ok(path)
}

/// Render the front-matter + body per Spec §3.6.
fn render(msg: &CompanionMessage) -> String {
    // Situation as snake_case serde string (strips surrounding quotes from JSON).
    let situation_str = serde_json::to_string(&msg.situation)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned();

    // generated_at as RFC3339 with local offset, seconds resolution.
    let generated_at_local = msg.generated_at.with_timezone(&chrono::Local);
    let generated_at_str =
        generated_at_local.to_rfc3339_opts(SecondsFormat::Secs, false);

    let mut out = String::new();
    writeln!(out, "---").unwrap();
    writeln!(out, "id: {}", msg.id).unwrap();
    writeln!(out, "situation: {}", situation_str).unwrap();
    writeln!(out, "template_id: {}", msg.template_id).unwrap();
    writeln!(out, "locale: {}", msg.locale).unwrap();
    writeln!(out, "generated_at: {}", generated_at_str).unwrap();
    writeln!(out, "---").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "{}", msg.body).unwrap();
    writeln!(out).unwrap();
    write!(out, ">>> response: <unset>").unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use mur_common::companion::Situation;
    use tempfile::TempDir;

    fn sample_msg() -> CompanionMessage {
        CompanionMessage {
            id: "01HQ_TEST_ID_001".to_string(),
            situation: Situation::MorningGreeting,
            template_id: "greet_warm_zh_001".to_string(),
            locale: "zh-TW".to_string(),
            body: "早安 David。今天想從哪一件小事開始？".to_string(),
            generated_at: Utc
                .with_ymd_and_hms(2026, 4, 29, 7, 13, 3)
                .unwrap(),
        }
    }

    #[test]
    fn happy_path_writes_file_with_correct_content() {
        let dir = TempDir::new().unwrap();
        let msg = sample_msg();

        let path = write_inbox_md(dir.path(), &msg).expect("write_inbox_md failed");

        assert!(path.exists(), "file should exist");
        let content = std::fs::read_to_string(&path).unwrap();

        // Front-matter delimiters
        assert!(content.starts_with("---\n"), "must start with ---");
        assert!(content.contains("\n---\n"), "must have closing ---");

        // Front-matter fields
        assert!(content.contains("id: 01HQ_TEST_ID_001"));
        assert!(content.contains("situation: morning_greeting"));
        assert!(content.contains("template_id: greet_warm_zh_001"));
        assert!(content.contains("locale: zh-TW"));

        // Body
        assert!(
            content.contains("早安 David。今天想從哪一件小事開始？"),
            "body must appear in file"
        );

        // Response line
        assert!(
            content.ends_with(">>> response: <unset>"),
            "must end with response placeholder; got: {content:?}"
        );

        // Blank line before body (after closing ---)
        let after_yaml = content.split("---\n").nth(2).expect("split failed");
        assert!(
            after_yaml.starts_with('\n'),
            "blank line must follow closing ---"
        );
    }

    #[test]
    fn duplicate_id_returns_already_exists() {
        let dir = TempDir::new().unwrap();
        let msg = sample_msg();

        write_inbox_md(dir.path(), &msg).expect("first write should succeed");

        let err = write_inbox_md(dir.path(), &msg).expect_err("second write must fail");
        let io_err = err
            .downcast::<std::io::Error>()
            .expect("must be an io::Error");
        assert_eq!(
            io_err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "must be AlreadyExists"
        );
    }
}
