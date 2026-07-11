// mur-research-gateway/src/config.rs
//
// Centralized gateway configuration (Task 6). Loaded ONCE at startup
// (`server::McpServer::new`) from `~/.mur/config.yaml`'s `research_gateway:`
// block; env vars override whatever the YAML (or its defaults) produced.
//
// YAML-reading pattern mirrors `mur_common::config::Config`'s per-section
// parsing (see `ConversationsConfig` / the `missing_conversations_section_is_fine`
// test in mur-common/src/config.rs): parse the whole file as a generic
// `serde_yaml::Value`, pull out the `research_gateway` key, deserialize it
// into a `#[serde(default)]` struct so a missing file or missing block
// resolves to all-defaults rather than an error. This crate deliberately
// does NOT depend on mur-core's `store::config::load_config` (which returns
// the full, mur-core-only `Config` type and always writes a default file to
// disk) nor on mur-common's full `Config` struct (wrong layering for a
// standalone gateway binary) — a local, narrowly-scoped struct is the
// better fit here, per the same "read only what you need" pattern
// `ConversationsConfig` demonstrates for a single section.

use crate::browser::BrowserCfg;
use mur_common::agent::ENV_MCP_DENY_HOSTS as ENV_DENY_HOSTS;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---- documented defaults (CLAUDE.md rule 1: no hardcoded values) ----

/// Tier-1 (plain `reqwest` GET) fetch timeout, in seconds.
pub const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 20;

/// Tier-2/3 (agent-browser: lightpanda / chrome render + search) timeout, in
/// seconds. Deliberately its OWN budget, longer than the tier-1 GET timeout —
/// spinning up a real browser engine and rendering JS legitimately takes
/// longer than a plain HTTP GET.
pub const DEFAULT_BROWSER_TIMEOUT_SECS: u64 = 60;

/// Default number of search hits returned when the caller doesn't specify
/// `limit`.
pub const DEFAULT_SEARCH_LIMIT: usize = 8;

/// Default cap on the CHARACTERS of `fetch` page text returned to the worker.
/// A full page can otherwise overflow the model's context (deep-research turns
/// died with anthropic 400 "prompt is too long"). ~12–15k tokens/fetch; ~10
/// fetches fit a 200k window with reasoning room. `0` disables the cap.
pub const DEFAULT_MAX_FETCH_CHARS: usize = 50_000;

/// Hard floor/ceiling `search`'s effective `limit` (caller-supplied or
/// default) is clamped to.
pub const MIN_SEARCH_LIMIT: usize = 1;
pub const MAX_SEARCH_LIMIT: usize = 20;

/// Default `agent-browser` binary name, resolved via `PATH`.
pub const DEFAULT_AGENT_BROWSER_BIN: &str = "agent-browser";

/// Chrome-tier stealth flags (comma-separated, forwarded verbatim as
/// `agent-browser` args). MUST NEVER be forwarded to the lightpanda tier —
/// see `browser::build_fetch_argv`'s doc comment /
/// `gotcha_agent_browser_lightpanda_engine_dead`.
pub const DEFAULT_CHROME_STEALTH_ARGS: &str =
    "--no-sandbox,--disable-blink-features=AutomationControlled";

/// Default installed Lightpanda path, relative to `mur_home`. Verified
/// present on a real install 2026-07-08 — see
/// `gotcha_agent_browser_lightpanda_engine_dead`. Only ever used when it
/// actually exists on disk (`default_lightpanda_path` below) — never claim a
/// path that isn't there.
pub const DEFAULT_LIGHTPANDA_RELATIVE_PATH: &str = "aura/lightpanda";

/// Default obscura install path, relative to `mur_home` (mirrors Lightpanda's
/// `aura/` install location).
pub const DEFAULT_OBSCURA_RELATIVE_PATH: &str = "aura/obscura";

/// obscura's sibling worker binary, relative to `mur_home`. Both it and
/// `DEFAULT_OBSCURA_RELATIVE_PATH` must exist for auto-detect to pick obscura.
pub const DEFAULT_OBSCURA_WORKER_RELATIVE_PATH: &str = "aura/obscura-worker";

// ---- env var names ----

// ENV_DENY_HOSTS (imported above as `ENV_MCP_DENY_HOSTS`) is shared with
// `mur-agent-runtime`'s `proxy_env_for`, which sets it on this gateway's own
// child env when the operator grants a `--deny-host` overlay — single
// definition in mur-common so the two crates can never drift (CLAUDE.md
// rule 1).

/// Brave Search API subscription token. When present (env or YAML), `search`
/// uses Brave's first-class web-search API instead of scraping DuckDuckGo's
/// HTML endpoint; absent, it falls back to DDG (zero-config, keyless). Brave's
/// free tier (2k queries/mo) covers a personal deep-research user at $0 — the
/// key is a reliability upgrade, never a hard requirement.
const ENV_BRAVE_KEY: &str = "MUR_RESEARCH_BRAVE_KEY";
const ENV_FETCH_TIMEOUT_SECS: &str = "MUR_RESEARCH_TIMEOUT_SECS";
const ENV_BROWSER_TIMEOUT_SECS: &str = "MUR_RESEARCH_BROWSER_TIMEOUT_SECS";
const ENV_SEARCH_LIMIT: &str = "MUR_RESEARCH_SEARCH_LIMIT";
const ENV_MAX_FETCH_CHARS: &str = "MUR_RESEARCH_MAX_FETCH_CHARS";
const ENV_AGENT_BROWSER_BIN: &str = "MUR_RESEARCH_AGENT_BROWSER_BIN";
const ENV_LIGHTPANDA_PATH: &str = "MUR_RESEARCH_LIGHTPANDA_PATH";
const ENV_CHROME_STEALTH_ARGS: &str = "MUR_RESEARCH_CHROME_STEALTH_ARGS";
const ENV_RENDER_ENGINE: &str = "MUR_RESEARCH_RENDER_ENGINE";
const ENV_OBSCURA_PATH: &str = "MUR_RESEARCH_OBSCURA_PATH";

/// Fully-resolved gateway configuration — YAML defaults merged with env
/// overrides, ready to use for the lifetime of the process.
pub struct GatewayConfig {
    pub deny_hosts: Vec<String>,
    /// Tier-1 (plain GET) fetch timeout.
    pub timeout: Duration,
    /// Tier-2/3 (browser-rendered fetch + search) timeout — its own budget,
    /// see `DEFAULT_BROWSER_TIMEOUT_SECS`.
    pub browser_timeout: Duration,
    pub browser: BrowserCfg,
    pub search_limit: usize,
    /// Max characters of `fetch` page text returned to the worker; `0` = no cap.
    pub max_fetch_chars: usize,
    /// Brave Search API token; `Some` → `search` uses Brave, `None` → DDG.
    pub brave_api_key: Option<String>,
}

/// Raw `research_gateway:` YAML shape. Every field is optional/defaulted so
/// a config.yaml with no `research_gateway:` block, or one that only sets a
/// subset of fields, still parses cleanly (mirrors every sub-config in
/// mur-common/src/config.rs).
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct GatewayConfigYaml {
    deny_hosts: Vec<String>,
    timeout_secs: Option<u64>,
    browser_timeout_secs: Option<u64>,
    search_limit: Option<usize>,
    max_fetch_chars: Option<usize>,
    brave_api_key: Option<String>,
    agent_browser_bin: Option<String>,
    lightpanda_path: Option<String>,
    chrome_stealth_args: Option<String>,
    render_engine: Option<String>,
    obscura_path: Option<String>,
}

/// Resolve `~/.mur` (or `$MUR_HOME` if set). Mirrors the `MUR_HOME`
/// precedence in `mur-core/src/store/config.rs::config_path`, but reads
/// `$HOME` directly instead of pulling the `dirs` crate — the same tradeoff
/// `mur_common::config::default_mur_dir` makes, since this gateway binary is
/// deliberately dependency-light (no mur-core, no `dirs`).
pub fn mur_home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("MUR_HOME")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".mur")
}

/// Load the gateway config from `<mur_home>/config.yaml`, falling back to
/// defaults if the file is missing or unparseable, then apply env overrides.
/// Intended to be called exactly once, at startup (`McpServer::new`).
pub fn load(mur_home: &Path) -> GatewayConfig {
    let path = mur_home.join("config.yaml");
    let yaml = std::fs::read_to_string(&path).unwrap_or_default();
    load_from_yaml(&yaml, mur_home)
}

/// Testable core of `load`: parse `yaml` (may be empty, or simply missing
/// the `research_gateway:` key — both resolve to all-defaults), then apply
/// env overrides. `mur_home` is used only to resolve the default Lightpanda
/// path.
pub fn load_from_yaml(yaml: &str, mur_home: &Path) -> GatewayConfig {
    let raw: GatewayConfigYaml = serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()
        .and_then(|v| v.get("research_gateway").cloned())
        .and_then(|v| serde_yaml::from_value(v).ok())
        .unwrap_or_default();

    let deny_hosts = env_deny_hosts().unwrap_or(raw.deny_hosts);

    let timeout_secs = env_u64(ENV_FETCH_TIMEOUT_SECS)
        .or(raw.timeout_secs)
        .unwrap_or(DEFAULT_FETCH_TIMEOUT_SECS);

    let browser_timeout_secs = env_u64(ENV_BROWSER_TIMEOUT_SECS)
        .or(raw.browser_timeout_secs)
        .unwrap_or(DEFAULT_BROWSER_TIMEOUT_SECS);

    let search_limit = env_usize(ENV_SEARCH_LIMIT)
        .or(raw.search_limit)
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(MIN_SEARCH_LIMIT, MAX_SEARCH_LIMIT);

    let max_fetch_chars = env_usize(ENV_MAX_FETCH_CHARS)
        .or(raw.max_fetch_chars)
        .unwrap_or(DEFAULT_MAX_FETCH_CHARS);

    let brave_api_key =
        non_empty_env(ENV_BRAVE_KEY).or_else(|| raw.brave_api_key.filter(|s| !s.is_empty()));

    let agent_browser_bin = non_empty_env(ENV_AGENT_BROWSER_BIN)
        .or(raw.agent_browser_bin)
        .unwrap_or_else(|| DEFAULT_AGENT_BROWSER_BIN.to_string());

    let lightpanda_path = non_empty_env(ENV_LIGHTPANDA_PATH)
        .or_else(|| raw.lightpanda_path.filter(|s| !s.is_empty()))
        .or_else(|| default_lightpanda_path(mur_home));

    let chrome_stealth_args = non_empty_env(ENV_CHROME_STEALTH_ARGS)
        .or(raw.chrome_stealth_args)
        .unwrap_or_else(|| DEFAULT_CHROME_STEALTH_ARGS.to_string());

    let render_engine = non_empty_env(ENV_RENDER_ENGINE)
        .or(raw.render_engine)
        .map(|s| match s.trim().to_ascii_lowercase().as_str() {
            "obscura" => crate::browser::RenderEngine::Obscura,
            "agent-browser" => crate::browser::RenderEngine::AgentBrowser,
            "lightpanda" => crate::browser::RenderEngine::Lightpanda,
            unrecognized => {
                tracing::warn!(
                    "unrecognized render_engine '{}'; falling back to agent-browser",
                    unrecognized
                );
                crate::browser::RenderEngine::AgentBrowser
            }
        })
        .unwrap_or_else(|| auto_detect_render_engine(mur_home));

    let obscura_path = non_empty_env(ENV_OBSCURA_PATH)
        .or_else(|| raw.obscura_path.filter(|s| !s.is_empty()))
        .or_else(|| default_obscura_path(mur_home));

    GatewayConfig {
        deny_hosts,
        timeout: Duration::from_secs(timeout_secs),
        browser_timeout: Duration::from_secs(browser_timeout_secs),
        browser: BrowserCfg {
            agent_browser_bin,
            lightpanda_path,
            chrome_stealth_args,
            render_engine,
            obscura_path,
        },
        search_limit,
        max_fetch_chars,
        brave_api_key,
    }
}

/// Only used when neither env nor `research_gateway.lightpanda_path` supply
/// a path AND the default path actually exists on disk — never claim a path
/// that isn't there (matches the pre-Task-6 behavior in `server.rs`).
fn default_lightpanda_path(mur_home: &Path) -> Option<String> {
    let path = mur_home.join(DEFAULT_LIGHTPANDA_RELATIVE_PATH);
    path.exists().then(|| path.to_string_lossy().to_string())
}

/// Default obscura path — only when it actually exists on disk (never claim a
/// path that isn't there), matching `default_lightpanda_path`.
fn default_obscura_path(mur_home: &Path) -> Option<String> {
    let path = mur_home.join(DEFAULT_OBSCURA_RELATIVE_PATH);
    path.exists().then(|| path.to_string_lossy().to_string())
}

/// Auto-detect the render engine when nothing is explicitly configured:
/// Prefer native lightpanda when present (fastest, usually pre-installed,
/// egress-governed), else obscura IF both its binaries are installed at the
/// default aura paths (real content + sandbox), else agent-browser. Never
/// picks engines it can't run. Explicit env/YAML still override.
fn auto_detect_render_engine(mur_home: &Path) -> crate::browser::RenderEngine {
    // 1. Prefer NATIVE lightpanda — fastest, usually already installed at
    //    aura/lightpanda, and egress-governed (head-to-head 2026-07-11).
    if mur_home.join(DEFAULT_LIGHTPANDA_RELATIVE_PATH).exists() {
        return crate::browser::RenderEngine::Lightpanda;
    }
    // 2. else obscura when both its binaries are present.
    let obscura = mur_home.join(DEFAULT_OBSCURA_RELATIVE_PATH);
    let worker = mur_home.join(DEFAULT_OBSCURA_WORKER_RELATIVE_PATH);
    if obscura.exists() && worker.exists() {
        return crate::browser::RenderEngine::Obscura;
    }
    // 3. else the agent-browser wrapper.
    crate::browser::RenderEngine::AgentBrowser
}

fn env_deny_hosts() -> Option<Vec<String>> {
    // Treat an empty/whitespace-only value as ABSENT (fall through to YAML /
    // default), same as every other env field via `non_empty_env` — otherwise
    // `export MUR_RESEARCH_DENY_HOSTS=` would silently wipe the YAML-configured
    // SSRF blocklist overlay. A non-empty value still overrides.
    non_empty_env(ENV_DENY_HOSTS).map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var`/`remove_var` are process-global; cargo runs tests in
    // this file in parallel threads by default, so every test that mutates an
    // env var serializes on this lock — mirrors
    // `mur-agent-runtime/tests/model_resolution.rs::HOME_LOCK`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // Brief's exact Step-1 failing test.
    #[test]
    fn config_defaults_and_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_FETCH_TIMEOUT_SECS, "45");
        }
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert_eq!(c.timeout.as_secs(), 45); // env override
        assert!(c.search_limit >= 1); // documented default present
        unsafe {
            std::env::remove_var(ENV_FETCH_TIMEOUT_SECS);
        }
    }

    #[test]
    fn defaults_when_file_and_block_absent() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert_eq!(c.timeout.as_secs(), DEFAULT_FETCH_TIMEOUT_SECS);
        assert_eq!(c.browser_timeout.as_secs(), DEFAULT_BROWSER_TIMEOUT_SECS);
        assert_eq!(c.search_limit, DEFAULT_SEARCH_LIMIT);
        assert!(c.deny_hosts.is_empty());
        assert_eq!(c.browser.agent_browser_bin, DEFAULT_AGENT_BROWSER_BIN);
        assert_eq!(c.browser.chrome_stealth_args, DEFAULT_CHROME_STEALTH_ARGS);
        assert_eq!(c.browser.lightpanda_path, None);
    }

    #[test]
    fn defaults_when_yaml_has_no_research_gateway_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let c = load_from_yaml("some_other_key:\n  foo: bar\n", Path::new("/nonexistent"));
        assert_eq!(c.search_limit, DEFAULT_SEARCH_LIMIT);
    }

    #[test]
    fn yaml_block_overrides_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let yaml = "\
research_gateway:
  search_limit: 15
  deny_hosts: [\"example.internal\", \"metadata.internal\"]
  agent_browser_bin: \"custom-browser\"
  chrome_stealth_args: \"--flag-a,--flag-b\"
";
        let c = load_from_yaml(yaml, Path::new("/nonexistent"));
        assert_eq!(c.search_limit, 15);
        assert_eq!(c.deny_hosts, vec!["example.internal", "metadata.internal"]);
        assert_eq!(c.browser.agent_browser_bin, "custom-browser");
        assert_eq!(c.browser.chrome_stealth_args, "--flag-a,--flag-b");
    }

    #[test]
    fn search_limit_is_clamped_to_documented_bounds() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let yaml = "research_gateway:\n  search_limit: 999\n";
        let c = load_from_yaml(yaml, Path::new("/nonexistent"));
        assert_eq!(c.search_limit, MAX_SEARCH_LIMIT);
    }

    #[test]
    fn browser_timeout_env_override_is_independent_of_fetch_timeout() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_BROWSER_TIMEOUT_SECS, "90");
        }
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert_eq!(c.browser_timeout.as_secs(), 90);
        assert_eq!(c.timeout.as_secs(), DEFAULT_FETCH_TIMEOUT_SECS); // unaffected
        unsafe {
            std::env::remove_var(ENV_BROWSER_TIMEOUT_SECS);
        }
    }

    #[test]
    fn deny_hosts_env_override_wins_over_yaml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_DENY_HOSTS, "a.example, b.example");
        }
        let c = load_from_yaml(
            "research_gateway:\n  deny_hosts: [\"c.example\"]\n",
            Path::new("/nonexistent"),
        );
        assert_eq!(c.deny_hosts, vec!["a.example", "b.example"]);
        unsafe {
            std::env::remove_var(ENV_DENY_HOSTS);
        }
    }

    #[test]
    fn empty_deny_hosts_env_does_not_wipe_yaml_blocklist() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // An empty MUR_RESEARCH_DENY_HOSTS must be treated as ABSENT, not as
        // "clear the blocklist" — otherwise it would silently wipe the
        // YAML-configured SSRF overlay (security-relevant).
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_DENY_HOSTS, "");
        }
        let c = load_from_yaml(
            "research_gateway:\n  deny_hosts: [\"blocked.example\"]\n",
            Path::new("/nonexistent"),
        );
        assert_eq!(c.deny_hosts, vec!["blocked.example"]);
        unsafe {
            std::env::remove_var(ENV_DENY_HOSTS);
        }
    }

    #[test]
    fn lightpanda_path_env_override_wins_and_need_not_exist() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_LIGHTPANDA_PATH, "/x/lightpanda");
        }
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert_eq!(c.browser.lightpanda_path.as_deref(), Some("/x/lightpanda"));
        unsafe {
            std::env::remove_var(ENV_LIGHTPANDA_PATH);
        }
    }

    #[test]
    fn lightpanda_default_path_absent_when_not_on_disk() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert_eq!(c.browser.lightpanda_path, None);
    }

    #[test]
    fn mur_home_dir_honors_mur_home_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var("MUR_HOME", "/tmp/mur-research-gateway-test-home");
        }
        assert_eq!(
            mur_home_dir(),
            PathBuf::from("/tmp/mur-research-gateway-test-home")
        );
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }

    #[test]
    fn max_fetch_chars_default_env_yaml_precedence() {
        let _g = ENV_LOCK.lock().unwrap();
        // Default when nothing set.
        unsafe {
            std::env::remove_var("MUR_RESEARCH_MAX_FETCH_CHARS");
        }
        let cfg = load_from_yaml("", std::path::Path::new("/tmp"));
        assert_eq!(cfg.max_fetch_chars, DEFAULT_MAX_FETCH_CHARS);
        // YAML sets it.
        let cfg = load_from_yaml(
            "research_gateway:\n  max_fetch_chars: 1234\n",
            std::path::Path::new("/tmp"),
        );
        assert_eq!(cfg.max_fetch_chars, 1234);
        // Env overrides YAML.
        unsafe {
            std::env::set_var("MUR_RESEARCH_MAX_FETCH_CHARS", "42");
        }
        let cfg = load_from_yaml(
            "research_gateway:\n  max_fetch_chars: 1234\n",
            std::path::Path::new("/tmp"),
        );
        assert_eq!(cfg.max_fetch_chars, 42);
        unsafe {
            std::env::remove_var("MUR_RESEARCH_MAX_FETCH_CHARS");
        }
    }

    #[test]
    fn render_engine_defaults_agentbrowser_env_overrides_obscura() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert!(matches!(
            c.browser.render_engine,
            crate::browser::RenderEngine::AgentBrowser
        ));
        // SAFETY: env mutation guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(ENV_RENDER_ENGINE, "obscura");
        }
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert!(matches!(
            c.browser.render_engine,
            crate::browser::RenderEngine::Obscura
        ));
        unsafe {
            std::env::remove_var(ENV_RENDER_ENGINE);
        }
    }

    #[test]
    fn render_engine_env_lightpanda_resolves() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: env mutation guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(ENV_RENDER_ENGINE, "lightpanda");
        }
        let c = load_from_yaml("", Path::new("/nonexistent"));
        assert!(matches!(
            c.browser.render_engine,
            crate::browser::RenderEngine::Lightpanda
        ));
        unsafe {
            std::env::remove_var(ENV_RENDER_ENGINE);
        }
    }

    /// Creates a fresh scratch dir under the OS temp dir for a single test,
    /// suffixed with `label` for readability + isolation across parallel runs.
    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mur_research_gateway_test_{}_{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn auto_detect_prefers_lightpanda_when_present() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mur_home = scratch_dir("lightpanda_only");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("lightpanda"), b"").expect("write lightpanda stub");

        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::Lightpanda
        );

        let _ = std::fs::remove_dir_all(&mur_home);
    }

    #[test]
    fn auto_detect_lightpanda_wins_over_obscura() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mur_home = scratch_dir("lightpanda_vs_obscura");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("lightpanda"), b"").expect("write lightpanda stub");
        std::fs::write(aura.join("obscura"), b"").expect("write obscura stub");
        std::fs::write(aura.join("obscura-worker"), b"").expect("write obscura-worker stub");

        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::Lightpanda
        );

        let _ = std::fs::remove_dir_all(&mur_home);
    }

    #[test]
    fn auto_detect_picks_obscura_when_binaries_present() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mur_home = scratch_dir("obscura_both");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("obscura"), b"").expect("write obscura stub");
        std::fs::write(aura.join("obscura-worker"), b"").expect("write obscura-worker stub");

        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::Obscura
        );

        let _ = std::fs::remove_dir_all(&mur_home);
    }

    #[test]
    fn auto_detect_requires_both_obscura_binaries() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // Only the main binary present -> AgentBrowser.
        let mur_home = scratch_dir("obscura_main_only");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("obscura"), b"").expect("write obscura stub");
        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::AgentBrowser
        );
        let _ = std::fs::remove_dir_all(&mur_home);

        // Only the worker present -> AgentBrowser.
        let mur_home = scratch_dir("obscura_worker_only");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("obscura-worker"), b"").expect("write obscura-worker stub");
        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::AgentBrowser
        );
        let _ = std::fs::remove_dir_all(&mur_home);
    }

    #[test]
    fn render_engine_env_override_wins_even_when_obscura_installed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mur_home = scratch_dir("obscura_env_override");
        let aura = mur_home.join("aura");
        std::fs::create_dir_all(&aura).expect("create aura dir");
        std::fs::write(aura.join("obscura"), b"").expect("write obscura stub");
        std::fs::write(aura.join("obscura-worker"), b"").expect("write obscura-worker stub");

        // SAFETY: env mutation guarded by ENV_LOCK above.
        unsafe {
            std::env::set_var(ENV_RENDER_ENGINE, "agent-browser");
        }
        let c = load_from_yaml("", &mur_home);
        assert_eq!(
            c.browser.render_engine,
            crate::browser::RenderEngine::AgentBrowser
        );
        unsafe {
            std::env::remove_var(ENV_RENDER_ENGINE);
        }

        let _ = std::fs::remove_dir_all(&mur_home);
    }
}
