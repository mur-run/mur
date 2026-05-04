use anyhow::Result;

pub(crate) async fn cmd_hook_prompt(_tool: &str) -> Result<()> {
    Ok(())
}

pub(crate) async fn cmd_hook_tool(_tool: &str) -> Result<()> {
    Ok(())
}

pub(crate) async fn cmd_hook_stop(_tool: &str) -> Result<()> {
    Ok(())
}

pub(crate) async fn cmd_hook_session_start(_tool: &str) -> Result<()> {
    Ok(())
}

pub(crate) fn cmd_hook_stats() -> Result<()> {
    println!("hook stats: not yet implemented (M5)");
    Ok(())
}
