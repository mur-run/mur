use super::*;

pub(super) async fn run_deep_research(
    action: Option<DeepResearchAction>,
    question: Option<String>,
    run_id: Option<String>,
) -> Result<()> {
    let mur_home = crate::paths::mur_root(None);
    cmd::fleet::state_migrate::migrate_all(&mur_home);
    match (action, question) {
        (
            Some(DeepResearchAction::Provision {
                count,
                prefix,
                model,
                grant_egress,
                grant_browser,
                deny_hosts,
                yes,
                render_engine,
            }),
            _,
        ) => cmd::deep_research::provision::cmd_provision(
            &mur_home,
            prefix.as_deref(),
            count,
            model.as_deref(),
            grant_egress,
            grant_browser,
            &deny_hosts,
            yes,
            render_engine.as_deref(),
        )?,
        (Some(DeepResearchAction::Setup), _) => cmd::deep_research::setup::cmd_setup(&mur_home)?,
        (Some(DeepResearchAction::Doctor { render }), _) => {
            let mut exec = cmd::deep_research::browser::system_render_exec;
            cmd::deep_research::browser::doctor(
                &mur_home,
                &cmd::deep_research::browser::install_plan(&mur_common::deps::current_platform()),
                &mut std::io::stdout(),
                &mut cmd::deep_research::browser::system_runner,
                render.then_some(&mut exec as cmd::deep_research::browser::RenderExec<'_>),
            )?
        }
        (
            Some(DeepResearchAction::Secret {
                brave,
                tavily,
                serp_api,
                firecrawl,
                clear,
                list,
            }),
            _,
        ) => cmd::deep_research::secret::run(
            &mur_home.join("config.yaml"),
            brave,
            tavily,
            serp_api,
            firecrawl,
            clear,
            list,
        )?,
        (
            Some(DeepResearchAction::Run {
                name,
                max_iterations,
                deadline,
                budget_usd,
            }),
            _,
        ) => {
            cmd::deep_research::run::cmd_deep_research_run(
                &mur_home,
                &name,
                max_iterations,
                deadline,
                budget_usd,
                None,
                None,
            )
            .await?
        }
        (None, Some(q)) => cmd::deep_research::ask::cmd_ask(&mur_home, &q, run_id).await?,
        (Some(DeepResearchAction::Status), _) | (None, None) => {
            cmd::deep_research::panel::cmd_panel(&mur_home)?
        }
    }
    Ok(())
}
