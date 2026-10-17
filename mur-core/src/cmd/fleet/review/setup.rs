//! #1762: `mur fleet review-setup` — create or reconcile a main/reviewer pair
//! that `mur fleet review` can run against without hand-built agents.
//!
//! Same shape as `mur deep-research setup`: [`run_wizard`] is pure Q&A over
//! injected streams, [`plan_member`] is the pure reconcile decision, and
//! [`cmd_review_setup`] applies it through the existing agent commands.
//!
//! Reconcile rules (idempotent):
//! - a missing agent is created; an existing one is never deleted;
//! - `hitl.timeout_secs` is raised to the stuck window, never lowered;
//! - a system prompt is replaced only when it is the `mur agent create`
//!   placeholder or the role prompt this command last wrote — a hand-edited
//!   prompt is kept unless the user confirms;
//! - the reviewer's write-tool denials are added on creation only, so a user
//!   who later opened them up is not overridden on a re-run;
//! - fleet channels are not touched: sessions create their own.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::agent::ToolPolicy;
use mur_common::limits::Stuck;
use sha2::{Digest, Sha256};

use super::constants::{
    DEFAULT_REVIEW_MAIN_AGENT, DEFAULT_REVIEW_REVIEWER_AGENT, REVIEW_MAIN_ROLE_PROMPT,
    REVIEW_REVIEWER_ROLE_PROMPT, REVIEW_SETUP_PROMPT_HASH_FILE, REVIEWER_DENIED_TOOLS,
};
use super::preflight::required_hitl_timeout;
use super::schema::Role;

/// Flags as typed. `None` = ask (interactive) or use the default.
#[derive(Debug, Default, Clone)]
pub struct SetupArgs {
    pub main: Option<String>,
    pub reviewer: Option<String>,
    pub main_model: Option<String>,
    pub reviewer_model: Option<String>,
    pub no_interactive: bool,
    /// Replace a hand-edited system prompt with the role prompt.
    pub overwrite_prompts: bool,
}

/// What the wizard settled on for one side of the pair.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberChoice {
    pub name: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WizardAnswers {
    pub main: MemberChoice,
    pub reviewer: MemberChoice,
}

/// What `review-setup` knows about one agent before it changes anything.
#[derive(Debug, Clone, Default)]
pub struct MemberState {
    pub exists: bool,
    pub model_ref: Option<String>,
    pub hitl_timeout_secs: u32,
    /// Current `sys_prompt.md`, `None` when unreadable or absent.
    pub sys_prompt: Option<String>,
    /// Hash `review-setup` recorded the last time it wrote the prompt.
    pub recorded_prompt_hash: Option<String>,
}

/// How the role prompt is handled for one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAction {
    /// Already the current role prompt.
    Keep,
    /// Placeholder or our own older prompt: safe to replace.
    Write,
    /// Hand-edited: kept unless the user confirms (or `--overwrite-prompts`).
    UserEdited,
}

/// The reconcile decision for one member; applying it is [`apply_member`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberPlan {
    pub create: bool,
    /// `Some` = write this `model_ref`.
    pub set_model: Option<String>,
    /// `Some` = raise `hitl.timeout_secs` to this.
    pub raise_timeout: Option<u32>,
    pub prompt: PromptAction,
}

impl MemberPlan {
    /// Whether a running agent has to restart to pick the change up (the
    /// runtime reads its profile and prompt once, at startup).
    fn needs_restart(&self, prompt_written: bool) -> bool {
        self.set_model.is_some() || self.raise_timeout.is_some() || prompt_written
    }
}

pub(crate) fn role_prompt(role: Role) -> &'static str {
    match role {
        Role::Main => REVIEW_MAIN_ROLE_PROMPT,
        Role::Reviewer => REVIEW_REVIEWER_ROLE_PROMPT,
    }
}

pub(crate) fn prompt_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The pure reconcile decision. `required_timeout` is `None` when stuck
/// detection is off (no window to outlast).
pub(crate) fn plan_member(
    name: &str,
    role: Role,
    model: &str,
    state: &MemberState,
    required_timeout: Option<u32>,
) -> MemberPlan {
    if !state.exists {
        return MemberPlan {
            create: true,
            set_model: None, // `cmd_create` binds it
            raise_timeout: required_timeout,
            prompt: PromptAction::Write,
        };
    }
    let target = role_prompt(role);
    let prompt = match state.sys_prompt.as_deref() {
        Some(p) if p == target => PromptAction::Keep,
        None => PromptAction::Write,
        Some(p) if p == crate::cmd::agent::lifecycle::default_sys_prompt(name) => {
            PromptAction::Write
        }
        Some(p) if state.recorded_prompt_hash.as_deref() == Some(prompt_hash(p).as_str()) => {
            PromptAction::Write
        }
        Some(_) => PromptAction::UserEdited,
    };
    MemberPlan {
        create: false,
        set_model: (state.model_ref.as_deref() != Some(model)).then(|| model.to_string()),
        raise_timeout: required_timeout.filter(|need| state.hitl_timeout_secs < *need),
        prompt,
    }
}

fn read_line(input: &mut dyn BufRead) -> Result<String> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("review-setup aborted: end of input");
    }
    Ok(line.trim().to_string())
}

/// A model pick: Enter = `default`, a number = that row, or a registry key.
fn pick_model(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    label: &str,
    choices: &[String],
    default: &str,
) -> Result<String> {
    loop {
        write!(output, "{label} model, number or name [{default}]: ")?;
        output.flush()?;
        let ans = read_line(input)?;
        if ans.is_empty() {
            return Ok(default.to_string());
        }
        if let Ok(n) = ans.parse::<usize>() {
            if let Some(m) = n.checked_sub(1).and_then(|i| choices.get(i)) {
                return Ok(m.clone());
            }
            writeln!(output, "  no model #{n}")?;
            continue;
        }
        if choices.iter().any(|m| m == &ans) {
            return Ok(ans);
        }
        writeln!(output, "  '{ans}' is not in models.yaml")?;
    }
}

fn pick_name(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    label: &str,
    default: &str,
) -> Result<String> {
    write!(output, "{label} agent name [{default}]: ")?;
    output.flush()?;
    let ans = read_line(input)?;
    Ok(if ans.is_empty() {
        default.to_string()
    } else {
        ans
    })
}

/// The default model for a side: the flag, else the agent's current
/// `model_ref`, else (main) the first registry key / (reviewer) the first key
/// that differs from main's, so diversity is the zero-effort path.
fn default_model(
    flag: Option<&str>,
    current: Option<&str>,
    choices: &[String],
    avoid: Option<&str>,
) -> Option<String> {
    flag.or(current)
        .map(str::to_string)
        .or_else(|| choices.iter().find(|m| Some(m.as_str()) != avoid).cloned())
        .or_else(|| choices.first().cloned())
}

/// Pure Q&A. `current_model` reports an existing agent's `model_ref` so the
/// default keeps it. Non-interactive runs never call this.
pub fn run_wizard(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    args: &SetupArgs,
    choices: &[String],
    current_model: &dyn Fn(&str) -> Option<String>,
) -> Result<WizardAnswers> {
    if choices.is_empty() {
        bail!(
            "models.yaml has no models: add one first with `mur model add` or `mur model connect`"
        );
    }
    let main = match &args.main {
        Some(n) => n.clone(),
        None => pick_name(input, output, "Main", DEFAULT_REVIEW_MAIN_AGENT)?,
    };
    let reviewer = match &args.reviewer {
        Some(n) => n.clone(),
        None => pick_name(input, output, "Reviewer", DEFAULT_REVIEW_REVIEWER_AGENT)?,
    };
    if main == reviewer {
        bail!("main and reviewer must be different agents (got '{main}' for both)");
    }
    writeln!(output, "Models in models.yaml:")?;
    for (i, m) in choices.iter().enumerate() {
        writeln!(output, "  {}. {m}", i + 1)?;
    }
    let main_model = match &args.main_model {
        Some(m) => m.clone(),
        None => {
            let d = default_model(None, current_model(&main).as_deref(), choices, None)
                .context("no default model")?;
            pick_model(input, output, "Main", choices, &d)?
        }
    };
    let reviewer_model = match &args.reviewer_model {
        Some(m) => m.clone(),
        None => {
            let d = default_model(
                None,
                current_model(&reviewer).as_deref(),
                choices,
                Some(&main_model),
            )
            .context("no default model")?;
            pick_model(input, output, "Reviewer", choices, &d)?
        }
    };
    Ok(WizardAnswers {
        main: MemberChoice {
            name: main,
            model: main_model,
        },
        reviewer: MemberChoice {
            name: reviewer,
            model: reviewer_model,
        },
    })
}

/// `--no-interactive`: every gap is filled from defaults, nothing is asked.
pub fn answers_from_flags(
    args: &SetupArgs,
    choices: &[String],
    current_model: &dyn Fn(&str) -> Option<String>,
) -> Result<WizardAnswers> {
    let main = args
        .main
        .clone()
        .unwrap_or_else(|| DEFAULT_REVIEW_MAIN_AGENT.to_string());
    let reviewer = args
        .reviewer
        .clone()
        .unwrap_or_else(|| DEFAULT_REVIEW_REVIEWER_AGENT.to_string());
    if main == reviewer {
        bail!("--main and --reviewer must be different agents (got '{main}' for both)");
    }
    let need =
        "no model to use: pass --main-model / --reviewer-model or add one with `mur model add`";
    let main_model = default_model(
        args.main_model.as_deref(),
        current_model(&main).as_deref(),
        choices,
        None,
    )
    .context(need)?;
    let reviewer_model = default_model(
        args.reviewer_model.as_deref(),
        current_model(&reviewer).as_deref(),
        choices,
        Some(&main_model),
    )
    .context(need)?;
    Ok(WizardAnswers {
        main: MemberChoice {
            name: main,
            model: main_model,
        },
        reviewer: MemberChoice {
            name: reviewer,
            model: reviewer_model,
        },
    })
}

/// A model that is not a registry key would leave `model_ref` unresolvable.
fn check_models(answers: &WizardAnswers, choices: &[String]) -> Result<()> {
    for m in [&answers.main.model, &answers.reviewer.model] {
        if !choices.iter().any(|c| c == m) {
            bail!("model '{m}' is not in models.yaml (see `mur model list`)");
        }
    }
    Ok(())
}

pub(crate) fn read_member_state(mur_home: &Path, name: &str) -> Result<MemberState> {
    let dir = mur_home.join("agents").join(name);
    let path = dir.join("profile.yaml");
    if !path.is_file() {
        return Ok(MemberState::default());
    }
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;
    Ok(MemberState {
        exists: true,
        model_ref: profile.model_ref.clone(),
        hitl_timeout_secs: profile.hitl.timeout_secs,
        sys_prompt: std::fs::read_to_string(dir.join(&profile.sys_prompt_file)).ok(),
        recorded_prompt_hash: std::fs::read_to_string(dir.join(REVIEW_SETUP_PROMPT_HASH_FILE))
            .ok()
            .map(|s| s.trim().to_string()),
    })
}

/// Apply one member's plan. Returns whether the prompt was written. Uses the
/// agent commands, which resolve the MUR home themselves (see the caller).
fn apply_member(
    mur_home: &Path,
    choice: &MemberChoice,
    role: Role,
    plan: &MemberPlan,
    write_prompt: bool,
) -> Result<bool> {
    use crate::cmd::agent::{load_profile_for_edit, save_profile};
    if plan.create {
        crate::cmd::agent::lifecycle::cmd_create(
            &choice.name,
            true,
            None,
            Some(choice.model.clone()),
            None,
        )?;
    }
    let (path, mut profile) = load_profile_for_edit(&choice.name)?;
    if let Some(m) = &plan.set_model {
        profile.model_ref = Some(m.clone());
    }
    if let Some(t) = plan.raise_timeout {
        profile.hitl.timeout_secs = t;
    }
    if plan.create && role == Role::Reviewer {
        for tool in REVIEWER_DENIED_TOOLS {
            crate::cmd::deep_research::provision::upsert_tool_rule(
                &mut profile.entitlements.tools,
                tool.to_string(),
                ToolPolicy::Deny,
            );
        }
    }
    save_profile(&path, &mut profile)?;
    if write_prompt {
        let dir = mur_home.join("agents").join(&choice.name);
        let text = role_prompt(role);
        crate::yaml_edit::write_atomic(&dir.join(&profile.sys_prompt_file), text.as_bytes())?;
        crate::yaml_edit::write_atomic(
            &dir.join(REVIEW_SETUP_PROMPT_HASH_FILE),
            prompt_hash(text).as_bytes(),
        )?;
    }
    Ok(write_prompt)
}

/// Yes only for a literal `y` / `yes`; the safe answer (keep) is the default.
fn confirm(input: &mut dyn BufRead, output: &mut dyn Write, question: &str) -> Result<bool> {
    write!(output, "{question} [y/N]: ")?;
    output.flush()?;
    Ok(matches!(
        read_line(input)?.to_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// The stuck window `mur fleet review` will run with, resolved the same way
/// the session resolves it, so the timeout raised here is what preflight wants.
fn session_stuck(mur_home: &Path) -> Result<Stuck> {
    let probe = super::session::session_fleet("review-setup", vec![], String::new(), "");
    Ok(crate::cmd::fleet::loop_run::fleet_bounds(mur_home, &probe, None, None)?.stuck)
}

pub fn cmd_review_setup(mur_home: &Path, args: SetupArgs) -> Result<()> {
    let interactive = !args.no_interactive;
    if interactive && !std::io::stdin().is_terminal() {
        bail!(
            "`review-setup` is interactive; in scripts pass --no-interactive \
             (with --main / --reviewer / --main-model / --reviewer-model as needed)"
        );
    }
    let registry = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml"))?;
    let choices: Vec<String> = registry.models.keys().cloned().collect();
    let canon = |n: &str| crate::a2a_dial::canonicalize_agent_name(mur_home, n);
    let current = |n: &str| {
        read_member_state(mur_home, &canon(n))
            .ok()
            .and_then(|s| s.model_ref)
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let mut answers = if interactive {
        run_wizard(&mut input, &mut out, &args, &choices, &current)?
    } else {
        answers_from_flags(&args, &choices, &current)?
    };
    answers.main.name = canon(&answers.main.name);
    answers.reviewer.name = canon(&answers.reviewer.name);
    check_models(&answers, &choices)?;
    if let (Some(a), Some(b)) = (
        registry.models.get(&answers.main.model),
        registry.models.get(&answers.reviewer.model),
    ) && a.provider == b.provider
    {
        println!(
            "note: main and reviewer both use provider '{}'; a reviewer on a different \
             provider catches more of what the main model misses",
            a.provider
        );
    }

    let need = required_hitl_timeout(session_stuck(mur_home)?);
    // The agent commands below (`cmd_create`, `load_profile_for_edit`, …)
    // re-resolve the home themselves; `mur_home` comes from the same resolver
    // (`paths::mur_root(None)` in dispatch), so both name one directory and no
    // environment mutation is needed.

    for (choice, role) in [
        (&answers.main, Role::Main),
        (&answers.reviewer, Role::Reviewer),
    ] {
        let state = read_member_state(mur_home, &choice.name)?;
        let plan = plan_member(&choice.name, role, &choice.model, &state, need);
        let write_prompt = match plan.prompt {
            PromptAction::Keep => false,
            PromptAction::Write => true,
            PromptAction::UserEdited if args.overwrite_prompts => true,
            PromptAction::UserEdited if interactive => confirm(
                &mut input,
                &mut out,
                &format!(
                    "'{}' has a hand-edited system prompt. Replace it with the review role prompt?",
                    choice.name
                ),
            )?,
            PromptAction::UserEdited => {
                println!(
                    "kept '{}''s hand-edited system prompt (pass --overwrite-prompts to replace it)",
                    choice.name
                );
                false
            }
        };
        let was_running = !plan.create
            && crate::cmd::deep_research::status::is_agent_running(mur_home, &choice.name);
        let wrote = apply_member(mur_home, choice, role, &plan, write_prompt)?;
        if plan.create {
            println!("created '{}' ({})", choice.name, choice.model);
        } else {
            println!("updated '{}'", choice.name);
        }
        if was_running && plan.needs_restart(wrote) {
            crate::cmd::agent::cmd_restart(
                std::slice::from_ref(&choice.name),
                false,
                false,
                false,
            )?;
        } else {
            crate::cmd::agent::start::cmd_start(&choice.name)?;
        }
    }

    super::preflight::preflight(
        mur_home,
        &[&answers.main.name, &answers.reviewer.name],
        session_stuck(mur_home)?,
    )
    .context("the agents were set up but the review preflight still fails")?;

    println!(
        "\nReady. The reviewer can read but not edit; grant the main agent the repo it works on:\n  \
         mur agent perm allow-write {main} <repo path>\n\nThen run:\n  \
         mur fleet review --main {main} --reviewer {reviewer} \"<task>\"",
        main = answers.main.name,
        reviewer = answers.reviewer.name,
    );
    Ok(())
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod setup_tests;
