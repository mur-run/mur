use super::*;

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
    allow_domain: &[String],
) -> Result<()> {
    let browser = select_browser(requested_browser)?;
    // Before anything is created: the app-bundle scan above cannot tell
    // whether Playwright has the build it will actually launch.
    engine_check::preflight(
        browser.playwright_name(),
        &mur_browser::server::install_dir(&mur_home()?),
        mur_browser::chromium::system_browsers_dir().as_deref(),
    )?;
    paths::validate_name(site)?;
    let parsed =
        url::Url::parse(url).map_err(|error| anyhow::anyhow!("invalid --url {url:?}: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("--url must be an absolute http(s) URL");
    }
    let allow_domains = resolve_allow_domains(&parsed, allow_domain)?;
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
        &allow_domains,
        chrono::Utc::now(),
        &KeychainStateKeyStore,
    )?;
    println!("saved encrypted browser profile {site:?}");
    Ok(())
}

/// Allowlist to store with a profile: the explicit `--allow-domain` values,
/// or the login URL's host when none were given. Entries must be bare hosts
/// (no scheme, port, path, or wildcard) so `guard` matching stays unambiguous.
fn resolve_allow_domains(login: &url::Url, given: &[String]) -> Result<Vec<String>> {
    if given.is_empty() {
        let host = login
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("--url must be an absolute http(s) URL"))?;
        return Ok(vec![host.trim_end_matches('.').to_ascii_lowercase()]);
    }
    let mut out: Vec<String> = Vec::with_capacity(given.len());
    for raw in given {
        let domain = raw.trim().trim_end_matches('.').to_ascii_lowercase();
        let bare = !domain.is_empty()
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            && !domain.starts_with('.')
            && !domain.contains("..");
        if !bare {
            bail!(
                "invalid --allow-domain {raw:?}: use a bare host like example.com \
                 (subdomains are included automatically)"
            );
        }
        if !out.contains(&domain) {
            out.push(domain);
        }
    }
    Ok(out)
}

#[test]
fn allow_domains_default_to_login_host() {
    let login = url::Url::parse("https://Login.Example.com./sso?x=1").unwrap();
    assert_eq!(
        resolve_allow_domains(&login, &[]).unwrap(),
        vec!["login.example.com".to_owned()]
    );
}

#[test]
fn allow_domains_normalise_and_dedupe_explicit_values() {
    let login = url::Url::parse("https://login.example.com/").unwrap();
    let given = vec![
        "Example.com".to_owned(),
        "example.com.".to_owned(),
        "cdn.example.net".to_owned(),
    ];
    assert_eq!(
        resolve_allow_domains(&login, &given).unwrap(),
        vec!["example.com".to_owned(), "cdn.example.net".to_owned()]
    );
}

#[test]
fn allow_domains_reject_non_bare_hosts() {
    let login = url::Url::parse("https://example.com/").unwrap();
    for bad in [
        "https://example.com",
        "example.com/path",
        "example.com:443",
        "*.example.com",
        ".example.com",
        "a..b",
        "",
    ] {
        let err = resolve_allow_domains(&login, &[bad.to_owned()]);
        assert!(err.is_err(), "{bad:?} should be rejected");
    }
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
