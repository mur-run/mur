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
pub fn render_passed(output: &str) -> bool {
    output.contains(RENDER_MARKER)
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
                     nothing was installed; retry with `mur fleet install-deps {} --program lightpanda`.",
                    super::status::DEFAULT_FLEET_NAME
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
        write!(output, "Type 'yes' to do this now (anything else = skip): ")?;
        output.flush()?;
        let mut line = String::new();
        input.read_line(&mut line)?;
        if line.trim() != "yes" {
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
                "  install with: mur fleet install-deps {} --program lightpanda",
                super::status::DEFAULT_FLEET_NAME
            )?,
            InstallPlan::AgentBrowser => {
                writeln!(output, "  install with:")?;
                print_install_plan(output, plan)?;
            }
        }
        writeln!(
            output,
            "  or answer 'yes' to the browser question in `mur deep-research setup`."
        )?;
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
mod tests {
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
        assert!(
            out.contains("install-deps deep-research --program lightpanda"),
            "{out}"
        );
    }

    #[test]
    fn doctor_points_at_install_deps_when_lightpanda_is_available() {
        let (home, _g) = bare_home();
        let mut run = |_: &[&str]| -> Result<bool> { panic!("doctor is read-only") };
        let mut out = Vec::new();
        assert!(doctor(home.path(), &lightpanda_plan(), &mut out, &mut run, None).is_err());
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("mur fleet install-deps deep-research --program lightpanda"),
            "{out}"
        );
        assert!(!out.contains("npm"), "{out}");
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
        // Lightpanda `--dump markdown` shape.
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
        let mut exec = |_: &[String], _: Duration| -> RenderRun {
            panic!("obscura is outside the smoke test")
        };
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
}
