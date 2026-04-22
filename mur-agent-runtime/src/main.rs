use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    mur_agent_runtime::supervisor::entrypoint().await
}
