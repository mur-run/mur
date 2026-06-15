//! CLI driver for `mur agent wizard`: terminal prompts, progress printing, review gate.
use crate::agent_wizard::{self, catalog, stages::WizardHooks, Progress};
use crate::agent_wizard::draft::WizardDraft;

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

pub fn run(
    role: Option<String>,
    workspace: Option<String>,
    headless: bool,
    _no_llm: bool,
) -> anyhow::Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let catalog = catalog::load_catalog(&mur_home);

    let role_id = match role {
        Some(r) => r,
        None => prompt_role_choice(&catalog)?,
    };
    let manifest = catalog
        .iter()
        .find(|m| m.id == role_id)
        .ok_or_else(|| {
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
    let mut hooks = CliHooks { headless };
    let outcome = agent_wizard::run_wizard(manifest, &ws, "claude_sonnet", &mut hooks)?;
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
