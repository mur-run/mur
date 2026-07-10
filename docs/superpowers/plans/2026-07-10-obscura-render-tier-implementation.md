# obscura Render-Tier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add obscura as a config-selected render engine in `mur-research-gateway`'s tier-2/3 fetch path, routed through the egress proxy, keeping agent-browser/Lightpanda as the default until a live head-to-head flips it.

**Architecture:** `fetch_rendered` gains an engine switch. When `render_engine = Obscura`, it drives the `obscura` binary (`obscura fetch <url> --dump markdown`) instead of `agent-browser`, and — this is the governance win the spike proved — forwards the gateway child's own `HTTP_PROXY` credential to obscura's `--proxy` flag so tier-2/3 egress finally goes through the loopback egress proxy (Lightpanda/Chrome can't). Default stays `AgentBrowser`; obscura is opt-in via config/env until Q3-full validates it, then a gated task flips the default.

**Tech Stack:** Rust (edition 2024), the dependency-light `mur-research-gateway` crate (shells out to a binary — does NOT link obscura), `sandbox-exec`/SBPL + Landlock (runtime-side exec allowlist).

**Evidence basis:** `docs/superpowers/plans/2026-07-10-spike-obscura-render-tier.md` (Q1 GO: V8 renders under our sandbox; Q2 YES: `--proxy http://token:@127.0.0.1:PORT` sends `Proxy-Authorization: Basic <token>` = our egress-proxy mechanism).

## Global Constraints

- No hardcoded values — new knobs are consts + env + YAML, mirroring the existing `DEFAULT_*` / `ENV_*` / `GatewayConfigYaml` pattern in `config.rs`.
- Single source file ≤ 800 lines (`browser.rs` is 331 now; stay under).
- `mur-research-gateway` stays dependency-light — no new crate deps; obscura is a subprocess.
- Default behavior UNCHANGED after Tasks 1–6 (default `render_engine = AgentBrowser`). Only Task 8 (gated on Q3-full) flips it.
- Brand: user-facing strings say "MUR"; internal identifiers/paths lowercase.
- Commit per task. Reply prose zh-TW; code/commits/comments English.

---

### Task 1: `RenderEngine` config knob

**Files:**
- Modify: `mur-research-gateway/src/browser.rs` (add enum + `BrowserCfg` fields)
- Modify: `mur-research-gateway/src/config.rs` (YAML field, env, defaults, construction)
- Test: inline `#[cfg(test)]` in both files

**Interfaces:**
- Produces: `pub enum RenderEngine { AgentBrowser, Obscura }` (Default = `AgentBrowser`); `BrowserCfg.render_engine: RenderEngine`; `BrowserCfg.obscura_path: Option<String>`.
- Consumes: existing `BrowserCfg { agent_browser_bin, lightpanda_path, chrome_stealth_args }`, `config::load_from_yaml`, `non_empty_env`, `mur_home_dir`.

- [ ] **Step 1: Write the failing test (config.rs)**

```rust
#[test]
fn render_engine_defaults_agentbrowser_env_overrides_obscura() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(matches!(c.browser.render_engine, crate::browser::RenderEngine::AgentBrowser));
    // SAFETY: env mutation guarded by ENV_LOCK.
    unsafe { std::env::set_var(ENV_RENDER_ENGINE, "obscura"); }
    let c = load_from_yaml("", Path::new("/nonexistent"));
    assert!(matches!(c.browser.render_engine, crate::browser::RenderEngine::Obscura));
    unsafe { std::env::remove_var(ENV_RENDER_ENGINE); }
}
```

- [ ] **Step 2: Run it — expect FAIL** (`ENV_RENDER_ENGINE`, `render_engine` don't exist)

Run: `cargo test -p mur-research-gateway render_engine_defaults -- --nocapture`
Expected: FAIL (unresolved name).

- [ ] **Step 3: Add the enum + BrowserCfg fields (browser.rs)**

```rust
/// Which subprocess renders JS pages for tier-2/3 `fetch`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum RenderEngine {
    /// `agent-browser` (Lightpanda tier-2 / Chrome tier-3) — current default.
    #[default]
    AgentBrowser,
    /// `obscura` — self-contained embedded-V8 engine; renders under the kernel
    /// sandbox and routes egress through `--proxy` (spike 2026-07-10).
    Obscura,
}
```

Add to `BrowserCfg`:
```rust
pub struct BrowserCfg {
    pub agent_browser_bin: String,
    pub lightpanda_path: Option<String>,
    pub chrome_stealth_args: String, // comma-separated; empty = none
    pub render_engine: RenderEngine,
    /// Path to the `obscura` binary; the sibling `obscura-worker` must live
    /// beside it. Only consulted when `render_engine == Obscura`.
    pub obscura_path: Option<String>,
}
```

- [ ] **Step 4: Wire config.rs (consts, env, YAML, construction)**

Add near the other `ENV_*` / `DEFAULT_*`:
```rust
/// Render engine for tier-2/3 `fetch`: "agent-browser" (default) or "obscura".
const ENV_RENDER_ENGINE: &str = "MUR_RESEARCH_RENDER_ENGINE";
const ENV_OBSCURA_PATH: &str = "MUR_RESEARCH_OBSCURA_PATH";
/// Default obscura install path, relative to `mur_home` (mirrors Lightpanda's
/// `aura/` install location).
pub const DEFAULT_OBSCURA_RELATIVE_PATH: &str = "aura/obscura";
```

Add to `GatewayConfigYaml`:
```rust
    render_engine: Option<String>,
    obscura_path: Option<String>,
```

In `load_from_yaml`, before building `BrowserCfg`:
```rust
    let render_engine = non_empty_env(ENV_RENDER_ENGINE)
        .or(raw.render_engine)
        .map(|s| match s.trim().to_ascii_lowercase().as_str() {
            "obscura" => crate::browser::RenderEngine::Obscura,
            _ => crate::browser::RenderEngine::AgentBrowser,
        })
        .unwrap_or_default();

    let obscura_path = non_empty_env(ENV_OBSCURA_PATH)
        .or_else(|| raw.obscura_path.filter(|s| !s.is_empty()))
        .or_else(|| default_obscura_path(mur_home));
```

Add helper beside `default_lightpanda_path`:
```rust
/// Default obscura path — only when it actually exists on disk (never claim a
/// path that isn't there), matching `default_lightpanda_path`.
fn default_obscura_path(mur_home: &Path) -> Option<String> {
    let path = mur_home.join(DEFAULT_OBSCURA_RELATIVE_PATH);
    path.exists().then(|| path.to_string_lossy().to_string())
}
```

Extend the `BrowserCfg { … }` construction with `render_engine, obscura_path,`.

- [ ] **Step 5: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway 2>&1 | tail -5`
Expected: PASS (all existing + new).

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/browser.rs mur-research-gateway/src/config.rs
git commit -m "feat(research-gateway): RenderEngine config knob (default agent-browser)"
```

---

### Task 2: `build_obscura_argv` (pure)

**Files:**
- Modify: `mur-research-gateway/src/browser.rs`
- Test: inline

**Interfaces:**
- Produces: `fn build_obscura_argv(url: &str, proxy: Option<&str>, timeout: Duration) -> Vec<String>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn obscura_argv_dumps_markdown_and_threads_proxy() {
    let base = build_obscura_argv("https://example.com", None, Duration::from_secs(20));
    assert_eq!(base[0], "fetch");
    assert_eq!(base[1], "https://example.com");
    assert!(base.windows(2).any(|w| w[0] == "--dump" && w[1] == "markdown"));
    assert!(base.windows(2).any(|w| w[0] == "--timeout" && w[1] == "20"));
    assert!(!base.iter().any(|a| a == "--proxy"));

    let proxied = build_obscura_argv("https://example.com", Some("http://t:@127.0.0.1:9"), Duration::from_secs(20));
    assert!(proxied.windows(2).any(|w| w[0] == "--proxy" && w[1] == "http://t:@127.0.0.1:9"));
}
```

- [ ] **Step 2: Run it — expect FAIL** (`build_obscura_argv` undefined)

Run: `cargo test -p mur-research-gateway obscura_argv -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// Build the `obscura fetch` argv. `--dump markdown` yields cleaner text than
/// the tier-1 tag-strip (spike Q3b). `proxy` (the gateway's own HTTP_PROXY
/// credential, `http://<token>:@host:port`) becomes `--proxy` so obscura's
/// egress goes through the loopback egress proxy (spike Q2). Pure →
/// unit-testable, no env/subprocess.
fn build_obscura_argv(url: &str, proxy: Option<&str>, timeout: Duration) -> Vec<String> {
    let mut a = vec![
        "fetch".to_string(),
        url.to_string(),
        "--dump".to_string(),
        "markdown".to_string(),
        "--timeout".to_string(),
        timeout.as_secs().to_string(),
    ];
    if let Some(p) = proxy {
        a.push("--proxy".to_string());
        a.push(p.to_string());
    }
    a
}
```

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway obscura_argv`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/browser.rs
git commit -m "feat(research-gateway): build_obscura_argv (--dump markdown + --proxy)"
```

---

### Task 3: proxy flag from the gateway's env

**Files:**
- Modify: `mur-research-gateway/src/browser.rs`
- Test: inline (env-locked)

**Interfaces:**
- Produces: `fn render_proxy_flag() -> Option<String>` — reads `HTTP_PROXY`, then `HTTPS_PROXY`; non-empty only.

- [ ] **Step 1: Write the failing test**

```rust
// Process-global env — serialize with the same lock style config.rs tests use.
static RENDER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn render_proxy_flag_reads_http_proxy_env() {
    let _g = RENDER_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: guarded by RENDER_ENV_LOCK.
    unsafe { std::env::remove_var("HTTP_PROXY"); std::env::remove_var("HTTPS_PROXY"); }
    assert_eq!(render_proxy_flag(), None);
    unsafe { std::env::set_var("HTTP_PROXY", "http://tok:@127.0.0.1:5555"); }
    assert_eq!(render_proxy_flag().as_deref(), Some("http://tok:@127.0.0.1:5555"));
    unsafe { std::env::remove_var("HTTP_PROXY"); }
}
```

- [ ] **Step 2: Run it — expect FAIL**

Run: `cargo test -p mur-research-gateway render_proxy_flag -- --nocapture --test-threads=1`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// The proxy URL obscura should route through, read from the gateway's OWN
/// environment. The runtime sets `HTTP_PROXY=http://<token>:x@127.0.0.1:<port>`
/// on this child (see mur-agent-runtime `proxy_env_for`); obscura does NOT
/// honor the env var itself, so we translate it into its `--proxy` flag.
/// Absent (dev/unsandboxed) → no proxy, direct connect.
fn render_proxy_flag() -> Option<String> {
    std::env::var("HTTP_PROXY")
        .ok()
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .filter(|s| !s.is_empty())
}
```

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway render_proxy_flag -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/browser.rs
git commit -m "feat(research-gateway): render_proxy_flag reads gateway HTTP_PROXY"
```

---

### Task 4: `fetch_rendered` engine dispatch

**Files:**
- Modify: `mur-research-gateway/src/browser.rs`
- Test: inline

**Interfaces:**
- Produces: `fn plan_render(url: &str, cfg: &BrowserCfg, want_chrome: bool, proxy: Option<&str>) -> (String, Vec<String>, u8)` returning `(bin, argv, tier)`.
- Consumes: `build_fetch_argv`, `build_obscura_argv`, `RenderEngine`, `render_proxy_flag`.
- `fetch_rendered` signature UNCHANGED: `(url, deny, cfg, want_chrome, timeout) -> Result<FetchResult, FetchError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn plan_render_dispatches_on_engine() {
    let ab = BrowserCfg {
        agent_browser_bin: "agent-browser".into(),
        lightpanda_path: Some("/x/lightpanda".into()),
        chrome_stealth_args: "--no-sandbox".into(),
        render_engine: RenderEngine::AgentBrowser,
        obscura_path: None,
    };
    let (bin, _argv, tier) = plan_render("https://example.com", &ab, false, None);
    assert_eq!(bin, "agent-browser");
    assert_eq!(tier, 2); // lightpanda present, not chrome

    let ob = BrowserCfg {
        render_engine: RenderEngine::Obscura,
        obscura_path: Some("/opt/obscura".into()),
        ..ab
    };
    let (bin, argv, tier) = plan_render("https://example.com", &ob, true /*ignored*/, Some("http://t:@127.0.0.1:9"));
    assert_eq!(bin, "/opt/obscura");
    assert_eq!(tier, 2); // obscura is one engine; want_chrome ignored
    assert_eq!(argv[0], "fetch");
    assert!(argv.windows(2).any(|w| w[0] == "--proxy" && w[1] == "http://t:@127.0.0.1:9"));
}
```

- [ ] **Step 2: Run it — expect FAIL** (`plan_render` undefined; `BrowserCfg` new fields via `..ab` need `Clone`? use full literal)

Run: `cargo test -p mur-research-gateway plan_render`
Expected: FAIL.

- [ ] **Step 3: Implement `plan_render` + thread it through `fetch_rendered`**

```rust
/// Decide (binary, argv, tier) for a render, dispatching on the engine. Pure
/// (proxy passed in) → unit-testable without spawning. obscura is a single
/// engine covering JS render, so `want_chrome` is ignored and the tier is 2;
/// the agent-browser path keeps the lightpanda(2)/chrome(3) split.
fn plan_render(
    url: &str,
    cfg: &BrowserCfg,
    want_chrome: bool,
    proxy: Option<&str>,
) -> (String, Vec<String>, u8) {
    match cfg.render_engine {
        RenderEngine::Obscura => {
            let bin = cfg.obscura_path.clone().unwrap_or_else(|| "obscura".to_string());
            // ponytail: obscura's --timeout is set inside build_obscura_argv from
            // the same browser_timeout the subprocess is bounded by; passed at the
            // call site (fetch_rendered). Use a placeholder here overwritten below.
            (bin, build_obscura_argv(url, proxy, Duration::from_secs(0)), 2)
        }
        RenderEngine::AgentBrowser => {
            let tier = if want_chrome || cfg.lightpanda_path.is_none() { 3 } else { 2 };
            (cfg.agent_browser_bin.clone(), build_fetch_argv(url, cfg, want_chrome), tier)
        }
    }
}
```

> **Note for implementer:** the `Duration::from_secs(0)` placeholder above is a
> smell — instead thread `timeout` into `plan_render` so `build_obscura_argv`
> gets the real value. Preferred final signature:
> `plan_render(url, cfg, want_chrome, proxy, timeout)`. Update the test's calls
> to pass `Duration::from_secs(20)` and assert `--timeout 20`. Pick this
> cleaner form; the placeholder is only shown to flag the dependency.

Rewrite `fetch_rendered` body to use it:
```rust
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
    let proxy = render_proxy_flag();
    let (bin, argv, tier) = plan_render(screened.as_str(), cfg, want_chrome, proxy.as_deref(), timeout);
    let text = run_agent_browser(&bin, &argv, timeout).await?;
    Ok(FetchResult { url: screened.to_string(), status: 200, title: None, text, tier })
}
```

(`run_agent_browser` is already a generic bounded subprocess runner — reuse as-is; a rename to `run_render_subprocess` is optional polish, not required.)

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/browser.rs
git commit -m "feat(research-gateway): fetch_rendered dispatches agent-browser vs obscura"
```

---

### Task 5: don't escalate to Chrome when engine is obscura

**Files:**
- Modify: `mur-research-gateway/src/server.rs` (`handle_fetch` escalation guard)
- Test: inline in server.rs

**Interfaces:**
- Consumes: `cfg.render_engine`, existing `should_escalate_to_chrome`.

- [ ] **Step 1: Write the failing test** — an obscura-config fetch that returns empty must NOT trigger a second (tier-3) render call.

```rust
#[test]
fn obscura_engine_does_not_escalate_to_chrome() {
    // plan_render for obscura always yields tier 2; the server must gate the
    // tier-3 re-call on the engine being AgentBrowser.
    // (Unit-level: assert the guard predicate the handler uses.)
    let ob = /* BrowserCfg with render_engine: Obscura */;
    assert!(!crate::server::render_can_escalate(&ob));
    let ab = /* BrowserCfg with render_engine: AgentBrowser */;
    assert!(crate::server::render_can_escalate(&ab));
}
```

- [ ] **Step 2: Run it — expect FAIL** (`render_can_escalate` undefined)

Run: `cargo test -p mur-research-gateway obscura_engine_does_not_escalate`
Expected: FAIL.

- [ ] **Step 3: Implement the guard + use it in `handle_fetch`**

```rust
/// Tier-3 (Chrome) escalation only makes sense for the agent-browser engine —
/// obscura is a single engine, so re-rendering with `want_chrome=true` would
/// just run obscura again. Gate the escalation on this.
pub(crate) fn render_can_escalate(cfg: &crate::config::GatewayConfig) -> bool {
    matches!(cfg.browser.render_engine, crate::browser::RenderEngine::AgentBrowser)
}
```

In `handle_fetch`, change the escalation condition (around server.rs:155) to also require `render_can_escalate(&self.config)` before the second `fetch_rendered(&url, deny, cfg, true, …)` call.

(Adjust the test to build real `GatewayConfig`/`BrowserCfg` values or split the predicate to take `&BrowserCfg` — implementer's choice; keep it unit-testable without a live server.)

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/server.rs
git commit -m "feat(research-gateway): skip chrome escalation for obscura engine"
```

---

### Task 6: preflight + install/exec-allowlist wiring

**Files:**
- Modify: `mur-research-gateway/src/browser.rs` (`Preflight` + `preflight_from_versions`)
- Modify (runtime, verify first): the exec allowlist that grants the gateway process-exec over `~/.mur/aura/` — `mur-agent-runtime/src/sandbox/policy.rs` (`fs_exec` / `spawn_allowed_paths`) and wherever the research gateway's policy is built. **Spike Layer-2 proved BOTH `obscura` and `obscura-worker` must be exec-allowlisted.**
- Test: inline

**Interfaces:**
- Produces: `Preflight::ObscuraMissing` variant; obscura branch in preflight.

- [ ] **Step 1: Verify the current lightpanda exec grant**

Run:
```bash
grep -rn "aura\|lightpanda\|fs_exec\|spawn_allowed_paths\|exec" mur-agent-runtime/src/sandbox/policy.rs mur-core/src/cmd/deep_research/provision.rs
```
Determine whether `~/.mur/aura/` is granted as an exec DIRECTORY (`fs_exec`) — if so, obscura + obscura-worker siblings are already covered and NO runtime change is needed. If it's a per-file `spawn_allowed_paths` grant for `aura/lightpanda`, add `aura/obscura` and `aura/obscura-worker`.

- [ ] **Step 2: Write the failing preflight test**

```rust
#[test]
fn preflight_flags_missing_obscura() {
    let cfg = BrowserCfg {
        agent_browser_bin: "agent-browser".into(),
        lightpanda_path: None,
        chrome_stealth_args: String::new(),
        render_engine: RenderEngine::Obscura,
        obscura_path: Some("/nonexistent/obscura".into()),
    };
    assert_eq!(preflight(&cfg), Preflight::ObscuraMissing);
}
```

- [ ] **Step 3: Run it — expect FAIL** (`ObscuraMissing` undefined)

Run: `cargo test -p mur-research-gateway preflight_flags_missing_obscura`
Expected: FAIL.

- [ ] **Step 4: Add the variant + obscura preflight branch**

Add `ObscuraMissing` to `enum Preflight`. In `preflight` (or `preflight_from_versions`), when `render_engine == Obscura`: check `obscura_path` exists AND its sibling `obscura-worker` exists; else `ObscuraMissing`. obscura needs no `agent-browser --version` probe (different binary), so short-circuit the agent-browser checks for the obscura engine.

- [ ] **Step 5: Apply the runtime exec-allowlist change if Step 1 requires it**

If `aura/` is not already an exec-dir grant, extend the grant to cover both obscura binaries wherever `lightpanda` is granted. Add a runtime test mirroring the existing lightpanda-grant test. (If Step 1 shows `aura/` is a dir grant, note "no change needed" in the commit and skip.)

- [ ] **Step 6: Run tests — expect PASS**

Run: `cargo test -p mur-research-gateway && cargo test -p mur-agent-runtime sandbox 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(research-gateway): obscura preflight + exec-allowlist both binaries"
```

---

### Task 7: docs + install note

**Files:**
- Modify: `docs/architecture/runtime-overview.md` (research-gateway section) — document `render_engine`, `MUR_RESEARCH_RENDER_ENGINE`, `obscura_path`, the `~/.mur/aura/obscura{,-worker}` install, and the `--proxy` egress-governance behavior.
- Modify: the spike doc — mark the implementation plan as underway.

- [ ] **Step 1:** Add a short "Render engine (obscura, opt-in)" subsection: how to install (download the platform tarball to `~/.mur/aura/`, keep both binaries), how to enable (`MUR_RESEARCH_RENDER_ENGINE=obscura` or YAML `research_gateway.render_engine: obscura`), and that obscura egress is proxy-governed.
- [ ] **Step 2: Commit** `docs: obscura render engine (opt-in) + install/egress notes`

---

### Task 8: GATED — Q3-full head-to-head, then flip the default

> **Do NOT start until Tasks 1–7 are merged AND a live head-to-head has run.**
> This is the carry-in from the spike; it changes the default and must be
> evidence-backed.

**Files:**
- Modify: `mur-research-gateway/src/browser.rs` (`RenderEngine::default` → `Obscura`) OR `config.rs` default resolution.

- [ ] **Step 1:** Run obscura vs a live Lightpanda install on ~10 real research targets (JS-heavy docs, vendor pages, a GitHub README). Record extraction quality, latency, memory under 4× concurrency. Append results to the spike findings.
- [ ] **Step 2:** If obscura ≥ Lightpanda on quality with no stability/perf regression, flip `#[default]` to `Obscura` (and update the Task-1 default test). Else keep `AgentBrowser` default and document why.
- [ ] **Step 3: Commit** the flip (or the "keep default" note) with the measurement link.

---

## Self-Review

**Spec coverage:** Config knob (T1) ✓ · obscura argv (T2) ✓ · proxy egress governance = spike Q2 (T3) ✓ · engine dispatch (T4) ✓ · no-chrome-escalation (T5) ✓ · preflight + exec-allowlist both binaries = spike Q1 Layer-2 (T6) ✓ · docs/install (T7) ✓ · Q3-full + default flip = spike carry-in (T8, gated) ✓.

**Type consistency:** `RenderEngine` used identically across browser.rs/config.rs; `plan_render` returns `(String, Vec<String>, u8)` and `fetch_rendered` keeps its existing signature/return `Result<FetchResult, FetchError>`; `build_obscura_argv` timeout dependency flagged in T4 (thread `timeout` into `plan_render`, don't leave the `from_secs(0)` placeholder).

**Placeholder scan:** The one deliberate smell (`Duration::from_secs(0)` in T4 Step 3) is explicitly called out with the corrected signature to adopt — not left as a silent TODO.

**Open item for the implementer:** T6 Step 1 is a genuine verification branch (is `aura/` a dir-grant or a per-file grant?) — the runtime change in T6 Step 5 is conditional on its outcome.
