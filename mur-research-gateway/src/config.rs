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
use mur_common::research_provider::{PROVIDER_PREFERENCE, SearchProvider};
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

/// DuckDuckGo's server-rendered (no-JS) HTML search endpoint — the keyless
/// default for tier-1 `search`. Overridable because this host is a single
/// point of failure: networks that blackhole DDG (SYN dropped despite DNS
/// resolving) leave keyless search dead with no signal but a timeout. Point
/// it at a DDG mirror or an egress proxy that can reach it. The response
/// parser still expects DuckDuckGo's HTML shape — this swaps the host, not
/// the search engine. For a genuinely different engine, configure a Brave
/// key (`brave_api_key` / `MUR_RESEARCH_BRAVE_KEY`) instead.
pub const DEFAULT_SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

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
///
/// Every provider's env var is now DERIVED from its slug
/// (`SearchProvider::env_var`), so nothing reads this constant at runtime.
/// It survives as the literal, pre-existing spelling that
/// `brave_env_var_spelling_is_unchanged` pins the derivation against — if the
/// two ever diverge, every shipped `MUR_RESEARCH_BRAVE_KEY` stops being read.
#[cfg(test)]
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
const ENV_SEARCH_ENDPOINT: &str = "MUR_RESEARCH_SEARCH_ENDPOINT";

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
    /// Configured search-provider keys, in [`PROVIDER_PREFERENCE`] order.
    /// Empty → `search` uses the keyless DuckDuckGo tier.
    pub search_keys: Vec<(SearchProvider, String)>,
    /// Tier-1 search endpoint (DuckDuckGo-shaped HTML). See
    /// [`DEFAULT_SEARCH_ENDPOINT`].
    pub search_endpoint: String,
}

impl GatewayConfig {
    /// The configured key for `provider`, if any.
    ///
    /// Runtime search walks `search_keys` in order rather than asking for one
    /// provider, so only the tests call this — but they are what pin the
    /// per-provider precedence (env > `_ref` > plaintext), which is the part
    /// most likely to break silently.
    #[cfg(test)]
    pub fn key_for(&self, provider: SearchProvider) -> Option<&str> {
        self.search_keys
            .iter()
            .find(|(p, _)| *p == provider)
            .map(|(_, k)| k.as_str())
    }
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
    agent_browser_bin: Option<String>,
    lightpanda_path: Option<String>,
    chrome_stealth_args: Option<String>,
    render_engine: Option<String>,
    obscura_path: Option<String>,
    search_endpoint: Option<String>,
}

/// Resolve one provider's key from, in order: its env var, its
/// `<slug>_api_key_ref` SecretRef, its legacy `<slug>_api_key` plaintext.
///
/// Driven by the provider's slug rather than a typed field per provider, so
/// adding a backend to `SearchProvider` needs no change here and the write
/// side (`mur deep-research secret`) cannot spell a key differently from the
/// read side.
fn resolve_provider_key(
    provider: SearchProvider,
    raw: Option<&serde_yaml::Value>,
) -> Option<String> {
    let field = |name: &str| -> Option<String> {
        raw.and_then(|v| v.get(name))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
    };
    non_empty_env(&provider.env_var())
        .or_else(|| resolve_key_ref(provider, field(&provider.config_key_ref()).as_deref()))
        .or_else(|| field(&provider.config_key_plain()))
}

/// Resolve `research_gateway.<slug>_api_key_ref` — a mur-common `SecretRef`
/// string (`keychain:mur/brave`, `env:BRAVE_KEY`, `file:...`, `cmd:...`) —
/// to the actual key, so the secret never has to sit in config.yaml as
/// plaintext. Precedence sits between the env override and the legacy
/// plaintext field. An unparseable or unresolvable ref warns and falls
/// through to plaintext rather than silently disabling the provider.
fn resolve_key_ref(provider: SearchProvider, raw_ref: Option<&str>) -> Option<String> {
    let raw_ref = raw_ref.filter(|s| !s.trim().is_empty())?;
    match raw_ref.parse::<mur_common::secret::SecretRef>() {
        Ok(sref) => {
            let resolved = sref.resolve_to_string_blocking().filter(|s| !s.is_empty());
            if resolved.is_none() {
                tracing::warn!(
                    provider = provider.slug(),
                    r#ref = raw_ref,
                    "{}_api_key_ref did not resolve to a secret; \
                     falling back to plaintext {}_api_key (if any)",
                    provider.slug(),
                    provider.slug()
                );
            }
            resolved
        }
        Err(e) => {
            tracing::warn!(
                provider = provider.slug(),
                r#ref = raw_ref,
                error = %e,
                "{}_api_key_ref is not a valid SecretRef; falling back to plaintext",
                provider.slug()
            );
            None
        }
    }
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
    let raw_value: Option<serde_yaml::Value> = serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()
        .and_then(|v| v.get("research_gateway").cloned());
    let raw: GatewayConfigYaml = raw_value
        .clone()
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

    // Walk providers in preference order so `search` tries Brave first — an
    // existing install with a Brave key must never be silently moved onto a
    // different backend by this change.
    let search_keys: Vec<(SearchProvider, String)> = PROVIDER_PREFERENCE
        .iter()
        .filter_map(|&p| resolve_provider_key(p, raw_value.as_ref()).map(|k| (p, k)))
        .collect();

    let search_endpoint = non_empty_env(ENV_SEARCH_ENDPOINT)
        .or(raw.search_endpoint)
        .unwrap_or_else(|| DEFAULT_SEARCH_ENDPOINT.to_string());

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
        search_keys,
        search_endpoint,
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
mod tests;
