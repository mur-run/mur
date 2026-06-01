use tracing_subscriber::EnvFilter;

mod jsonrpc;
mod server;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr) // logs to stderr so stdout stays clean for JSON-RPC
        .init();

    tracing::info!("mur-mcp-server starting");

    let mut server = server::McpServer::new();

    while let Some(request) = jsonrpc::read_request() {
        let response = server.handle(request).await;
        jsonrpc::write_response(&response);
    }

    tracing::info!("mur-mcp-server shutting down");
}
