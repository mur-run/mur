//! `mur official` — browse + install from the official MUR catalog.
use anyhow::Result;

use crate::official::client::fetch_catalog;
use crate::official::install::{install_item, installed_agent_name};

pub(crate) async fn cmd_official_list() -> Result<()> {
    let base = crate::auth::server_url();
    let items = fetch_catalog(&reqwest::Client::new(), &base).await?;
    if items.is_empty() {
        println!("No official items published yet.");
        return Ok(());
    }
    println!("{:<32} {:<6} {:<8} DESCRIPTION", "ID", "TIER", "VERSION");
    for i in &items {
        println!(
            "{:<32} {:<6} {:<8} {}",
            i.id, i.tier, i.version, i.description
        );
    }
    if crate::auth::load_tokens().is_none() {
        println!("\nLog in with `mur login` to install (pro items need a MUR Pro subscription).");
    }
    Ok(())
}

pub(crate) async fn cmd_official_install(id: &str) -> Result<()> {
    install_item(id).await?;
    println!("Installed official item {id}");
    if let Some(name) = installed_agent_name(id) {
        println!("Talk to it with `mur agent cli {name}`.");
    }
    Ok(())
}
