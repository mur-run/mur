// mur-research-gateway/src/browser.rs
//
// Escalation tiers 2 (agent-browser --engine lightpanda) and 3 (--engine
// chrome), plus `search`, plus `preflight`. Drives `agent-browser` as a
// subprocess — see docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md §5.
//
// LOAD-BEARING: the egress proxy only sees tier-1 (reqwest) connections; the
// browser subprocess opens its own connections the proxy cannot observe.
// Therefore `deny_hosts` + the SSRF guard MUST be enforced here, in gateway
// code, BEFORE spawning agent-browser — never delegate that to the proxy.

use crate::fetcher::{self, FetchError, FetchResult};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub struct BrowserCfg {
    pub agent_browser_bin: String,
    pub lightpanda_path: Option<String>,
    pub chrome_stealth_args: String, // comma-separated; empty = none
}

#[derive(Debug, PartialEq)]
pub enum Preflight {
    Full,
    LightpandaMissing,
    AgentBrowserTooOld(String),
    AgentBrowserMissing,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Build the agent-browser argv for a single fetch. Pure → unit-testable.
///
/// lightpanda tier: `--engine lightpanda --executable-path PATH --args ""`
/// (MANDATORY: stealth args must never reach lightpanda — it errors out on
/// them, verified `gotcha_agent_browser_lightpanda_engine_dead`).
/// chrome tier: `--engine chrome` + stealth args.
pub fn build_fetch_argv(url: &str, cfg: &BrowserCfg, want_chrome: bool) -> Vec<String> {
    let mut a = Vec::new();
    if want_chrome || cfg.lightpanda_path.is_none() {
        a.push("--engine".into());
        a.push("chrome".into());
        for s in cfg.chrome_stealth_args.split(',').filter(|s| !s.is_empty()) {
            a.push(s.to_string());
        }
    } else {
        a.push("--engine".into());
        a.push("lightpanda".into());
        a.push("--executable-path".into());
        a.push(cfg.lightpanda_path.clone().unwrap());
        a.push("--args".into());
        a.push(String::new()); // MANDATORY empty — see module doc
    }
    a.push("--session".into());
    a.push(session_id(url)); // per-fetch isolation: no shared cookie jars
    a.push("open".into());
    a.push(url.to_string());
    a.push("snapshot".into());
    a
}

/// Per-FETCH unique session id so even two concurrent fetches of the SAME url
/// get distinct cookie jars (Global Constraint: per-fetch isolation). The
/// URL's FNV-1a hash keeps ids meaningful/greppable; a process-wide atomic
/// counter guarantees uniqueness across calls (NOT random/time — deterministic
/// within a run, collision-free). FNV-1a keeps the hash fast and filesystem-safe.
fn session_id(url: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let seq = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("rg-{h:016x}-{seq:016x}")
}

/// Monotonic per-fetch counter mixed into every `session_id` — see its doc.
static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Production preflight: shells out to `agent-browser --version` and combines
/// the result with `cfg` via `preflight_from_versions`. NOT exercised by unit
/// tests (those call `preflight_from_versions` directly with fixed inputs) —
/// this is the only function in the module that touches a real subprocess.
pub fn preflight(cfg: &BrowserCfg) -> Preflight {
    let output = std::process::Command::new(&cfg.agent_browser_bin)
        .arg("--version")
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            preflight_from_versions(true, parse_version(&stdout).as_deref(), cfg)
        }
        // Ran but reported failure (e.g. unrecognized flag on a very old
        // build) — present but version unknown; treat conservatively as too
        // old rather than silently Full.
        Ok(_) => Preflight::AgentBrowserTooOld("unknown".into()),
        Err(_) => Preflight::AgentBrowserMissing,
    }
}

/// Pull the first whitespace-separated token that looks like a version
/// (`0.31.1`, `v0.31.1`) out of `agent-browser --version` output.
fn parse_version(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|t| {
            let t = t.trim_start_matches('v');
            t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.')
        })
        .map(|t| t.trim_start_matches('v').to_string())
}

pub fn preflight_from_versions(
    ab_present: bool,
    ab_version: Option<&str>,
    cfg: &BrowserCfg,
) -> Preflight {
    if !ab_present {
        return Preflight::AgentBrowserMissing;
    }
    // ab present but version unparseable → treat as too-old, never fall through
    // to Full. Silently proceeding as Full on an unknown version is forbidden
    // (brief: "never silently proceed as Full").
    match ab_version {
        Some(v) if !version_ge(v, 0, 28) => return Preflight::AgentBrowserTooOld(v.into()),
        Some(_) => {}
        None => return Preflight::AgentBrowserTooOld("unknown".into()),
    }
    if cfg.lightpanda_path.is_none() {
        return Preflight::LightpandaMissing;
    }
    Preflight::Full
}

fn version_ge(v: &str, maj: u32, min: u32) -> bool {
    let mut it = v.trim_start_matches('v').split('.');
    let a: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let b: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    a > maj || (a == maj && b >= min)
}

/// Spawn agent-browser with `argv`, bounded by `timeout` (same source tier-1
/// uses), and return its stdout. Enforces the tier-1 body cap on stdout so a
/// runaway render can't buffer unbounded. Shared by `fetch_rendered`/`search`.
async fn run_agent_browser(
    bin: &str,
    argv: &[String],
    timeout: Duration,
) -> Result<String, FetchError> {
    let fut = tokio::process::Command::new(bin).args(argv).output();
    let out = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| FetchError::Http("agent-browser timed out".into()))?
        .map_err(|e| FetchError::Http(format!("spawn agent-browser: {e}")))?;
    if !out.status.success() {
        return Err(FetchError::Http(
            String::from_utf8_lossy(&out.stderr).into(),
        ));
    }
    // TODO(Phase 3): stream stdout to cap before full buffering.
    if out.stdout.len() > fetcher::MAX_BODY_BYTES {
        return Err(FetchError::TooLarge);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Fetch a JS-rendered page. Pre-spawn SSRF/deny screen (proxy can't see the
/// browser's connections — spec §5), then drive agent-browser under `timeout`.
/// `want_chrome` forces tier 3; otherwise tier 2 (lightpanda) is used when
/// available.
pub async fn fetch_rendered(
    url: &str,
    deny: &[String],
    cfg: &BrowserCfg,
    want_chrome: bool,
    timeout: Duration,
) -> Result<FetchResult, FetchError> {
    let screened = fetcher::screen_url_blocking(url, deny)
        .await
        .map_err(FetchError::Guard)?;
    let argv = build_fetch_argv(screened.as_str(), cfg, want_chrome);
    let text = run_agent_browser(&cfg.agent_browser_bin, &argv, timeout).await?;
    let tier = if want_chrome || cfg.lightpanda_path.is_none() {
        3
    } else {
        2
    };
    Ok(FetchResult {
        url: screened.to_string(),
        status: 200,
        title: None,
        text,
        tier,
    })
}

/// Web search. Drives agent-browser to a search-results page and parses hits.
/// v1 rides the browser engine — a dedicated search-provider API is out of
/// scope per spec §11. Same pre-spawn screen and `timeout` as `fetch_rendered`;
/// tier follows `cfg` (lightpanda when available, else chrome) — never
/// caller-selectable, since search has no anti-bot escalation path of its own.
///
/// `deny` is the SAME operator `deny_hosts` overlay `fetch_rendered` screens
/// against — passed here too so `mur deep-research provision --deny-host X`
/// applies uniformly across every gateway tool, not just `fetch`. The fixed
/// search-engine host itself is never caller-supplied, but an operator can
/// still deny it (or any other public host reachable from a redirect/hit)
/// via the same overlay; the always-on private/loopback/link-local rule
/// applies regardless (net_guard).
pub async fn search(
    query: &str,
    limit: usize,
    deny: &[String],
    cfg: &BrowserCfg,
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let mut search_url =
        url::Url::parse("https://html.duckduckgo.com/html/").expect("static URL is valid");
    search_url.query_pairs_mut().append_pair("q", query);

    let screened = fetcher::screen_url_blocking(search_url.as_str(), deny)
        .await
        .map_err(FetchError::Guard)?;

    let argv = build_fetch_argv(screened.as_str(), cfg, false);
    let text = run_agent_browser(&cfg.agent_browser_bin, &argv, timeout).await?;
    Ok(parse_search_hits(&text, limit))
}

/// Small, deliberately minimal parser: agent-browser's `snapshot` renders
/// markdown-style links (`[title](url)`); one hit per line that has one,
/// snippet is the next non-empty, non-link line. Good enough for v1 — the
/// upstream search engine's HTML is not a contract MUR controls (spec §11:
/// a dedicated search API is out of scope), so don't over-invest here.
fn parse_search_hits(text: &str, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if hits.len() >= limit {
            break;
        }
        let Some((title, url)) = parse_markdown_link(line) else {
            continue;
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let snippet = lines
            .peek()
            .filter(|l| !l.trim().is_empty() && parse_markdown_link(l).is_none())
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
    }
    hits
}

/// Extract `(title, url)` from a single `[title](url)` markdown link, if the
/// line contains exactly that shape. Returns `None` for anything else.
fn parse_markdown_link(line: &str) -> Option<(String, String)> {
    let start = line.find('[')?;
    let close = line[start..].find(']')? + start;
    if line[close + 1..].starts_with('(') {
        let open_paren = close + 1;
        let end = line[open_paren..].find(')')? + open_paren;
        let title = line[start + 1..close].trim().to_string();
        let url = line[open_paren + 1..end].trim().to_string();
        if title.is_empty() || url.is_empty() {
            return None;
        }
        return Some((title, url));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[allow(clippy::comparison_to_empty)] // brief's exact assertion form is the contract
    fn lightpanda_command_forces_empty_args() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: "--no-sandbox".into(),
        };
        let argv = build_fetch_argv("https://example.com", &cfg, false);
        // lightpanda tier MUST pass --args "" and --executable-path, and MUST NOT carry stealth args
        assert!(argv.windows(2).any(|w| w[0] == "--args" && w[1] == ""));
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--executable-path" && w[1] == "/x/lightpanda")
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--engine" && w[1] == "lightpanda")
        );
        assert!(!argv.iter().any(|a| a.contains("no-sandbox")));
    }
    #[test]
    fn chrome_tier_carries_stealth_args() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: "--no-sandbox".into(),
        };
        let argv = build_fetch_argv("https://example.com", &cfg, true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--engine" && w[1] == "chrome")
        );
        assert!(argv.iter().any(|a| a == "--no-sandbox"));
    }
    #[test]
    fn preflight_degrades_when_lightpanda_missing() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: None,
            chrome_stealth_args: String::new(),
        };
        // With no lightpanda path, preflight must not claim Full.
        assert!(!matches!(
            preflight_from_versions(true, Some("0.31.1"), &cfg),
            Preflight::Full
        ));
    }
    #[test]
    fn preflight_full_when_all_present() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: String::new(),
        };
        assert!(matches!(
            preflight_from_versions(true, Some("0.31.1"), &cfg),
            Preflight::Full
        ));
    }
    #[test]
    fn preflight_reports_too_old_version() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: String::new(),
        };
        assert!(matches!(
            preflight_from_versions(true, Some("0.27.0"), &cfg),
            Preflight::AgentBrowserTooOld(v) if v == "0.27.0"
        ));
    }
    #[test]
    fn preflight_missing_when_absent() {
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: String::new(),
        };
        assert!(matches!(
            preflight_from_versions(false, None, &cfg),
            Preflight::AgentBrowserMissing
        ));
    }
    #[test]
    fn preflight_unparseable_version_is_not_full() {
        // agent-browser ran (present) but its version output couldn't be
        // parsed → must NOT silently proceed as Full even with lightpanda
        // present; degrade to AgentBrowserTooOld("unknown").
        let cfg = BrowserCfg {
            agent_browser_bin: "agent-browser".into(),
            lightpanda_path: Some("/x/lightpanda".into()),
            chrome_stealth_args: String::new(),
        };
        let pf = preflight_from_versions(true, None, &cfg);
        assert!(!matches!(pf, Preflight::Full));
        assert!(matches!(pf, Preflight::AgentBrowserTooOld(v) if v == "unknown"));
    }
    #[test]
    fn version_ge_handles_v_prefix_and_bounds() {
        assert!(version_ge("v0.28.0", 0, 28));
        assert!(version_ge("0.31.1", 0, 28));
        assert!(version_ge("1.0.0", 0, 28));
        assert!(!version_ge("0.27.9", 0, 28));
    }
    #[test]
    fn session_id_is_unique_per_fetch() {
        // Per-FETCH uniqueness: even two calls with the SAME url must differ
        // (distinct cookie jars for concurrent same-url fetches).
        assert_ne!(
            session_id("https://a.example"),
            session_id("https://a.example")
        );
        assert_ne!(
            session_id("https://a.example"),
            session_id("https://b.example")
        );
        assert!(session_id("https://a.example").starts_with("rg-"));
    }
    #[test]
    fn parses_markdown_search_hits_with_snippet() {
        let text = "\
Intro text\n\
[Rust Programming Language](https://www.rust-lang.org/)\n\
A language empowering everyone.\n\
\n\
[Another Hit](https://example.com/page)\n\
Some snippet text.\n\
[Not a search result](not-a-url)\n";
        let hits = parse_search_hits(text, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust Programming Language");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
        assert_eq!(hits[0].snippet, "A language empowering everyone.");
        assert_eq!(hits[1].title, "Another Hit");
        assert_eq!(hits[1].snippet, "Some snippet text.");
    }
    #[test]
    fn parse_search_hits_respects_limit() {
        let text = "[A](https://a.example)\n[B](https://b.example)\n[C](https://c.example)\n";
        assert_eq!(parse_search_hits(text, 2).len(), 2);
    }
}
