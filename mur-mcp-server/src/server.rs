use crate::jsonrpc::{Request, Response};
use serde_json::Value;

pub struct McpServer;

impl McpServer {
    pub fn new() -> Self {
        Self
    }

    pub async fn handle(&mut self, request: Request) -> Response {
        let _ = request;
        Response::error(None, -32601, "Method not found".into())
    }
}
