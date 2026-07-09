// mur-research-gateway/src/main.rs
use tracing_subscriber::EnvFilter;

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

    let mut server = server::McpServer::new();

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
