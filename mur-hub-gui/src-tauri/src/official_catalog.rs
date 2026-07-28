//! Hub-side bridge to the official catalog on app.mur.run.
//!
//! Thin wrappers over `mur_core::official` so the agent wizard can browse and
//! install curated agents/fleets without shelling out to the `mur` CLI. The
//! trust chain (account-bound license verified fail-closed, signature checks in
//! the import paths) lives in mur-core and is shared with the CLI — nothing is
//! re-implemented here.

use serde::Serialize;

#[derive(Serialize)]
pub struct CatalogItemView {
    pub id: String,
    pub tier: String,
    pub version: String,
    pub description: String,
    /// `agents/<name>` → `<name>`; fleets install several agents, so None.
    pub agent_name: Option<String>,
}

/// Public listing — no auth. Errors surface the server's own message.
#[tauri::command]
pub async fn official_list() -> Result<Vec<CatalogItemView>, String> {
    let base = mur_core::auth::server_url();
    let items = mur_core::official::client::fetch_catalog(&reqwest::Client::new(), &base)
        .await
        .map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|i| CatalogItemView {
            agent_name: mur_core::official::install::installed_agent_name(&i.id)
                .map(str::to_string),
            id: i.id,
            tier: i.tier,
            version: i.version,
            description: i.description,
        })
        .collect())
}

/// True when a login is stored — the wizard uses it to explain *why* install is
/// unavailable instead of failing at the click.
#[tauri::command]
pub fn official_logged_in() -> bool {
    mur_core::auth::load_tokens().is_some()
}

/// Download + install one catalog item. Returns the installed agent name for
/// `agents/<name>` ids so the wizard can offer it an appearance.
#[tauri::command]
pub async fn official_install(id: String) -> Result<Option<String>, String> {
    mur_core::official::install::install_item(&id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(mur_core::official::install::installed_agent_name(&id).map(str::to_string))
}
