//! Live end-to-end coverage for `SlashCmd::Browser`: a real `mur-agent-runtime`
//! child, dialed over its real Unix socket, exercising the exact path
//! `browser_cmd::handle` runs in production — `App`, `handle_slash`,
//! `spawn_stream` → `dial_message_streaming`.
//!
//! This closes the one gap the design spec's §5 checklist left open: prior
//! verification only checked `mur agent skill add` in isolation and asserted
//! the dispatch arm's *existence*, never that a live agent actually receives
//! the turn.
//!
//! The fixture profile is pinned to `provider: echo` — the deliberate,
//! no-network test stub (`supervisor_runner/provider.rs`'s `"echo"` arm,
//! `RunnerBackend::StubEcho`) — rather than the base fixture's
//! `provider: ollama`, which this test's live runtime would actually try to
//! dial at `127.0.0.1:11434`; nothing listens there in this environment, so
//! the turn would fail with a connect error rather than complete.
//! `StubEcho` answers through `Task.messages`, never through the `sink`
//! (only `RunnerBackend::{Llm,CliSpawn}` stream token deltas — see
//! `task_runner.rs`'s `run_sync_inner` match), so this test's own read of the
//! reply must come from the terminal `Done` task, via `stream::task_outcome`
//! (the same function `stream_handler::handle_stream`'s `Done` arm calls in
//! production), not from accumulated `Delta` text. An earlier version of this
//! test assumed `MUR_LLM_MOCK=1` would make the runtime call a
//! `MockBackend` — wrong: that env var is read only by
//! `mur-core::conversations::backend::factory` (the `mur ask`/summarize
//! pipeline); `mur-agent-runtime` has its own, separate LLM client stack
//! under `mur-agent-runtime/src/llm/` and never reads it. Kept as a
//! documented no-op env var below anyway, since it is harmless.
//!
//! The runtime binary must be present alongside the `mur` lib's test binary
//! (built by the same `cargo build -p mur-agent-runtime`, found by a bounded
//! walk up from `current_exe()` to `target/<profile>/`); if absent, the test skips
//! rather than failing CI environments that only build the CLI.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use mur_common::agent::AgentProfile;

use super::super::stream::STREAM_CHANNEL_CAP;
use super::super::*;

const RUNTIME_BIN_NAME: &str = "mur-agent-runtime";
/// How far above the test executable `target/<profile>/` can sit. Two levels
/// before Cargo 1.100 (`<profile>/deps/<test>`); the build-dir layout nests
/// the test deeper (`<profile>/build/<pkg>/<hash>/deps/<test>`). Same bound as
/// `mur-daemon/tests/cross_router_dispatch_e2e.rs`'s `find_profile_bin`.
const PROFILE_DIR_MAX_DEPTH: usize = 6;

fn locate_runtime_binary() -> Option<PathBuf> {
    find_profile_bin(&std::env::current_exe().ok()?, RUNTIME_BIN_NAME)
}

/// Walk up from the test executable to the first ancestor holding `name`,
/// rather than assume a fixed depth (which broke on Cargo 1.100's build-dir
/// layout). Bounded so a stray `mur-agent-runtime` high above the target dir
/// — `~/`, `/usr/local/` — is never picked up as the one this build produced.
fn find_profile_bin(test_exe: &Path, name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    test_exe
        .ancestors()
        .skip(1)
        .take(PROFILE_DIR_MAX_DEPTH)
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

mod find_profile_bin_tests {
    use super::*;

    fn layout(test_rel: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("target").join("debug");
        let bin = profile.join(format!(
            "{RUNTIME_BIN_NAME}{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(&bin, b"").unwrap();
        let exe = profile.join(test_rel);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"").unwrap();
        (root, exe, bin)
    }

    #[test]
    fn finds_runtime_from_the_pre_1_100_deps_layout() {
        let (_root, exe, bin) = layout("deps/mur_core-0123456789abcdef");
        assert_eq!(find_profile_bin(&exe, RUNTIME_BIN_NAME), Some(bin));
    }

    #[test]
    fn finds_runtime_from_the_1_100_build_dir_layout() {
        let (_root, exe, bin) = layout("build/mur-core/0123456789abcdef/deps/mur_core-0123");
        assert_eq!(find_profile_bin(&exe, RUNTIME_BIN_NAME), Some(bin));
    }

    /// Put the runtime in `dir` and the test executable `levels` directories
    /// below it, so the runtime is the `levels + 1`th ancestor the walk visits.
    fn runtime_above(dir: &Path, levels: usize) -> (PathBuf, PathBuf) {
        let bin = dir.join(format!(
            "{RUNTIME_BIN_NAME}{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(&bin, b"").unwrap();
        let mut exe = dir.to_path_buf();
        for i in 0..levels {
            exe.push(format!("d{i}"));
        }
        std::fs::create_dir_all(&exe).unwrap();
        exe.push("test");
        std::fs::write(&exe, b"").unwrap();
        (exe, bin)
    }

    #[test]
    fn a_runtime_exactly_at_the_depth_bound_is_found() {
        let root = tempfile::tempdir().unwrap();
        let (exe, bin) = runtime_above(root.path(), PROFILE_DIR_MAX_DEPTH - 1);
        assert_eq!(find_profile_bin(&exe, RUNTIME_BIN_NAME), Some(bin));
    }

    #[test]
    fn a_runtime_one_past_the_depth_bound_is_ignored() {
        // Every directory the bounded walk visits sits inside `root`, so a
        // stray runtime in the system temp dir cannot satisfy it.
        let root = tempfile::tempdir().unwrap();
        let (exe, _bin) = runtime_above(root.path(), PROFILE_DIR_MAX_DEPTH);
        assert_eq!(find_profile_bin(&exe, RUNTIME_BIN_NAME), None);
    }

    #[test]
    fn a_directory_named_like_the_runtime_is_not_the_runtime() {
        // The `pad` level keeps all six visited ancestors inside `root`,
        // never the shared system temp dir above it.
        let root = tempfile::tempdir().unwrap();
        let exe = root
            .path()
            .join("pad/build/mur-agent-runtime/0123456789abcdef/deps/t");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(find_profile_bin(&exe, RUNTIME_BIN_NAME), None);
    }
}

struct RuntimeGuard {
    child: Child,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

fn write_profile(home: &std::path::Path, name: &str, sock_path: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut profile = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    // `default_for_tests()`'s fixture carries `provider: ollama`, which this
    // test's live runtime would actually dial — nothing is listening on
    // 127.0.0.1:11434 here, so the turn would fail with a connect error
    // rather than complete. `echo` is the deliberate, no-network test stub
    // (`supervisor_runner/provider.rs`'s `"echo"` arm, `RunnerBackend::StubEcho`) —
    // the same override pattern `supervisor_runner/tests.rs`'s own fixture helper
    // uses (`p.model.provider = provider.to_string()`).
    profile.model.provider = "echo".to_string();
    profile.model.name = "echo".to_string();
    profile.transport.stdio = true;
    profile.transport.socket.enabled = true;
    profile.transport.socket.bind = format!("unix://{sock_path}");
    // This sandbox environment cannot itself call `sandbox_init` (a test
    // process is already inside one on macOS CI-alikes), so the runtime
    // must be allowed to run advisory-only rather than refuse to start.
    profile.entitlements.fail_closed_on_sandbox_error = false;
    let yaml = serde_yaml_ng::to_string(&profile).unwrap();
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

fn install_browser_skill(home: &std::path::Path, agent: &str) {
    // Mirrors what `ensure_mur_skill` ships globally and what `/browser
    // --add` -> `manage::skill_add` installs per-agent: a `skills/browser`
    // subdir carrying a canonical `skill.yaml`. Built directly here (rather
    // than via `cmd_skill_add`) so this test exercises only the D5 dispatch
    // path under test, not D6's reuse of the add pipeline — that path
    // already has its own coverage (`sync_skill_tests`, manual e2e in the
    // implementation turn).
    let dir = home
        .join("agents")
        .join(agent)
        .join("skills")
        .join("browser");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = "name: browser\n\
         version: 1.0.0\n\
         publisher: human:test\n\
         category: workflow\n\
         description: Browser automation hub skill\n\
         content:\n  abstract: Browser skill hub.\n  context: Full procedure body.\n";
    std::fs::write(dir.join("skill.yaml"), manifest).unwrap();

    // `App`'s `skills` list (what the `--add` guard checks) is populated at
    // startup from the profile's `skills:` entries, not by scanning disk —
    // so the profile needs the pointer too.
    let profile_path = home.join("agents").join(agent).join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path).unwrap();
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    profile.skills.push("skills/browser".to_string());
    std::fs::write(&profile_path, serde_yaml_ng::to_string(&profile).unwrap()).unwrap();
}

fn boot_runtime(
    home: &std::path::Path,
    agent: &str,
    runtime_bin: &str,
    sock_path: &str,
) -> RuntimeGuard {
    let child = Command::new(runtime_bin)
        .env("MUR_HOME", home)
        .env("MUR_LLM_MOCK", "1")
        .args(["--profile", agent])
        .spawn()
        .expect("spawn runtime");
    let lock = home.join("agents").join(agent).join("running.lock");
    let sock = std::path::Path::new(sock_path);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(meta) = std::fs::metadata(&lock)
            && meta.len() > 0
            && sock.exists()
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        found,
        "runtime did not come up within 5s: running.lock non-empty = {}, socket {sock_path} present = {}",
        std::fs::metadata(&lock)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        sock.exists()
    );
    RuntimeGuard { child }
}

/// Drain `rx` until `Done`/`Err`/`TurnLost` (or timeout).
///
/// The reply comes from `Done`'s terminal `Task` via `stream::task_outcome`
/// — the same function `stream_handler::handle_stream`'s `Done` arm calls in
/// production — falling back to accumulated `Delta` text only if the task
/// carries no message of its own. `RunnerBackend::StubEcho` (this test's
/// actual backend — see `write_profile`) answers only through
/// `Task.messages`, never the delta sink, so a delta-only read would see an
/// empty string on a turn that in fact completed and answered.
fn drain_turn(rx: &mut mpsc::Receiver<StreamMsg>, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut deltas = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timed out waiting for turn to finish; partial reply: {deltas:?}"
            ));
        }
        let recv = std::thread::scope(|_| {
            // blocking_recv with a timeout via a small poll loop — rx is a
            // tokio mpsc::Receiver but this test body is sync (`#[test]`,
            // not `#[tokio::test]`), matching `spawn_stream`'s own use of
            // `blocking_send` from a plain `std::thread::spawn`.
            rx.blocking_recv()
        });
        match recv {
            Some(StreamMsg::Delta { text, .. }) => deltas.push_str(&text),
            Some(StreamMsg::Done { task, .. }) => {
                return match stream::task_outcome(&task) {
                    Ok((reply, _)) if !reply.is_empty() => Ok(reply),
                    Ok(_) => Ok(deltas),
                    Err(cause) => Err(format!("Done carried a failed task: {cause}")),
                };
            }
            Some(StreamMsg::Err { error, .. }) => return Err(format!("Err: {error}")),
            Some(StreamMsg::TurnLost { note, .. }) => return Err(format!("TurnLost: {note}")),
            Some(_) => {} // Hitl/StepStarted/etc — not expected for this turn, ignore
            None => return Err("channel closed before Done".to_string()),
        }
    }
}

#[test]
fn browser_add_then_mode_arg_reaches_a_live_agent_as_a_turn() {
    // The readiness probe waits for `agent.sock` to appear as a file, which is
    // the Unix-socket transport. Before `locate_runtime_binary` learned the
    // `.exe` suffix this test silently skipped on Windows; keep it that way
    // explicitly rather than run a Unix-only probe there.
    if cfg!(not(unix)) {
        eprintln!("(skipping — live runtime probe is Unix-socket only)");
        return;
    }
    let Some(runtime_bin) = locate_runtime_binary() else {
        eprintln!("(skipping — mur-agent-runtime binary not present in target dir)");
        return;
    };
    let runtime_str = runtime_bin.to_str().unwrap().to_string();

    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let sock_path = home.join("agents").join("browsertest").join("agent.sock");
    let sock_str = sock_path.to_str().unwrap().to_string();

    write_profile(home, "browsertest", &sock_str);
    install_browser_skill(home, "browsertest");
    // The turn below is only sent once its user event is in the channel
    // (C1), and under `MUR_CHANNEL_REQUIRE_SIG` that append needs this
    // agent's writer key. Plant it before boot so the runtime loads the same
    // key (`supervisor/identity.rs` falls back to an ephemeral one otherwise)
    // instead of this test refusing the turn on the CI step that sets it.
    crate::channel_writer::plant_identity_for(home, "browsertest");

    let _guard = boot_runtime(home, "browsertest", &runtime_str, &sock_str);

    // `complete::load_agent_skills` — what actually fills `app.skills`
    // (`term.rs:94`, the real TUI startup call) — resolves its own MUR home
    // via `resolve_mur_home()` rather than taking `home` as an argument (see
    // the NOTE on `complete.rs`'s `MenuContext::load`), so it must be
    // pointed at this tempdir through the env var it *does* honor.
    // SAFETY: env mutation isn't thread-safe across parallel tests — this
    // whole file must run with `--test-threads=1` (already required by
    // `boot_runtime`'s shared ports/sockets convention).
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", home);

    // Build a real App against this home/agent, same as the TUI does, and
    // load `app.skills` exactly as `term.rs` does at startup.
    let session = Session::create(home, "browsertest").unwrap();
    let mut app = App::new(
        home.to_path_buf(),
        "browsertest".to_string(),
        session,
        &theme::ANSI,
    );
    app.skills = complete::load_agent_skills(&app.agent);
    app.menu_ctx = complete::MenuContext::load(&app.home, &app.agent);
    assert!(
        app.skills.iter().any(|c| c.display == "/browser"),
        "fixture must have the browser skill attached before the mode-arg branch is meaningful: {:?}",
        app.skills
            .iter()
            .map(|c| c.display.clone())
            .collect::<Vec<_>>()
    );

    let (tx, mut rx) = mpsc::channel(STREAM_CHANNEL_CAP);

    // The exact call `SlashCmd::Browser(args)` makes in `slash_cmds.rs`.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(browser_cmd::handle(
            &mut app,
            vec!["testing".to_string()],
            &tx,
        ));

    // D5's whole point: this must be a live turn, not a system notice.
    assert!(
        app.streaming,
        "browser_cmd::handle with a mode arg must start a turn (D5), got messages: {:?}",
        app.messages
            .iter()
            .map(|m| (m.role, m.text.clone()))
            .collect::<Vec<_>>()
    );
    let last_user = app
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .expect("a User bubble was recorded for the turn");
    assert!(
        last_user.text.contains("browser skill") && last_user.text.contains("testing"),
        "turn text should carry the mode instruction: {:?}",
        last_user.text
    );

    let reply = drain_turn(&mut rx, Duration::from_secs(10))
        .expect("the live agent must actually answer the turn, not time out or error");
    // `RunnerBackend::StubEcho` (`write_profile` pins `provider: echo`)
    // answers by echoing the turn's input text back prefixed `"echo: "`
    // (`task_runner.rs`'s `echo_response`) — proof this specific reply came
    // from a live round trip through the runtime, not a stale/empty channel
    // read, which is D5's whole point (a live turn, not a system notice
    // logged locally with nothing sent).
    assert!(
        reply.starts_with("echo: ") && reply.contains("testing"),
        "expected the echo backend to echo the turn text back, got: {reply:?}"
    );
}

#[test]
fn browser_mode_arg_without_add_refuses_locally_no_turn() {
    // No runtime needed at all: this is the guard branch, which must never
    // reach the network. Confirms the negative case the live test above
    // doesn't cover.
    let mut app = App::test_fixture();
    app.menu_ctx = complete::MenuContext::load(&app.home, &app.agent); // no skills installed
    let (tx, _rx) = mpsc::channel(STREAM_CHANNEL_CAP);

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(browser_cmd::handle(
            &mut app,
            vec!["testing".to_string()],
            &tx,
        ));

    assert!(
        !app.streaming,
        "must not start a turn when the skill isn't attached"
    );
    assert!(
        app.messages.iter().any(|m| m.text.contains("not attached")),
        "expected the local refusal notice, got: {:?}",
        app.messages
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    );
}
