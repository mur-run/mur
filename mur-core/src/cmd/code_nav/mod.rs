//! `mur code-nav setup` (plan 2026-10-03 code-nav, Phase 3; decision P3-D1:
//! a standalone command shaped like `mur browser setup`).
//!
//! The pure planner (3.1), both installers (3.2, 3.3), the serena config
//! generator (3.4), the profile entry (3.5), the consent flow that
//! applies them (3.6, [`setup::run`]), the pre-installed pyright (3.6b), and the
//! pre-installed intelephense (#1945), and the sealed smoke launch that
//! closes setup ([`smoke`], #1981), and the Vue server linked into serena's
//! resources dir ([`vue_install`], #1982).

pub mod ast_grep_install;
pub mod ast_grep_wire;
pub mod consent;
pub mod doctor;
pub mod node_ls_install;
pub mod plan;
pub mod pyright_install;
pub mod serena_config;
pub mod serena_entry;
pub mod serena_install;
pub mod serena_project;
pub mod setup;
pub mod smoke;
pub mod smoke_fetch;
pub mod smoke_probe;
pub mod vue_install;
pub mod wizard;

/// Every `mur code-nav setup` flag, as clap parsed it.
pub struct SetupCli {
    pub agent: Option<String>,
    pub wizard: bool,
    pub agents: Vec<String>,
    pub paths: Vec<std::path::PathBuf>,
    pub restart: bool,
    pub with_serena: bool,
    pub project: Option<std::path::PathBuf>,
    pub entry: String,
    pub no_ast_grep: bool,
    pub lsp: Vec<String>,
    pub lsp_only: Vec<String>,
    pub yes: bool,
}

/// `mur code-nav setup`, with or without `--wizard`. Blocking: call it off
/// the async runtime.
pub fn run_setup(c: SetupCli) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    let consent = crate::cmd::browser::setup::Consent::new(std::io::stdin().is_terminal(), c.yes);
    let (input, out) = (&mut std::io::stdin().lock(), &mut std::io::stdout());
    if c.wizard {
        let args = wizard::Args {
            agents: c.agents,
            paths: c.paths,
            restart: c.restart,
        };
        return wizard::run(args, consent, input, out);
    }
    let exact_lsp = !c.lsp_only.is_empty();
    let args = setup::Args {
        agent: anyhow::Context::context(c.agent, "--agent is required without --wizard")?,
        project: c.project,
        entry: c.entry,
        flags: plan::Flags {
            with_serena: c.with_serena,
            no_ast_grep: c.no_ast_grep,
            exact_lsp,
            lsp: if exact_lsp { c.lsp_only } else { c.lsp },
        },
    };
    setup::run(args, consent, input, out)
}
