use mur_agent_runtime::protocol::mcp_client::McpClient;
use mur_common::agent::McpServerEntry;

#[tokio::test]
async fn initialize_and_tools_list() {
    let bin = env!("CARGO_BIN_EXE_mock_mcp");
    let entry = McpServerEntry {
        name: "mock".into(),
        command: bin.into(),
        args: vec![],
    };
    let mut client = McpClient::spawn(&entry).await.expect("spawn");
    let info = client.initialize().await.expect("init");
    assert_eq!(info.server_name, "mock_mcp");
    let tools = client.list_tools().await.expect("list");
    assert!(tools.iter().any(|t| t.name == "echo"));
    client.shutdown().await;
}
