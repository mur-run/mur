//! `mur browser` command handlers. Heavy implementation lives in `mur-browser`.

use std::{fs, path::PathBuf};
#[cfg(unix)]
use std::{os::unix::fs::FileTypeExt, sync::Arc, time::Duration};

use anyhow::{Result, bail};
#[cfg(test)]
use mur_browser::auth::write_meta;
#[cfg(unix)]
use mur_browser::broker::SocketClient;
#[cfg(unix)]
use mur_browser::broker::{Broker, KeychainStore};
use mur_browser::{
    auth::{Handoff, ProfileMeta, save_profile},
    paths,
    proxy::{BrokerHook, capture_storage_state, playwright_command, run_stdio},
    recorder::{Mode, RecordHook, Run},
    state::KeychainStateKeyStore,
};

/// Start the transparent Playwright MCP proxy for one recording session.
///
/// Slice 1 deliberately validates only CLI-level inputs then forwards every
/// JSON-RPC line unchanged. Recorder, broker, and replay hooks replace the
/// `ForwardHook` in later slices without changing this MCP launch contract.
#[cfg(unix)]
pub async fn record(
    run: &str,
    profile: Option<&str>,
    mode: &str,
    trace: bool,
    extra: &[String],
) -> Result<()> {
    mur_browser::paths::validate_name(run)?;
    if let Some(profile) = profile {
        mur_browser::paths::validate_name(profile)?;
    }
    if !matches!(mode, "test" | "automation") {
        bail!("mode must be test|automation, got {mode:?}");
    }

    let mut args = extra.to_vec();
    // These flags are intentionally merely forwarded. `@playwright/mcp`
    // owns their validation, keeping this proxy compatible with new releases.
    if trace {
        args.push("--save-trace".into());
    }
    let run_state = Run {
        name: run.to_string(),
        mode: mode.parse::<Mode>()?,
        profile: profile.map(ToOwned::to_owned),
        recorded_at: chrono::Utc::now(),
        steps: Vec::new(),
    };
    let mur_home = mur_home()?;
    let socket = paths::broker_socket(&mur_home);
    remove_stale_broker_socket(&socket)?;
    let token = fresh_broker_token();
    let broker = Arc::new(Broker::new(token.clone(), Arc::new(KeychainStore)));
    let socket_for_task = socket.clone();
    let broker_task = tokio::spawn(async move { broker.serve(&socket_for_task).await });

    // Wait until the 0600 socket exists before the proxy can make a secret
    // call. A short bounded wait turns startup races into a clear error.
    if let Err(error) = wait_for_socket(&socket).await {
        broker_task.abort();
        let _ = std::fs::remove_file(&socket);
        return Err(error);
    }

    let hook = BrokerHook::new(
        RecordHook::with_actions_path(run_state, paths::run_actions(&mur_home, run)),
        Arc::new(SocketClient::new(&socket, token)),
    );
    let result = run_stdio(playwright_command(&args), hook).await;
    // The child has ended; kill the broker and unlink its private endpoint
    // even when Playwright exited with an error.
    broker_task.abort();
    let _ = broker_task.await;
    let _ = std::fs::remove_file(&socket);
    result
}

#[cfg(not(unix))]
pub async fn record(
    _run: &str,
    _profile: Option<&str>,
    _mode: &str,
    _trace: bool,
    _extra: &[String],
) -> Result<()> {
    bail!("browser recording requires Unix domain sockets and is unavailable on this platform")
}

fn mur_home() -> Result<std::path::PathBuf> {
    std::env::var_os("MUR_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".mur")))
        .ok_or_else(|| anyhow::anyhow!("cannot determine MUR home (set MUR_HOME)"))
}

fn fresh_broker_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(unix)]
fn remove_stale_broker_socket(socket: &std::path::Path) -> Result<()> {
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(socket).map_err(Into::into)
        }
        Ok(_) => bail!(
            "refusing to replace non-socket broker path: {}",
            socket.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
async fn wait_for_socket(socket: &std::path::Path) -> Result<()> {
    for _ in 0..30 {
        if socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    bail!("secret broker did not create socket: {}", socket.display())
}

/// Serve the private secret-broker socket. The token must arrive through the
/// environment rather than argv, so shells and process listings never expose it.
#[cfg(unix)]
pub async fn broker() -> Result<()> {
    let token = std::env::var(mur_browser::broker::TOKEN_ENV)
        .map_err(|_| anyhow::anyhow!("{} is required", mur_browser::broker::TOKEN_ENV))?;
    if token.len() < 32 {
        bail!(
            "{} must be an unguessable capability token",
            mur_browser::broker::TOKEN_ENV
        );
    }
    let mur_home = mur_home()?;
    let socket = paths::broker_socket(&mur_home);
    remove_stale_broker_socket(&socket)?;
    let result = Arc::new(Broker::new(token, Arc::new(KeychainStore)))
        .serve(&socket)
        .await;
    let _ = std::fs::remove_file(socket);
    result
}

#[cfg(not(unix))]
pub async fn broker() -> Result<()> {
    bail!("browser secret broker requires Unix domain sockets and is unavailable on this platform")
}

/// List recorded runs under `~/.mur/browser/runs/`.
pub fn list() -> Result<()> {
    let runs = runs_dir()?;
    if !runs.exists() {
        return Ok(());
    }
    let mut names = fs::read_dir(&runs)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.file_name()))
        .filter_map(|name| name.into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// Print a recorded run exactly as stored.  Parsing first avoids presenting a
/// malformed file as a valid recording and keeps traversal out of this path.
pub fn show(name: &str) -> Result<()> {
    paths::validate_name(name)?;
    let path = paths::run_actions(&mur_home()?, name);
    let yaml = fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("read browser run {}: {error}", path.display()))?;
    mur_browser::recorder::from_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("invalid browser run {}: {error}", path.display()))?;
    print!("{yaml}");
    Ok(())
}

/// Show the locally persisted recording/profile inventory.  No browser is
/// launched, so this remains safe to call from diagnostics and scripts.
pub fn status() -> Result<()> {
    let home = mur_home()?;
    let mut profiles = profile_statuses(&home)?;
    let mut runs = directory_names(runs_dir()?)?;
    profiles.sort();
    runs.sort();
    println!(
        "profiles: {}",
        if profiles.is_empty() {
            "(none)".into()
        } else {
            profiles.join(", ")
        }
    );
    println!(
        "runs: {}",
        if runs.is_empty() {
            "(none)".into()
        } else {
            runs.join(", ")
        }
    );
    Ok(())
}

fn profile_statuses(home: &std::path::Path) -> Result<Vec<String>> {
    directory_names(paths::browser_root(home).join("profiles"))?
        .into_iter()
        .map(|site| {
            let state = paths::profile_state(home, &site);
            let meta_path = paths::profile_meta(home, &site);
            let label = if !state.is_file() {
                format!("{site} (incomplete)")
            } else if !meta_path.is_file() {
                format!("{site} (metadata missing)")
            } else {
                let yaml = fs::read_to_string(&meta_path).map_err(|error| {
                    anyhow::anyhow!(
                        "read browser profile metadata {}: {error}",
                        meta_path.display()
                    )
                })?;
                let meta: ProfileMeta = serde_yaml::from_str(&yaml).map_err(|error| {
                    anyhow::anyhow!(
                        "invalid browser profile metadata {}: {error}",
                        meta_path.display()
                    )
                })?;
                match meta.earliest_cookie_expires {
                    Some(expiry) => format!("{site} (cookie expires {expiry})"),
                    None => format!("{site} (authenticated {})", meta.last_auth),
                }
            };
            Ok(label)
        })
        .collect()
}

fn runs_dir() -> Result<PathBuf> {
    Ok(paths::browser_root(&mur_home()?).join("runs"))
}

fn directory_names(path: PathBuf) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.file_name()))
        .filter_map(|name| name.into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .collect())
}

#[test]
fn profile_statuses_marks_incomplete_profiles_without_reading_state() {
    let temp = tempfile::tempdir().unwrap();
    let profile = paths::profile_dir(temp.path(), "example");
    fs::create_dir_all(&profile).unwrap();
    assert_eq!(
        profile_statuses(temp.path()).unwrap(),
        vec!["example (incomplete)"]
    );
}

#[test]
fn profile_statuses_reports_cookie_expiry_from_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let profile = paths::profile_dir(temp.path(), "example");
    fs::create_dir_all(&profile).unwrap();
    fs::write(paths::profile_state(temp.path(), "example"), b"encrypted").unwrap();
    write_meta(
        &paths::profile_meta(temp.path(), "example"),
        &ProfileMeta {
            url: "https://example.test/login".into(),
            last_auth: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            earliest_cookie_expires: chrono::DateTime::from_timestamp(1_700_100_000, 0),
        },
    )
    .unwrap();
    assert_eq!(
        profile_statuses(temp.path()).unwrap(),
        vec!["example (cookie expires 2023-11-16 02:00:00 UTC)"]
    );
}

#[test]
fn directory_names_ignore_files_invalid_names_and_missing_directories() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("valid-run")).unwrap();
    fs::create_dir(temp.path().join(".hidden")).unwrap();
    fs::write(temp.path().join("not-a-run"), "x").unwrap();
    assert_eq!(
        directory_names(temp.path().to_path_buf()).unwrap(),
        vec!["valid-run"]
    );
    assert!(
        directory_names(temp.path().join("missing"))
            .unwrap()
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn browser_detection_finds_apps_on_mounted_volumes() {
    let temp = tempfile::tempdir().unwrap();
    let applications = temp.path().join("Applications");
    let firefox = applications.join("Firefox.app/Contents/MacOS/firefox");
    fs::create_dir_all(firefox.parent().unwrap()).unwrap();
    fs::write(&firefox, b"").unwrap();

    let browser = MACOS_BROWSERS
        .iter()
        .copied()
        .find(|browser| browser.engine == BrowserEngine::Firefox)
        .unwrap();
    assert!(browser_is_installed(browser, &[applications]));
}

/// Browser engine that Playwright MCP launches for an authentication handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum BrowserEngine {
    /// Google Chrome.
    Chrome,
    /// Chromium.
    Chromium,
    /// Mozilla Firefox.
    Firefox,
    /// Microsoft Edge.
    Msedge,
}

impl BrowserEngine {
    pub const fn playwright_name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
            Self::Msedge => "msedge",
        }
    }
}

/// A locally installed browser engine that Playwright MCP can launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstalledBrowser {
    engine: BrowserEngine,
    label: &'static str,
    bundle: &'static str,
    executable: &'static str,
}

#[cfg(target_os = "macos")]
const MACOS_BROWSERS: &[InstalledBrowser] = &[
    InstalledBrowser {
        engine: BrowserEngine::Chrome,
        label: "Google Chrome",
        bundle: "Google Chrome.app",
        executable: "Google Chrome",
    },
    InstalledBrowser {
        engine: BrowserEngine::Chromium,
        label: "Chromium",
        bundle: "Chromium.app",
        executable: "Chromium",
    },
    InstalledBrowser {
        engine: BrowserEngine::Firefox,
        label: "Firefox",
        bundle: "Firefox.app",
        executable: "firefox",
    },
    InstalledBrowser {
        engine: BrowserEngine::Msedge,
        label: "Microsoft Edge",
        bundle: "Microsoft Edge.app",
        executable: "Microsoft Edge",
    },
];

#[cfg(target_os = "macos")]
fn macos_application_directories() -> Vec<PathBuf> {
    let mut directories = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join("Applications"));
    }
    if let Ok(volumes) = fs::read_dir("/Volumes") {
        directories.extend(
            volumes
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path().join("Applications")),
        );
    }
    directories
}

#[cfg(target_os = "macos")]
fn browser_is_installed(browser: InstalledBrowser, application_directories: &[PathBuf]) -> bool {
    application_directories.iter().any(|directory| {
        directory
            .join(browser.bundle)
            .join("Contents/MacOS")
            .join(browser.executable)
            .is_file()
    })
}

fn installed_browsers() -> Vec<InstalledBrowser> {
    #[cfg(target_os = "macos")]
    {
        let application_directories = macos_application_directories();
        MACOS_BROWSERS
            .iter()
            .copied()
            .filter(|browser| browser_is_installed(*browser, &application_directories))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

fn select_browser(requested: Option<BrowserEngine>) -> Result<BrowserEngine> {
    if let Some(browser) = requested {
        return Ok(browser);
    }
    let installed = installed_browsers();
    match installed.as_slice() {
        [] => bail!(
            "no supported browser found. Install Chrome, Chromium, Firefox, or Microsoft Edge, then retry with --browser <engine>"
        ),
        [browser] => {
            eprintln!("Using detected browser: {}", browser.label);
            Ok(browser.engine)
        }
        choices => {
            eprintln!("Detected browsers:");
            for (index, browser) in choices.iter().enumerate() {
                eprintln!("  {}. {}", index + 1, browser.label);
            }
            eprint!("Choose a browser [1-{}]: ", choices.len());
            use std::io::Write;
            std::io::stderr().flush()?;
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer)? == 0 {
                bail!("browser selection cancelled: stdin closed");
            }
            let index = answer
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1));
            let browser = index.and_then(|index| choices.get(index)).ok_or_else(|| {
                anyhow::anyhow!("choose a number between 1 and {}", choices.len())
            })?;
            Ok(browser.engine)
        }
    }
}

/// Begin a headed authentication handoff for a browser profile.
///
/// The Playwright MCP server stays private to this command: it opens the login
/// page, the person signs in, then presses Enter. Only the transient storage
/// state crosses the transport boundary before it is encrypted locally.
pub async fn auth(
    site: &str,
    url: &str,
    reauth: bool,
    requested_browser: Option<BrowserEngine>,
) -> Result<()> {
    let browser = select_browser(requested_browser)?;
    paths::validate_name(site)?;
    let parsed =
        url::Url::parse(url).map_err(|error| anyhow::anyhow!("invalid --url {url:?}: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("--url must be an absolute http(s) URL");
    }
    let home = mur_home()?;
    let state = paths::profile_state(&home, site);
    if state.exists() && !reauth {
        bail!("browser profile {site:?} already has encrypted state; use --reauth to replace it");
    }

    let mut handoff = Handoff::new();
    handoff.start()?;
    handoff.handoff()?;
    let storage_state = capture_storage_state(url, browser.playwright_name()).await?;
    handoff.continue_after_login()?;
    // Schema validation in `save_profile` is the minimum safe verification:
    // it proves the capture is Playwright state, without claiming a site-
    // specific authenticated signal that this generic CLI cannot know.
    handoff.verified()?;
    save_profile(
        &mut handoff,
        &state,
        &paths::profile_meta(&home, site),
        &storage_state,
        url,
        chrono::Utc::now(),
        &KeychainStateKeyStore,
    )?;
    println!("saved encrypted browser profile {site:?}");
    Ok(())
}

/// Keep Phase 1's advertised CLI truthful until each later slice lands.
pub fn not_yet(action: &str) -> Result<()> {
    bail!("mur browser {action} is not implemented yet (see browser Phase 1 slices)")
}
