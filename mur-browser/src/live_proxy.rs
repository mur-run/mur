//! Gap 2 of the live-mode design: hand the runtime's egress-proxy credentials
//! to Playwright MCP.
//!
//! Chromium does not read `HTTP_PROXY`, and `--proxy-server` drops userinfo,
//! so the only way to get `Proxy-Authorization` into the CONNECT is the MCP
//! config file (`browser.launchOptions.proxy.{server,username,password}`).
//! The token is the entry token the runtime already exports in
//! `HTTPS_PROXY` (`mcp_client.rs`, `http://<token>:x@127.0.0.1:<port>`); see
//! spec D2 (revised) for why it is per-entry and not per-run.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Environment variable the runtime sets on every proxied MCP child.
pub const PROXY_ENV: &str = "HTTPS_PROXY";

/// Proxy endpoint plus the credential Chromium must present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCredentials {
    /// `http://host:port`, userinfo stripped — what Playwright's `server` takes.
    pub server: String,
    pub username: String,
    pub password: String,
}

/// Parse the runtime's proxy URL. Refuses anything without a username: an
/// unauthenticated launch would be challenged with 407 forever (Gap 1), and
/// falling back to no proxy would violate D1.
pub fn parse_proxy_url(raw: Option<&str>) -> Result<ProxyCredentials> {
    let raw = raw
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("{PROXY_ENV} is not set; live mode needs the egress proxy"))?;
    let url = url::Url::parse(raw).with_context(|| format!("{PROXY_ENV} is not a URL"))?;
    if url.scheme() != "http" {
        bail!("{PROXY_ENV} scheme must be http, got {}", url.scheme());
    }
    let host = url
        .host_str()
        .with_context(|| format!("{PROXY_ENV} has no host"))?;
    let port = url
        .port()
        .with_context(|| format!("{PROXY_ENV} has no port"))?;
    let username = url.username();
    if username.is_empty() {
        bail!("{PROXY_ENV} carries no token; refusing to launch an unproxied browser");
    }
    // The runtime mints the token as uuid hex and the password is a literal
    // `x` (`mcp_client.rs`), so the userinfo never needs percent-decoding.
    Ok(ProxyCredentials {
        server: format!("http://{host}:{port}"),
        username: username.to_owned(),
        password: url.password().unwrap_or("").to_owned(),
    })
}

/// Origin put in `network.blockedOrigins` so Playwright MCP installs a
/// `context.route` handler. Nothing is meant to be blocked: `.invalid` is a
/// reserved TLD (RFC 2606) that never resolves.
///
/// Why a route at all: proxy credentials make Playwright enable CDP
/// `Fetch` interception (`handleAuthRequests`), so every request pauses in
/// `Fetch.requestPaused`. Without a route handler the first navigation
/// request stays paused and `browser_navigate` hangs until its timeout.
/// With any route installed, Playwright continues paused requests.
///
/// The allowlist stays where it is — the egress proxy's `allow_hosts`. An
/// `allowedOrigins` list here would be a second copy to keep in sync, and a
/// host missing from it fails inside the browser (`ERR_BLOCKED_BY_CLIENT`)
/// before the proxy ever logs it.
pub const ROUTE_SENTINEL_ORIGIN: &str = "https://blocked.invalid";

/// The Playwright MCP config document carrying the proxy credentials.
pub fn config_json(creds: &ProxyCredentials) -> serde_json::Value {
    serde_json::json!({
        "browser": {
            "launchOptions": {
                "proxy": {
                    "server": creds.server,
                    "username": creds.username,
                    "password": creds.password,
                }
            }
        },
        "network": {
            "blockedOrigins": [ROUTE_SENTINEL_ORIGIN],
        }
    })
}

/// A config file that is removed when dropped. Playwright reads it during
/// launch; callers drop it once MCP `initialize` returns.
#[derive(Debug)]
pub struct ConfigFile {
    dir: PathBuf,
    path: PathBuf,
}

impl ConfigFile {
    /// Write the config into a fresh 0700 directory under `parent`, file
    /// mode 0600. The token is a secret; do not rely on the umask.
    pub fn write(parent: &Path, creds: &ProxyCredentials) -> Result<Self> {
        let dir = parent.join(format!("mur-browser-live-{}", uuid::Uuid::new_v4()));
        create_private_dir(&dir)?;
        let path = dir.join("playwright-mcp.json");
        let body = serde_json::to_vec_pretty(&config_json(creds))?;
        write_private_file(&path, &body)?;
        Ok(Self { dir, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where `@playwright/mcp` writes its output (console logs, saved
    /// files). Inside the private 0700 directory, so it goes when this does,
    /// and it is the only root `console_inline` will read from.
    pub fn output_dir(&self) -> PathBuf {
        self.dir.join(OUTPUT_SUBDIR)
    }

    /// The `--config`, `--output-dir`, and `--file-paths` arguments for
    /// `@playwright/mcp`. Absolute paths let `console_inline` resolve a
    /// console link without knowing the server's working directory.
    pub fn args(&self) -> Vec<String> {
        vec![
            "--config".to_owned(),
            self.path.to_string_lossy().into_owned(),
            "--output-dir".to_owned(),
            self.output_dir().to_string_lossy().into_owned(),
            "--file-paths".to_owned(),
            FILE_PATHS_ABSOLUTE.to_owned(),
        ]
    }
}

/// Subdirectory of the private config dir that holds server output.
pub const OUTPUT_SUBDIR: &str = "output";
/// `--file-paths` value making every link in a response absolute.
pub const FILE_PATHS_ABSOLUTE: &str = "absolute";

/// `@playwright/mcp` flag choosing how action tools (navigate, click, ...)
/// carry the page snapshot.
pub const SNAPSHOT_MODE_FLAG: &str = "--snapshot-mode";
/// Action tools carry no snapshot; `browser_snapshot` still returns one inline.
///
/// The default (`full`) does not inline it either: it writes
/// `.playwright-mcp/page-*.yml` and answers with only a link. A live agent is
/// sealed, so following that link means the built-in `read_file`, which needs
/// approval a non-interactive caller cannot give (#1685). With `none` the
/// only way to see the page is `browser_snapshot`, whose answer is inline.
pub const SNAPSHOT_MODE_NONE: &str = "none";

/// Launch arguments for `@playwright/mcp` in live mode (Gap 0). Mirrors
/// replay's headless flags so live and replay exercise the same Chromium,
/// plus the proxy config. `--isolated` keeps the profile in memory so the
/// token-bearing launch leaves nothing on disk once the child exits.
pub fn launch_args(config: &ConfigFile) -> Vec<String> {
    let mut args = vec![
        "--headless".to_owned(),
        "--isolated".to_owned(),
        "--browser=chromium".to_owned(),
        SNAPSHOT_MODE_FLAG.to_owned(),
        SNAPSHOT_MODE_NONE.to_owned(),
    ];
    args.extend(crate::chromium::headless_exe_args(
        crate::chromium::system_browsers_dir().as_deref(),
    ));
    // Host-level escape hatch (`--no-sandbox` inside an outer seal, D-note).
    args.extend(crate::chromium::system_extra_args());
    args.extend(config.args());
    args
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn create_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("create private config directory {}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(dir)
            .with_context(|| format!("create private config directory {}", dir.display()))
    }
}

fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("create proxy config {}", path.display()))?;
    f.write_all(body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://0193abcdef:x@127.0.0.1:54421";

    #[test]
    fn parses_runtime_proxy_url_into_server_and_token() {
        let c = parse_proxy_url(Some(URL)).unwrap();
        assert_eq!(c.server, "http://127.0.0.1:54421");
        assert_eq!(c.username, "0193abcdef");
        assert_eq!(c.password, "x");
    }

    #[test]
    fn missing_env_is_refused() {
        let e = parse_proxy_url(None).unwrap_err().to_string();
        assert!(e.contains(PROXY_ENV), "{e}");
    }

    #[test]
    fn url_without_token_is_refused() {
        let e = parse_proxy_url(Some("http://127.0.0.1:54421"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("no token"), "{e}");
    }

    #[test]
    fn config_json_puts_credentials_under_launch_options() {
        let c = parse_proxy_url(Some(URL)).unwrap();
        let v = config_json(&c);
        assert_eq!(
            v["browser"]["launchOptions"]["proxy"]["server"],
            "http://127.0.0.1:54421"
        );
        assert_eq!(
            v["browser"]["launchOptions"]["proxy"]["username"],
            "0193abcdef"
        );
        assert_eq!(v["browser"]["launchOptions"]["proxy"]["password"], "x");
        // The userinfo must never leak into `server`; Playwright would drop it
        // and Chromium would then be challenged forever.
        assert!(
            !v["browser"]["launchOptions"]["proxy"]["server"]
                .as_str()
                .unwrap()
                .contains('@')
        );
    }

    #[test]
    fn config_json_installs_a_route_without_a_browser_allowlist() {
        let c = parse_proxy_url(Some(URL)).unwrap();
        let v = config_json(&c);
        // The sentinel only exists to make Playwright install a route; it
        // must stay unresolvable so it can never block a real site.
        assert_eq!(
            v["network"]["blockedOrigins"],
            serde_json::json!([ROUTE_SENTINEL_ORIGIN])
        );
        assert!(ROUTE_SENTINEL_ORIGIN.ends_with(".invalid"));
        // `allowedOrigins` would duplicate the egress proxy's allowlist.
        assert!(v["network"].get("allowedOrigins").is_none());
    }

    #[test]
    fn config_file_is_0600_in_0700_dir_and_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let c = parse_proxy_url(Some(URL)).unwrap();
        let file = ConfigFile::write(tmp.path(), &c).unwrap();
        let path = file.path().to_path_buf();
        let dir = path.parent().unwrap().to_path_buf();
        assert_eq!(file.args()[0], "--config");
        assert_eq!(file.args()[1], path.to_string_lossy());
        // Console logs land inside the private dir, linked absolutely, so
        // `console_inline` can read them back and they vanish on drop.
        let o = file
            .args()
            .iter()
            .position(|a| a == "--output-dir")
            .unwrap();
        assert_eq!(file.args()[o + 1], file.output_dir().to_string_lossy());
        assert!(file.output_dir().starts_with(&dir));
        let f = file
            .args()
            .iter()
            .position(|a| a == "--file-paths")
            .unwrap();
        assert_eq!(file.args()[f + 1], FILE_PATHS_ABSOLUTE);
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(body, config_json(&c));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(file);
        assert!(!dir.exists(), "config dir must be removed on drop");
    }

    #[test]
    fn launch_args_are_isolated_and_carry_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let c = parse_proxy_url(Some(URL)).unwrap();
        let file = ConfigFile::write(tmp.path(), &c).unwrap();
        let args = launch_args(&file);
        assert!(args.contains(&"--isolated".to_owned()), "{args:?}");
        assert!(args.contains(&"--headless".to_owned()), "{args:?}");
        // #1685: no snapshot may be parked in a file the sealed agent can only
        // reach through an approval-gated `read_file`.
        let m = args
            .iter()
            .position(|a| a == SNAPSHOT_MODE_FLAG)
            .expect("--snapshot-mode present");
        assert_eq!(args[m + 1], SNAPSHOT_MODE_NONE);
        let i = args
            .iter()
            .position(|a| a == "--config")
            .expect("--config present");
        assert_eq!(args[i + 1], file.path().to_string_lossy());
        // The token travels only inside the file, never on the command line.
        assert!(args.iter().all(|a| !a.contains("0193abcdef")), "{args:?}");
    }
}
