//! Browser-backed lock: an exported `assert_text` passes and fails exactly
//! where replay's `browser_verify_text_visible` does.
//!
//! Opt-in: set `MUR_BROWSER_E2E=1`. Everything else skips, because this needs
//! `node` plus the pinned `@playwright/mcp` install and its Chromium (both from
//! `mur browser setup`). The spec runs under that install's own `playwright`
//! test runner, so no extra npm package is fetched.
//!
//! Inside a sandboxed host, pass Chromium `--no-sandbox` through
//! `MUR_BROWSER_CHROMIUM_ARGS`; it is forwarded to the launch untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

use mur_browser::recorder::{Action, Mode, Run, Step};

const GATE: &str = "MUR_BROWSER_E2E";

fn gated() -> bool {
    if std::env::var_os(GATE).is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    eprintln!("skipped: set {GATE}=1 to run the export assert_text e2e");
    false
}

/// The pinned install's `node_modules/playwright` (ships the test runner).
fn playwright_pkg() -> PathBuf {
    let home = mur_browser::paths::system_mur_home().expect("MUR home resolves");
    let pkg = mur_browser::server::install_dir(&home)
        .join("node_modules")
        .join("playwright");
    assert!(
        pkg.join("cli.js").is_file(),
        "{} missing — run `mur browser setup`",
        pkg.display()
    );
    pkg
}

fn step(n: u32, action: Action, value: &str) -> Step {
    Step {
        step: n,
        intent: "e2e".to_string(),
        intent_auto: false,
        action,
        value: Some(value.to_string()),
        locators: Vec::new(),
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

fn spec_for(html: &str, text: &str) -> String {
    let run = Run {
        name: "assert-text-e2e".to_string(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![
            step(1, Action::Goto, &format!("data:text/html,{html}")),
            step(2, Action::AssertText, text),
        ],
    };
    mur_browser::export::to_spec_ts(&run).expect("test-mode run exports")
}

/// Lay out a throwaway project whose `@playwright/test` resolves to the
/// pinned runner, so the exported `import ... from '@playwright/test'` works.
fn project(dir: &Path, pw: &Path) {
    let shim = dir.join("node_modules").join("@playwright").join("test");
    std::fs::create_dir_all(&shim).unwrap();
    std::fs::write(
        shim.join("package.json"),
        r#"{"name":"@playwright/test","main":"index.js"}"#,
    )
    .unwrap();
    let test_js = serde_json::to_string(&pw.join("test.js")).unwrap();
    std::fs::write(
        shim.join("index.js"),
        format!("module.exports = require({test_js});\n"),
    )
    .unwrap();

    let args: Vec<String> = std::env::var(mur_browser::chromium::EXTRA_ARGS_ENV)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let config = format!(
        "module.exports = {{ testDir: __dirname, outputDir: __dirname + '/out', \
         timeout: 30000, expect: {{ timeout: 3000 }}, \
         use: {{ launchOptions: {{ args: {} }} }} }};\n",
        serde_json::to_string(&args).unwrap()
    );
    std::fs::write(dir.join("playwright.config.js"), config).unwrap();
}

/// Run the exported spec for one page; `true` when Playwright passes it.
fn spec_passes(html: &str, text: &str) -> bool {
    let pw = playwright_pkg();
    let dir = tempfile::tempdir().unwrap();
    project(dir.path(), &pw);
    std::fs::write(dir.path().join("case.spec.ts"), spec_for(html, text)).unwrap();
    let out = Command::new("node")
        .arg(pw.join("cli.js"))
        .args(["test", "-c", "playwright.config.js", "--reporter=line"])
        .current_dir(dir.path())
        .output()
        .expect("node runs");
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(
        log.contains("1 passed") || log.contains("1 failed"),
        "playwright did not run the case:\n{log}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.status.success()
}

#[test]
fn hidden_first_match_does_not_fail_when_a_later_one_is_visible() {
    if !gated() {
        return;
    }
    assert!(spec_passes(
        "<p hidden>Welcome</p><p>Welcome</p>",
        "Welcome"
    ));
}

#[test]
fn text_only_inside_a_child_frame_passes() {
    if !gated() {
        return;
    }
    assert!(spec_passes(
        "<iframe srcdoc=\"<p>Welcome</p>\"></iframe>",
        "Welcome"
    ));
}

/// The check is not vacuous: hidden-only or absent text still fails, as it
/// does in replay.
#[test]
fn hidden_only_or_absent_text_fails() {
    if !gated() {
        return;
    }
    assert!(!spec_passes("<p hidden>Welcome</p>", "Welcome"));
    assert!(!spec_passes("<p>Goodbye</p>", "Welcome"));
}
