use std::path::Path;

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

/// Channel engines only: Playwright launches `chrome` and `msedge` from the
/// installed application, so an app bundle is the right signal for them. It is
/// the wrong one for `chromium` and `firefox`, which launch Playwright's own
/// cached build no matter what sits in `/Applications` (#2097).
#[cfg(target_os = "macos")]
const MACOS_BROWSERS: &[InstalledBrowser] = &[
    InstalledBrowser {
        engine: BrowserEngine::Chrome,
        label: "Google Chrome",
        bundle: "Google Chrome.app",
        executable: "Google Chrome",
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

/// Engines Playwright launches from its own cache, with the label shown when
/// asking. `chromium` first: it is what `mur browser setup` installs and what
/// replay launches.
const OWN_BUILD_ENGINES: &[(BrowserEngine, &str)] = &[
    (BrowserEngine::Chromium, "Chromium (Playwright build)"),
    (BrowserEngine::Firefox, "Firefox (Playwright build)"),
];

/// What `select_browser` should do, decided without touching the terminal.
#[derive(Debug, Eq, PartialEq)]
enum Selection {
    Use(BrowserEngine, &'static str),
    Ask(Vec<(BrowserEngine, &'static str)>),
    Nothing,
}

/// Pick from what the launch can actually start. `own_ready` is
/// [`mur_browser::engines::own_build_ready`] for an own-build engine;
/// `apps` are the channel engines whose application is installed.
///
/// Chromium wins outright unless its build is *known* missing: "cannot tell"
/// (no pinned manifest yet) still means chromium, the engine MUR launches by
/// default, and preflight lets Playwright speak. Other own-build engines count
/// only when their build is known present, so an app bundle alone never makes
/// one a candidate.
fn plan_selection(
    own_ready: impl Fn(BrowserEngine) -> Option<bool>,
    apps: &[InstalledBrowser],
) -> Selection {
    let mut launchable = Vec::new();
    for &(engine, label) in OWN_BUILD_ENGINES {
        match (engine, own_ready(engine)) {
            (BrowserEngine::Chromium, Some(true) | None) => return Selection::Use(engine, label),
            (_, Some(true)) => launchable.push((engine, label)),
            _ => {}
        }
    }
    launchable.extend(apps.iter().map(|app| (app.engine, app.label)));
    match launchable.len() {
        0 => Selection::Nothing,
        1 => Selection::Use(launchable[0].0, launchable[0].1),
        _ => Selection::Ask(launchable),
    }
}

fn select_browser(
    requested: Option<BrowserEngine>,
    install_dir: &Path,
    browsers: Option<&Path>,
) -> Result<BrowserEngine> {
    if let Some(browser) = requested {
        return Ok(browser);
    }
    let own_ready = |engine: BrowserEngine| {
        mur_browser::engines::own_build_ready(install_dir, browsers, engine.playwright_name())
    };
    match plan_selection(own_ready, &installed_browsers()) {
        Selection::Nothing => bail!(
            "no launchable browser found. Run `mur browser setup` to install Playwright's \
             Chromium, or pick one with --browser <engine>"
        ),
        Selection::Use(engine, label) => {
            eprintln!("Using browser: {label}");
            Ok(engine)
        }
        Selection::Ask(choices) => {
            eprintln!("Launchable browsers:");
            for (index, (_, label)) in choices.iter().enumerate() {
                eprintln!("  {}. {label}", index + 1);
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
            let (engine, _) = index.and_then(|index| choices.get(index)).ok_or_else(|| {
                anyhow::anyhow!("choose a number between 1 and {}", choices.len())
            })?;
            Ok(*engine)
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
    bind: Option<&str>,
) -> Result<()> {
    let install_dir = mur_browser::server::install_dir(&mur_home()?);
    let browsers = mur_browser::chromium::system_browsers_dir();
    let browser = select_browser(requested_browser, &install_dir, browsers.as_deref())?;
    // Before anything is created: an explicit --browser, or a chromium whose
    // cache state is unknown, may still lack the build the launch needs.
    engine_check::preflight(browser.playwright_name(), &install_dir, browsers.as_deref())?;
    paths::validate_name(site)?;
    let parsed =
        url::Url::parse(url).map_err(|error| anyhow::anyhow!("invalid --url {url:?}: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("--url must be an absolute http(s) URL");
    }
    let allow_domains = resolve_allow_domains(&parsed, allow_domain)?;
    let home = mur_home()?;
    // Before the login, so a mistyped agent never costs a sign-in.
    let bind = bind
        .map(|agent| super::bind::resolve_agent(&home, agent))
        .transpose()?;
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
    let mut out = std::io::stdout();
    match bind {
        Some(agent) => super::bind::bind_flag(&home, site, &agent, &mut out),
        None => {
            super::bind::after_auth(&home, site, &mut out);
            Ok(())
        }
    }
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
    let chrome = applications.join("Google Chrome.app/Contents/MacOS/Google Chrome");
    fs::create_dir_all(chrome.parent().unwrap()).unwrap();
    fs::write(&chrome, b"").unwrap();

    let browser = MACOS_BROWSERS
        .iter()
        .copied()
        .find(|browser| browser.engine == BrowserEngine::Chrome)
        .unwrap();
    assert!(browser_is_installed(browser, &[applications]));
}

#[cfg(test)]
const CHROME_APP: InstalledBrowser = InstalledBrowser {
    engine: BrowserEngine::Chrome,
    label: "Google Chrome",
    bundle: "Google Chrome.app",
    executable: "Google Chrome",
};

/// #2097: setup installed Playwright Chromium, only Firefox.app is in
/// /Applications. Chromium must be used with no prompt, and Firefox must not
/// even be considered.
#[test]
fn chromium_build_wins_over_firefox_app() {
    let ready = |e: BrowserEngine| Some(e == BrowserEngine::Chromium);
    assert_eq!(
        plan_selection(ready, &[CHROME_APP]),
        Selection::Use(BrowserEngine::Chromium, "Chromium (Playwright build)")
    );
}

/// No pinned manifest yet: cannot tell, so stay on MUR's default engine.
#[test]
fn unknown_cache_state_defaults_to_chromium() {
    assert_eq!(
        plan_selection(|_| None, &[]),
        Selection::Use(BrowserEngine::Chromium, "Chromium (Playwright build)")
    );
}

#[test]
fn nothing_launchable_is_reported_not_guessed() {
    assert_eq!(plan_selection(|_| Some(false), &[]), Selection::Nothing);
}

#[test]
fn several_launchable_engines_prompt() {
    let ready = |e: BrowserEngine| Some(e == BrowserEngine::Firefox);
    assert_eq!(
        plan_selection(ready, &[CHROME_APP]),
        Selection::Ask(vec![
            (BrowserEngine::Firefox, "Firefox (Playwright build)"),
            (BrowserEngine::Chrome, "Google Chrome"),
        ])
    );
}

/// End to end through the real cache check: a chromium build at the pinned
/// revision is picked; with it absent and nothing else, selection refuses.
#[test]
fn select_browser_reads_the_playwright_cache() {
    let install = tempfile::tempdir().unwrap();
    let manifest = install
        .path()
        .join("node_modules/playwright-core/browsers.json");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(
        &manifest,
        r#"{"browsers":[{"name":"chromium","revision":"1200"},{"name":"firefox","revision":"1549"}]}"#,
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let own_ready = |e: BrowserEngine| {
        mur_browser::engines::own_build_ready(
            install.path(),
            Some(cache.path()),
            e.playwright_name(),
        )
    };
    assert_eq!(plan_selection(own_ready, &[]), Selection::Nothing);
    let build = cache.path().join("chromium-1200");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("INSTALLATION_COMPLETE"), b"").unwrap();
    assert_eq!(
        plan_selection(own_ready, &[]),
        Selection::Use(BrowserEngine::Chromium, "Chromium (Playwright build)")
    );
}
