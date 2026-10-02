use super::*;
use std::io::Cursor;

/// L2 stand-in for the L1 tests: never opens a browser.
fn no_render(_: &[String], _: Duration) -> RenderRun {
    RenderRun::SpawnFailed("no browser in tests".into())
}

/// Download stand-in for tests that must never reach the network.
fn no_fetch(_: &CuratedRecipe, _: &Path) -> Result<Vec<PathBuf>> {
    panic!("no download expected in this test")
}

/// Empty PATH + empty home: nothing is installed, deterministically.
fn bare_home() -> (tempfile::TempDir, mur_common::test_env::EnvGuard) {
    let home = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("PATH", "");
    (home, envg)
}

#[test]
fn missing_browser_prints_commands_and_declining_runs_nothing() {
    let (home, _g) = bare_home();
    let mut calls: Vec<String> = Vec::new();
    let mut run = |a: &[&str]| {
        calls.push(a.join(" "));
        Ok(true)
    };
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(b"y\n".to_vec()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut no_render,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("npm i -g agent-browser@latest"),
        "commands shown first: {out}"
    );
    assert!(out.contains("agent-browser install"));
    assert!(
        calls.is_empty(),
        "'y' is not consent; nothing may run: {calls:?}"
    );
}

#[test]
fn literal_yes_runs_the_install_steps_in_order() {
    let (home, _g) = bare_home();
    let mut calls: Vec<String> = Vec::new();
    let mut run = |a: &[&str]| {
        calls.push(a.join(" "));
        Ok(true)
    };
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(b"yes\n".to_vec()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut no_render,
    )
    .unwrap();
    assert_eq!(
        calls,
        ["npm i -g agent-browser@latest", "agent-browser install"]
    );
    // The fake runner installs nothing, so the re-check must say so rather
    // than claim success.
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("still not on PATH")
    );
}

#[test]
fn a_failed_install_stops_early_and_is_not_an_error() {
    let (home, _g) = bare_home();
    let mut calls = 0;
    let mut run = |_: &[&str]| {
        calls += 1;
        Ok(false)
    };
    let mut out = Vec::new();
    let r = ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(b"yes\n".to_vec()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut no_render,
    );
    assert!(r.is_ok(), "setup must not fail over a browser install");
    assert_eq!(calls, 1, "second step must not run after the first failed");
}

#[test]
fn a_present_browser_is_smoke_tested_not_installed() {
    let (home, _g) = bare_home();
    let aura = home.path().join("aura");
    std::fs::create_dir_all(&aura).unwrap();
    std::fs::write(aura.join("lightpanda"), b"x").unwrap();
    let mut calls: Vec<Vec<String>> = Vec::new();
    let mut run = |a: &[&str]| {
        calls.push(a.iter().map(|s| s.to_string()).collect());
        Ok(true)
    };
    let mut out = Vec::new();
    // No input at all: a found browser must never prompt.
    ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(Vec::new()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut no_render,
    )
    .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0][1], "version",
        "lightpanda takes a subcommand, not a flag"
    );
    assert!(String::from_utf8(out).unwrap().contains("runs"));
}

#[test]
fn doctor_never_installs_and_errors_when_missing() {
    let (home, _g) = bare_home();
    let mut calls = 0;
    let mut run = |_: &[&str]| {
        calls += 1;
        Ok(true)
    };
    let mut out = Vec::new();
    assert!(
        doctor(
            home.path(),
            &InstallPlan::AgentBrowser,
            &mut out,
            &mut run,
            None
        )
        .is_err()
    );
    assert_eq!(calls, 0, "doctor is read-only");
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("npm i -g agent-browser@latest")
    );
}

fn lightpanda_plan() -> InstallPlan {
    InstallPlan::Lightpanda(CuratedRecipe {
        description: "lightpanda".into(),
        url: "https://example.invalid/lightpanda".into(),
        sha256: "ab".repeat(32),
        install_to: Some(LIGHTPANDA_RELATIVE.into()),
        executable: true,
        archive: None,
    })
}

/// D2: a platform with a curated recipe gets native Lightpanda;
/// one without gets the npm fallback (D2b).
#[test]
fn install_plan_prefers_the_curated_lightpanda_recipe() {
    match install_plan("aarch64-macos") {
        InstallPlan::Lightpanda(r) => {
            assert_eq!(r.install_to.as_deref(), Some(LIGHTPANDA_RELATIVE));
        }
        other => panic!("expected lightpanda, got {other:?}"),
    }
    assert_eq!(install_plan("sparc-solaris"), InstallPlan::AgentBrowser);
}

/// D3: URL, sha256 prefix and destination are shown before consent, and
/// anything but a literal `yes` downloads nothing.
#[test]
fn lightpanda_plan_is_printed_first_and_needs_a_literal_yes() {
    let (home, _g) = bare_home();
    let mut fetched = 0;
    let mut fetch = |_: &CuratedRecipe, _: &Path| {
        fetched += 1;
        Ok(vec![])
    };
    let mut run = |_: &[&str]| -> Result<bool> { panic!("npm must not run") };
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &lightpanda_plan(),
        &mut Cursor::new(b"y\n".to_vec()),
        &mut out,
        &mut run,
        &mut fetch,
        &mut no_render,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("https://example.invalid/lightpanda"), "{out}");
    assert!(out.contains(&"ab".repeat(8)), "sha256 prefix shown: {out}");
    assert!(out.contains("~/.mur/aura/lightpanda"), "{out}");
    assert!(
        !out.contains("npm"),
        "no npm on the lightpanda route: {out}"
    );
    assert_eq!(fetched, 0);
}

/// `yes` → the deps installer places lightpanda, and the result is then
/// smoke-tested like any found browser.
#[test]
fn literal_yes_installs_lightpanda_then_smoke_tests_it() {
    let (home, _g) = bare_home();
    let mut fetch = |r: &CuratedRecipe, h: &Path| {
        let dst = h.join(r.install_to.as_ref().unwrap());
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, b"x").unwrap();
        Ok(vec![dst])
    };
    let mut calls: Vec<String> = Vec::new();
    let mut run = |a: &[&str]| {
        calls.push(a.join(" "));
        Ok(true)
    };
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &lightpanda_plan(),
        &mut Cursor::new(b"yes\n".to_vec()),
        &mut out,
        &mut run,
        &mut fetch,
        &mut no_render,
    )
    .unwrap();
    assert!(home.path().join(LIGHTPANDA_RELATIVE).exists());
    assert_eq!(calls.len(), 1, "only the L1 version check: {calls:?}");
    assert!(calls[0].ends_with("version"), "{calls:?}");
}

/// D3: a failed download says so and does NOT fall through to npm.
#[test]
fn a_failed_lightpanda_download_never_falls_back_to_npm() {
    let (home, _g) = bare_home();
    let mut fetch =
        |_: &CuratedRecipe, _: &Path| -> Result<Vec<PathBuf>> { bail!("sha256 mismatch") };
    let mut ran = 0;
    let mut run = |_: &[&str]| {
        ran += 1;
        Ok(true)
    };
    let mut out = Vec::new();
    let r = ensure_render_browser(
        home.path(),
        &lightpanda_plan(),
        &mut Cursor::new(b"yes\n".to_vec()),
        &mut out,
        &mut run,
        &mut fetch,
        &mut no_render,
    );
    assert!(r.is_ok(), "setup must not fail over a browser install");
    assert_eq!(ran, 0, "no npm, no smoke test after a failed download");
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("sha256 mismatch"), "{out}");
    assert!(out.contains("re-run `mur deep-research setup`"), "{out}");
}

#[test]
fn doctor_points_at_setup_when_lightpanda_is_available() {
    let (home, _g) = bare_home();
    let mut run = |_: &[&str]| -> Result<bool> { panic!("doctor is read-only") };
    let mut out = Vec::new();
    assert!(doctor(home.path(), &lightpanda_plan(), &mut out, &mut run, None).is_err());
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("install with: mur deep-research setup"),
        "{out}"
    );
    assert!(!out.contains("npm"), "{out}");
}

fn empty_fleet() -> mur_common::fleet::Fleet {
    serde_yaml::from_str("name: deep-research\nchannel_id: fleet-deep-research\n").unwrap()
}

#[test]
fn declare_lightpanda_adds_a_curated_file_dep_once() {
    let mut f = empty_fleet();
    assert!(declare_lightpanda(&mut f, "aarch64-macos"));
    assert!(!declare_lightpanda(&mut f, "aarch64-macos"), "idempotent");
    assert_eq!(f.requires_programs.len(), 1);
    let d = &f.requires_programs[0];
    assert_eq!(d.registry.as_deref(), Some("lightpanda"));
    assert!(mur_common::deps::registry::is_curated("lightpanda"));
    assert_eq!(
        d.detect,
        mur_common::deps::DetectMethod::File {
            file: "aura/lightpanda".into()
        }
    );
}

#[test]
fn render_browser_name_follows_the_install_plan() {
    assert_eq!(render_browser_name("aarch64-macos"), "lightpanda");
    assert_eq!(render_browser_name("x86_64-linux"), "lightpanda");
    assert_eq!(render_browser_name("x86_64-windows"), "agent-browser");
}

#[test]
fn declare_lightpanda_skips_platforms_without_a_recipe() {
    let mut f = empty_fleet();
    assert!(!declare_lightpanda(&mut f, "x86_64-windows"));
    assert!(f.requires_programs.is_empty());
}

/// Mirrors the gateway's `auto_detect_render_engine`
/// (`mur-research-gateway/src/config.rs:373`): a native lightpanda at
/// `aura/lightpanda` wins even when obscura is fully installed too.
#[test]
fn auto_detect_engine_prefers_native_lightpanda() {
    let home = tempfile::tempdir().unwrap();
    let aura = home.path().join("aura");
    std::fs::create_dir_all(&aura).unwrap();
    for bin in ["lightpanda", "obscura", "obscura-worker"] {
        std::fs::write(aura.join(bin), b"").unwrap();
    }
    assert_eq!(auto_detect_engine(home.path()), RenderEngine::Lightpanda);
}

/// Without lightpanda the gateway takes obscura only when BOTH its
/// binaries exist; a lone `obscura` is not enough and falls through.
#[test]
fn auto_detect_engine_falls_back_to_obscura_only_with_both_binaries() {
    let home = tempfile::tempdir().unwrap();
    let aura = home.path().join("aura");
    std::fs::create_dir_all(&aura).unwrap();
    std::fs::write(aura.join("obscura"), b"").unwrap();
    assert_eq!(auto_detect_engine(home.path()), RenderEngine::AgentBrowser);

    std::fs::write(aura.join("obscura-worker"), b"").unwrap();
    assert_eq!(auto_detect_engine(home.path()), RenderEngine::Obscura);
}

/// Nothing native under `aura/` → the agent-browser wrapper, same as the
/// gateway's last branch.
#[test]
fn auto_detect_engine_falls_back_to_agent_browser_on_an_empty_home() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(auto_detect_engine(home.path()), RenderEngine::AgentBrowser);
}

/// Mirrors the gateway's `build_lightpanda_argv`
/// (`mur-research-gateway/src/browser.rs:122`) minus the proxy: the smoke
/// test runs in the user's terminal against a loopback page, so no
/// `--http-proxy`, and no `--block-private-networks` (the gateway omits it
/// too, and it would block the loopback page).
#[test]
fn lightpanda_render_argv_fetches_markdown_without_proxy_or_private_block() {
    let argv = lightpanda_render_argv("/h/.mur/aura/lightpanda", "http://127.0.0.1:9/");
    assert_eq!(argv[0], "/h/.mur/aura/lightpanda");
    assert_eq!(argv[1], "fetch");
    assert_eq!(argv[2], "http://127.0.0.1:9/");
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--dump" && w[1] == "markdown")
    );
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--http-timeout" && w[1] == "30000")
    );
    assert!(!argv.iter().any(|a| a == "--http-proxy"));
    assert!(!argv.iter().any(|a| a == "--block-private-networks"));
}

#[test]
fn chrome_render_argv_opens_and_snapshots_without_stealth_or_executable_path() {
    let argv = chrome_render_argv("/usr/local/bin/agent-browser", "http://127.0.0.1:9/");
    assert_eq!(argv[0], "/usr/local/bin/agent-browser");
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--engine" && w[1] == "chrome")
    );
    assert!(!argv.iter().any(|a| a == "--executable-path"));
    // No stealth args: a loopback page does not need them (spec §4 L2).
    assert!(!argv.iter().any(|a| a == "--args"));
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--session" && w[1] == format!("mur-smoke-{}", std::process::id()))
    );
    let open = argv.iter().position(|a| a == "open").expect("open");
    assert_eq!(argv[open + 1], "http://127.0.0.1:9/");
    assert_eq!(argv.last().map(String::as_str), Some("snapshot"));
}

#[test]
fn the_render_page_source_never_contains_the_marker() {
    // The whole L2 check rests on this: the marker only exists after JS runs.
    assert!(!RENDER_PAGE.contains(RENDER_MARKER));
    assert!(RENDER_PAGE.contains("6*7"));
}

#[test]
fn render_passed_accepts_output_with_the_js_computed_marker() {
    // Lightpanda `--dump markdown` shape: it escapes `-` (observed on
    // 1.0.0-nightly.7813).
    assert!(render_passed("MUR\\-RENDER\\-42\n"));
    // Unescaped (`--dump html`, other versions).
    assert!(render_passed("MUR-RENDER-42\n"));
    // agent-browser `snapshot` shape.
    assert!(render_passed("- document:\n  - generic: MUR-RENDER-42\n"));
}

#[test]
fn render_passed_rejects_raw_html_that_was_fetched_but_not_executed() {
    assert!(!render_passed(RENDER_PAGE));
    assert!(!render_passed("MUR-RENDER-'+(6*7)"));
    assert!(!render_passed(""));
}

/// Plays the browser: one GET, whole response back as a string.
fn http_get(url: &str) -> String {
    use std::io::Read;
    let addr = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix('/'))
        .expect("url shape is http://<addr>/");
    let mut stream = std::net::TcpStream::connect(addr).expect("server is listening");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn the_render_page_server_binds_loopback_only() {
    let server = serve_render_page().unwrap();
    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "{}",
        server.url
    );
    assert!(server.url.ends_with('/'), "{}", server.url);
    http_get(&server.url);
    assert!(server.finish());
}

#[test]
fn the_render_page_server_answers_one_request_with_the_page_then_closes() {
    let server = serve_render_page().unwrap();
    let response = http_get(&server.url);
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-type: text/html"),
        "{response}"
    );
    assert!(
        response.ends_with(&format!("\r\n\r\n{RENDER_PAGE}")),
        "{response}"
    );
    // One page, then closed: the listener is gone once the thread ends.
    let addr = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    assert!(server.finish(), "the thread reports it served");
    assert!(std::net::TcpStream::connect(addr).is_err());
}

#[test]
fn a_silent_preconnect_does_not_wedge_the_render_page_server() {
    // Chrome may open a speculative connection and never send on it. The
    // server must skip it and still answer the real request behind it.
    let server = spawn_page_server(
        std::time::Duration::from_secs(10),
        std::time::Duration::from_millis(200),
    )
    .unwrap();
    let addr = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let _silent = std::net::TcpStream::connect(addr).unwrap();
    let response = http_get(&server.url);
    assert!(response.ends_with(RENDER_PAGE), "{response}");
    assert!(server.finish());
}

#[test]
fn the_render_page_server_gives_up_when_no_browser_connects() {
    let server = spawn_page_server(
        std::time::Duration::from_millis(150),
        std::time::Duration::from_millis(50),
    )
    .unwrap();
    assert!(!server.handle.join().unwrap(), "nothing was served");
}

#[test]
fn finish_stops_a_server_whose_browser_never_came() {
    // The browser already exited without dialing; do not wait out the budget.
    let server = spawn_page_server(Duration::from_secs(60), Duration::from_millis(50)).unwrap();
    let started = Instant::now();
    assert!(!server.finish());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

// ── L2 runner ──────────────────────────────────────────────────────────

/// Every call the stand-in browser received: (argv, timeout).
type Calls = Vec<(Vec<String>, Duration)>;

fn loopback_url_in(argv: &[String]) -> String {
    argv.iter()
        .find(|a| a.starts_with("http://127.0.0.1:"))
        .cloned()
        .expect("the render argv carries the loopback url")
}

fn exited(success: bool, stdout: &str, stderr: &str) -> RenderRun {
    RenderRun::Exited {
        success,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

/// A stand-in browser: `fetch` decides whether it actually loads the page
/// (as a real one would) before reporting `result`. A `close` is recorded
/// and answered with success.
fn run_l2(engine: RenderEngine, fetch: bool, result: RenderRun) -> (L2Outcome, Calls) {
    let mut calls: Calls = Vec::new();
    let mut result = Some(result);
    let mut exec = |argv: &[String], timeout: Duration| {
        calls.push((argv.to_vec(), timeout));
        if argv.last().map(String::as_str) == Some("close") {
            return exited(true, "", "");
        }
        if fetch {
            http_get(&loopback_url_in(argv));
        }
        result.take().expect("one render per smoke test")
    };
    let outcome = render_smoke(engine, "/bin/fake-browser", &mut exec).unwrap();
    (outcome, calls)
}

#[test]
fn l2_passes_when_the_browser_ran_the_page_script() {
    let (outcome, calls) = run_l2(
        RenderEngine::Lightpanda,
        true,
        exited(true, "# MUR-RENDER-42\n", ""),
    );
    assert!(matches!(outcome, L2Outcome::Passed { .. }), "{outcome:?}");
    assert_eq!(
        calls.len(),
        1,
        "lightpanda has no session to close: {calls:?}"
    );
    let (argv, timeout) = &calls[0];
    assert_eq!(
        argv,
        &lightpanda_render_argv("/bin/fake-browser", &loopback_url_in(argv))
    );
    assert_eq!(*timeout, RENDER_TIMEOUT);
}

#[test]
fn l2_fails_as_no_js_when_the_page_came_back_as_raw_html() {
    let (outcome, _) = run_l2(
        RenderEngine::Lightpanda,
        true,
        exited(true, RENDER_PAGE, ""),
    );
    assert_eq!(
        outcome,
        L2Outcome::Failed {
            why: L2Failure::NoJs,
            stderr_tail: vec![]
        }
    );
}

#[test]
fn l2_a_nonzero_exit_fails_even_when_the_marker_printed() {
    // The gateway treats a non-zero exit as a failed render, whatever stdout says.
    let (outcome, _) = run_l2(
        RenderEngine::Lightpanda,
        true,
        exited(false, RENDER_MARKER, "boom\n"),
    );
    assert_eq!(
        outcome,
        L2Outcome::Failed {
            why: L2Failure::ExitedNonZero,
            stderr_tail: vec!["boom".into()]
        }
    );
}

#[test]
fn l2_never_reached_keeps_the_last_three_stderr_lines_without_waiting_out_the_server() {
    let started = Instant::now();
    let (outcome, _) = run_l2(
        RenderEngine::Lightpanda,
        false,
        exited(false, "", "one\ntwo\n\nthree\nfour\n"),
    );
    assert_eq!(
        outcome,
        L2Outcome::Failed {
            why: L2Failure::NeverReached,
            stderr_tail: vec!["two".into(), "three".into(), "four".into()]
        }
    );
    // The page server's own budget is 35s; the runner must stop it, not wait.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn l2_chrome_closes_its_smoke_session_afterwards() {
    let (outcome, calls) = run_l2(
        RenderEngine::AgentBrowser,
        true,
        exited(true, "- text \"MUR-RENDER-42\"\n", ""),
    );
    assert!(matches!(outcome, L2Outcome::Passed { .. }), "{outcome:?}");
    assert_eq!(calls.len(), 2, "{calls:?}");
    let render = &calls[0].0;
    assert_eq!(
        render,
        &chrome_render_argv("/bin/fake-browser", &loopback_url_in(render))
    );
    let session = format!("mur-smoke-{}", std::process::id());
    assert_eq!(
        calls[1].0,
        ["/bin/fake-browser", "--session", session.as_str(), "close"]
    );
    assert!(
        calls[1].1 < RENDER_TIMEOUT,
        "close gets its own short budget"
    );
}

#[test]
fn l2_chrome_session_is_closed_even_after_a_timeout() {
    let (outcome, calls) = run_l2(RenderEngine::AgentBrowser, false, RenderRun::TimedOut);
    assert_eq!(
        outcome,
        L2Outcome::Failed {
            why: L2Failure::TimedOut,
            stderr_tail: vec![]
        }
    );
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1].0.last().unwrap(), "close");
}

#[test]
fn l2_permission_denied_is_the_sandbox_not_a_broken_browser() {
    let (outcome, calls) = run_l2(RenderEngine::AgentBrowser, false, RenderRun::Denied);
    assert_eq!(outcome, L2Outcome::Sandboxed);
    assert_eq!(
        calls.len(),
        1,
        "nothing started, so no session to close: {calls:?}"
    );
}

#[test]
fn l2_is_not_run_for_obscura() {
    let mut exec =
        |_: &[String], _: Duration| -> RenderRun { panic!("obscura is outside the smoke test") };
    assert_eq!(
        render_smoke(RenderEngine::Obscura, "/bin/fake", &mut exec).unwrap(),
        L2Outcome::NotTested
    );
}

fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[cfg(unix)]
#[test]
fn system_render_exec_captures_exit_stdout_and_stderr() {
    let run = system_render_exec(
        &owned(&["/bin/sh", "-c", "echo out; echo err >&2; exit 3"]),
        Duration::from_secs(10),
    );
    assert_eq!(run, exited(false, "out\n", "err\n"));
}

#[cfg(unix)]
#[test]
fn system_render_exec_kills_a_render_that_overruns_its_budget() {
    let started = Instant::now();
    let run = system_render_exec(&owned(&["/bin/sleep", "10"]), Duration::from_millis(200));
    assert_eq!(run, RenderRun::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn system_render_exec_maps_permission_denied_to_the_sandbox_case() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("not-executable");
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
    let run = system_render_exec(&owned(&[bin.to_str().unwrap()]), Duration::from_secs(5));
    assert_eq!(run, RenderRun::Denied);
}

/// `%SystemRoot%\System32\<exe>`: absolute, so the test cannot pick up
/// a same-named binary from the runner's PATH.
#[cfg(windows)]
fn system32(exe: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\{exe}")
}

#[cfg(windows)]
#[test]
fn system_render_exec_captures_exit_stdout_and_stderr_on_windows() {
    let run = system_render_exec(
        &owned(&[
            &system32("cmd.exe"),
            "/C",
            "echo out& 1>&2 echo err& exit /b 3",
        ]),
        Duration::from_secs(10),
    );
    let (success, stdout, stderr) = match run {
        RenderRun::Exited {
            success,
            stdout,
            stderr,
        } => (success, stdout, stderr),
        other => panic!("expected the child to exit: {other:?}"),
    };
    assert!(!success, "exit /b 3 must not count as success");
    assert_eq!(stdout.trim_end(), "out", "{stdout:?}");
    assert_eq!(stderr.trim_end(), "err", "{stderr:?}");
}

#[cfg(windows)]
#[test]
fn system_render_exec_kills_a_render_that_overruns_its_budget_on_windows() {
    // `ping -n 11` runs ~10s (one echo per second). It is a direct child
    // (no cmd.exe wrapper), so `Child::kill` terminates the process we time.
    let started = Instant::now();
    let run = system_render_exec(
        &owned(&[&system32("PING.EXE"), "-n", "11", "127.0.0.1"]),
        Duration::from_millis(200),
    );
    assert_eq!(run, RenderRun::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn system_render_exec_reports_a_missing_binary_as_a_spawn_failure() {
    let run = system_render_exec(
        &owned(&["/nonexistent/mur-browser"]),
        Duration::from_secs(5),
    );
    assert!(matches!(run, RenderRun::SpawnFailed(_)), "{run:?}");
}

#[test]
fn smoke_argv_uses_version_flag_for_agent_browser() {
    assert_eq!(smoke_argv("/usr/local/bin/agent-browser")[1], "--version");
}

// ── L1 → L2 wiring (setup + doctor) ──────────────────────────────────

fn home_with_lightpanda() -> (tempfile::TempDir, mur_common::test_env::EnvGuard) {
    let (home, g) = bare_home();
    let aura = home.path().join("aura");
    std::fs::create_dir_all(&aura).unwrap();
    std::fs::write(aura.join("lightpanda"), b"x").unwrap();
    (home, g)
}

/// A fake browser that really fetches the loopback page, then "renders" it.
fn rendering_exec(
    calls: &mut Vec<Vec<String>>,
) -> impl FnMut(&[String], Duration) -> RenderRun + '_ {
    move |argv: &[String], _| {
        calls.push(argv.to_vec());
        http_get(&loopback_url_in(argv));
        exited(true, "# page\nMUR-RENDER-42\n", "")
    }
}

#[test]
fn setup_renders_with_the_engine_the_gateway_would_pick() {
    let (home, _g) = home_with_lightpanda();
    let mut run = |_: &[&str]| Ok(true);
    let mut calls = Vec::new();
    let mut exec = rendering_exec(&mut calls);
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(Vec::new()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut exec,
    )
    .unwrap();
    drop(exec);
    let out = String::from_utf8(out).unwrap();
    assert_eq!(calls.len(), 1, "one render, no chrome close: {calls:?}");
    assert!(calls[0][0].ends_with("lightpanda"));
    assert_eq!(calls[0][1], "fetch");
    assert!(out.contains("✓ rendered"), "{out}");
}

#[test]
fn a_browser_that_fails_l1_is_never_asked_to_render() {
    let (home, _g) = home_with_lightpanda();
    let mut run = |_: &[&str]| Ok(false);
    let mut rendered = 0;
    let mut exec = |_: &[String], _: Duration| {
        rendered += 1;
        RenderRun::TimedOut
    };
    let mut out = Vec::new();
    ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(Vec::new()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut exec,
    )
    .unwrap();
    assert_eq!(rendered, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("did not pass the version check")
    );
}

#[test]
fn a_failed_lightpanda_render_warns_but_never_fails_setup() {
    let (home, _g) = home_with_lightpanda();
    let mut run = |_: &[&str]| Ok(true);
    let mut exec = |argv: &[String], _: Duration| {
        http_get(&loopback_url_in(argv));
        exited(
            true,
            "document.getElementById('o').textContent='MUR-RENDER-'+(6*7)",
            "",
        )
    };
    let mut out = Vec::new();
    let r = ensure_render_browser(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut Cursor::new(Vec::new()),
        &mut out,
        &mut run,
        &mut no_fetch,
        &mut exec,
    );
    assert!(r.is_ok(), "spec D5: a failed smoke test never fails setup");
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("did not run its script"), "{out}");
    assert!(out.contains("will not fall back to Chrome"), "{out}");
}

#[test]
fn doctor_renders_only_with_the_render_flag() {
    let (home, _g) = home_with_lightpanda();
    let mut run = |_: &[&str]| Ok(true);

    let mut out = Vec::new();
    doctor(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut out,
        &mut run,
        None,
    )
    .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("add --render"));

    let mut calls = Vec::new();
    let mut exec = rendering_exec(&mut calls);
    let mut out = Vec::new();
    doctor(
        home.path(),
        &InstallPlan::AgentBrowser,
        &mut out,
        &mut run,
        Some(&mut exec),
    )
    .unwrap();
    drop(exec);
    assert_eq!(calls.len(), 1);
    assert!(String::from_utf8(out).unwrap().contains("✓ rendered"));
}

#[test]
fn a_doctor_render_warning_is_not_a_doctor_error() {
    let (home, _g) = home_with_lightpanda();
    let mut run = |_: &[&str]| Ok(true);
    let mut exec = |_: &[String], _: Duration| RenderRun::Denied;
    let mut out = Vec::new();
    assert!(
        doctor(
            home.path(),
            &InstallPlan::AgentBrowser,
            &mut out,
            &mut run,
            Some(&mut exec)
        )
        .is_ok()
    );
    assert!(String::from_utf8(out).unwrap().contains("sandbox"));
}
