//! Render-browser preflight for deep research: check, offer install, smoke test.
//!
//! Before this, answering `yes` to the wizard's render-browser question on a
//! machine with no browser printed a one-line note and still ended with
//! "Setup complete" — the worker silently had no rendered fetch, and nobody
//! found out until a JS-only page came back empty mid-research.
//!
//! Install discipline: the exact commands are printed FIRST, and they run only
//! on a literal `yes` — same consent rule as egress and the browser grant
//! itself. Nothing is ever installed silently, and a declined or failed install
//! never fails setup: plain fetch still works without a render browser.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use mur_common::deps::registry::CuratedRecipe;

use super::provision::{
    LIGHTPANDA_RELATIVE, OBSCURA_RELATIVE, OBSCURA_WORKER_RELATIVE, render_binaries,
};

/// The engine the gateway will pick when no `render_engine` is configured.
/// mur-core does not depend on `mur-research-gateway`, so this mirrors its
/// `auto_detect_render_engine` (`mur-research-gateway/src/config.rs:373`);
/// keep the two in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderEngine {
    Lightpanda,
    Obscura,
    AgentBrowser,
}

/// Same order as the gateway: native lightpanda → obscura (both binaries) →
/// agent-browser. Existence only, like the gateway — a broken lightpanda is
/// still the one that gets picked.
pub fn auto_detect_engine(mur_home: &Path) -> RenderEngine {
    if mur_home.join(LIGHTPANDA_RELATIVE).exists() {
        return RenderEngine::Lightpanda;
    }
    if mur_home.join(OBSCURA_RELATIVE).exists() && mur_home.join(OBSCURA_WORKER_RELATIVE).exists() {
        return RenderEngine::Obscura;
    }
    RenderEngine::AgentBrowser
}

/// The npm fallback (spec D2b), in order — used only on a platform with no
/// curated Lightpanda recipe. `agent-browser install` pulls the
/// Chrome-for-Testing build the chrome engine falls back to; without it the
/// npm package alone cannot render.
pub const INSTALL_STEPS: &[&[&str]] = &[
    &["npm", "i", "-g", "agent-browser@latest"],
    &["agent-browser", "install"],
];

/// What setup offers to install when no render browser is present.
///
/// Native Lightpanda first (spec D2), from the same curated, sha256-pinned
/// recipe `mur fleet install-deps` uses — it is what the gateway auto-detects
/// first, and the one that actually rendered under the sandbox. npm
/// agent-browser only where no Lightpanda build exists for the platform.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallPlan {
    Lightpanda(CuratedRecipe),
    AgentBrowser,
}

/// Pick the plan for `platform` (`mur_common::deps::current_platform()`).
pub fn install_plan(platform: &str) -> InstallPlan {
    match mur_common::deps::registry::recipe("lightpanda", platform) {
        Some(r) => InstallPlan::Lightpanda(r),
        None => InstallPlan::AgentBrowser,
    }
}

/// The browser setup's consent prompt names: the one `install_plan` would put
/// in place for `platform`, which is also the one the gateway then
/// auto-detects first. Lightpanda where a curated recipe exists, agent-browser
/// (the npm fallback) elsewhere.
pub fn render_browser_name(platform: &str) -> &'static str {
    match install_plan(platform) {
        InstallPlan::Lightpanda(_) => "lightpanda",
        InstallPlan::AgentBrowser => "agent-browser",
    }
}

/// Declare native Lightpanda in the fleet's `requires_programs`, so
/// `mur fleet doctor/install-deps deep-research` see it and can install it
/// from the curated recipe. Only on platforms that have a recipe — elsewhere
/// the declaration would be an uninstallable, permanently "missing" dep.
/// Idempotent: returns false when nothing was added.
pub fn declare_lightpanda(fleet: &mut mur_common::fleet::Fleet, platform: &str) -> bool {
    if mur_common::deps::registry::recipe("lightpanda", platform).is_none()
        || fleet
            .requires_programs
            .iter()
            .any(|d| d.name == "lightpanda")
    {
        return false;
    }
    fleet.requires_programs.push(mur_common::deps::ProgramDep {
        name: "lightpanda".into(),
        detect: mur_common::deps::DetectMethod::File {
            file: "aura/lightpanda".into(),
        },
        reason: "render browser for JS-only pages (deep-research gateway)".into(),
        hint: Some("mur deep-research setup".into()),
        registry: Some("lightpanda".into()),
        recipe: None,
    });
    true
}

/// Download + verify + place one curated recipe under `mur_home`. Injected so
/// tests never touch the network.
pub type Fetcher<'a> = &'a mut dyn FnMut(&CuratedRecipe, &Path) -> Result<Vec<PathBuf>>;

/// Real fetcher: the deps installer (sha256 checked before anything is
/// written). Setup is synchronous and may be called from inside the CLI's
/// runtime, so — like `agent/mcp_add.rs` — the download runs on its own thread
/// with its own current-thread runtime instead of borrowing the caller's.
pub fn system_fetcher(recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow::anyhow!("build download runtime: {e}"))?
                .block_on(crate::cmd::deps::installer::install(recipe, mur_home))
        })
        .join()
        .map_err(|_| anyhow::anyhow!("download thread panicked"))?
    })
}

/// Run one command; `Ok(true)` when it exited 0. Injected so tests never touch
/// npm or the network.
pub type Runner<'a> = &'a mut dyn FnMut(&[&str]) -> Result<bool>;

/// Real runner: inherits the terminal so npm's progress is visible.
pub fn system_runner(argv: &[&str]) -> Result<bool> {
    let (prog, args) = argv.split_first().expect("argv is never empty");
    let status = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("could not start `{prog}`: {e}"))?;
    Ok(status.success())
}

/// The argv that proves a found binary actually executes. Version flags only —
/// they start the binary without opening a page or a network connection.
pub fn smoke_argv(bin: &str) -> Vec<String> {
    let flag = if Path::new(bin).ends_with("lightpanda") {
        "version"
    } else {
        "--version"
    };
    vec![bin.to_string(), flag.to_string()]
}

/// L2 render timeout, in the same unit the gateway passes to `--http-timeout`.
/// 30s rather than the gateway's 20s default: a cold first start is slower.
const RENDER_TIMEOUT_MS: u64 = 30_000;

/// L2 argv for native Lightpanda. A copy of the gateway's `build_lightpanda_argv`
/// (`mur-research-gateway/src/browser.rs:122`) — mur-core cannot depend on the
/// gateway — without `--http-proxy`: the smoke test runs outside the sandbox
/// against a loopback page, so there is no egress proxy to thread through.
pub fn lightpanda_render_argv(bin: &str, url: &str) -> Vec<String> {
    vec![
        bin.to_string(),
        "fetch".to_string(),
        url.to_string(),
        "--dump".to_string(),
        "markdown".to_string(),
        "--http-timeout".to_string(),
        RENDER_TIMEOUT_MS.to_string(),
    ]
}

/// L2 argv for agent-browser + Chrome. Mirrors the chrome branch of the
/// gateway's `build_fetch_argv` (`mur-research-gateway/src/browser.rs:63`)
/// minus the stealth `--args`: a loopback page does not need them. The session
/// is named per process so a leftover smoke session never collides with a
/// gateway `rg-…` session.
pub fn chrome_render_argv(bin: &str, url: &str) -> Vec<String> {
    vec![
        bin.to_string(),
        "--engine".to_string(),
        "chrome".to_string(),
        "--session".to_string(),
        smoke_session(),
        "open".to_string(),
        url.to_string(),
        "snapshot".to_string(),
    ]
}

/// What L2 looks for. It appears only after the page's script runs: the
/// source below spells it as `'MUR-RENDER-'+(6*7)`, never as the literal.
pub const RENDER_MARKER: &str = "MUR-RENDER-42";

/// The one page the L2 loopback server returns (spec §4 L2).
pub const RENDER_PAGE: &str = "<div id=\"o\"></div><script>document.getElementById('o').textContent='MUR-RENDER-'+(6*7)</script>";

/// L2 verdict. A browser that fetched the HTML but never executed JS returns
/// the raw source (`6*7`), which does not contain the marker, so it fails.
///
/// Backslashes are dropped before matching: Lightpanda's `--dump markdown`
/// escapes `-`, printing the rendered marker as `MUR\-RENDER\-42`.
pub fn render_passed(output: &str) -> bool {
    output.replace('\\', "").contains(RENDER_MARKER)
}

/// How long the loopback server waits for a browser to show up at all. The
/// browser is itself bounded by `RENDER_TIMEOUT_MS`; the slack covers a cold
/// process start before it even dials.
const PAGE_SERVER_BUDGET: Duration = Duration::from_millis(RENDER_TIMEOUT_MS + 5_000);

/// How long one accepted connection may sit silent before it is dropped.
/// Chrome opens speculative preconnects that may never carry a request; a
/// blocking read on one of those must not starve the real request behind it.
const PAGE_SERVER_CONN_IDLE: Duration = Duration::from_secs(2);

/// Start the L2 page server (spec §4 L2): `127.0.0.1:0`, std `TcpListener`,
/// one thread, one page, then closed. Returns the URL to hand the browser and
/// a handle that yields `true` if a page was actually served — `false` means
/// the browser never reached it, which is a different failure from "reached
/// it but did not run JS".
pub fn serve_render_page() -> Result<PageServer> {
    spawn_page_server(PAGE_SERVER_BUDGET, PAGE_SERVER_CONN_IDLE)
}

/// A running L2 page server. `url` goes to the browser; `finish` ends it.
pub struct PageServer {
    pub url: String,
    handle: JoinHandle<bool>,
    stop: Arc<AtomicBool>,
}

impl PageServer {
    /// Call once the browser process is gone: stops waiting for a connection
    /// that will never come (instead of sitting out the whole budget) and
    /// reports whether the page was served.
    pub fn finish(self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or(false)
    }
}

fn spawn_page_server(budget: Duration, conn_idle: Duration) -> Result<PageServer> {
    // Loopback only, never 0.0.0.0: the smoke test must not open a port to the LAN.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    // std has no accept timeout; poll so an absent browser cannot pin the thread.
    listener.set_nonblocking(true)?;
    let url = format!("http://{}/", listener.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_seen = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline && !stop_seen.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if answer_with_render_page(stream, conn_idle) {
                        // Dropping the listener here is the "then closed" part.
                        return true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return false,
            }
        }
        false
    });
    Ok(PageServer { url, handle, stop })
}

/// Read one request head; if it is a GET, send the page and close. Anything
/// else (silent preconnect, garbage, early hang-up) is dropped unanswered.
fn answer_with_render_page(mut stream: TcpStream, idle: Duration) -> bool {
    // On macOS an accepted socket inherits the listener's O_NONBLOCK.
    if stream.set_nonblocking(false).is_err() || stream.set_read_timeout(Some(idle)).is_err() {
        return false;
    }
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
        if head.len() > 16 * 1024 {
            return false;
        }
    }
    if !head.starts_with(b"GET ") {
        return false;
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{RENDER_PAGE}",
        RENDER_PAGE.len()
    );
    stream.write_all(response.as_bytes()).is_ok() && stream.flush().is_ok()
}

/// Wall-clock budget for one L2 render (spec §3.1: 30s, cold Chrome included).
pub const RENDER_TIMEOUT: Duration = Duration::from_millis(RENDER_TIMEOUT_MS);

/// Budget for the best-effort session `close` after an agent-browser render.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for a pipe to hit EOF after the process exited. A
/// grandchild that inherited stdout must not hang the smoke test.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// Enough for any honest render of a one-line page; the rest is discarded.
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;

/// How one bounded browser invocation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderRun {
    Exited {
        success: bool,
        stdout: String,
        stderr: String,
    },
    /// Killed after its budget ran out.
    TimedOut,
    /// Spawn got `PermissionDenied` — a sandbox, not a broken browser.
    Denied,
    /// Spawn failed for any other reason (missing binary, …).
    SpawnFailed(String),
}

/// Runs one browser argv under a timeout. Injected so tests never open a browser.
pub type RenderExec<'a> = &'a mut dyn FnMut(&[String], Duration) -> RenderRun;

/// Why an L2 run did not pass. Each maps to a different message: the fix for
/// "never reached the page" is not the fix for "reached it, ran no JS".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum L2Failure {
    /// The page server never answered: the browser did not start or could not dial loopback.
    NeverReached,
    /// Exited non-zero after loading the page; the gateway counts that as a failed render.
    ExitedNonZero,
    /// Loaded the page, exited 0, but the marker is missing: JS did not run.
    NoJs,
    TimedOut,
    CouldNotStart(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum L2Outcome {
    Passed {
        elapsed: Duration,
    },
    Failed {
        why: L2Failure,
        /// The last 3 non-empty stderr lines, for the ⚠ report (spec §3.1).
        stderr_tail: Vec<String>,
    },
    /// Spawn hit `PermissionDenied`; says nothing about the browser itself.
    Sandboxed,
    /// Obscura: opt-in, outside the smoke test (spec §3.1).
    NotTested,
}

/// Per-process agent-browser session, kept apart from the gateway's `rg-…` sessions.
fn smoke_session() -> String {
    format!("mur-smoke-{}", std::process::id())
}

fn stderr_tail(stderr: &str) -> Vec<String> {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(3)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

/// L2 (spec §3.1): serve the page on loopback, render it with `engine` under
/// `RENDER_TIMEOUT`, judge the output, then best-effort close the
/// agent-browser session. `bin` is the binary the caller already passed
/// through L1 — the runner does not re-check it.
pub fn render_smoke(engine: RenderEngine, bin: &str, exec: RenderExec<'_>) -> Result<L2Outcome> {
    let argv_for: fn(&str, &str) -> Vec<String> = match engine {
        RenderEngine::Lightpanda => lightpanda_render_argv,
        RenderEngine::AgentBrowser => chrome_render_argv,
        RenderEngine::Obscura => return Ok(L2Outcome::NotTested),
    };
    let server = serve_render_page()?;
    let argv = argv_for(bin, &server.url);
    let started = Instant::now();
    let run = exec(&argv, RENDER_TIMEOUT);
    let elapsed = started.elapsed();
    // The browser is gone either way; stop the server instead of letting it
    // sit out its 35s budget.
    let served = server.finish();

    let started_a_session = matches!(run, RenderRun::Exited { .. } | RenderRun::TimedOut);
    if engine == RenderEngine::AgentBrowser && started_a_session {
        // `--session <name> close` per agent-browser's README (0.31.1). Best
        // effort: a failed close must not turn a passing render into a failure.
        let session = smoke_session();
        let _ = exec(
            &[bin.to_string(), "--session".into(), session, "close".into()],
            CLOSE_TIMEOUT,
        );
    }

    let failed = |why, stderr: &str| L2Outcome::Failed {
        why,
        stderr_tail: stderr_tail(stderr),
    };
    Ok(match run {
        RenderRun::Denied => L2Outcome::Sandboxed,
        RenderRun::SpawnFailed(msg) => failed(L2Failure::CouldNotStart(msg), ""),
        RenderRun::TimedOut => failed(L2Failure::TimedOut, ""),
        RenderRun::Exited { stderr, .. } if !served => failed(L2Failure::NeverReached, &stderr),
        RenderRun::Exited {
            success: false,
            stderr,
            ..
        } => failed(L2Failure::ExitedNonZero, &stderr),
        RenderRun::Exited { stdout, stderr, .. } if !render_passed(&stdout) => {
            failed(L2Failure::NoJs, &stderr)
        }
        RenderRun::Exited { .. } => L2Outcome::Passed { elapsed },
    })
}

/// Read a pipe to EOF on its own thread, keeping at most `MAX_CAPTURE_BYTES`
/// but draining the rest so the child never blocks on a full pipe.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.by_ref().take(MAX_CAPTURE_BYTES).read_to_end(&mut kept);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        let _ = tx.send(String::from_utf8_lossy(&kept).into_owned());
    });
    rx
}

/// Real `RenderExec`: spawn with captured output, kill at `timeout`.
pub fn system_render_exec(argv: &[String], timeout: Duration) -> RenderRun {
    let Some((prog, args)) = argv.split_first() else {
        return RenderRun::SpawnFailed("empty argv".into());
    };
    let mut child = match Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return RenderRun::Denied,
        Err(e) => return RenderRun::SpawnFailed(format!("could not start `{prog}`: {e}")),
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Out of time, or cannot even poll it: kill and reap. The drain
            // threads are left to finish on their own.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return RenderRun::TimedOut;
            }
        }
    };
    RenderRun::Exited {
        success: status.success(),
        stdout: stdout.recv_timeout(PIPE_GRACE).unwrap_or_default(),
        stderr: stderr.recv_timeout(PIPE_GRACE).unwrap_or_default(),
    }
}

/// L1 (spec §3.1): version-check every found binary. Returns the ones that ran.
fn smoke(output: &mut dyn Write, bins: &[String], run: Runner<'_>) -> Result<Vec<String>> {
    let mut ok = Vec::new();
    for bin in bins {
        let argv = smoke_argv(bin);
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        match run(&refs) {
            Ok(true) => {
                writeln!(output, "  ✓ {bin} runs")?;
                ok.push(bin.clone());
            }
            Ok(false) => writeln!(output, "  ✗ {bin} exited non-zero on `{}`", argv[1])?,
            Err(e) => writeln!(output, "  ✗ {bin}: {e}")?,
        }
    }
    Ok(ok)
}

impl RenderEngine {
    fn name(self) -> &'static str {
        match self {
            RenderEngine::Lightpanda => "lightpanda",
            RenderEngine::Obscura => "obscura",
            RenderEngine::AgentBrowser => "agent-browser",
        }
    }
}

/// The binary L2 should drive for `engine`, taken from the L1 survivors so a
/// browser that cannot even print its version is never asked to render.
fn engine_binary(engine: RenderEngine, passed: &[String]) -> Option<&String> {
    let wanted = match engine {
        RenderEngine::Lightpanda => "lightpanda",
        RenderEngine::AgentBrowser => "agent-browser",
        RenderEngine::Obscura => return None,
    };
    passed.iter().find(|b| Path::new(b).ends_with(wanted))
}

/// L2 for whatever the gateway would auto-detect, then the verdict. Runs only
/// when that engine's binary passed L1. Never an error: a failed render is a
/// ⚠ report, not a failed setup (spec D5).
fn render_check(
    output: &mut dyn Write,
    mur_home: &Path,
    passed: &[String],
    exec: RenderExec<'_>,
) -> Result<()> {
    let engine = auto_detect_engine(mur_home);
    if engine == RenderEngine::Obscura {
        writeln!(
            output,
            "  – obscura is the selected engine; its render is not smoke-tested"
        )?;
        return Ok(());
    }
    let Some(bin) = engine_binary(engine, passed) else {
        writeln!(
            output,
            "  ⚠ {} is the engine the gateway will pick, but it did not pass the version check — no render test",
            engine.name()
        )?;
        return Ok(());
    };
    writeln!(
        output,
        "Render test ({} on a local JS-only page):",
        engine.name()
    )?;
    let outcome = render_smoke(engine, bin, exec)?;
    report_l2(output, engine, &outcome)
}

fn report_l2(output: &mut dyn Write, engine: RenderEngine, outcome: &L2Outcome) -> Result<()> {
    match outcome {
        L2Outcome::Passed { elapsed } => {
            writeln!(output, "  ✓ rendered in {:.1}s", elapsed.as_secs_f64())?;
        }
        L2Outcome::Sandboxed => {
            writeln!(
                output,
                "  ⚠ could not spawn the browser (permission denied) — that is a sandbox, not a broken browser"
            )?;
        }
        L2Outcome::NotTested => {
            writeln!(output, "  – not tested for this engine")?;
        }
        L2Outcome::Failed { why, stderr_tail } => {
            let what = match why {
                L2Failure::NeverReached => "the browser never fetched the page".to_string(),
                L2Failure::ExitedNonZero => "exited non-zero after loading the page".to_string(),
                L2Failure::NoJs => "fetched the page but did not run its script".to_string(),
                L2Failure::TimedOut => format!("no result within {}s", RENDER_TIMEOUT.as_secs()),
                L2Failure::CouldNotStart(msg) => msg.clone(),
            };
            writeln!(output, "  ⚠ render failed: {what}")?;
            for line in stderr_tail {
                writeln!(output, "      {line}")?;
            }
            if engine == RenderEngine::Lightpanda {
                writeln!(
                    output,
                    "    the gateway will still pick lightpanda and will not fall back to Chrome."
                )?;
            }
        }
    }
    Ok(())
}

/// Everything the install would do, printed before any consent (spec D3):
/// URL, sha256 prefix and destination for Lightpanda; the npm commands for the
/// fallback.
fn print_install_plan(output: &mut dyn Write, plan: &InstallPlan) -> Result<()> {
    match plan {
        InstallPlan::Lightpanda(r) => {
            writeln!(output, "    download  {}", r.url)?;
            writeln!(
                output,
                "    sha256    {}… (checked before anything is written)",
                &r.sha256[..r.sha256.len().min(16)]
            )?;
            writeln!(output, "    install   ~/.mur/{LIGHTPANDA_RELATIVE}")?;
        }
        InstallPlan::AgentBrowser => {
            writeln!(
                output,
                "    (no native Lightpanda build for this platform — using agent-browser)"
            )?;
            for step in INSTALL_STEPS {
                writeln!(output, "    {}", step.join(" "))?;
            }
        }
    }
    Ok(())
}

/// Run the plan after consent. `Ok(false)` = failed; the reason is already
/// printed. A failed Lightpanda download never falls through to npm (D3).
fn run_install_plan(
    output: &mut dyn Write,
    mur_home: &Path,
    plan: &InstallPlan,
    run: Runner<'_>,
    fetch: Fetcher<'_>,
) -> Result<bool> {
    match plan {
        InstallPlan::Lightpanda(r) => match fetch(r, mur_home) {
            Ok(_) => Ok(true),
            Err(e) => {
                writeln!(
                    output,
                    "  ✗ Lightpanda download failed: {e:#}\n    \
                     nothing was installed; re-run `mur deep-research setup` and answer 'yes' to the browser question."
                )?;
                Ok(false)
            }
        },
        InstallPlan::AgentBrowser => {
            for step in INSTALL_STEPS {
                let ok = run(step).unwrap_or_else(|e| {
                    let _ = writeln!(output, "  ✗ {e}");
                    false
                });
                if !ok {
                    writeln!(output, "  ✗ `{}` failed.", step.join(" "))?;
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// Wizard hook, called only after the user said `yes` to the browser grant.
///
/// Found → L1 + L2. Missing → print the `plan` (Lightpanda, or npm only where
/// there is no Lightpanda build), ask once for a literal `yes`, run it,
/// re-check, L1 + L2. Always `Ok`: the caller goes on to
/// `grant_render_browser`, which grants whatever is present now. Never writes
/// `render_engine` (D4), so the gateway's auto-detect still applies.
pub fn ensure_render_browser(
    mur_home: &Path,
    plan: &InstallPlan,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    run: Runner<'_>,
    fetch: Fetcher<'_>,
    exec: RenderExec<'_>,
) -> Result<()> {
    let mut bins = render_binaries(mur_home);
    if bins.is_empty() {
        writeln!(output, "\nNo render browser found. Installing one does:")?;
        print_install_plan(output, plan)?;
        if !crate::cmd::consent::literal_yes(input, output)? {
            writeln!(
                output,
                "  skipped — plain fetch still works; run `mur deep-research doctor` later."
            )?;
            return Ok(());
        }
        if !run_install_plan(output, mur_home, plan, run, fetch)? {
            writeln!(output, "  continuing without a render browser.")?;
            return Ok(());
        }
        bins = render_binaries(mur_home);
        if bins.is_empty() {
            let why = match plan {
                InstallPlan::Lightpanda(_) => {
                    format!("~/.mur/{LIGHTPANDA_RELATIVE} is still missing")
                }
                InstallPlan::AgentBrowser => {
                    "`agent-browser` is still not on PATH — check your npm prefix".to_string()
                }
            };
            writeln!(output, "  ✗ installed, but {why}.")?;
            return Ok(());
        }
    }
    writeln!(output, "Render browser smoke test:")?;
    let passed = smoke(output, &bins, run)?;
    render_check(output, mur_home, &passed, exec)?;
    Ok(())
}

/// `mur deep-research doctor`: read-only. Reports, runs L1, and prints the
/// install commands when nothing is found — never installs. `render` adds L2
/// (`--render`): a real render of a loopback page. Errors when no render
/// browser runs, so scripts can gate on it; an L2 ⚠ alone does not error.
pub fn doctor(
    mur_home: &Path,
    plan: &InstallPlan,
    output: &mut dyn Write,
    run: Runner<'_>,
    render: Option<RenderExec<'_>>,
) -> Result<()> {
    let bins = render_binaries(mur_home);
    writeln!(output, "Render browser:")?;
    if bins.is_empty() {
        writeln!(
            output,
            "  ✗ none found (checked ~/.mur/aura/lightpanda and agent-browser on PATH)"
        )?;
        match plan {
            InstallPlan::Lightpanda(_) => writeln!(
                output,
                "  install with: mur deep-research setup (answer 'yes' to the browser question)"
            )?,
            InstallPlan::AgentBrowser => {
                writeln!(output, "  install with:")?;
                print_install_plan(output, plan)?;
                writeln!(
                    output,
                    "  or answer 'yes' to the browser question in `mur deep-research setup`."
                )?;
            }
        }
        bail!("no render browser installed");
    }
    let passed = smoke(output, &bins, run)?;
    if passed.is_empty() {
        bail!("render browser found but none of it runs");
    }
    match render {
        Some(exec) => render_check(output, mur_home, &passed, exec)?,
        None => writeln!(
            output,
            "  (version check only — add --render for a real render of a local page)"
        )?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
