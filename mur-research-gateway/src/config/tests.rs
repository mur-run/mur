use super::*;

/// Every provider's env var, config key and keychain ref is derived from
/// its slug now. If that derivation ever stops reproducing the literal
/// `MUR_RESEARCH_BRAVE_KEY` that shipped installs already export, their
/// Brave key silently stops being read — so pin the two together.
#[test]
fn brave_env_var_spelling_is_unchanged() {
    assert_eq!(SearchProvider::Brave.env_var(), ENV_BRAVE_KEY);
}

#[test]
fn brave_key_ref_resolves_and_beats_plaintext() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var(ENV_BRAVE_KEY);
    envg.set_var("TEST_BRAVE_REF_KEY", "from-ref");
    let yaml = "research_gateway:\n  brave_api_key: \"plain\"\n  brave_api_key_ref: \"env:TEST_BRAVE_REF_KEY\"\n";
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.key_for(SearchProvider::Brave), Some("from-ref"));

    // Unresolvable ref falls through to plaintext instead of disabling Brave.
    envg.unset_var("TEST_BRAVE_REF_KEY");
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.key_for(SearchProvider::Brave), Some("plain"));

    // Invalid ref string also falls through.
    let yaml_bad =
        "research_gateway:\n  brave_api_key: \"plain\"\n  brave_api_key_ref: \"bogus\"\n";
    let c = load_from_yaml(yaml_bad, Path::new("/nonexistent"));
    assert_eq!(c.key_for(SearchProvider::Brave), Some("plain"));
}

/// The three new providers must read their keys through exactly the same
/// three-step precedence Brave already had — env, then `_ref`, then
/// plaintext — without a typed YAML field per provider.
#[test]
fn every_provider_resolves_ref_and_plaintext() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    for p in SearchProvider::all() {
        envg.unset_var(p.env_var());
    }
    envg.set_var("TEST_TAVILY_REF", "tavily-from-ref");
    let yaml = "\
research_gateway:
  tavily_api_key_ref: \"env:TEST_TAVILY_REF\"
  serpapi_api_key: \"serp-plain\"
  firecrawl_api_key: \"fire-plain\"
";
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.key_for(SearchProvider::Tavily), Some("tavily-from-ref"));
    assert_eq!(c.key_for(SearchProvider::SerpApi), Some("serp-plain"));
    assert_eq!(c.key_for(SearchProvider::Firecrawl), Some("fire-plain"));
    // Unconfigured provider stays absent rather than resolving to "".
    assert_eq!(c.key_for(SearchProvider::Brave), None);
    envg.unset_var("TEST_TAVILY_REF");
}

/// Env beats both YAML forms, for a provider that never had an env var
/// before this change.
#[test]
fn provider_env_var_beats_yaml() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_RESEARCH_TAVILY_KEY", "from-env");
    let c = load_from_yaml(
        "research_gateway:\n  tavily_api_key: \"plain\"\n",
        Path::new("/nonexistent"),
    );
    assert_eq!(c.key_for(SearchProvider::Tavily), Some("from-env"));
    envg.unset_var("MUR_RESEARCH_TAVILY_KEY");
}

/// Brave must stay FIRST in the list `search` walks. An install that
/// already has a Brave key must not be silently moved onto a newly added
/// backend just because three more providers now exist.
#[test]
fn brave_is_tried_before_newer_providers() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    for p in SearchProvider::all() {
        envg.unset_var(p.env_var());
    }
    let yaml = "\
research_gateway:
  firecrawl_api_key: \"fire\"
  brave_api_key: \"brave\"
  tavily_api_key: \"tav\"
";
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    let order: Vec<_> = c.search_keys.iter().map(|(p, _)| *p).collect();
    assert_eq!(order.first(), Some(&SearchProvider::Brave), "{order:?}");
    assert_eq!(order.len(), 3);
}

/// No keys configured at all stays the zero-config default: an empty list,
/// which `search` reads as "go straight to keyless DuckDuckGo".
#[test]
fn no_configured_keys_means_keyless_search() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    for p in SearchProvider::all() {
        envg.unset_var(p.env_var());
    }
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(c.search_keys.is_empty());
}

// Brief's exact Step-1 failing test.
#[test]
fn config_defaults_and_env_override() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(ENV_FETCH_TIMEOUT_SECS, "45");
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert_eq!(c.timeout.as_secs(), 45); // env override
    assert!(c.search_limit >= 1); // documented default present
    envg.unset_var(ENV_FETCH_TIMEOUT_SECS);
}

#[test]
fn search_endpoint_precedence_default_then_yaml_then_env() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var(ENV_SEARCH_ENDPOINT);
    let yaml = "research_gateway:\n  search_endpoint: \"https://ddg.mirror.test/html/\"\n";

    // 1. neither set -> the documented default
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert_eq!(c.search_endpoint, DEFAULT_SEARCH_ENDPOINT);

    // 2. YAML set -> YAML wins over the default
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.search_endpoint, "https://ddg.mirror.test/html/");

    // 3. env set -> env wins over YAML (same precedence as every sibling)
    envg.set_var(ENV_SEARCH_ENDPOINT, "https://from-env.test/html/");
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.search_endpoint, "https://from-env.test/html/");
    envg.unset_var(ENV_SEARCH_ENDPOINT);
}

#[test]
fn defaults_when_file_and_block_absent() {
    let _envg = mur_common::test_env::EnvGuard::hold();
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
    let _envg = mur_common::test_env::EnvGuard::hold();
    let c = load_from_yaml("some_other_key:\n  foo: bar\n", Path::new("/nonexistent"));
    assert_eq!(c.search_limit, DEFAULT_SEARCH_LIMIT);
}

#[test]
fn yaml_block_overrides_defaults() {
    let _envg = mur_common::test_env::EnvGuard::hold();
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
    let _envg = mur_common::test_env::EnvGuard::hold();
    let yaml = "research_gateway:\n  search_limit: 999\n";
    let c = load_from_yaml(yaml, Path::new("/nonexistent"));
    assert_eq!(c.search_limit, MAX_SEARCH_LIMIT);
}

#[test]
fn browser_timeout_env_override_is_independent_of_fetch_timeout() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(ENV_BROWSER_TIMEOUT_SECS, "90");
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert_eq!(c.browser_timeout.as_secs(), 90);
    assert_eq!(c.timeout.as_secs(), DEFAULT_FETCH_TIMEOUT_SECS); // unaffected
    envg.unset_var(ENV_BROWSER_TIMEOUT_SECS);
}

#[test]
fn deny_hosts_env_override_wins_over_yaml() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(ENV_DENY_HOSTS, "a.example, b.example");
    let c = load_from_yaml(
        "research_gateway:\n  deny_hosts: [\"c.example\"]\n",
        Path::new("/nonexistent"),
    );
    assert_eq!(c.deny_hosts, vec!["a.example", "b.example"]);
    envg.unset_var(ENV_DENY_HOSTS);
}

#[test]
fn empty_deny_hosts_env_does_not_wipe_yaml_blocklist() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // An empty MUR_RESEARCH_DENY_HOSTS must be treated as ABSENT, not as
    // "clear the blocklist" — otherwise it would silently wipe the
    // YAML-configured SSRF overlay (security-relevant).
    envg.set_var(ENV_DENY_HOSTS, "");
    let c = load_from_yaml(
        "research_gateway:\n  deny_hosts: [\"blocked.example\"]\n",
        Path::new("/nonexistent"),
    );
    assert_eq!(c.deny_hosts, vec!["blocked.example"]);
    envg.unset_var(ENV_DENY_HOSTS);
}

#[test]
fn lightpanda_path_env_override_wins_and_need_not_exist() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(ENV_LIGHTPANDA_PATH, "/x/lightpanda");
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert_eq!(c.browser.lightpanda_path.as_deref(), Some("/x/lightpanda"));
    envg.unset_var(ENV_LIGHTPANDA_PATH);
}

#[test]
fn lightpanda_default_path_absent_when_not_on_disk() {
    let _envg = mur_common::test_env::EnvGuard::hold();
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert_eq!(c.browser.lightpanda_path, None);
}

#[test]
fn mur_home_dir_honors_mur_home_env() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", "/tmp/mur-research-gateway-test-home");
    assert_eq!(
        mur_home_dir(),
        PathBuf::from("/tmp/mur-research-gateway-test-home")
    );
    envg.unset_var("MUR_HOME");
}

#[test]
fn max_fetch_chars_default_env_yaml_precedence() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // Default when nothing set.
    envg.unset_var("MUR_RESEARCH_MAX_FETCH_CHARS");
    let cfg = load_from_yaml("", std::path::Path::new("/tmp"));
    assert_eq!(cfg.max_fetch_chars, DEFAULT_MAX_FETCH_CHARS);
    // YAML sets it.
    let cfg = load_from_yaml(
        "research_gateway:\n  max_fetch_chars: 1234\n",
        std::path::Path::new("/tmp"),
    );
    assert_eq!(cfg.max_fetch_chars, 1234);
    // Env overrides YAML.
    envg.set_var("MUR_RESEARCH_MAX_FETCH_CHARS", "42");
    let cfg = load_from_yaml(
        "research_gateway:\n  max_fetch_chars: 1234\n",
        std::path::Path::new("/tmp"),
    );
    assert_eq!(cfg.max_fetch_chars, 42);
    envg.unset_var("MUR_RESEARCH_MAX_FETCH_CHARS");
}

#[test]
fn render_engine_defaults_agentbrowser_env_overrides_obscura() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(matches!(
        c.browser.render_engine,
        crate::browser::RenderEngine::AgentBrowser
    ));
    envg.set_var(ENV_RENDER_ENGINE, "obscura");
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(matches!(
        c.browser.render_engine,
        crate::browser::RenderEngine::Obscura
    ));
    envg.unset_var(ENV_RENDER_ENGINE);
}

#[test]
fn render_engine_env_lightpanda_resolves() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(ENV_RENDER_ENGINE, "lightpanda");
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(matches!(
        c.browser.render_engine,
        crate::browser::RenderEngine::Lightpanda
    ));
    envg.unset_var(ENV_RENDER_ENGINE);
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
    let _envg = mur_common::test_env::EnvGuard::hold();
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
    let _envg = mur_common::test_env::EnvGuard::hold();
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
    let _envg = mur_common::test_env::EnvGuard::hold();
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
    let _envg = mur_common::test_env::EnvGuard::hold();

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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let mur_home = scratch_dir("obscura_env_override");
    let aura = mur_home.join("aura");
    std::fs::create_dir_all(&aura).expect("create aura dir");
    std::fs::write(aura.join("obscura"), b"").expect("write obscura stub");
    std::fs::write(aura.join("obscura-worker"), b"").expect("write obscura-worker stub");

    envg.set_var(ENV_RENDER_ENGINE, "agent-browser");
    let c = load_from_yaml("", &mur_home);
    assert_eq!(
        c.browser.render_engine,
        crate::browser::RenderEngine::AgentBrowser
    );
    envg.unset_var(ENV_RENDER_ENGINE);

    let _ = std::fs::remove_dir_all(&mur_home);
}
