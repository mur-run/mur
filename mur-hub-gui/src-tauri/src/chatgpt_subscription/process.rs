//! Login/logout and gateway lifecycle — the only place this provider spawns
//! anything other than `codex app-server`.
//!
//! Two things are gated on an explicit boolean from the UI and fail *before*
//! any process starts: `logout(confirmed: false)` and
//! `gateway_install(consented: false)`. Signing out affects every Codex
//! client on the machine; installing the gateway writes a login service.
//! Child output is capped and stripped of control characters before it can
//! reach a diagnostic, and no path here reads the credential file.

use super::{ChatGptAccountView, read_account};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const SHORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Combined stdout+stderr kept per child.
const OUTPUT_CAP: usize = 32 * 1024;
/// The loopback route the runtime dials; `/__mur/health` sits beside it.
pub const CHATGPT_GATEWAY_BASE: &str = "http://127.0.0.1:8088/codex/v1";
const HEALTH_URL: &str = "http://127.0.0.1:8088/__mur/health";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INSTALL_POLL: Duration = Duration::from_secs(10);
const INSTALL_POLL_STEP: Duration = Duration::from_millis(500);
/// Mirrors `mur_model_gateway::install::SERVICE_LABEL`.
const GATEWAY_SERVICE_LABEL: &str = "run.mur-model-gateway";
const GATEWAY_BIN: &str = "mur-model-gateway";

pub const LOGOUT_CONFIRMATION_REQUIRED: &str =
    "confirmation required: signing out affects Codex CLI and IDE";
pub const INSTALL_CONSENT_REQUIRED: &str =
    "consent required: installing the gateway writes a login service";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LoginResult {
    pub authenticated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct GatewayStatusView {
    /// Binary or service descriptor found.
    pub installed: bool,
    /// A valid health reply came back — not merely a service file.
    pub running: bool,
    pub codex_hook: bool,
    /// `chatgpt` / `apikey` / `missing`; only `chatgpt` is ready for this provider.
    pub credential_mode: Option<String>,
    pub compression: bool,
}

/// Two Hub windows must not open two browser login flows.
static LOGIN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ── bounded child runner ────────────────────────────────────────────────

async fn drain<R: tokio::io::AsyncRead + Unpin>(mut r: R, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    // Keep reading past the cap so a chatty child never blocks on a full pipe.
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    kept
}

/// Lossy UTF-8 with control characters removed (newline and tab kept), so a
/// terminal escape in child output cannot reach a diagnostic string.
pub(crate) fn sanitize(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Run to completion under `timeout`; `(exit_ok, combined_output)`. The
/// child is killed and reaped on timeout or cancellation (`kill_on_drop`).
async fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<(bool, String), String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let run = async {
        let (mut out, err) = tokio::join!(drain(stdout, OUTPUT_CAP), drain(stderr, OUTPUT_CAP));
        out.extend_from_slice(&err);
        out.truncate(OUTPUT_CAP);
        (child.wait().await, out)
    };
    let (status, out) = tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| format!("timed out after {timeout:?}"))?;
    let ok = status.map_err(|e| e.to_string())?.success();
    Ok((ok, sanitize(&out)))
}

// ── codex login / logout ────────────────────────────────────────────────

/// `codex login`, then ask the account — exit code zero alone is not success.
pub async fn login(codex: &Path) -> LoginResult {
    let _one_at_a_time = LOGIN_LOCK.lock().await;
    let failed = |error: String| LoginResult {
        authenticated: false,
        error: Some(error),
    };
    let mut cmd = Command::new(codex);
    cmd.arg("login");
    let output = match run_bounded(cmd, LOGIN_TIMEOUT).await {
        Ok((true, out)) => out,
        Ok((false, out)) => return failed(format!("codex login failed: {}", out.trim())),
        Err(e) => return failed(format!("codex login: {e}")),
    };
    match read_account(codex).await {
        Ok(ChatGptAccountView {
            logged_in: true, ..
        }) => LoginResult {
            authenticated: true,
            error: None,
        },
        Ok(a) => failed(format!(
            "codex login finished but no ChatGPT account is signed in (auth: {}). {}",
            a.auth_mode.as_deref().unwrap_or("none"),
            output.trim()
        )),
        Err(e) => failed(e.to_string()),
    }
}

/// Global sign-out. Refuses without `confirmed` — nothing is spawned.
pub async fn logout(codex: &Path, confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    let mut cmd = Command::new(codex);
    cmd.arg("logout");
    let (ok, out) = run_bounded(cmd, SHORT_TIMEOUT).await?;
    if !ok {
        return Err(format!("codex logout failed: {}", out.trim()));
    }
    match read_account(codex).await {
        Ok(a) if a.logged_in => {
            Err("codex logout ran but a ChatGPT account is still signed in".into())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ── gateway ─────────────────────────────────────────────────────────────

/// Mirrors `mur_model_gateway::install::InstallPaths::resolve` (user scope).
fn service_file() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(
            dirs::home_dir()?
                .join("Library/LaunchAgents")
                .join(format!("{GATEWAY_SERVICE_LABEL}.plist")),
        )
    } else if cfg!(target_os = "linux") {
        Some(dirs::config_dir()?.join("systemd/user/mur-model-gateway.service"))
    } else {
        None
    }
}

pub fn resolve_gateway() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(p) = crate::cli_tools::shell_which(GATEWAY_BIN) {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    [
        PathBuf::from("/opt/homebrew/bin").join(GATEWAY_BIN),
        PathBuf::from("/usr/local/bin").join(GATEWAY_BIN),
        home.join(".local/bin").join(GATEWAY_BIN),
        home.join(".cargo/bin").join(GATEWAY_BIN),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

#[derive(Debug, PartialEq, Eq)]
struct Health {
    codex_hook: bool,
    credential: String,
    compression: bool,
}

/// Strict: every field present and of the right shape, `status == "ok"`,
/// credential one of the three documented kinds. Anything else is not a
/// gateway we understand, so it is not "running" for our purposes.
fn parse_health(v: &serde_json::Value) -> Option<Health> {
    if v["status"].as_str()? != "ok" {
        return None;
    }
    let credential = v["codexCredential"].as_str()?;
    if !matches!(credential, "chatgpt" | "apikey" | "missing") {
        return None;
    }
    Some(Health {
        codex_hook: v["codexHook"].as_bool()?,
        credential: credential.to_string(),
        compression: v["compression"].as_bool()?,
    })
}

async fn fetch_health(url: &str) -> Option<Health> {
    let client = reqwest::Client::builder()
        .timeout(HEALTH_TIMEOUT)
        .no_proxy()
        .build()
        .ok()?;
    let v: serde_json::Value = client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    parse_health(&v)
}

async fn status_at(url: &str, installed: bool) -> GatewayStatusView {
    match fetch_health(url).await {
        Some(h) => GatewayStatusView {
            installed: true,
            running: true,
            codex_hook: h.codex_hook,
            credential_mode: Some(h.credential),
            compression: h.compression,
        },
        None => GatewayStatusView {
            installed,
            ..Default::default()
        },
    }
}

pub async fn gateway_status() -> GatewayStatusView {
    let installed = tokio::task::spawn_blocking(|| {
        resolve_gateway().is_some() || service_file().is_some_and(|p| p.exists())
    })
    .await
    .unwrap_or(false);
    status_at(HEALTH_URL, installed).await
}

/// Load (or reload) the service the gateway's `install` wrote. `install`
/// itself only writes the descriptor and prints the `launchctl` lines.
/// Platform-split with `#[cfg]`, not `cfg!`: the macOS arm needs
/// `std::os::unix`, which does not exist on a Windows build.
#[cfg(target_os = "macos")]
async fn activate_service() -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let file = service_file().ok_or("gateway service is not supported on this platform")?;
    let uid = std::fs::metadata(dirs::home_dir().ok_or("no home dir")?)
        .map_err(|e| e.to_string())?
        .uid();
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{GATEWAY_SERVICE_LABEL}");
    // A previous load holds the old plist; unload first, ignore "not loaded".
    let mut bootout = Command::new("launchctl");
    bootout.args(["bootout", &target]);
    let _ = run_bounded(bootout, SHORT_TIMEOUT).await;
    let mut bootstrap = Command::new("launchctl");
    bootstrap.args(["bootstrap", &domain, &file.to_string_lossy()]);
    let (ok, out) = run_bounded(bootstrap, SHORT_TIMEOUT).await?;
    if !ok {
        return Err(format!("launchctl bootstrap failed: {}", out.trim()));
    }
    let mut enable = Command::new("launchctl");
    enable.args(["enable", &target]);
    let _ = run_bounded(enable, SHORT_TIMEOUT).await;
    Ok(())
}

#[cfg(target_os = "linux")]
async fn activate_service() -> Result<(), String> {
    for args in [
        vec!["--user", "daemon-reload"],
        vec!["--user", "enable", "--now", "mur-model-gateway.service"],
        vec!["--user", "restart", "mur-model-gateway.service"],
    ] {
        let mut c = Command::new("systemctl");
        c.args(&args);
        let (ok, out) = run_bounded(c, SHORT_TIMEOUT).await?;
        if !ok {
            return Err(format!(
                "systemctl {} failed: {}",
                args.join(" "),
                out.trim()
            ));
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn activate_service() -> Result<(), String> {
    Err("gateway service is not supported on this platform".into())
}

/// Install/repair the gateway service with the Codex credential source.
/// Refuses without `consented` — nothing is spawned. Preserves compression
/// if the running service already has it on.
pub async fn gateway_install(gateway: &Path, consented: bool) -> Result<GatewayStatusView, String> {
    if !consented {
        return Err(INSTALL_CONSENT_REQUIRED.into());
    }
    let keep_compress = fetch_health(HEALTH_URL)
        .await
        .is_some_and(|h| h.compression);
    let mut cmd = Command::new(gateway);
    cmd.args(["install", "--token-source-codex", "codex"]);
    if keep_compress {
        cmd.arg("--compress");
    }
    let (ok, out) = run_bounded(cmd, SHORT_TIMEOUT).await?;
    if !ok {
        return Err(format!("mur-model-gateway install failed: {}", out.trim()));
    }
    activate_service().await?;
    let deadline = tokio::time::Instant::now() + INSTALL_POLL;
    loop {
        let view = status_at(HEALTH_URL, true).await;
        if view.running || tokio::time::Instant::now() >= deadline {
            return Ok(view);
        }
        tokio::time::sleep(INSTALL_POLL_STEP).await;
    }
}

// ── Tauri commands ──────────────────────────────────────────────────────

async fn codex_or_err() -> Result<PathBuf, String> {
    super::resolve_codex_async()
        .await
        .ok_or_else(|| super::ControlError::CliMissing.to_string())
}

#[tauri::command]
pub async fn chatgpt_login() -> Result<LoginResult, String> {
    Ok(login(&codex_or_err().await?).await)
}

#[tauri::command]
pub async fn chatgpt_logout(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    logout(&codex_or_err().await?, true).await
}

#[tauri::command]
pub async fn chatgpt_gateway_status() -> Result<GatewayStatusView, String> {
    Ok(gateway_status().await)
}

#[tauri::command]
pub async fn chatgpt_gateway_install(consented: bool) -> Result<GatewayStatusView, String> {
    if !consented {
        return Err(INSTALL_CONSENT_REQUIRED.into());
    }
    let gateway = tokio::task::spawn_blocking(resolve_gateway)
        .await
        .ok()
        .flatten()
        .ok_or("mur-model-gateway is not installed (brew install mur-run/tap/mur-model-gateway)")?;
    gateway_install(&gateway, true).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A fake binary that records each invocation's argv to `marker`.
    fn fake_bin(dir: &tempfile::TempDir, body: &str) -> (PathBuf, PathBuf) {
        let marker = dir.path().join("invoked");
        let bin = dir.path().join("codex");
        let src = format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", marker.display());
        std::fs::File::create(&bin)
            .unwrap()
            .write_all(src.as_bytes())
            .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, marker)
    }

    #[tokio::test]
    async fn nothing_runs_without_the_boolean() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, marker) = fake_bin(&dir, "exit 0");
        assert_eq!(
            logout(&bin, false).await.err().unwrap(),
            LOGOUT_CONFIRMATION_REQUIRED
        );
        assert_eq!(
            gateway_install(&bin, false).await.err().unwrap(),
            INSTALL_CONSENT_REQUIRED
        );
        assert!(
            !marker.exists(),
            "a process was spawned without confirmation"
        );
    }

    #[tokio::test]
    async fn child_output_is_capped_and_stripped() {
        let dir = tempfile::tempdir().unwrap();
        // 100 KiB of 'a' with an ANSI escape up front, on stdout and stderr.
        let (bin, _) = fake_bin(
            &dir,
            "printf '\\033[31mred\\033[0m\\n'; head -c 102400 /dev/zero | tr '\\0' a; head -c 102400 /dev/zero | tr '\\0' b >&2; exit 3",
        );
        let mut cmd = Command::new(&bin);
        cmd.arg("anything");
        let (ok, out) = run_bounded(cmd, SHORT_TIMEOUT).await.unwrap();
        assert!(!ok);
        assert!(out.len() <= OUTPUT_CAP, "{}", out.len());
        assert!(!out.contains('\u{1b}'), "escape leaked");
        assert!(out.starts_with("[31mred[0m\n"), "{}", &out[..20]);
        assert_eq!(sanitize(b"a\x07b\tc\r\nd"), "ab\tc\nd");
    }

    /// `codex login` exiting 0 is not success: the account must say chatgpt.
    #[tokio::test]
    async fn login_believes_the_account_not_the_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, _) = fake_bin(
            &dir,
            concat!(
                "case \"$1\" in\n",
                "  login) exit 0;;\n",
                "  app-server) while IFS= read -r line; do\n",
                "    id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\n",
                "    [ -z \"$id\" ] && continue\n",
                "    case \"$line\" in\n",
                "      *initialize*) printf '{\"id\":%s,\"result\":{}}\\n' \"$id\";;\n",
                "      *) printf '{\"id\":%s,\"result\":{\"account\":{\"type\":\"apiKey\"}}}\\n' \"$id\";;\n",
                "    esac\n  done;;\n",
                "esac"
            ),
        );
        let r = login(&bin).await;
        assert!(!r.authenticated);
        assert!(r.error.unwrap().contains("apiKey"));

        let (bin, _) = fake_bin(&dir, "exit 1");
        let r = login(&bin).await;
        assert!(!r.authenticated);
        assert!(r.error.unwrap().contains("codex login failed"));
    }

    #[test]
    fn health_is_parsed_strictly() {
        let ok = serde_json::json!({"status":"ok","codexHook":true,"codexCredential":"apikey","compression":false});
        assert_eq!(
            parse_health(&ok),
            Some(Health {
                codex_hook: true,
                credential: "apikey".into(),
                compression: false
            })
        );
        for bad in [
            serde_json::json!({"status":"ok"}),
            serde_json::json!({"status":"degraded","codexHook":true,"codexCredential":"chatgpt","compression":true}),
            serde_json::json!({"status":"ok","codexHook":true,"codexCredential":"sk-live-token","compression":true}),
            serde_json::json!({"status":"ok","codexHook":"yes","codexCredential":"chatgpt","compression":true}),
        ] {
            assert_eq!(parse_health(&bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_closed_port_is_installed_but_not_running() {
        let v = status_at("http://127.0.0.1:9/__mur/health", true).await;
        assert!(v.installed);
        assert!(!v.running);
        assert_eq!(v.credential_mode, None);
    }
}
