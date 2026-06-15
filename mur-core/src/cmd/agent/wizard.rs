//! CLI driver for `mur agent wizard`: terminal prompts, progress printing, review gate.
use crate::agent_wizard::draft::WizardDraft;
use crate::agent_wizard::{self, Progress, catalog, stages::WizardHooks};

struct CliHooks {
    headless: bool,
}

impl WizardHooks for CliHooks {
    fn on_progress(&mut self, p: &Progress) {
        eprintln!("  [{:?}] {}", p.stage, p.message);
    }

    fn review_gate(&mut self, draft: WizardDraft) -> Option<WizardDraft> {
        println!("\n=== Review drafts for '{}' ===", draft.role.name);
        for s in &draft.skills {
            println!("- skill: {}", s.name);
        }
        println!("- prompt: {} chars", draft.prompt.markdown.len());
        println!(
            "- entitlements: write={:?} spawn={:?} hosts={:?}",
            draft.entitlements.allow_write,
            draft.entitlements.allow_spawn,
            draft.entitlements.allow_host
        );
        if self.headless {
            println!("(--headless: auto-approved)");
            return Some(draft);
        }
        print!("\nApprove and create this agent? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if line.trim().eq_ignore_ascii_case("y") {
            Some(draft)
        } else {
            println!("Aborted.");
            None
        }
    }
}

pub async fn run(
    role: Option<String>,
    workspace: Option<String>,
    headless: bool,
    no_llm: bool,
) -> anyhow::Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let catalog = catalog::load_catalog(&mur_home);

    let role_id = match role {
        Some(r) => r,
        None => prompt_role_choice(&catalog)?,
    };
    let manifest = catalog.iter().find(|m| m.id == role_id).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown role '{}'. Known: {:?}",
            role_id,
            catalog.iter().map(|m| &m.id).collect::<Vec<_>>()
        )
    })?;

    let ws = workspace.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string()
    });

    // Build the LLM client if the user hasn't opted out. Task 5 will wire the live
    // ChatBackendAdapter here; for now, no_llm=true is the only supported path in Plan 2
    // (the async run_wizard is ready; live backend wiring is Task 5).
    let llm: Option<std::sync::Arc<dyn crate::agent_wizard::llm::WizardLlm>> = if no_llm {
        None
    } else {
        // Task 5: build_llm_for_stage(&mur_home, None, "agent.wizard")
        // Until then, fall back to stub path with a warning so `mur agent wizard` still works.
        eprintln!(
            "warning: live LLM wiring pending (Task 5); using deterministic stubs. Pass --no-llm to silence."
        );
        None
    };

    let mut hooks = CliHooks { headless };
    let outcome =
        agent_wizard::run_wizard(manifest, &ws, "claude_sonnet", llm, None, &mut hooks).await?;
    if outcome.created {
        println!("\n✅ Created and started agent '{}'.", outcome.agent_name);
    } else {
        println!("\nNo agent created.");
    }
    Ok(())
}

fn prompt_role_choice(catalog: &[catalog::RoleManifest]) -> anyhow::Result<String> {
    println!("Choose a role preset:");
    for (i, m) in catalog.iter().enumerate() {
        println!("  {}) {} — {}", i + 1, m.id, m.charter);
    }
    println!("  (or type a new id for a custom role)");
    print!("> ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim();
    if let Ok(n) = s.parse::<usize>()
        && (1..=catalog.len()).contains(&n)
    {
        return Ok(catalog[n - 1].id.clone());
    }
    // Custom role: in Plan 1 (--no-llm), require it to already exist as a manifest.
    anyhow::bail!(
        "custom roles need an LLM (Plan 2) or a manifest in ~/.mur/wizard/roles/; \
        pick a listed preset for now"
    )
}
