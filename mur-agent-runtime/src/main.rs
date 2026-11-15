use anyhow::Result;

// No `#[tokio::main]`: the supervisor seals its sandbox before it builds the
// runtime, so no thread exists that the seal would miss (GHSA-j8mv-27gx-x7xj).
fn main() -> Result<()> {
    // Before any MCP server or browser is spawned, so children inherit it.
    mur_agent_runtime::fd_limit::raise_nofile_soft_limit();
    mur_agent_runtime::supervisor::entrypoint()
}
