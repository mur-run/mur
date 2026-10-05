use super::*;

pub(super) async fn run_browser(action: BrowserAction) -> Result<()> {
    match action {
        BrowserAction::Record {
            run,
            profile,
            mode,
            trace,
            extra,
        } => cmd::browser::record(&run, profile.as_deref(), &mode, trace, &extra).await?,
        BrowserAction::Replay {
            name,
            profile,
            heal,
            max_heal_ratio,
            dry_run,
        } => cmd::browser::replay(&name, profile.as_deref(), heal, max_heal_ratio, dry_run).await?,
        BrowserAction::Auth {
            site,
            url,
            reauth,
            browser,
            allow_domain,
        } => cmd::browser::auth(&site, &url, reauth, browser, &allow_domain).await?,
        BrowserAction::Broker => cmd::browser::broker().await?,
        BrowserAction::List => cmd::browser::list()?,
        BrowserAction::Show { name } => cmd::browser::show(&name)?,
        BrowserAction::Export { name, out } => cmd::browser::export(&name, out.as_deref())?,
        BrowserAction::Status => cmd::browser::status()?,
        BrowserAction::Doctor { live } => {
            let home = dirs::home_dir();
            let browsers =
                cmd::browser::doctor::browsers_dir(&|k: &str| std::env::var_os(k), home.as_deref());
            let mut out = std::io::stdout();
            cmd::browser::doctor::doctor(
                &mut out,
                // The PATH `playwright_command` spawns `npx` with, not
                // the augmented one — a pass must mean replay finds it.
                &std::env::var_os("PATH").unwrap_or_default(),
                browsers.as_deref(),
                &mut cmd::browser::doctor::system_probe,
            )?;
            if live {
                cmd::browser::doctor::live_check(&mut out).await?;
            } else {
                println!("  (install check only — add --live to launch a headless browser)");
            }
        }
        BrowserAction::Setup { agent, yes } => {
            use std::io::IsTerminal;
            let home = dirs::home_dir();
            let browsers =
                cmd::browser::doctor::browsers_dir(&|k: &str| std::env::var_os(k), home.as_deref());
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            let consent = cmd::browser::setup::Consent::new(stdin.is_terminal(), yes);
            cmd::browser::setup::prepare(
                consent,
                &mut stdin.lock(),
                &mut out,
                // Same PATH as doctor and replay: the install must use
                // the `npx` replay will spawn.
                &std::env::var_os("PATH").unwrap_or_default(),
                browsers.as_deref(),
                &mut cmd::browser::doctor::system_probe,
                &mut cmd::browser::setup::system_installer,
            )?;
            // Before the grants: `allow-read` refuses a path that does
            // not exist yet, and the install dir is one of them.
            cmd::browser::server_install::ensure(
                consent,
                &cmd::agent::resolve_mur_home()?,
                &mut stdin.lock(),
                &mut out,
                &mut cmd::browser::server_install::system_installer,
            )?;
            cmd::browser::setup::grant_perms(
                agent.as_deref(),
                consent,
                &mut stdin.lock(),
                &mut out,
            )?;
            cmd::browser::doctor::live_check(&mut out).await?;
            // After the live test: the entry is only useful once rendering
            // was proven, and the probe it runs needs the grants above.
            cmd::browser::setup::register_entry(agent.as_deref(), &mut out)?;
            println!("mur browser is ready.");
        }
        BrowserAction::Prune {
            keep,
            older_than,
            dry_run,
        } => cmd::browser::prune(keep, older_than, dry_run)?,
    }
    Ok(())
}
