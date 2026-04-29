//! OTel GenAI + mur.* JSONL writer, with notification side-channel
//! so the stdio/socket transport can stream notifications to callers.

use mur_common::telemetry::*;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

const CHANNEL_BUF: usize = 256;

#[derive(Debug, Clone)]
pub enum Event {
    LlmCall {
        trace_id: String,
        task_id: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        latency_ms: u64,
        cost_usd: f64,
        provider: String,
    },
    ToolCall {
        trace_id: String,
        task_id: String,
        mcp_server: String,
        tool: String,
        duration_ms: u64,
        ok: bool,
    },
    Error {
        kind: String,
        message: String,
        task_id: Option<String>,
        recoverable: bool,
    },
    Warning {
        kind: String,
        message: String,
    },
    Heartbeat {
        uptime_s: u64,
        mem_mb: u64,
        active_tasks: u32,
    },
    TaskProgress {
        task_id: String,
        stage: String,
        message: Option<String>,
        percent: Option<u8>,
    },
}

pub struct TelemetryWriter {
    tx: mpsc::Sender<Event>,
}

impl TelemetryWriter {
    /// Spawn the background writer task. Returns a handle that submits events
    /// plus a receiver that downstream transports subscribe to for live
    /// notification forwarding.
    pub async fn new(
        dir: PathBuf,
        agent_name: String,
        agent_uuid: String,
    ) -> std::io::Result<(Self, mpsc::Receiver<Value>)> {
        fs::create_dir_all(&dir).await?;
        let (in_tx, mut in_rx) = mpsc::channel::<Event>(CHANNEL_BUF);
        let (out_tx, out_rx) = mpsc::channel::<Value>(CHANNEL_BUF);
        tokio::spawn(async move {
            while let Some(ev) = in_rx.recv().await {
                let notif = event_to_notification(&ev, &agent_name, &agent_uuid);
                let today = chrono::Utc::now().format("%Y-%m-%d");
                let path = dir.join(format!("{today}.jsonl"));
                if let Ok(mut f) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                {
                    let line = format!(
                        "{}\n",
                        serde_json::to_string(&notif["params"]).unwrap_or_default()
                    );
                    let _ = f.write_all(line.as_bytes()).await;
                }
                let _ = out_tx.send(notif).await;
            }
        });
        Ok((Self { tx: in_tx }, out_rx))
    }

    pub async fn emit(&self, ev: Event) {
        let _ = self.tx.send(ev).await;
    }

    pub async fn flush(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn event_to_notification(ev: &Event, name: &str, uuid: &str) -> Value {
    let mut params = json!({
        MUR_AGENT_NAME: name,
        MUR_AGENT_UUID: uuid,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let method = match ev {
        Event::LlmCall {
            trace_id,
            task_id,
            model,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
            provider,
        } => {
            params[GEN_AI_PROVIDER_NAME] = json!(provider);
            params[GEN_AI_REQUEST_MODEL] = json!(model);
            params[GEN_AI_USAGE_INPUT_TOKENS] = json!(input_tokens);
            params[GEN_AI_USAGE_OUTPUT_TOKENS] = json!(output_tokens);
            params["latency_ms"] = json!(latency_ms);
            params["cost_usd"] = json!(cost_usd);
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            METHOD_LLM_CALL
        }
        Event::ToolCall {
            trace_id,
            task_id,
            mcp_server,
            tool,
            duration_ms,
            ok,
        } => {
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            params[MUR_MCP_SERVER] = json!(mcp_server);
            params["tool"] = json!(tool);
            params["duration_ms"] = json!(duration_ms);
            params["ok"] = json!(ok);
            METHOD_TOOL_CALL
        }
        Event::Error {
            kind,
            message,
            task_id,
            recoverable,
        } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            params["recoverable"] = json!(recoverable);
            if let Some(t) = task_id {
                params[MUR_TASK_ID] = json!(t);
            }
            METHOD_ERROR
        }
        Event::Warning { kind, message } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            METHOD_WARNING
        }
        Event::Heartbeat {
            uptime_s,
            mem_mb,
            active_tasks,
        } => {
            params["uptime_s"] = json!(uptime_s);
            params["mem_mb"] = json!(mem_mb);
            params["active_tasks"] = json!(active_tasks);
            METHOD_HEARTBEAT
        }
        Event::TaskProgress {
            task_id,
            stage,
            message,
            percent,
        } => {
            params[MUR_TASK_ID] = json!(task_id);
            params["stage"] = json!(stage);
            if let Some(m) = message {
                params["message"] = json!(m);
            }
            if let Some(p) = percent {
                params["percent"] = json!(p);
            }
            METHOD_TASK_PROGRESS
        }
    };
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}
