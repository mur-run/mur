use anyhow::Result;

pub(crate) async fn cmd_serve(port: u16, open: bool, readonly: bool) -> Result<()> {
    let mur_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("HOME directory not found"))?
        .join(".mur");

    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let state = crate::server::AppState {
        patterns_dir: mur_dir.join("patterns"),
        workflows_dir: mur_dir.join("workflows"),
        pipelines_dir: mur_dir.join("pipelines"),
        index_dir: mur_dir.join("index"),
        config: crate::server::ServerConfig { readonly },
        events_tx,
    };

    let open_url = if open {
        Some(format!("http://localhost:{}", port))
    } else {
        None
    };

    crate::server::run_server(state, port, open_url).await
}
