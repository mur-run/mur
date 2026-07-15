//! `mur deep-research setup` — interactive first-time wizard.
//!
//! Pure Q&A over injected streams (`run_wizard`) + an orchestration wrapper
//! (`cmd_setup`) that reconciles the EXISTING provision/fleet state so a
//! re-run is idempotent: existing worker profiles are updated in place,
//! missing ones are created, surplus ones (count shrunk) are stopped and
//! dropped from the fleet's members (never deleted), and the fleet itself
//! is created only if missing — otherwise its members/budget are updated in
//! place, preserving channel history. Egress consent requires the literal
//! word "yes" — never defaulted or implied.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::provision::{DEFAULT_WORKER_COUNT, DEFAULT_WORKER_MODEL, DEFAULT_WORKER_PREFIX};
use super::status::{DEFAULT_FLEET_NAME, collect_status};

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
        model_choices
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_WORKER_MODEL.to_string())
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
    } else if model_choices.is_empty() || model_choices.iter().any(|m| m == &ans) {
        // Empty registry (fresh machine, nothing added yet) is an
        // intentional escape hatch — allow any typed name in that case.
        ans
    } else {
        bail!(
            "unknown model '{ans}' — not in the registry (run `mur model add` first, \
             or pick one of the listed choices)"
        );
    };

    // Q2: worker count
    write!(output, "Number of workers [{DEFAULT_WORKER_COUNT}]: ")?;
    output.flush()?;
    let ans = read_line(input)?;
    let count = if ans.is_empty() {
        DEFAULT_WORKER_COUNT
    } else {
        ans.parse::<usize>()
            .context("worker count must be a positive integer")?
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
        ans.parse::<f64>()
            .context("budget must be a number (USD)")?
    };
    if !budget_usd.is_finite() || budget_usd <= 0.0 {
        bail!("budget must be > 0");
    }

    // Q4: egress consent — literal "yes" only.
    writeln!(
        output,
        "\nEgress: workers reach the web ONLY through the audited research-gateway.\n\
         Granting egress lets that gateway reach ANY host except your deny list,\n\
         with every request audited. Without it, deep research cannot fetch pages."
    )?;
    write!(
        output,
        "Type 'yes' to grant audited egress (anything else = skip): "
    )?;
    output.flush()?;
    let egress = read_line(input)? == "yes";

    Ok(WizardAnswers {
        model,
        count,
        budget_usd,
        egress,
    })
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

    // `provision_one`/`grant_egress`/`load_profile_for_edit` all re-derive
    // their home directory from `MUR_HOME` (see provision.rs's `#
    // Concurrency` note) — set it once for this whole reconcile.
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }

    let status = collect_status(mur_home, DEFAULT_FLEET_NAME);
    let existing: std::collections::HashSet<String> =
        status.workers.iter().map(|w| w.name.clone()).collect();

    let target_names: Vec<String> = (1..=a.count)
        .map(|i| format!("{DEFAULT_WORKER_PREFIX}_{i}"))
        .collect();

    if target_names.iter().all(|n| existing.contains(n)) {
        println!("workers already provisioned — updating model/budget/fleet only");
    }
    let mut restart_needed: Vec<String> = Vec::new();
    for name in &target_names {
        if existing.contains(name) {
            let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
            let model_changed = profile.model_ref.as_deref() != Some(a.model.as_str());
            profile.model_ref = Some(a.model.clone());
            crate::cmd::agent::save_profile(&path, &mut profile)?;
            // The runtime loads its profile once at startup — a running worker
            // keeps the OLD model until restarted (we never auto-restart: it
            // could kill an in-flight conversation).
            if model_changed && super::status::is_agent_running(mur_home, name) {
                restart_needed.push(name.clone());
            }
        } else {
            super::provision::provision_one(mur_home, name, &a.model, None)?;
            println!("provisioned {name}");
        }
    }
    if !restart_needed.is_empty() {
        println!(
            "⚠ {} running with the previous model — restart to apply {}:\n  mur agent stop {}  # then start them again",
            restart_needed.join(", "),
            a.model,
            restart_needed.join(" ")
        );
    }

    // Workers above the new count (count shrunk on a re-run): stop them,
    // never delete the profile — removal stays a manual `mur agent remove`.
    for w in &status.workers {
        if !target_names.contains(&w.name) {
            match crate::cmd::agent::lifecycle::cmd_stop(&w.name) {
                Ok(()) => println!("stopped {} (no longer in the worker count)", w.name),
                Err(e) => {
                    // Already stopped, or some other benign state — never
                    // fail setup over a worker that isn't running.
                    println!("{} left as-is ({e})", w.name);
                }
            }
        }
    }

    // Fleet: create only if missing; otherwise update members/budget in
    // place — the channel and its history are preserved, never recreated.
    let fleet_path = crate::cmd::fleet::store::fleet_path(mur_home, DEFAULT_FLEET_NAME);
    if !fleet_path.exists() {
        crate::cmd::fleet::create::cmd_fleet_create(
            mur_home,
            DEFAULT_FLEET_NAME,
            target_names.clone(),
            None, // router defaults to the concierge
            Some("deep research".into()),
            None,
        )?;
    }

    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, DEFAULT_FLEET_NAME)?;
    fleet.members = target_names.clone();
    let mut loop_cfg = fleet
        .loop_cfg
        .take()
        .unwrap_or(mur_common::fleet::FleetLoop {
            trigger: "manual".to_string(),
            max_iterations: 0,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: String::new(),
        });
    loop_cfg.budget_usd = a.budget_usd;
    fleet.loop_cfg = Some(loop_cfg);
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;

    // Egress: only when the wizard answer is the literal "yes", and only
    // for workers that don't already have it — never revoke.
    if a.egress {
        for w in &status.workers {
            if target_names.contains(&w.name) && !w.egress_granted {
                super::provision::grant_egress(mur_home, &w.name, &[], true)?;
            }
        }
        // Newly-provisioned workers in this run also need the grant (they
        // weren't in `status.workers` since they didn't exist yet).
        for name in &target_names {
            if !existing.contains(name) {
                super::provision::grant_egress(mur_home, name, &[], true)?;
            }
        }
    }

    // Seed the fleet_run authorization so the concierge can trigger THIS
    // fleet from inside murmur (the setup wizard is the user's explicit
    // consent moment — same starter block `mur init` seeds on fresh
    // installs). Appends only if no fleet_run key exists; user-authored
    // settings are never touched. Requires a concierge restart to apply.
    if crate::cmd::init::append_fleet_run_if_absent(&mur_home.join("config.yaml"))? {
        println!(
            "✓ Authorized the concierge to run deep-research from murmur \
             (config.yaml fleet_run) — restart the `mur` agent to apply"
        );
    }

    println!("\nSetup complete. Run: mur deep-research \"<your question>\"");
    Ok(())
}

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

    #[test]
    fn unknown_typed_model_rejected() {
        let err = match answers("nonexistent_model\n\n\nno\n", &["claude_haiku"]) {
            Ok(_) => panic!("expected an error for an unknown model name"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("nonexistent_model"));
    }
}
