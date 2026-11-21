//! The `@playwright/mcp` capability a recorded run depends on.
//!
//! `browser_verify_*` — the tools an assert step is recorded from and
//! replayed with — only exist when the server starts with the `testing`
//! capability. Replay always asked for it; recording did not, so a recording
//! session could not make the assert steps replay knows how to run.
//!
//! The server keeps ONE capability list, and each source replaces the one
//! below it wholesale: the last `--caps` flag, else `PLAYWRIGHT_MCP_CAPS`,
//! else `capabilities` in the config file. Checked against 0.0.82: started
//! with `--caps=pdf --caps=testing` it serves no `browser_pdf_save`. So
//! `testing` cannot be pushed as one more flag — that would silently drop
//! what the caller asked for. It is merged into the caller's own list.

use std::path::Path;

/// The capability that turns on the `browser_verify_*` tools.
pub const TESTING: &str = "testing";

const CAPS_FLAG: &str = "--caps";
const CAPS_ENV: &str = "PLAYWRIGHT_MCP_CAPS";
const CONFIG_FLAG: &str = "--config";
const CONFIG_ENV: &str = "PLAYWRIGHT_MCP_CONFIG";

/// `--caps=testing`, for a launch whose arguments MUR builds itself (replay).
pub fn testing_arg() -> String {
    caps_arg(&[TESTING.to_owned()])
}

fn caps_arg(caps: &[String]) -> String {
    format!("{CAPS_FLAG}={}", caps.join(","))
}

/// The value of the last `flag` in `args`, as `--flag=value` or `--flag value`.
/// The last one is the one the server uses.
fn last_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let prefix = format!("{flag}=");
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(&prefix) {
            found = Some(value);
        } else if arg == flag {
            found = iter.next().map(String::as_str).or(found);
        }
    }
    found.filter(|value| !value.is_empty())
}

/// Split a comma-separated capability list the way the server does.
fn split(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|cap| !cap.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `capabilities` from a JSON config file. `None` when the file cannot be
/// read as JSON (the server also accepts INI) or the field is not a list of
/// strings: the caller's list is then unknown, not empty.
fn config_caps(path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let config: serde_json::Value =
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()?;
    match config.get("capabilities") {
        None => Some(Vec::new()),
        Some(list) => list
            .as_array()?
            .iter()
            .map(|cap| cap.as_str().map(str::to_owned))
            .collect(),
    }
}

/// The capability list the server would use for `args` and `env`, or `None`
/// when it sits in a config file this cannot read.
fn caller_caps(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Option<Vec<String>> {
    let set = |key: &str| env(key).filter(|value| !value.is_empty());
    if let Some(list) = last_value(args, CAPS_FLAG) {
        return Some(split(list));
    }
    if let Some(list) = set(CAPS_ENV) {
        return Some(split(&list));
    }
    match last_value(args, CONFIG_FLAG)
        .map(str::to_owned)
        .or_else(|| set(CONFIG_ENV))
    {
        Some(path) => config_caps(Path::new(&path)),
        None => Some(Vec::new()),
    }
}

/// The argument to add to a recording launch so it has [`TESTING`] on top of
/// whatever capabilities the caller chose. Nothing when `testing` is already
/// there, or when the caller's list cannot be read: dropping capabilities the
/// caller configured is worse than a session without assert tools, which is
/// what that launch had before.
pub fn record_args(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let Some(mut caps) = caller_caps(args, env) else {
        tracing::debug!(
            "capabilities come from a config file that is not JSON; not adding {TESTING}"
        );
        return Vec::new();
    };
    if caps.iter().any(|cap| cap == TESTING) {
        return Vec::new();
    }
    caps.push(TESTING.to_owned());
    vec![caps_arg(&caps)]
}

/// [`record_args`] for this process's environment.
pub fn system_record_args(args: &[String]) -> Vec<String> {
    record_args(args, &|key| std::env::var(key).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_plain_launch_gets_testing() {
        assert_eq!(record_args(&[], &no_env), ["--caps=testing"]);
        assert_eq!(testing_arg(), "--caps=testing");
    }

    #[test]
    fn the_callers_caps_are_kept_not_replaced() {
        // The server uses only the last `--caps`, so the merged list has to
        // carry the caller's capabilities itself.
        for extra in [&["--caps=pdf"][..], &["--caps", "pdf"][..]] {
            assert_eq!(record_args(&args(extra), &no_env), ["--caps=pdf,testing"]);
        }
        assert_eq!(
            record_args(&args(&["--caps=vision", "--caps=pdf, devtools"]), &no_env),
            ["--caps=pdf,devtools,testing"]
        );
    }

    #[test]
    fn testing_already_chosen_adds_nothing() {
        assert!(record_args(&args(&["--caps=pdf,testing"]), &no_env).is_empty());
        let env = |key: &str| (key == CAPS_ENV).then(|| "testing".to_owned());
        assert!(record_args(&[], &env).is_empty());
    }

    #[test]
    fn env_caps_are_merged_and_a_flag_outranks_them() {
        let env = |key: &str| (key == CAPS_ENV).then(|| "pdf".to_owned());
        assert_eq!(record_args(&[], &env), ["--caps=pdf,testing"]);
        assert_eq!(
            record_args(&args(&["--caps=vision"]), &env),
            ["--caps=vision,testing"]
        );
        // An empty variable is unset to the server too.
        let empty = |key: &str| (key == CAPS_ENV).then(String::new);
        assert_eq!(record_args(&[], &empty), ["--caps=testing"]);
    }

    #[test]
    fn config_file_caps_are_merged() {
        let dir = tempfile::tempdir().unwrap();
        let with = dir.path().join("with.json");
        std::fs::write(&with, r#"{"capabilities": ["pdf"]}"#).unwrap();
        let without = dir.path().join("without.json");
        std::fs::write(&without, r#"{"browser": {"isolated": true}}"#).unwrap();

        let flag = |path: &Path| args(&["--config", &path.display().to_string()]);
        assert_eq!(record_args(&flag(&with), &no_env), ["--caps=pdf,testing"]);
        assert_eq!(record_args(&flag(&without), &no_env), ["--caps=testing"]);

        let path = with.display().to_string();
        let env = move |key: &str| (key == CONFIG_ENV).then(|| path.clone());
        assert_eq!(record_args(&[], &env), ["--caps=pdf,testing"]);
    }

    #[test]
    fn an_unreadable_config_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("config.ini");
        std::fs::write(&ini, "capabilities = pdf\n").unwrap();
        let flag = args(&[&format!("--config={}", ini.display())]);
        assert!(record_args(&flag, &no_env).is_empty());
        // An explicit `--caps` still decides, whatever the config holds.
        let mut both = flag;
        both.push("--caps=pdf".to_owned());
        assert_eq!(record_args(&both, &no_env), ["--caps=pdf,testing"]);
    }
}
