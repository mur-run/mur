//! MCP stdio proxy: the agent talks to us, we talk to `@playwright/mcp`.
//!
//! Wire format is newline-delimited JSON-RPC 2.0 in both directions. The
//! proxy owns two things the later slices need:
//!
//! 1. **Correlation** — every agent request is remembered by id so the
//!    matching response can be shown to the hook together with the request
//!    that caused it (the recorder needs "which tool was this?").
//! 2. **A private line to the server** — [`Downstream::call`] lets a hook
//!    make its own requests (e.g. `browser_generate_locator`) without the
//!    agent ever seeing them. Internal ids are namespaced `mur-<pid>-<n>`.
//!
//! Slice 1 ships [`ForwardHook`] (pure relay). Slices 2/4/5 plug in via
//! [`Hook`].

use crate::broker::BrokerClient;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// A JSON-RPC request as seen on the wire. `params` is kept as raw JSON so
/// hooks can rewrite it (secret substitution) before forwarding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// For `tools/call`, the tool name (`browser_click`, `mur_intent`, …).
    pub fn tool_name(&self) -> Option<&str> {
        if self.method != "tools/call" {
            return None;
        }
        self.params.as_ref()?.get("name")?.as_str()
    }

    /// For `tools/call`, the `arguments` object.
    pub fn tool_args(&self) -> Option<&Value> {
        if self.method != "tools/call" {
            return None;
        }
        self.params.as_ref()?.get("arguments")
    }

    /// Mutable `arguments` (creates an empty object if absent).
    pub fn tool_args_mut(&mut self) -> Option<&mut Value> {
        if self.method != "tools/call" {
            return None;
        }
        let params = self.params.get_or_insert_with(|| json!({}));
        let obj = params.as_object_mut()?;
        Some(obj.entry("arguments").or_insert_with(|| json!({})))
    }
}

/// What a hook wants done with an agent request.
#[derive(Debug)]
pub enum Decision {
    /// Send this (possibly rewritten) request to the server.
    Forward(Request),
    /// Answer the agent ourselves; the server never sees it.
    Reply(Value),
    /// Refuse with a JSON-RPC error. `code` follows JSON-RPC conventions
    /// (`-32602` invalid params, `-32000` server error).
    Reject { code: i32, message: String },
}

/// Intercept points. Slice 1 only forwards; later slices override.
///
/// Both methods run under a mutex, so a hook may keep plain mutable state.
pub trait Hook: Send + 'static {
    /// Called for every agent request (has an `id`) **and** notification
    /// (no `id`). Rejecting a notification is silently a drop.
    fn on_request(
        &mut self,
        req: Request,
        down: &Downstream,
    ) -> impl std::future::Future<Output = Decision> + Send;

    /// Called with the server's response to an agent request. `req` is the
    /// original (post-rewrite) request. Return the response to hand back —
    /// usually the same value, possibly redacted.
    fn on_response(
        &mut self,
        req: &Request,
        resp: Value,
        down: &Downstream,
    ) -> impl std::future::Future<Output = Value> + Send;

    /// Extra tools this hook implements itself (merged into the server's
    /// `tools/list` result). Default: none.
    fn extra_tools(&self) -> Vec<Value> {
        Vec::new()
    }
}

pub struct BrokerHook<H> {
    inner: H,
    broker: Arc<dyn BrokerClient>,
    leases: HashMap<String, (String, Option<Value>)>,
    /// Request as seen before secret substitution. The recorder receives this
    /// copy, so its YAML retains `{{secret:…}}`, never the Keychain value.
    original_requests: HashMap<String, Request>,
}

impl<H> BrokerHook<H> {
    pub fn new(inner: H, broker: Arc<dyn BrokerClient>) -> Self {
        Self {
            inner,
            broker,
            leases: HashMap::new(),
            original_requests: HashMap::new(),
        }
    }
}

impl<H: Hook> Hook for BrokerHook<H> {
    async fn on_request(&mut self, mut req: Request, down: &Downstream) -> Decision {
        let Some(args) = req.tool_args().cloned() else {
            return self.inner.on_request(req, down).await;
        };
        // Requests with no secret placeholder never touch the broker. For a
        // placeholder-bearing request, however, broker failure means the call
        // is rejected before it can reach Playwright (fail closed).
        if !contains_placeholder(&args) {
            return self.inner.on_request(req, down).await;
        }
        let original = req.clone();
        let transformed = match self.broker.transform(args).await {
            Ok(value) => value,
            Err(error) => {
                return Decision::Reject {
                    code: -32000,
                    message: format!("secret broker unavailable: {error}"),
                };
            }
        };
        if let Some(slot) = req.tool_args_mut() {
            *slot = transformed.value;
        } else {
            return Decision::Reject {
                code: -32602,
                message: "tools/call arguments are not an object".into(),
            };
        }
        match self.inner.on_request(req, down).await {
            Decision::Forward(req) => {
                if let Some(id) = req.id.as_ref() {
                    let key = id_key(id);
                    // Keep the pre-substitution request exclusively for the
                    // recorder; Playwright still receives `req` with values
                    // resolved by the broker.
                    self.original_requests.insert(key.clone(), original);
                    self.leases
                        .insert(key, (transformed.lease, transformed.boundary));
                }
                Decision::Forward(req)
            }
            other => other,
        }
    }

    async fn on_response(&mut self, req: &Request, resp: Value, down: &Downstream) -> Value {
        let Some(id) = &req.id else {
            return self.inner.on_response(req, resp, down).await;
        };
        // The inner hook (notably RecordHook) must see the request the agent
        // supplied, before secret substitution. This prevents plaintext from
        // ever entering actions.yaml.
        let original = self.original_requests.remove(&id_key(id));
        let response = match original.as_ref() {
            Some(original) => self.inner.on_response(original, resp, down).await,
            None => self.inner.on_response(req, resp, down).await,
        };
        let Some((lease, boundary)) = self.leases.remove(&id_key(id)) else {
            return response;
        };
        match self.broker.redact(lease, boundary, response).await {
            Ok(redacted) => redacted,
            Err(error) => {
                json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32000, "message":format!("secret broker redaction failed: {error}")}})
            }
        }
    }

    fn extra_tools(&self) -> Vec<Value> {
        self.inner.extra_tools()
    }
}

/// Pure relay — what slice 1 ships.
#[derive(Debug, Default)]
pub struct ForwardHook;

impl Hook for ForwardHook {
    async fn on_request(&mut self, req: Request, _down: &Downstream) -> Decision {
        Decision::Forward(req)
    }

    async fn on_response(&mut self, _req: &Request, resp: Value, _down: &Downstream) -> Value {
        resp
    }
}

type PendingInternal = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

/// Handle for sending our own requests to the server.
#[derive(Clone)]
pub struct Downstream {
    to_server: mpsc::UnboundedSender<String>,
    pending: PendingInternal,
    next_id: Arc<AtomicU64>,
    id_prefix: Arc<String>,
}

impl Downstream {
    fn new(to_server: mpsc::UnboundedSender<String>) -> Self {
        Self {
            to_server,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            id_prefix: Arc::new(format!("mur-{}-", std::process::id())),
        }
    }

    fn is_internal_id(&self, id: &Value) -> bool {
        id.as_str()
            .map(|s| s.starts_with(self.id_prefix.as_str()))
            .unwrap_or(false)
    }

    /// Fire a request the agent never sees and wait for the full JSON-RPC
    /// response object (`result` or `error` still inside).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("{}{}", self.id_prefix, n);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))?;
        self.to_server.send(line).context("server stdin closed")?;
        rx.await
            .context("server exited before answering internal call")
    }

    /// Send a notification the server never needs to acknowledge.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }))?;
        self.to_server.send(line).context("server stdin closed")
    }

    /// Convenience: `tools/call` and return the whole response.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        self.call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Navigate a headed browser, wait for an explicit human confirmation, and
    /// use Playwright's unsafe-code tool to write its storage state into MUR's
    /// private output directory. `@playwright/mcp` does not expose a dedicated
    /// storage-state tool, so this is the supported escape hatch for invoking
    /// `BrowserContext.storageState` without carrying session JSON over MCP.
    pub async fn capture_storage_state(
        &self,
        url: &str,
        state_path: &std::path::Path,
    ) -> Result<()> {
        let navigate = self
            .call_tool("browser_navigate", json!({ "url": url }))
            .await?;
        ensure_tool_succeeded("browser_navigate", &navigate)?;

        eprintln!(
            "Complete sign-in in the opened browser, then press Enter here to save the profile."
        );
        let mut confirmation = String::new();
        let read =
            tokio::task::spawn_blocking(move || std::io::stdin().read_line(&mut confirmation))
                .await
                .context("wait for login confirmation task")??;
        if read == 0 {
            bail!("login confirmation cancelled: stdin closed");
        }

        let code = storage_state_export_code(state_path)?;
        let state = self
            .call_tool("browser_run_code_unsafe", json!({ "code": code }))
            .await?;
        ensure_tool_succeeded("browser_run_code_unsafe", &state)?;
        Ok(())
    }

    /// Forward a raw line (used for agent → server traffic).
    fn send_raw(&self, line: String) -> Result<()> {
        self.to_server.send(line).context("server stdin closed")
    }
}

fn storage_state_export_code(state_path: &std::path::Path) -> Result<String> {
    // Serialize the path as a JavaScript string literal, rather than
    // interpolating it directly into code. The directory is private and
    // is removed immediately after its contents are encrypted.
    let path = serde_json::to_string(&state_path.to_string_lossy())?;
    Ok(format!(
        "async (page) => {{ await page.context().storageState({{ path: {path} }}); return 'storage state saved'; }}"
    ))
}

fn ensure_response_succeeded<'a>(method: &str, response: &'a Value) -> Result<&'a Value> {
    if let Some(error) = response.get("error") {
        bail!("{method} failed: {error}");
    }
    response
        .get("result")
        .ok_or_else(|| anyhow::anyhow!("{method} returned neither result nor error"))
}

fn ensure_tool_succeeded<'a>(tool: &str, response: &'a Value) -> Result<&'a Value> {
    let result = ensure_response_succeeded(tool, response)?;
    // MCP tool failures are successful JSON-RPC responses carrying
    // `result.isError`, rather than JSON-RPC `error`. Do not prompt a human to
    // complete login when Playwright never opened a browser.
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let detail = result
            .get("content")
            .map(Value::to_string)
            .unwrap_or_else(|| result.to_string());
        bail!("{tool} failed: {detail}");
    }
    Ok(result)
}

/// Build the argv passed to `npx` for the Playwright MCP package.
fn playwright_args(extra_args: &[String]) -> Vec<String> {
    let mut args = vec!["-y".to_owned(), crate::PLAYWRIGHT_MCP_PKG.to_owned()];
    args.extend(extra_args.iter().cloned());
    args
}

/// Build the `npx @playwright/mcp` command. Extra args are passed through
/// verbatim (`--headless`, `--isolated`, `--storage-state=…`).
pub fn playwright_command(extra_args: &[String]) -> Command {
    let mut cmd = Command::new("npx");
    cmd.args(playwright_args(extra_args));
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    cmd
}

/// Spawn the server and relay between our stdin/stdout and it.
/// Returns when the agent closes stdin or the server exits.
pub async fn run_stdio<H: Hook>(mut cmd: Command, hook: H) -> Result<()> {
    let mut child: Child = cmd
        .spawn()
        .context("spawn downstream MCP server (is `npx` on PATH and in the spawn allowlist?)")?;
    let child_in = child.stdin.take().context("child stdin")?;
    let child_out = child.stdout.take().context("child stdout")?;
    let result = run_io(
        tokio::io::stdin(),
        tokio::io::stdout(),
        child_in,
        child_out,
        hook,
    )
    .await;
    let _ = child.kill().await;
    result
}

/// Spawn a headed Playwright MCP server in an isolated temporary directory,
/// then return the storage state it exported using `browser_run_code_unsafe`.
/// The unencrypted file remains in the private directory only long enough for
/// this function to read it and `save_profile` to encrypt it locally.
pub async fn capture_storage_state(url: &str, browser: &str) -> Result<Vec<u8>> {
    let output = private_output_dir()?;
    let output_arg = format!("--output-dir={}", output.display());
    let browser_arg = format!("--browser={browser}");
    // @playwright/mcp runs headed by default; its CLI has no `--headed` flag.
    let mut cmd = playwright_command(&["--isolated".into(), browser_arg, output_arg]);
    let mut child = match cmd.spawn().context(
        "spawn headed Playwright MCP server (is `npx` on PATH and in the spawn allowlist?)",
    ) {
        Ok(child) => child,
        Err(error) => {
            // No state exists yet, but do not leave private scratch directories
            // behind when the server cannot start.
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let child_in = match child.stdin.take().context("child stdin") {
        Ok(stdin) => stdin,
        Err(error) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let child_out = match child.stdout.take().context("child stdout") {
        Ok(stdout) => stdout,
        Err(error) => {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&output);
            return Err(error);
        }
    };
    let state_path = output.join("storage-state.json");
    let result = capture_storage_state_io(child_in, child_out, url, &state_path).await;
    let _ = child.kill().await;
    let state = match result {
        Ok(()) => std::fs::read(&state_path).with_context(|| {
            format!(
                "read captured browser storage state {}",
                state_path.display()
            )
        }),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_dir_all(&output);
    state
}

fn private_output_dir() -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("mur-browser-auth-{}", uuid::Uuid::new_v4()));
    // The Playwright tool writes unencrypted session cookies here. Do not rely
    // on the caller's umask: only this user may traverse the directory.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .with_context(|| {
                format!("create private browser output directory {}", path.display())
            })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(&path).with_context(|| {
            format!("create private browser output directory {}", path.display())
        })?;
    }
    Ok(path)
}

async fn capture_storage_state_io<SI, SO>(
    server_in: SI,
    server_out: SO,
    url: &str,
    state_path: &std::path::Path,
) -> Result<()>
where
    SI: AsyncWrite + Unpin + Send + 'static,
    SO: AsyncRead + Unpin + Send + 'static,
{
    let (to_server, mut to_server_rx) = mpsc::unbounded_channel::<String>();
    let down = Downstream::new(to_server);
    let server_task = tokio::spawn(async move {
        let mut server_in = server_in;
        while let Some(line) = to_server_rx.recv().await {
            server_in.write_all(line.as_bytes()).await?;
            server_in.write_all(b"\n").await?;
            server_in.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    });
    let downstream = down.clone();
    let response_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_out).lines();
        while let Some(line) = lines.next_line().await? {
            let value: Value = serde_json::from_str(&line)?;
            if let Some(id) = value.get("id")
                && downstream.is_internal_id(id)
                && let Some(sender) = downstream
                    .pending
                    .lock()
                    .await
                    .remove(id.as_str().unwrap_or(""))
            {
                let _ = sender.send(value);
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let initialized = down
        .call(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "mur browser auth",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
        .await
        .context("initialize headed Playwright MCP session");
    let result = match initialized {
        Ok(response) => match ensure_response_succeeded("initialize", &response) {
            Ok(_) => match down
                .notify("notifications/initialized", json!({}))
                .context("confirm headed Playwright MCP initialization")
            {
                Ok(()) => down.capture_storage_state(url, state_path).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    drop(down);
    // The response reader owns a `Downstream` clone, and therefore the last
    // sender to the server writer. Stop it before awaiting the writer or this
    // private session would wait forever for its own channel to close.
    response_task.abort();
    let _ = response_task.await;
    let _ = server_task.await;
    result
}

/// Transport-agnostic core, so tests can drive it with in-memory pipes.
///
/// * `agent_in`  — what the agent writes (our stdin)
/// * `agent_out` — what the agent reads (our stdout)
/// * `server_in` — the server's stdin
/// * `server_out`— the server's stdout
pub async fn run_io<H, AI, AO, SI, SO>(
    agent_in: AI,
    mut agent_out: AO,
    mut server_in: SI,
    server_out: SO,
    hook: H,
) -> Result<()>
where
    H: Hook,
    AI: AsyncRead + Unpin + Send + 'static,
    AO: AsyncWrite + Unpin + Send + 'static,
    SI: AsyncWrite + Unpin + Send + 'static,
    SO: AsyncRead + Unpin + Send + 'static,
{
    let (to_server_tx, mut to_server_rx) = mpsc::unbounded_channel::<String>();
    let (to_agent_tx, mut to_agent_rx) = mpsc::unbounded_channel::<String>();
    let down = Downstream::new(to_server_tx);
    let hook = Arc::new(Mutex::new(hook));
    // agent request id (stringified) → original request, for on_response
    let agent_pending: Arc<Mutex<HashMap<String, Request>>> = Arc::new(Mutex::new(HashMap::new()));

    // Writer: server stdin
    let w_server = tokio::spawn(async move {
        while let Some(line) = to_server_rx.recv().await {
            if server_in.write_all(line.as_bytes()).await.is_err()
                || server_in.write_all(b"\n").await.is_err()
                || server_in.flush().await.is_err()
            {
                break;
            }
        }
    });

    // Writer: agent stdout
    let w_agent = tokio::spawn(async move {
        while let Some(line) = to_agent_rx.recv().await {
            if agent_out.write_all(line.as_bytes()).await.is_err()
                || agent_out.write_all(b"\n").await.is_err()
                || agent_out.flush().await.is_err()
            {
                break;
            }
        }
    });

    // Reader: agent → (hook) → server
    let r_agent = {
        let down = down.clone();
        let hook = hook.clone();
        let agent_pending = agent_pending.clone();
        let to_agent = to_agent_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_in).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "agent sent non-JSON line; dropped");
                        continue;
                    }
                };
                // Agent answering a server-initiated request: pass straight through.
                if v.get("method").is_none() {
                    let _ = down.send_raw(line);
                    continue;
                }
                let req: Request = match serde_json::from_value(v) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed request; dropped");
                        continue;
                    }
                };
                let id = req.id.clone();
                let decision = hook.lock().await.on_request(req, &down).await;
                match decision {
                    Decision::Forward(req) => {
                        if let Some(id) = &req.id {
                            agent_pending.lock().await.insert(id_key(id), req.clone());
                        }
                        match serde_json::to_string(&req) {
                            Ok(line) => {
                                let _ = down.send_raw(line);
                            }
                            Err(e) => tracing::warn!(error = %e, "serialize request"),
                        }
                    }
                    Decision::Reply(result) => {
                        if let Some(id) = id {
                            let _ = to_agent
                                .send(json!({"jsonrpc":"2.0","id":id,"result":result}).to_string());
                        }
                    }
                    Decision::Reject { code, message } => {
                        if let Some(id) = id {
                            let _ = to_agent.send(
                                json!({"jsonrpc":"2.0","id":id,
                                       "error":{"code":code,"message":message}})
                                .to_string(),
                            );
                        }
                    }
                }
            }
        })
    };

    // Reader: server → (route) → agent or internal waiter
    let r_server = {
        let down = down.clone();
        let hook = hook.clone();
        let agent_pending = agent_pending.clone();
        let to_agent = to_agent_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(server_out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        // Playwright MCP occasionally logs to stdout; keep it
                        // out of the agent's JSON stream.
                        tracing::debug!(line = %line, "server non-JSON stdout");
                        continue;
                    }
                };
                let is_response = v.get("method").is_none();
                let Some(id) = v.get("id").cloned() else {
                    // notification from server
                    let _ = to_agent.send(line);
                    continue;
                };
                if is_response && down.is_internal_id(&id) {
                    if let Some(tx) = down.pending.lock().await.remove(id.as_str().unwrap_or("")) {
                        let _ = tx.send(v);
                    }
                    continue;
                }
                if is_response {
                    let req = agent_pending.lock().await.remove(&id_key(&id));
                    let out = match req {
                        Some(req) => {
                            let mut resp = hook.lock().await.on_response(&req, v, &down).await;
                            if req.method == "tools/list" {
                                merge_extra_tools(&mut resp, hook.lock().await.extra_tools());
                            }
                            resp
                        }
                        None => v,
                    };
                    let _ = to_agent.send(out.to_string());
                } else {
                    // server-initiated request (roots/list, sampling) → agent
                    let _ = to_agent.send(line);
                }
            }
        })
    };

    // Whichever side hangs up first ends the session.
    tokio::select! {
        _ = r_agent => {}
        _ = r_server => {}
    }
    drop(down);
    drop(to_agent_tx);
    let _ = w_server.await;
    let _ = w_agent.await;
    Ok(())
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

fn contains_placeholder(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("{{secret:"),
        Value::Array(items) => items.iter().any(contains_placeholder),
        Value::Object(map) => map.values().any(contains_placeholder),
        _ => false,
    }
}

fn merge_extra_tools(resp: &mut Value, extra: Vec<Value>) {
    if extra.is_empty() {
        return;
    }
    if let Some(tools) = resp
        .get_mut("result")
        .and_then(|r| r.get_mut("tools"))
        .and_then(|t| t.as_array_mut())
    {
        tools.extend(extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// A fake server: echoes `{"id":…,"result":{"echo":<params>}}` for every
    /// request, and answers `tools/list` with one tool.
    async fn fake_server(mut inp: impl AsyncRead + Unpin, mut out: impl AsyncWrite + Unpin) {
        let mut lines = BufReader::new(&mut inp).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let v: Value = serde_json::from_str(&line).unwrap();
            let id = v["id"].clone();
            let result = if v["method"] == "tools/list" {
                json!({"tools":[{"name":"browser_navigate"}]})
            } else {
                json!({"echo": v["params"]})
            };
            let resp = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
            out.write_all(resp.as_bytes()).await.unwrap();
            out.write_all(b"\n").await.unwrap();
        }
    }

    /// Hook that rejects `browser_storage_state`, answers `mur_ping` itself,
    /// and makes one internal call on `browser_click` to prove `Downstream`.
    struct TestHook {
        internal_seen: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone)]
    struct FakeBroker;

    impl BrokerClient for FakeBroker {
        fn transform<'a>(
            &'a self,
            mut value: Value,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = anyhow::Result<crate::broker::Transform>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                replace_test_secret(&mut value, "{{secret:pchome/PASSWORD}}", "ActualSecret123!");
                Ok(crate::broker::Transform {
                    value,
                    lease: "one-shot-test-lease".into(),
                    boundary: Some(json!({"keys":["PASSWORD"]})),
                })
            })
        }

        fn redact<'a>(
            &'a self,
            lease: String,
            _boundary: Option<Value>,
            mut value: Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>>
        {
            Box::pin(async move {
                anyhow::ensure!(lease == "one-shot-test-lease", "unexpected broker lease");
                replace_test_secret(&mut value, "ActualSecret123!", "[redacted:PASSWORD]");
                Ok(value)
            })
        }
    }

    fn replace_test_secret(value: &mut Value, from: &str, to: &str) {
        match value {
            Value::String(text) => *text = text.replace(from, to),
            Value::Array(values) => values
                .iter_mut()
                .for_each(|value| replace_test_secret(value, from, to)),
            Value::Object(values) => values
                .values_mut()
                .for_each(|value| replace_test_secret(value, from, to)),
            _ => {}
        }
    }

    struct CaptureRequestHook {
        seen_response_request: Arc<Mutex<Option<Request>>>,
    }

    impl Hook for CaptureRequestHook {
        async fn on_request(&mut self, req: Request, _down: &Downstream) -> Decision {
            Decision::Forward(req)
        }

        async fn on_response(&mut self, req: &Request, resp: Value, _down: &Downstream) -> Value {
            *self.seen_response_request.lock().await = Some(req.clone());
            resp
        }
    }
    impl Hook for TestHook {
        async fn on_request(&mut self, req: Request, down: &Downstream) -> Decision {
            match req.tool_name() {
                Some("browser_storage_state") => Decision::Reject {
                    code: -32000,
                    message: "use mur browser auth".into(),
                },
                Some("mur_ping") => Decision::Reply(json!({"pong":true})),
                Some("browser_click") => {
                    let r = down
                        .call_tool("browser_generate_locator", json!({"ref":"e1"}))
                        .await
                        .unwrap();
                    self.internal_seen.lock().await.push(r);
                    Decision::Forward(req)
                }
                _ => Decision::Forward(req),
            }
        }
        async fn on_response(&mut self, _req: &Request, mut resp: Value, _d: &Downstream) -> Value {
            resp["result"]["hooked"] = json!(true);
            resp
        }
        fn extra_tools(&self) -> Vec<Value> {
            vec![json!({"name":"mur_ping"})]
        }
    }

    async fn read_line(r: &mut (impl AsyncRead + Unpin)) -> Value {
        let mut lines = BufReader::new(r).lines();
        let l = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout")
            .unwrap()
            .expect("eof");
        serde_json::from_str(&l).unwrap()
    }

    #[test]
    fn storage_state_export_code_quotes_hostile_paths() {
        let path = std::path::Path::new("/private/a\"b\\c/storage-state.json");
        let code = storage_state_export_code(path).unwrap();
        assert!(code.starts_with("async (page) =>"));
        assert!(
            code.contains("storageState({ path: \"/private/a\\\"b\\\\c/storage-state.json\" })")
        );
        assert!(code.ends_with("return 'storage state saved'; }"));
    }

    #[test]
    fn headed_auth_args_use_selected_browser_and_default_headed_mode() {
        let output_arg = "--output-dir=/private/tmp/mur-auth".to_owned();
        let browser_arg = "--browser=firefox".to_owned();
        let args = playwright_args(&["--isolated".into(), browser_arg.clone(), output_arg.clone()]);

        assert!(args.contains(&"--isolated".to_owned()));
        assert!(args.contains(&browser_arg));
        assert!(args.contains(&output_arg));
        assert!(
            !args.iter().any(|arg| arg == "--headed"),
            "@playwright/mcp is headed by default; --headed is not a supported option"
        );
    }

    #[tokio::test]
    async fn forward_reject_reply_and_internal_call() {
        let (agent_w, agent_in) = duplex(64 * 1024); // test writes → proxy stdin
        let (agent_out, mut agent_r) = duplex(64 * 1024); // proxy stdout → test reads
        let (server_in, srv_r) = duplex(64 * 1024); // proxy → server
        let (srv_w, server_out) = duplex(64 * 1024); // server → proxy

        tokio::spawn(fake_server(srv_r, srv_w));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let hook = TestHook {
            internal_seen: seen.clone(),
        };
        tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, hook));

        let mut agent_w = agent_w;
        let send = |v: Value| v.to_string() + "\n";

        // 1. plain forward + on_response touched it
        agent_w
            .write_all(
                send(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"browser_navigate","arguments":{"url":"https://x"}}}))
                .as_bytes(),
            )
            .await
            .unwrap();
        let r = read_line(&mut agent_r).await;
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["echo"]["name"], "browser_navigate");
        assert_eq!(r["result"]["hooked"], true);

        // 2. reject never reaches server
        agent_w
            .write_all(
                send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"browser_storage_state","arguments":{}}}))
                .as_bytes(),
            )
            .await
            .unwrap();
        let r = read_line(&mut agent_r).await;
        assert_eq!(r["id"], 2);
        assert_eq!(r["error"]["message"], "use mur browser auth");

        // 3. reply from hook
        agent_w
            .write_all(
                send(json!({"jsonrpc":"2.0","id":"s3","method":"tools/call",
                "params":{"name":"mur_ping","arguments":{}}}))
                .as_bytes(),
            )
            .await
            .unwrap();
        let r = read_line(&mut agent_r).await;
        assert_eq!(r["id"], "s3");
        assert_eq!(r["result"]["pong"], true);

        // 4. internal call happens before forward; agent sees only its own reply
        agent_w
            .write_all(
                send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":"browser_click","arguments":{"ref":"e1"}}}))
                .as_bytes(),
            )
            .await
            .unwrap();
        let r = read_line(&mut agent_r).await;
        assert_eq!(r["id"], 4);
        assert_eq!(r["result"]["echo"]["name"], "browser_click");
        let seen = seen.lock().await;
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0]["result"]["echo"]["name"],
            "browser_generate_locator"
        );
        assert!(seen[0]["id"].as_str().unwrap().starts_with("mur-"));

        // 5. tools/list gets extra tools merged
        agent_w
            .write_all(send(json!({"jsonrpc":"2.0","id":5,"method":"tools/list"})).as_bytes())
            .await
            .unwrap();
        let r = read_line(&mut agent_r).await;
        let names: Vec<_> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["browser_navigate", "mur_ping"]);
    }

    #[tokio::test]
    async fn broker_hook_forwards_secret_but_records_placeholder_and_redacts_response() {
        let (agent_w, agent_in) = duplex(64 * 1024);
        let (agent_out, mut agent_r) = duplex(64 * 1024);
        let (server_in, mut server_r) = duplex(64 * 1024);
        let (mut server_w, server_out) = duplex(64 * 1024);
        let seen = Arc::new(Mutex::new(None));
        let hook = BrokerHook::new(
            CaptureRequestHook {
                seen_response_request: seen.clone(),
            },
            Arc::new(FakeBroker),
        );
        tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, hook));

        let server = tokio::spawn(async move {
            let request = read_line(&mut server_r).await;
            assert_eq!(request["params"]["arguments"]["text"], "ActualSecret123!");
            let response = json!({
                "jsonrpc":"2.0", "id":request["id"],
                "result":{"content":"echo ActualSecret123!"}
            });
            server_w
                .write_all(response.to_string().as_bytes())
                .await
                .unwrap();
            server_w.write_all(b"\n").await.unwrap();
        });

        let mut agent_w = agent_w;
        agent_w.write_all(
            br#"{"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"browser_type","arguments":{"text":"{{secret:pchome/PASSWORD}}"}}}
"#,
        ).await.unwrap();
        let response = read_line(&mut agent_r).await;
        server.await.unwrap();

        assert_eq!(response["result"]["content"], "echo [redacted:PASSWORD]");
        assert!(!response.to_string().contains("ActualSecret123!"));
        let recorded = seen.lock().await.clone().expect("response hook request");
        assert_eq!(
            recorded.tool_args().unwrap()["text"],
            "{{secret:pchome/PASSWORD}}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_output_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = private_output_dir().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(path).unwrap();
    }

    #[test]
    fn request_accessors() {
        let mut r: Request = serde_json::from_value(json!({"jsonrpc":"2.0","id":1,
            "method":"tools/call","params":{"name":"browser_type","arguments":{"text":"a"}}}))
        .unwrap();
        assert_eq!(r.tool_name(), Some("browser_type"));
        assert_eq!(r.tool_args().unwrap()["text"], "a");
        r.tool_args_mut().unwrap()["text"] = json!("b");
        assert_eq!(r.tool_args().unwrap()["text"], "b");
        let n: Request =
            serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .unwrap();
        assert!(n.tool_name().is_none());
        assert!(n.id.is_none());
    }
}
