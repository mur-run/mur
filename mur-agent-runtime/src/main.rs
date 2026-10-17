use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Before any MCP server or browser is spawned, so children inherit it.
    mur_agent_runtime::fd_limit::raise_nofile_soft_limit();
    mur_agent_runtime::supervisor::entrypoint().await
}
