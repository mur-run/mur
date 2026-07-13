# Deep Research UX Simplification — Phase 1 (CLI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One-command deep research: `mur deep-research setup` wizard, bare `mur deep-research` status panel, and `mur deep-research "question"` smart run with safe auto-repair.

**Architecture:** A thin UX shell in `mur-core/src/cmd/deep_research/` over the existing `cmd_provision` / `cmd_fleet_create` / `cmd_deep_research_run` path. New modules: `status.rs` (pure status/preflight model), `setup.rs` (wizard), `ask.rs` (smart run). No runtime, gateway, or fleet-loop changes.

**Tech Stack:** Rust (edition 2024), clap derive, existing mur-core/mur-common APIs.

**Spec:** `docs/superpowers/specs/2026-07-13-deep-research-ux-simplification-design.md`

## Global Constraints

- Egress consent is explicit-only: wizard requires the literal word `yes`; nothing in preflight/auto-repair ever grants or modifies network entitlements.
- Auto-repair does ONLY: start workers, re-pin the gateway. Never rebuilds binaries, never touches grants.
- No hardcoded values: reuse `DEFAULT_WORKER_PREFIX`, `DEFAULT_WORKER_COUNT`, `DEFAULT_WORKER_MODEL`, `GATEWAY_MCP_NAME` from `provision.rs`; new defaults become named consts.
- `provision` / `run` subcommands stay byte-for-byte compatible.
- Build env: PATH `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`, `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Test with `cargo nextest run -p mur-core <filter>` (plain `cargo test --workspace` is flaky); bin-target tests need `RUST_MIN_STACK=33554432`.
- `cargo fmt` before every commit (CI Format gate). `cargo clippy -p mur-core -- -D warnings` must pass.
- User-facing brand string is "MUR"; command literals stay lowercase `mur`.

---

### Task 1: Status model (`status.rs`)

**Files:**
- Create: `mur-core/src/cmd/deep_research/status.rs`
- Modify: `mur-core/src/cmd/deep_research/mod.rs` (add `pub mod status;`)

**Interfaces:**
- Consumes: `mur_common::agent::{AgentProfile, McpNetMode}` (`AgentProfile::load(mur_home, name)`), `provision::{DEFAULT_WORKER_PREFIX, GATEWAY_MCP_NAME}`, `crate::cmd::fleet::store::fleet_path`.
- Produces (used by Tasks 2–4):
  - `pub struct WorkerStatus { pub name: String, pub running: bool, pub egress_granted: bool }`
  - `pub struct DeepResearchStatus { pub workers: Vec<WorkerStatus>, pub fleet_exists: bool, pub model: Option<String> }`
  - `pub fn collect_status(mur_home: &Path, fleet_name: &str) -> DeepResearchStatus`
  - `pub fn is_agent_running(mur_home: &Path, name: &str) -> bool`
  - `pub const DEFAULT_FLEET_NAME: &str = "deep-research";`

- [ ] **Step 1: Write the failing test** (bottom of `status.rs` in `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_on_empty_home_is_all_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = collect_status(tmp.path(), DEFAULT_FLEET_NAME);
        assert!(s.workers.is_empty());
        assert!(!s.fleet_exists);
        assert!(s.model.is_none());
    }

    #[test]
    fn agent_without_socket_is_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agents/dr_worker_1")).unwrap();
        assert!(!is_agent_running(tmp.path(), "dr_worker_1"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core deep_research::status`
Expected: compile FAIL (module/functions not defined yet — add the mod decl first, then it fails on missing fns).

- [ ] **Step 3: Implement**

```rust
//! Read-only status/preflight model for the deep-research UX shell.
//! Pure data collection — no repairs, no side effects.

use std::path::Path;

use mur_common::agent::{AgentProfile, McpNetMode};

use super::provision::{DEFAULT_WORKER_PREFIX, GATEWAY_MCP_NAME};

/// Canonical fleet name the wizard provisions and the smart run drives.
pub const DEFAULT_FLEET_NAME: &str = "deep-research";

pub struct WorkerStatus {
    pub name: String,
    pub running: bool,
    pub egress_granted: bool,
}

pub struct DeepResearchStatus {
    pub workers: Vec<WorkerStatus>,
    pub fleet_exists: bool,
    /// model_ref of the first worker (all provisioned alike).
    pub model: Option<String>,
}

/// An agent is "running" when its unix socket accepts a connection.
pub fn is_agent_running(mur_home: &Path, name: &str) -> bool {
    let sock = mur_home.join("agents").join(name).join("agent.sock");
    std::os::unix::net::UnixStream::connect(&sock).is_ok()
}

fn gateway_egress_granted(profile: &AgentProfile) -> bool {
    profile.mcp_servers.iter().any(|s| {
        s.name == GATEWAY_MCP_NAME
            && s.network
                .as_ref()
                .is_some_and(|n| n.mode == McpNetMode::BroadAudited)
    })
}

pub fn collect_status(mur_home: &Path, fleet_name: &str) -> DeepResearchStatus {
    let mut workers = Vec::new();
    let mut model = None;
    // Workers are `<prefix>_1..N`; scan the agents dir for that shape so a
    // custom --count keeps working without storing extra state.
    let agents_dir = mur_home.join("agents");
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&format!("{DEFAULT_WORKER_PREFIX}_")))
            .collect();
        names.sort();
        for name in names {
            let (running, egress) = match AgentProfile::load(mur_home, &name) {
                Ok(p) => {
                    if model.is_none() {
                        model = p.model_ref.clone();
                    }
                    (is_agent_running(mur_home, &name), gateway_egress_granted(&p))
                }
                Err(_) => (false, false),
            };
            workers.push(WorkerStatus { name, running, egress_granted: egress });
        }
    }
    let fleet_exists = crate::cmd::fleet::store::fleet_path(mur_home, fleet_name).exists();
    DeepResearchStatus { workers, fleet_exists, model }
}
```

Note for the implementer: check `AgentProfile.model_ref`'s exact field name/type with `grep -n "model_ref" mur-common/src/agent.rs` — if it is not `Option<String>`, adapt the two `model` lines (do NOT add a new field to the profile). `McpServerEntry.network` is `Option<McpServerNetwork>` with `.mode: McpNetMode` (verified). Windows: `is_agent_running` uses unix sockets — gate the fn body with `#[cfg(unix)]` and return `false` on non-unix:

```rust
pub fn is_agent_running(mur_home: &Path, name: &str) -> bool {
    #[cfg(unix)]
    {
        let sock = mur_home.join("agents").join(name).join("agent.sock");
        return std::os::unix::net::UnixStream::connect(&sock).is_ok();
    }
    #[cfg(not(unix))]
    {
        let _ = (mur_home, name);
        false
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core deep_research::status`
Expected: 2 passed.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt
cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/deep_research/
git commit -m "feat(deep-research): status model for UX shell (T1)"
```

---

### Task 2: Bare `mur deep-research` → status panel

**Files:**
- Modify: `mur-core/src/cli/actions.rs` (`DeepResearchAction` enum, ~line 661)
- Modify: `mur-core/src/dispatch.rs` (`Commands::DeepResearch` arm, ~line 480)
- Create: `mur-core/src/cmd/deep_research/panel.rs`
- Modify: `mur-core/src/cmd/deep_research/mod.rs` (add `pub mod panel;`)

**Interfaces:**
- Consumes: `status::{collect_status, DeepResearchStatus, DEFAULT_FLEET_NAME}` (Task 1).
- Produces: `pub fn render_panel(s: &DeepResearchStatus) -> String` and `pub fn cmd_panel(mur_home: &Path) -> anyhow::Result<()>` (prints `render_panel`); `DeepResearchAction` becomes optional in the CLI.

- [ ] **Step 1: Write the failing test** (in `panel.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deep_research::status::{DeepResearchStatus, WorkerStatus};

    #[test]
    fn panel_unconfigured_points_at_setup() {
        let s = DeepResearchStatus { workers: vec![], fleet_exists: false, model: None };
        let out = render_panel(&s);
        assert!(out.contains("mur deep-research setup"));
    }

    #[test]
    fn panel_lists_workers_and_egress() {
        let s = DeepResearchStatus {
            workers: vec![WorkerStatus {
                name: "dr_worker_1".into(),
                running: true,
                egress_granted: true,
            }],
            fleet_exists: true,
            model: Some("claude_haiku".into()),
        };
        let out = render_panel(&s);
        assert!(out.contains("dr_worker_1"));
        assert!(out.contains("running"));
        assert!(out.contains("claude_haiku"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core deep_research::panel`
Expected: FAIL (render_panel undefined).

- [ ] **Step 3: Implement `panel.rs`**

```rust
//! Bare `mur deep-research` status panel (read-only).

use std::path::Path;

use super::status::{collect_status, DeepResearchStatus, DEFAULT_FLEET_NAME};

pub fn render_panel(s: &DeepResearchStatus) -> String {
    if s.workers.is_empty() {
        return "Deep research is not set up yet.\n  Run `mur deep-research setup` to configure workers, model, budget and egress.\n".to_string();
    }
    let mut out = String::from("Deep research status\n");
    out.push_str(&format!(
        "  model: {}\n",
        s.model.as_deref().unwrap_or("(none — run setup)")
    ));
    out.push_str(&format!(
        "  fleet: {}\n",
        if s.fleet_exists { DEFAULT_FLEET_NAME } else { "(missing — run setup)" }
    ));
    for w in &s.workers {
        out.push_str(&format!(
            "  {} — {}, egress {}\n",
            w.name,
            if w.running { "running" } else { "stopped" },
            if w.egress_granted { "granted" } else { "NOT granted" },
        ));
    }
    out.push_str("\nRun research with: mur deep-research \"<your question>\"\n");
    out
}

pub fn cmd_panel(mur_home: &Path) -> anyhow::Result<()> {
    print!("{}", render_panel(&collect_status(mur_home, DEFAULT_FLEET_NAME)));
    Ok(())
}
```

- [ ] **Step 4: Wire the CLI (optional subcommand + optional positional)**

In `actions.rs`, the `Commands::DeepResearch` variant (find it with `grep -n "DeepResearch {" mur-core/src/cli/actions.rs`) becomes:

```rust
/// MUR-native deep research (wizard: `setup`; status: bare; run: pass a question)
#[command(args_conflicts_with_subcommands = true)]
DeepResearch {
    #[command(subcommand)]
    action: Option<DeepResearchAction>,
    /// Research question — runs the deep-research fleet directly
    question: Option<String>,
},
```

In `dispatch.rs`, the arm becomes (keep the two existing inner match arms verbatim):

```rust
Commands::DeepResearch { action, question } => {
    let mur_home = crate::paths::mur_root(None);
    match (action, question) {
        (Some(DeepResearchAction::Provision { .. }), _) => { /* existing arm unchanged */ }
        (Some(DeepResearchAction::Run { .. }), _) => { /* existing arm unchanged */ }
        (None, Some(q)) => {
            // Task 4 lands cmd_ask; until then print a stub error:
            anyhow::bail!("smart run not wired yet: {q}");
        }
        (None, None) => cmd::deep_research::panel::cmd_panel(&mur_home)?,
    }
}
```

(Adapt the destructuring to the real existing arms — move them inside `(Some(...), _)` patterns without changing their bodies.)

- [ ] **Step 5: Run tests + verify CLI parses**

```bash
cargo nextest run -p mur-core deep_research::panel
RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --bin mur cli 2>/dev/null || true  # CLI-parse smoke, pre-existing flaky set
cargo run -q -- deep-research | head -3
```
Expected: panel tests pass; bare command prints the not-set-up panel (on a dev machine with provisioned workers it prints the worker list).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add -A mur-core/src
git commit -m "feat(deep-research): bare command status panel (T2)"
```

---

### Task 3: `mur deep-research setup` wizard

**Files:**
- Create: `mur-core/src/cmd/deep_research/setup.rs`
- Modify: `mur-core/src/cmd/deep_research/mod.rs` (add `pub mod setup;`)
- Modify: `mur-core/src/cli/actions.rs` (add `Setup` variant)
- Modify: `mur-core/src/dispatch.rs` (add `Setup` arm)

**Interfaces:**
- Consumes: `provision::{cmd_provision, grant_egress, DEFAULT_WORKER_COUNT, DEFAULT_WORKER_MODEL, DEFAULT_WORKER_PREFIX}`; `crate::cmd::fleet::create::cmd_fleet_create(mur_home, name, members, router, goal, parallel)`; `crate::cmd::fleet::store::{load_fleet, save_fleet, fleet_path}`; `mur_common::model::ModelRegistry` (`load_from`, `default_path`, `.models: BTreeMap<String, ModelEntry>`); `status::DEFAULT_FLEET_NAME`.
- Produces:
  - `pub struct WizardAnswers { pub model: String, pub count: usize, pub budget_usd: f64, pub egress: bool }`
  - `pub fn run_wizard(input: &mut dyn BufRead, output: &mut dyn Write, model_choices: &[String]) -> anyhow::Result<WizardAnswers>` (pure I/O over injected streams — unit-testable)
  - `pub fn cmd_setup(mur_home: &Path) -> anyhow::Result<()>` (TTY guard + wizard + provision + fleet + budget persist)
  - `pub const DEFAULT_RUN_BUDGET_USD: f64 = 10.0;`

- [ ] **Step 1: Write the failing tests** (in `setup.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn answers(script: &str, choices: &[&str]) -> anyhow::Result<WizardAnswers> {
        let choices: Vec<String> = choices.iter().map(|s| s.to_string()).collect();
        let mut input = Cursor::new(script.as_bytes().to_vec());
        let mut out = Vec::new();
        run_wizard(&mut input, &mut out, &choices)
    }

    #[test]
    fn defaults_accepted_with_empty_lines_and_explicit_yes() {
        // model=default, count=default, budget=default, egress consent "yes"
        let a = answers("\n\n\nyes\n", &["claude_haiku", "claude_opus"]).unwrap();
        assert_eq!(a.model, "claude_haiku");
        assert_eq!(a.count, super::super::provision::DEFAULT_WORKER_COUNT);
        assert_eq!(a.budget_usd, DEFAULT_RUN_BUDGET_USD);
        assert!(a.egress);
    }

    #[test]
    fn egress_requires_literal_yes() {
        let a = answers("\n\n\ny\n", &["claude_haiku"]).unwrap();
        assert!(!a.egress, "'y' must NOT count as egress consent");
    }

    #[test]
    fn model_picked_by_number() {
        let a = answers("2\n\n\nno\n", &["claude_haiku", "claude_opus"]).unwrap();
        assert_eq!(a.model, "claude_opus");
    }

    #[test]
    fn bad_budget_rejected() {
        assert!(answers("\n\nnot-a-number\nyes\n", &["claude_haiku"]).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core deep_research::setup`
Expected: FAIL (types undefined).

- [ ] **Step 3: Implement**

```rust
//! `mur deep-research setup` — interactive first-time wizard.
//!
//! Pure Q&A over injected streams (`run_wizard`) + an orchestration wrapper
//! (`cmd_setup`) that calls the EXISTING provision/fleet paths. Egress
//! consent requires the literal word "yes" — never defaulted or implied.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::provision::{DEFAULT_WORKER_COUNT, DEFAULT_WORKER_MODEL};
use super::status::DEFAULT_FLEET_NAME;

/// Default per-run budget ceiling (USD) persisted to the fleet's
/// `loop.budget_usd`; the run loop's existing budget guard enforces it.
pub const DEFAULT_RUN_BUDGET_USD: f64 = 10.0;

pub struct WizardAnswers {
    pub model: String,
    pub count: usize,
    pub budget_usd: f64,
    pub egress: bool,
}

fn read_line(input: &mut dyn BufRead) -> Result<String> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("wizard aborted: end of input");
    }
    Ok(line.trim().to_string())
}

pub fn run_wizard(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    model_choices: &[String],
) -> Result<WizardAnswers> {
    // Q1: model
    let default_model = if model_choices.iter().any(|m| m == DEFAULT_WORKER_MODEL) {
        DEFAULT_WORKER_MODEL.to_string()
    } else {
        model_choices.first().cloned().unwrap_or_else(|| DEFAULT_WORKER_MODEL.to_string())
    };
    writeln!(output, "Worker model (registry aliases):")?;
    for (i, m) in model_choices.iter().enumerate() {
        writeln!(output, "  {}. {m}", i + 1)?;
    }
    write!(output, "Pick a number or name [{default_model}]: ")?;
    output.flush()?;
    let ans = read_line(input)?;
    let model = if ans.is_empty() {
        default_model
    } else if let Ok(n) = ans.parse::<usize>() {
        model_choices
            .get(n.checked_sub(1).context("model number must be >= 1")?)
            .with_context(|| format!("no model #{n}"))?
            .clone()
    } else {
        ans
    };

    // Q2: worker count
    write!(output, "Number of workers [{DEFAULT_WORKER_COUNT}]: ")?;
    output.flush()?;
    let ans = read_line(input)?;
    let count = if ans.is_empty() {
        DEFAULT_WORKER_COUNT
    } else {
        ans.parse::<usize>().context("worker count must be a positive integer")?
    };
    if count == 0 {
        bail!("worker count must be at least 1");
    }

    // Q3: per-run budget
    write!(output, "Per-run budget in USD [{DEFAULT_RUN_BUDGET_USD}]: ")?;
    output.flush()?;
    let ans = read_line(input)?;
    let budget_usd = if ans.is_empty() {
        DEFAULT_RUN_BUDGET_USD
    } else {
        ans.parse::<f64>().context("budget must be a number (USD)")?
    };
    if !(budget_usd > 0.0) {
        bail!("budget must be > 0");
    }

    // Q4: egress consent — literal "yes" only.
    writeln!(
        output,
        "\nEgress: workers reach the web ONLY through the audited research-gateway.\n\
         Granting egress lets that gateway reach ANY host except your deny list,\n\
         with every request audited. Without it, deep research cannot fetch pages."
    )?;
    write!(output, "Type 'yes' to grant audited egress (anything else = skip): ")?;
    output.flush()?;
    let egress = read_line(input)? == "yes";

    Ok(WizardAnswers { model, count, budget_usd, egress })
}

pub fn cmd_setup(mur_home: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "`setup` is interactive; in scripts use \
             `mur deep-research provision --model <m> --count <n> [--grant-egress --yes]`"
        );
    }
    let registry = mur_common::model::ModelRegistry::load_from(
        &mur_common::model::ModelRegistry::default_path()?,
    )?;
    let choices: Vec<String> = registry.models.keys().cloned().collect();

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stdout();
    let a = run_wizard(&mut input, &mut output, &choices)?;

    // Provision (idempotent-ish: bails per existing behavior if workers exist —
    // see Step 3b). Egress grant inside provision prompts y/N per worker; the
    // wizard already collected the REAL consent word, so pass yes=true only
    // when the user typed "yes".
    super::provision::cmd_provision(
        mur_home,
        None,
        Some(a.count),
        Some(&a.model),
        a.egress,
        &[],
        a.egress, // suppress the per-worker y/N re-prompt only after literal-yes consent
        None,
    )?;

    // Fleet (skip when it already exists — provision does not create it).
    if !crate::cmd::fleet::store::fleet_path(mur_home, DEFAULT_FLEET_NAME).exists() {
        let members: Vec<String> = (1..=a.count)
            .map(|i| format!("{}_{i}", super::provision::DEFAULT_WORKER_PREFIX))
            .collect();
        crate::cmd::fleet::create::cmd_fleet_create(
            mur_home,
            DEFAULT_FLEET_NAME,
            members,
            None, // router defaults to the concierge
            Some("deep research".into()),
            None,
        )?;
    }

    // Persist budget on the fleet loop config.
    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, DEFAULT_FLEET_NAME)?;
    fleet.r#loop.budget_usd = Some(a.budget_usd);
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;

    println!("\nSetup complete. Run: mur deep-research \"<your question>\"");
    Ok(())
}
```

Implementer notes (verify, adapt mechanically, keep semantics):
- `Fleet`'s loop field name: check `grep -n "r#loop\|pub loop_" mur-common/src/fleet.rs` — use the real field path for `budget_usd`.
- If `cmd_provision` hard-fails when a worker already exists, wrap it: when ALL `dr_worker_1..count` profiles already exist, print "workers already provisioned — updating budget/fleet only" and skip the provision call (that is the idempotent re-run path from the spec).
- `IsTerminal` is std (Rust ≥1.70) — no new dependency.

- [ ] **Step 4: Wire CLI**

`actions.rs` — add to `DeepResearchAction`:

```rust
/// Interactive first-time setup: model, worker count, budget, egress consent
Setup,
```

`dispatch.rs` — add arm:

```rust
(Some(DeepResearchAction::Setup), _) => cmd::deep_research::setup::cmd_setup(&mur_home)?,
```

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p mur-core deep_research::setup`
Expected: 4 passed. Also `cargo run -q -- deep-research setup < /dev/null` → errors with the non-interactive hint.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add -A mur-core/src
git commit -m "feat(deep-research): interactive setup wizard (T3)"
```

---

### Task 4: Smart run — `mur deep-research "question"`

**Files:**
- Create: `mur-core/src/cmd/deep_research/ask.rs`
- Modify: `mur-core/src/cmd/deep_research/mod.rs` (add `pub mod ask;`)
- Modify: `mur-core/src/dispatch.rs` (replace the Task-2 stub arm)

**Interfaces:**
- Consumes: `status::{collect_status, DEFAULT_FLEET_NAME}`; `crate::cmd::agent::start::cmd_start(name)`; `crate::cmd::agent_mcp_pin::cmd_mcp_pin` (check its real signature with `grep -n "pub fn cmd_mcp_pin" -A8 mur-core/src/cmd/agent_mcp_pin.rs`; call with force=true, non-interactive); `crate::cmd::fleet::store::{load_fleet, save_fleet}`; `super::run::cmd_deep_research_run(mur_home, name, max_iterations, deadline, budget_usd)` (async); `provision::GATEWAY_MCP_NAME`.
- Produces:
  - `pub enum PreflightAction { StartWorker(String), RepinGateway(String) }`
  - `pub fn plan_preflight(s: &DeepResearchStatus) -> anyhow::Result<Vec<PreflightAction>>` — pure; **errors** when any worker lacks egress or no workers exist (message points at `setup`), never plans a grant.
  - `pub async fn cmd_ask(mur_home: &Path, question: &str) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing tests** (in `ask.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deep_research::status::{DeepResearchStatus, WorkerStatus};

    fn worker(name: &str, running: bool, egress: bool) -> WorkerStatus {
        WorkerStatus { name: name.into(), running, egress_granted: egress }
    }

    #[test]
    fn no_workers_errors_pointing_at_setup() {
        let s = DeepResearchStatus { workers: vec![], fleet_exists: false, model: None };
        let err = plan_preflight(&s).unwrap_err().to_string();
        assert!(err.contains("mur deep-research setup"));
    }

    #[test]
    fn missing_egress_errors_and_never_plans_a_grant() {
        let s = DeepResearchStatus {
            workers: vec![worker("dr_worker_1", true, false)],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let err = plan_preflight(&s).unwrap_err().to_string();
        assert!(err.contains("egress"));
        assert!(err.contains("setup"));
    }

    #[test]
    fn stopped_worker_planned_for_start_and_repin_always() {
        let s = DeepResearchStatus {
            workers: vec![worker("dr_worker_1", false, true), worker("dr_worker_2", true, true)],
            fleet_exists: true,
            model: Some("m".into()),
        };
        let plan = plan_preflight(&s).unwrap();
        assert!(plan.iter().any(|a| matches!(a, PreflightAction::StartWorker(n) if n == "dr_worker_1")));
        assert!(!plan.iter().any(|a| matches!(a, PreflightAction::StartWorker(n) if n == "dr_worker_2")));
        // One re-pin per worker (idempotent, covers binary-swap drift):
        assert_eq!(
            plan.iter().filter(|a| matches!(a, PreflightAction::RepinGateway(_))).count(),
            2
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core deep_research::ask`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! `mur deep-research "question"` — preflight + safe auto-repair + run.
//!
//! Auto-repair is LIMITED to starting workers and re-pinning the gateway.
//! Egress/grants are never touched here (explicit consent lives in setup).

use std::path::Path;

use anyhow::{bail, Result};

use super::status::{collect_status, DeepResearchStatus, DEFAULT_FLEET_NAME};

pub enum PreflightAction {
    StartWorker(String),
    RepinGateway(String),
}

pub fn plan_preflight(s: &DeepResearchStatus) -> Result<Vec<PreflightAction>> {
    if s.workers.is_empty() {
        bail!("no deep-research workers found — run `mur deep-research setup` first");
    }
    if let Some(w) = s.workers.iter().find(|w| !w.egress_granted) {
        bail!(
            "worker {} has no audited egress grant — run `mur deep-research setup` \
             (egress is an explicit consent step; it is never granted automatically)",
            w.name
        );
    }
    if !s.fleet_exists {
        bail!("fleet '{DEFAULT_FLEET_NAME}' missing — run `mur deep-research setup`");
    }
    let mut plan = Vec::new();
    for w in &s.workers {
        if !w.running {
            plan.push(PreflightAction::StartWorker(w.name.clone()));
        }
        // Unconditional idempotent re-pin: cheaper than drift detection and
        // covers the known gateway-binary-swap failure mode.
        plan.push(PreflightAction::RepinGateway(w.name.clone()));
    }
    Ok(plan)
}

pub async fn cmd_ask(mur_home: &Path, question: &str) -> Result<()> {
    let status = collect_status(mur_home, DEFAULT_FLEET_NAME);
    for action in plan_preflight(&status)? {
        match action {
            PreflightAction::StartWorker(name) => {
                println!("starting worker {name} …");
                crate::cmd::agent::start::cmd_start(&name)?;
            }
            PreflightAction::RepinGateway(name) => {
                // force=true, non-interactive; adapt args to the real
                // cmd_mcp_pin signature (see Interfaces note).
                crate::cmd::agent_mcp_pin::cmd_mcp_pin(
                    &name,
                    super::provision::GATEWAY_MCP_NAME,
                    true, // force
                    true, // yes / non-interactive
                )?;
            }
        }
    }

    // The question becomes the fleet goal; the existing run loop reads it.
    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, DEFAULT_FLEET_NAME)?;
    fleet.goal = question.to_string();
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;

    // Budget comes from fleet.yaml loop.budget_usd (set by setup); pass None
    // overrides so the existing precedence applies unchanged.
    super::run::cmd_deep_research_run(mur_home, DEFAULT_FLEET_NAME, None, None, None).await?;

    println!(
        "\nreport: see the converged synthesis in the fleet channel — \
         `mur channel show fleet-{DEFAULT_FLEET_NAME}` (workers left running)"
    );
    Ok(())
}
```

Implementer notes:
- `cmd_mcp_pin` real signature: adapt the call (it may take `mur_home`/stdin-confirm style like elsewhere; keep force + non-interactive semantics). If it requires MUR_HOME env like `grant_egress` does, follow the `grant_egress` pattern in `provision.rs:222`.
- `cmd_start` signature is `pub fn cmd_start(name: &str) -> Result<()>` (verified). If workers need a beat to bind their socket before the run loop dials, poll `status::is_agent_running` for up to ~10 s after starting (simple `std::thread::sleep(250ms)` loop; bail with a clear error on timeout).
- Final report location: check what `cmd_deep_research_run` already prints on convergence; if it prints a report path, drop the extra channel hint and reuse its output.

- [ ] **Step 4: Replace the dispatch stub**

```rust
(None, Some(q)) => cmd::deep_research::ask::cmd_ask(&mur_home, &q).await?,
```

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p mur-core deep_research`
Expected: all status/panel/setup/ask tests pass.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add -A mur-core/src
git commit -m "feat(deep-research): smart run with safe preflight auto-repair (T4)"
```

---

### Task 5: Docs + operator E2E checklist

**Files:**
- Modify: `README.md` (deep-research section: add the 3-command UX)
- Modify: `CLAUDE.md` (CLI surface: one line for the new bare/setup/question forms)
- Modify: `docs/architecture/runtime-overview.md` (deep-research subsection, same content)

**Interfaces:** none (docs only).

- [ ] **Step 1: Update docs**

Add to each location (adjust heading levels to fit):

```markdown
Deep research, simplified:

    mur deep-research setup        # one-time wizard: model, workers, budget, egress consent
    mur deep-research              # status panel
    mur deep-research "question"   # preflight (start workers, re-pin gateway) + guarded run

`provision` / `run` remain as the flag-based advanced path. Egress is only ever
granted in `setup`/`provision --grant-egress` (explicit consent); the smart run
never touches grants.
```

- [ ] **Step 2: Operator E2E (manual — fleet loop is not automatable)**

Checklist for the operator run (record results in the PR body):
1. Fresh `MUR_HOME` (or `--purge` old workers): `mur deep-research` → prints "not set up".
2. `mur deep-research setup` → wizard completes; typing `y` at egress does NOT grant.
3. Re-run `setup` → idempotent (no duplicate workers).
4. `mur deep-research "Compare Ollama and LM Studio in three cited paragraphs"` → workers auto-start, run converges, report reachable.
5. `mur fleet stop deep-research` mid-run → loop bails (kill-switch intact).

- [ ] **Step 3: Commit**

```bash
git add README.md CLAUDE.md docs/architecture/runtime-overview.md
git commit -m "docs(deep-research): simplified setup/status/ask UX (T5)"
```

---

## Phase 2 (Hub GUI page)

Separate subsystem → separate plan, written after Phase 1 merges (its Tauri commands wrap `collect_status` / `run_wizard`-equivalent answers / `cmd_ask` from this plan). Do not start it from this document.

## Self-review notes

- Spec coverage: wizard (T3), bare panel (T2), smart run + auto-repair limits (T4), budget persistence (T3), docs (T5), Phase 2 deferred explicitly. Egress-consent invariant encoded as tests in T3 (`egress_requires_literal_yes`) and T4 (`missing_egress_errors_and_never_plans_a_grant`).
- Known adaptation points are flagged inline with the exact grep to run (`model_ref` field, `Fleet` loop field, `cmd_mcp_pin` signature, provision-exists behavior) — these are interface confirmations, not placeholders.
- Type consistency: `DeepResearchStatus`/`WorkerStatus` defined once in T1 and consumed by name in T2/T4; `DEFAULT_FLEET_NAME` single-sourced in T1.
