// mur-research-gateway/src/main.rs
use tracing_subscriber::EnvFilter;

mod audit;
mod browser;
mod config;
mod fetcher;
mod jsonrpc;
mod net_guard;
mod server;
mod tools;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr) // logs to stderr so stdout stays clean for JSON-RPC
        .init();

    tracing::info!("mur-research-gateway starting");

    // Config (env + ~/.mur/config.yaml's research_gateway: block) is loaded
    // ONCE here, inside McpServer::new — see config.rs. Preflight below reads
    // it back via `browser_cfg()` rather than loading its own copy, so
    // startup logging always reflects the exact config the server will use.
    let mut server = server::McpServer::new();

    // Preflight the browser toolchain once at startup so a missing/stale
    // agent-browser or absent Lightpanda is surfaced explicitly (never
    // silently) — degrade to tier 1 only, not "as if Full" (spec §5).
    match browser::preflight(server.browser_cfg()) {
        browser::Preflight::Full => {
            tracing::info!("browser preflight: full — tiers 2/3 (lightpanda/chrome) available")
        }
        other => tracing::warn!(
            "browser preflight degraded: {:?} — render/search fall back to tier 1 only",
            other
        ),
    }

    while let Some(request) = jsonrpc::read_request() {
        // JSON-RPC notifications (no `id`, e.g. `notifications/initialized`) must
        // NOT receive a response. Compute this before `request` is moved into handle.
        let is_notification = request.id.is_none() && request.method.starts_with("notifications/");
        let response = server.handle(request).await;
        if !is_notification {
            jsonrpc::write_response(&response);
        }
    }

    tracing::info!("mur-research-gateway shutting down");
}
