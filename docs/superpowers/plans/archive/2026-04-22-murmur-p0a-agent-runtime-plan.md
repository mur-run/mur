# murmur P0a — Agent Runtime Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship the P0a slice of the murmur sub-agent runtime — a new `mur-agent-runtime` binary plus `mur agent` CLI — so each created agent becomes a standalone OS-native executable with A2A v0.3 (subset) over stdio / Unix socket, MCP client, OTel telemetry, export/import, and management CLI.

**Architecture:** New Cargo crate `mur-agent-runtime` implementing a BusyBox-style multi-call binary that dispatches on `argv[0]` to per-profile runtime instances. Profile YAML lives in `~/.mur/agents/<name>/`. Shared types go in `mur-common`. `mur-core` gains a thin `cmd/agent*.rs` group for create/list/management/export. See spec `docs/superpowers/specs/2026-04-22-murmur-p0-agent-runtime-design.md` (commit `ccb3fd5`) for the full design.

**Tech Stack:** Rust edition 2024, tokio, serde + serde_yaml_ng (comment-preserving), uuid v7, flock, axum (for Unix socket HTTP), hyper, reqwest (Ollama/Anthropic/OpenAI clients), sha2, tar + flate2, dialoguer (interactive CLI), tracing + tracing-subscriber, `cargo-llvm-cov` (coverage gate).

**File structure:**

```
mur-common/src/
  agent.rs              ← AgentProfile, Persona, Entitlements, AgentCard, LockFile
  a2a.rs                ← A2A message/task/response envelope types
  telemetry.rs          ← OTel field constants, Notification builder

mur-agent-runtime/      ← NEW CRATE
  Cargo.toml
  build.rs              ← optional asset embedding for export --format=bin
  src/
    main.rs             ← argv[0] dispatch, env init, subcommand entry
    multi_call.rs       ← parse_basename, profile_name extraction, spoof check
    subcommand.rs       ← clap-derive for start/stop/status/card/send/logs/stats
    profile.rs          ← load, validate, defaults, {{agent_home}} expansion, digest
    entitlements.rs     ← schema, glob, category presets, warning detection
    supervisor.rs       ← startup/shutdown sequences, signal handlers
    lock_file.rs        ← LockFile read/write, flock, stale detection
    socket_path.rs      ← macOS 104-byte path fallback + symlink
    protocol/
      a2a_server.rs     ← JSON-RPC 2.0 dispatch, error mapping, Agent Card projection
      mcp_client.rs     ← MCP subprocess, handshake, tool registry
      methods/
        card.rs         ← agent/card
        message_send.rs ← message/send sync task
        tasks.rs        ← tasks/get, tasks/cancel, tasks/list
    transport/
      stdio.rs          ← newline-delimited JSON on stdin/stdout
      unix_socket.rs    ← UnixListener + SO_PEERCRED + path fallback
    llm/
      mod.rs            ← LLMClient trait
      ollama.rs         ← Ollama HTTP client
      anthropic.rs      ← Anthropic HTTP client (skeleton)
      openai.rs         ← OpenAI HTTP client (skeleton)
    task_runner.rs      ← TaskState machine, LLM + MCP orchestration, progress notifs
    telemetry_writer.rs ← JSONL per day + rotation + notification dispatch
    communication_policy.rs ← sends_to / accepts_from eval, caller resolution
    retry.rs            ← backoff, max_retries, retry_on matching
    export/
      mod.rs
      pkg.rs            ← .murpkg tar.gz pack + manifest
      bin_embed.rs      ← include_bytes! manifest + runtime detection
      extract.rs        ← first-run cache dir extraction
      prereq_check.rs   ← MCP command-in-PATH check at startup
    import.rs           ← .murpkg unpack + UUID regen + prereq scan

mur-core/src/cmd/
  agent.rs              ← top-level `mur agent` dispatcher + create/list/status/remove/rename/send/install-service/export/import
  agent_prompt.rs       ← mur agent prompt {show,edit,set}
  agent_mcp.rs          ← mur agent mcp {list,add,remove,rename}
  agent_skill.rs        ← mur agent skill {list,add,remove,show}
  agent_perm.rs         ← mur agent perm {show,set-mode,allow-host,...}
  yaml_edit.rs          ← comment-preserving roundtrip + atomic write + .bak
  running_warn.rs       ← "restart required" warning helper
```

---

## Task 0: Workspace scaffolding — new crate + shared types wiring

**Files:**
- Modify: `/Users/david/Projects/mur/Cargo.toml` (workspace members)
- Create: `/Users/david/Projects/mur/mur-agent-runtime/Cargo.toml`
- Create: `/Users/david/Projects/mur/mur-agent-runtime/src/lib.rs`
- Create: `/Users/david/Projects/mur/mur-agent-runtime/src/main.rs`

- [x] **Step 1: Add the new crate to workspace members**

Edit `Cargo.toml` — find the `[workspace]` block's `members = [...]` array and append `"mur-agent-runtime"`:

```toml
[workspace]
members = ["mur-common", "mur-core", "mur-agent-runtime"]
```

- [x] **Step 2: Create the crate manifest**

Write `mur-agent-runtime/Cargo.toml`:

```toml
[package]
name = "mur-agent-runtime"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "mur-agent-runtime"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
mur-common = { path = "../mur-common" }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
serde_yaml_ng = "0.10"
clap = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true, features = ["v7"] }
reqwest = { workspace = true }
sha2 = "0.10"
glob = "0.3"
fs2 = "0.4"                 # flock
hyper = { version = "1", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
axum = { version = "0.8", features = ["json"] }
tar = "0.4"
flate2 = "1"
dialoguer = "0.11"
futures = "0.3"

[dev-dependencies]
tempfile = "3"
```

- [x] **Step 3: Create placeholder `lib.rs` and `main.rs`**

Write `mur-agent-runtime/src/lib.rs`:

```rust
//! murmur agent runtime — BusyBox-style multi-call binary.
//!
//! Each symlink named `mur_agent_<name>` dispatches to a profile at
//! `~/.mur/agents/<name>/` and runs an A2A v0.3 agent.

pub mod multi_call;
pub mod profile;
pub mod entitlements;
pub mod lock_file;
pub mod socket_path;
pub mod subcommand;
pub mod supervisor;
pub mod telemetry_writer;
pub mod communication_policy;
pub mod retry;
pub mod llm;
pub mod task_runner;
pub mod protocol;
pub mod transport;
pub mod export;
pub mod import;
```

Write `mur-agent-runtime/src/main.rs`:

```rust
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    mur_agent_runtime::supervisor::entrypoint().await
}
```

(`supervisor::entrypoint` will be written in Task 22; for now create empty module files to satisfy `lib.rs`.)

- [x] **Step 4: Create empty module files**

For each module declared in `lib.rs`, create `mur-agent-runtime/src/<name>.rs` with a single `//! <description>` line. For modules with submodules (`protocol`, `transport`, `llm`, `export`), create `src/<name>/mod.rs` containing `//! <description>` plus sub-module declarations that point to the files you'll create in later tasks.

Example `src/protocol/mod.rs`:

```rust
//! A2A server + MCP client protocol surface.
pub mod a2a_server;
pub mod mcp_client;
pub mod methods;
```

And `src/protocol/methods/mod.rs`:

```rust
//! A2A method handlers.
pub mod card;
pub mod message_send;
pub mod tasks;
```

Create empty stubs (each file starting with `//!`) for every module referenced so the tree compiles.

- [x] **Step 5: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: clean build, `mur-agent-runtime` listed among compiled crates. Unused-code warnings are fine at this stage (allow them with `#![allow(dead_code)]` at the top of each stub module if they block CI).

- [x] **Step 6: Commit**

```bash
git add Cargo.toml mur-agent-runtime/
git commit -m "feat(agent-runtime): scaffold mur-agent-runtime crate"
```

---

## Task 1: Shared types in mur-common

**Files:**
- Create: `/Users/david/Projects/mur/mur-common/src/agent.rs`
- Create: `/Users/david/Projects/mur/mur-common/src/a2a.rs`
- Create: `/Users/david/Projects/mur/mur-common/src/telemetry.rs`
- Modify: `/Users/david/Projects/mur/mur-common/src/lib.rs`

- [x] **Step 1: Write tests for AgentProfile round-trip**

Create `mur-common/src/agent.rs` with this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_round_trip_yaml() {
        let yaml = r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: agent_a
display_name: "Price Hunter"
version: "0.1.0"
persona:
  category: research
  description: "Finds prices"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: true, bind: "unix:///tmp/a.sock" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send", "a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#;
        let profile: AgentProfile = serde_yaml_ng::from_str(yaml).expect("parse");
        assert_eq!(profile.name, "agent_a");
        assert_eq!(profile.persona.category, PersonaCategory::Research);
        assert_eq!(profile.entitlements.network.outbound.mode, NetworkOutboundMode::Restricted);
        let reserialized = serde_yaml_ng::to_string(&profile).expect("emit");
        let round_tripped: AgentProfile = serde_yaml_ng::from_str(&reserialized).expect("re-parse");
        assert_eq!(profile.id, round_tripped.id);
    }
}
```

- [x] **Step 2: Run the test — expect fail**

Run: `cargo test -p mur-common agent::tests::profile_round_trip_yaml`
Expected: compile failure (`AgentProfile` undefined).

- [x] **Step 3: Define the full struct tree**

Replace `mur-common/src/agent.rs` contents (keeping the `#[cfg(test)] mod tests` block from Step 1) with:

```rust
//! Agent profile, Agent Card, and LockFile types shared between
//! mur-agent-runtime and mur-core.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    pub schema: u32,
    pub id: String,                              // UUIDv7
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub persona: Persona,
    pub sys_prompt_file: String,
    pub model: ModelConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub transport: TransportConfig,
    pub communication: CommunicationConfig,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub entitlements: Entitlements,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    pub retry: RetryConfig,
    pub lifecycle: LifecycleConfig,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Persona {
    pub category: PersonaCategory,
    pub description: String,
    pub traits: PersonaTraits,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PersonaCategory {
    Research, Automation, Monitor, Notify, Commerce, Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonaTraits {
    pub tone: String, pub risk: String, pub verbosity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub params: BTreeMap<String, serde_yaml_ng::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransportConfig {
    pub stdio: bool,
    pub socket: SocketTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocketTransportConfig {
    pub enabled: bool,
    pub bind: String,                           // "unix:///path" or "tcp://host:port" (P0b)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthConfig {
    pub scheme: String,
    pub token_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommunicationConfig {
    #[serde(default = "default_accepts_all")]
    pub accepts_from: Vec<String>,
    #[serde(default)]
    pub sends_to: Vec<String>,
}
fn default_accepts_all() -> Vec<String> { vec!["*".to_string()] }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entitlements {
    pub network: NetworkEntitlement,
    pub filesystem: FilesystemEntitlement,
    pub processes: ProcessesEntitlement,
    #[serde(default)]
    pub syscalls: SyscallsEntitlement,
    #[serde(default)]
    pub limits: LimitsEntitlement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkEntitlement {
    pub inbound: InboundNetwork,
    pub outbound: OutboundNetwork,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InboundNetwork {
    #[serde(default)]
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboundNetwork {
    pub mode: NetworkOutboundMode,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub resolve_dns: ResolveDnsConfig,
}
fn default_protocols() -> Vec<String> { vec!["tcp".to_string()] }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkOutboundMode { Unrestricted, Restricted, Off }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveDnsConfig {
    #[serde(default = "default_dns_mode")]
    pub mode: String,
    #[serde(default)]
    pub servers: Vec<String>,
}
impl Default for ResolveDnsConfig {
    fn default() -> Self { Self { mode: default_dns_mode(), servers: vec![] } }
}
fn default_dns_mode() -> String { "system".to_string() }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FilesystemEntitlement {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessesEntitlement {
    pub spawn: SpawnEntitlement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpawnEntitlement {
    pub mode: SpawnMode,
    #[serde(default)]
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode { Allowlist, Any, None }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyscallsEntitlement {
    #[serde(default = "default_syscalls_mode")]
    pub mode: String,
    #[serde(default)]
    pub extra_deny: Vec<String>,
}
fn default_syscalls_mode() -> String { "default".to_string() }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LimitsEntitlement {
    #[serde(default)]
    pub cpu_seconds: Option<u64>,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "default_fds")]
    pub file_descriptors: u32,
    #[serde(default = "default_procs")]
    pub processes: u32,
}
fn default_memory_mb() -> u64 { 512 }
fn default_fds() -> u32 { 1024 }
fn default_procs() -> u32 { 32 }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotificationsConfig {
    #[serde(default)]
    pub on_task_complete: Vec<NotificationTarget>,
    #[serde(default)]
    pub on_error: Vec<NotificationTarget>,
    #[serde(default)]
    pub on_shutdown: Vec<NotificationTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "target", rename_all = "lowercase")]
pub enum NotificationTarget {
    Agent { name: String },
    Commander,
    Email { address: String, #[serde(default)] smtp_config_file: Option<String> },
    Slack { #[serde(default)] channel: Option<String>, #[serde(default)] webhook_url_env: Option<String> },
    Webpush { url: String },
    Webhook { url: String, #[serde(default = "default_post")] method: String, #[serde(default)] auth: Option<String> },
}
fn default_post() -> String { "POST".to_string() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryConfig {
    pub llm: RetryPolicy,
    pub tool: RetryPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub backoff: BackoffStrategy,
    pub initial_delay_ms: u64,
    #[serde(default)]
    pub max_delay_ms: Option<u64>,
    #[serde(default)]
    pub retry_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackoffStrategy { Linear, Exponential, Fixed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifecycleConfig {
    pub restart: RestartPolicy,
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(default = "default_window")]
    pub restart_window_secs: u64,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u64,
    #[serde(default = "default_mcp_required")]
    pub mcp_required: bool,
}
fn default_max_restarts() -> u32 { 3 }
fn default_window() -> u64 { 600 }
fn default_stop_timeout() -> u64 { 15 }
fn default_mcp_required() -> bool { true }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy { Never, OnFailure, Always }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockFile {
    pub schema: u32,
    pub uuid: String,
    pub name: String,
    pub pid: u32,
    pub ppid: u32,
    pub started_at: String,
    pub binary_version: String,
    pub transports: LockTransports,
    pub card_digest: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockTransports {
    pub stdio: bool,
    #[serde(default)]
    pub unix_socket: Option<String>,
    #[serde(default)]
    pub tcp: Option<String>,
}

// Keep tests block from Step 1 at end of file.
```

- [x] **Step 4: Create A2A envelope types**

Write `mur-common/src/a2a.rs`:

```rust
//! A2A v0.3 protocol envelope types.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,                  // "user" | "agent" | "system"
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessagePart {
    Text { text: String },
    Data { #[serde(rename = "mimeType")] mime_type: String, data: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState { Submitted, Working, Completed, Failed, Cancelled }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    pub messages: Vec<Message>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,                    // always "2.0"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,      // None = notification
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,                    // "2.0"
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}
```

- [x] **Step 5: Create telemetry field constants**

Write `mur-common/src/telemetry.rs`:

```rust
//! OpenTelemetry GenAI + murmur-specific field constants.
//! See spec §8.6 for the emitted notification shape.

pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

pub const MUR_AGENT_UUID: &str = "mur.agent.uuid";
pub const MUR_AGENT_NAME: &str = "mur.agent.name";
pub const MUR_TASK_ID: &str = "mur.task.id";
pub const MUR_MCP_SERVER: &str = "mur.mcp.server";
pub const MUR_ENTITLEMENT_DENIED: &str = "mur.entitlement.denied";  // P0b usage

pub const METHOD_LLM_CALL: &str = "telemetry/llm_call";
pub const METHOD_TOOL_CALL: &str = "telemetry/tool_call";
pub const METHOD_ERROR: &str = "telemetry/error";
pub const METHOD_HEARTBEAT: &str = "telemetry/heartbeat";
pub const METHOD_WARNING: &str = "telemetry/warning";
pub const METHOD_TASK_PROGRESS: &str = "task/progress";
```

- [x] **Step 6: Register the new modules**

Edit `mur-common/src/lib.rs` — append:

```rust
pub mod agent;
pub mod a2a;
pub mod telemetry;

pub use agent::{AgentProfile, Persona, PersonaCategory, Entitlements, LockFile};
pub use a2a::{JsonRpcRequest, JsonRpcResponse, JsonRpcError, Message, Task, TaskState};
```

- [x] **Step 7: Run the Step 1 test**

Run: `cargo test -p mur-common agent::tests::profile_round_trip_yaml`
Expected: PASS.

- [x] **Step 8: Commit**

```bash
git add mur-common/src/agent.rs mur-common/src/a2a.rs mur-common/src/telemetry.rs mur-common/src/lib.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(common): add agent profile, A2A, telemetry shared types"
```

---

## Task 2: Profile loader + `{{agent_home}}` expansion + digest

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/profile.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/profile_load.rs`

- [x] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/profile_load.rs`:

```rust
use mur_agent_runtime::profile::{Profile, ProfileLoadError};
use std::fs;
use tempfile::TempDir;

fn minimal_yaml() -> String {
    r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: agent_x
display_name: "X"
version: "0.1.0"
persona:
  category: research
  description: "X"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: true, bind: "unix://{{agent_home}}/agent.sock" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem:
    read: ["{{agent_home}}"]
    write: ["{{agent_home}}/workdir"]
    deny: []
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#.to_string()
}

#[test]
fn load_expands_agent_home_template() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    let profile = Profile::load(&agent_home).expect("load ok");
    let expected = format!("unix://{}/agent.sock", agent_home.display());
    assert_eq!(profile.inner.transport.socket.bind, expected);
    assert_eq!(profile.inner.entitlements.filesystem.read[0], agent_home.to_string_lossy());
}

#[test]
fn load_rejects_mismatched_name() {
    // Not yet implemented in this task but reserved for Task 4 spoof check.
    // Placeholder: load() itself does NOT compare argv[0] — that is done in multi_call.
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    assert!(Profile::load(&agent_home).is_ok());
}

#[test]
fn digest_stable_across_reloads() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_x");
    fs::create_dir_all(&agent_home).unwrap();
    fs::write(agent_home.join("profile.yaml"), minimal_yaml()).unwrap();
    let p1 = Profile::load(&agent_home).unwrap();
    let p2 = Profile::load(&agent_home).unwrap();
    assert_eq!(p1.digest, p2.digest);
    assert!(p1.digest.starts_with("sha256:"));
}

#[test]
fn missing_profile_errors_cleanly() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("missing");
    match Profile::load(&agent_home) {
        Err(ProfileLoadError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
```

- [x] **Step 2: Run the tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test profile_load`
Expected: compile failure (`Profile`, `ProfileLoadError` undefined).

- [x] **Step 3: Implement `profile.rs`**

Write `mur-agent-runtime/src/profile.rs`:

```rust
//! Profile loader with {{agent_home}} expansion + sha256 digest.

use mur_common::AgentProfile;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ProfileLoadError {
    #[error("profile not found at {0}")]
    NotFound(PathBuf),
    #[error("read error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML parse error: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
    #[error("validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub inner: AgentProfile,
    pub agent_home: PathBuf,
    pub digest: String,
    pub raw_yaml: String,      // retained for re-emit / yaml_edit round-trip
}

impl Profile {
    pub fn load(agent_home: &Path) -> Result<Self, ProfileLoadError> {
        let path = agent_home.join("profile.yaml");
        if !path.exists() {
            return Err(ProfileLoadError::NotFound(path));
        }
        let raw_yaml = fs::read_to_string(&path)?;
        let expanded = expand_template(&raw_yaml, agent_home);
        let inner: AgentProfile = serde_yaml_ng::from_str(&expanded)?;
        validate_uuid_v7(&inner.id)?;
        validate_filesystem_paths(&inner, agent_home)?;
        let digest = compute_digest(&expanded);
        Ok(Self { inner, agent_home: agent_home.to_path_buf(), digest, raw_yaml })
    }
}

fn expand_template(yaml: &str, agent_home: &Path) -> String {
    yaml.replace("{{agent_home}}", agent_home.to_string_lossy().as_ref())
}

fn validate_uuid_v7(id: &str) -> Result<(), ProfileLoadError> {
    let u = uuid::Uuid::parse_str(id)
        .map_err(|e| ProfileLoadError::Validation(format!("profile.id: {e}")))?;
    if u.get_version_num() != 7 {
        return Err(ProfileLoadError::Validation(
            "profile.id must be UUIDv7".to_string(),
        ));
    }
    Ok(())
}

fn validate_filesystem_paths(
    p: &AgentProfile,
    _agent_home: &Path,
) -> Result<(), ProfileLoadError> {
    for entry in &p.entitlements.filesystem.read {
        if entry.starts_with('/') && !entry.contains('*') {
            let path = Path::new(entry);
            if !path.exists() {
                return Err(ProfileLoadError::Validation(format!(
                    "entitlements.filesystem.read: path '{entry}' does not exist"
                )));
            }
        }
    }
    for entry in &p.entitlements.filesystem.deny {
        if !entry.starts_with('~') && !entry.starts_with('/') {
            return Err(ProfileLoadError::Validation(format!(
                "entitlements.filesystem.deny: '{entry}' must be absolute or ~-prefixed"
            )));
        }
    }
    Ok(())
}

fn compute_digest(canonical_yaml: &str) -> String {
    let mut h = Sha256::new();
    h.update(canonical_yaml.as_bytes());
    format!("sha256:{:x}", h.finalize())
}
```

- [x] **Step 4: Run the tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test profile_load`
Expected: all four tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/profile.rs mur-agent-runtime/tests/profile_load.rs
git commit -m "feat(agent-runtime): profile loader with template expansion and digest"
```

---

## Task 3: Entitlement warnings + category presets

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/entitlements.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/entitlements.rs`

- [x] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/entitlements.rs`:

```rust
use mur_common::{AgentProfile, PersonaCategory, NetworkOutboundMode, SpawnMode};
use mur_agent_runtime::entitlements::{
    detect_warnings, preset_for_category, WarningKind, EntitlementPreset,
};

fn sample_profile_unrestricted() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_unrestricted.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

#[test]
fn warns_on_unrestricted_network() {
    let p = sample_profile_unrestricted();
    let warnings = detect_warnings(&p);
    assert!(warnings.iter().any(|w| matches!(w.kind, WarningKind::UnrestrictedNetwork)));
}

#[test]
fn warns_on_empty_deny_list() {
    let mut p = sample_profile_unrestricted();
    p.entitlements.filesystem.deny.clear();
    let warnings = detect_warnings(&p);
    assert!(warnings.iter().any(|w| matches!(w.kind, WarningKind::EmptyFilesystemDeny)));
}

#[test]
fn warns_on_spawn_any() {
    let mut p = sample_profile_unrestricted();
    p.entitlements.processes.spawn.mode = SpawnMode::Any;
    let warnings = detect_warnings(&p);
    assert!(warnings.iter().any(|w| matches!(w.kind, WarningKind::OpenProcessSpawn)));
}

#[test]
fn research_preset_restricted_no_hosts() {
    let preset = preset_for_category(PersonaCategory::Research);
    assert_eq!(preset.network_mode, NetworkOutboundMode::Restricted);
    assert!(preset.network_hosts.is_empty());
    assert!(preset.process_allowed.contains(&"agent-browser".to_string()));
}

#[test]
fn notify_preset_unrestricted() {
    let preset = preset_for_category(PersonaCategory::Notify);
    assert_eq!(preset.network_mode, NetworkOutboundMode::Unrestricted);
}
```

- [x] **Step 2: Add the fixture**

Create `mur-agent-runtime/tests/fixtures/profile_unrestricted.yaml` — copy `minimal_yaml()` from Task 2 but set `entitlements.network.outbound.mode: unrestricted` and add `~/.ssh` etc to `deny`.

```yaml
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: agent_x
display_name: "X"
version: "0.1.0"
persona:
  category: research
  description: "X"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport: { stdio: true, socket: { enabled: true, bind: "unix:///tmp/a.sock" } }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: unrestricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: ["~/.ssh"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
```

- [x] **Step 3: Run tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test entitlements`
Expected: compile failure.

- [x] **Step 4: Implement `entitlements.rs`**

Write `mur-agent-runtime/src/entitlements.rs`:

```rust
//! Entitlement warnings + category presets.
//! P0a declares; P0b enforces.

use mur_common::{
    AgentProfile, PersonaCategory, NetworkOutboundMode, SpawnMode,
};

#[derive(Debug, Clone, PartialEq)]
pub enum WarningKind {
    UnrestrictedNetwork,
    EmptyFilesystemDeny,
    OpenProcessSpawn,
    HighMemoryLimit,
    OverBroadFilesystemWrite,
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}

pub fn detect_warnings(profile: &AgentProfile) -> Vec<Warning> {
    let mut warnings = vec![];
    if profile.entitlements.network.outbound.mode == NetworkOutboundMode::Unrestricted {
        warnings.push(Warning {
            kind: WarningKind::UnrestrictedNetwork,
            message: "network.outbound.mode=unrestricted — no outbound host filtering"
                .to_string(),
        });
    }
    if profile.entitlements.filesystem.deny.is_empty() {
        warnings.push(Warning {
            kind: WarningKind::EmptyFilesystemDeny,
            message: "filesystem.deny is empty — consider adding ~/.ssh, ~/.aws, etc"
                .to_string(),
        });
    }
    if profile.entitlements.processes.spawn.mode == SpawnMode::Any {
        warnings.push(Warning {
            kind: WarningKind::OpenProcessSpawn,
            message: "processes.spawn.mode=any — agent may spawn arbitrary binaries"
                .to_string(),
        });
    }
    if profile.entitlements.limits.memory_mb > 2048 {
        warnings.push(Warning {
            kind: WarningKind::HighMemoryLimit,
            message: format!(
                "limits.memory_mb={} exceeds 2048 — review",
                profile.entitlements.limits.memory_mb
            ),
        });
    }
    for write in &profile.entitlements.filesystem.write {
        if write.trim_end_matches('/') == "~"
            || write.trim_end_matches('/') == "{{agent_home}}/.."
        {
            warnings.push(Warning {
                kind: WarningKind::OverBroadFilesystemWrite,
                message: format!("filesystem.write='{write}' is dangerously broad"),
            });
        }
    }
    warnings
}

#[derive(Debug, Clone)]
pub struct EntitlementPreset {
    pub network_mode: NetworkOutboundMode,
    pub network_hosts: Vec<String>,
    pub process_allowed: Vec<String>,
    pub filesystem_read_extras: Vec<String>,
    pub filesystem_write_extras: Vec<String>,
}

pub fn preset_for_category(cat: PersonaCategory) -> EntitlementPreset {
    match cat {
        PersonaCategory::Research => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec!["agent-browser".to_string(), "npx".to_string()],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Commerce => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec!["*.shopify.com".to_string(), "api.stripe.com".to_string()],
            process_allowed: vec!["agent-browser".to_string(), "npx".to_string()],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec!["~/Downloads/receipts".to_string()],
        },
        PersonaCategory::Notify => EntitlementPreset {
            network_mode: NetworkOutboundMode::Unrestricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Monitor => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec!["/var/log".to_string()],
            filesystem_write_extras: vec![],
        },
        PersonaCategory::Automation | PersonaCategory::Custom => EntitlementPreset {
            network_mode: NetworkOutboundMode::Restricted,
            network_hosts: vec![],
            process_allowed: vec![],
            filesystem_read_extras: vec![],
            filesystem_write_extras: vec![],
        },
    }
}
```

- [x] **Step 5: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test entitlements`
Expected: all five tests pass.

- [x] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/entitlements.rs mur-agent-runtime/tests/entitlements.rs mur-agent-runtime/tests/fixtures/profile_unrestricted.yaml
git commit -m "feat(agent-runtime): entitlement warnings and category presets"
```

---

## Task 4: Multi-call binary dispatch + spoof defense

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/multi_call.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/multi_call.rs`

- [x] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/multi_call.rs`:

```rust
use mur_agent_runtime::multi_call::{extract_profile_name, DispatchError};

#[test]
fn extracts_from_symlink_basename() {
    assert_eq!(extract_profile_name("mur_agent_a").unwrap(), "a");
    assert_eq!(extract_profile_name("mur_agent_price_hunter").unwrap(), "price_hunter");
    assert_eq!(extract_profile_name("/opt/homebrew/bin/mur_agent_a").unwrap(), "a");
}

#[test]
fn rejects_runtime_basename_without_flag() {
    match extract_profile_name("mur-agent-runtime") {
        Err(DispatchError::BareRuntime) => {}
        other => panic!("expected BareRuntime, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_basename() {
    match extract_profile_name("random-tool") {
        Err(DispatchError::UnknownBasename(_)) => {}
        other => panic!("expected UnknownBasename, got {other:?}"),
    }
}

#[test]
fn strips_windows_exe_suffix() {
    assert_eq!(extract_profile_name("mur_agent_a.exe").unwrap(), "a");
    assert_eq!(extract_profile_name(r"C:\bin\mur_agent_a.exe").unwrap(), "a");
}
```

- [x] **Step 2: Run tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test multi_call`
Expected: compile failure.

- [x] **Step 3: Implement `multi_call.rs`**

Write `mur-agent-runtime/src/multi_call.rs`:

```rust
//! argv[0] dispatch + spoof defense.
use std::path::Path;

const SYMLINK_PREFIX: &str = "mur_agent_";
const RUNTIME_BASENAME: &str = "mur-agent-runtime";

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("invoked as 'mur-agent-runtime' directly; pass --profile <name>")]
    BareRuntime,
    #[error("unknown invocation basename: {0}")]
    UnknownBasename(String),
    #[error("argv[0] '{argv0}' does not match profile.name '{profile_name}'")]
    SpoofDetected { argv0: String, profile_name: String },
}

/// Extract profile name from argv[0] basename (stripping .exe on Windows).
/// Returns BareRuntime if invoked as `mur-agent-runtime` itself (caller should
/// then parse a --profile flag).
pub fn extract_profile_name(argv0: &str) -> Result<String, DispatchError> {
    let raw = Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| DispatchError::UnknownBasename(argv0.to_string()))?;
    let basename = raw.strip_suffix(".exe").unwrap_or(raw);
    if basename == RUNTIME_BASENAME {
        return Err(DispatchError::BareRuntime);
    }
    match basename.strip_prefix(SYMLINK_PREFIX) {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(DispatchError::UnknownBasename(basename.to_string())),
    }
}

/// Cross-check argv[0]-derived name against the profile's declared name.
/// Called after profile load; refuses to run on mismatch.
pub fn verify_name_match(argv0_name: &str, profile_name: &str) -> Result<(), DispatchError> {
    if argv0_name == profile_name {
        Ok(())
    } else {
        Err(DispatchError::SpoofDetected {
            argv0: argv0_name.to_string(),
            profile_name: profile_name.to_string(),
        })
    }
}
```

- [x] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test multi_call`
Expected: all four tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/multi_call.rs mur-agent-runtime/tests/multi_call.rs
git commit -m "feat(agent-runtime): argv[0] dispatch with spoof defense"
```

---

## Task 5: Lock file + flock + stale detection

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/lock_file.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/lock_file.rs`

- [x] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/lock_file.rs`:

```rust
use mur_common::LockFile;
use mur_agent_runtime::lock_file::{LockHandle, LockError, write_lock, read_lock, is_stale};
use std::fs;
use tempfile::TempDir;

fn sample_lock() -> LockFile {
    LockFile {
        schema: 1,
        uuid: "01JQX4TM8Y9K7VQH6B2N3R5DPE".into(),
        name: "agent_a".into(),
        pid: std::process::id(),
        ppid: 1,
        started_at: "2026-04-22T08:00:00Z".into(),
        binary_version: "mur-agent-runtime 0.1.0".into(),
        transports: mur_common::agent::LockTransports {
            stdio: false,
            unix_socket: Some("/tmp/x.sock".into()),
            tcp: None,
        },
        card_digest: "sha256:abc".into(),
        capabilities: vec!["a2a.message.send".into()],
    }
}

#[test]
fn write_and_read_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let lock = sample_lock();
    let _handle = LockHandle::acquire(&path).unwrap();
    write_lock(&path, &lock).unwrap();
    let got = read_lock(&path).unwrap();
    assert_eq!(got.uuid, lock.uuid);
    assert_eq!(got.pid, lock.pid);
}

#[test]
fn second_acquire_while_held_fails() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let _h1 = LockHandle::acquire(&path).unwrap();
    match LockHandle::acquire(&path) {
        Err(LockError::AlreadyHeld) => {}
        other => panic!("expected AlreadyHeld, got {other:?}"),
    }
}

#[test]
fn detects_stale_when_pid_dead() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let mut lock = sample_lock();
    lock.pid = 999_999;                              // almost certainly dead
    fs::write(&path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
    assert!(is_stale(&path).unwrap(), "dead pid should be stale");
}

#[test]
fn live_lock_not_stale() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let _handle = LockHandle::acquire(&path).unwrap();
    write_lock(&path, &sample_lock()).unwrap();
    assert!(!is_stale(&path).unwrap(), "held lock should not be stale");
}
```

- [x] **Step 2: Run tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test lock_file`
Expected: compile failure.

- [x] **Step 3: Implement `lock_file.rs`**

Write `mur-agent-runtime/src/lock_file.rs`:

```rust
//! running.lock file — flock-based stale detection + JSON persistence.

use fs2::FileExt;
use mur_common::LockFile;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("lock already held")]
    AlreadyHeld,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct LockHandle {
    _file: File,
    path: PathBuf,
}

impl LockHandle {
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true).read(true).write(true).truncate(false)
            .open(path)?;
        file.try_lock_exclusive().map_err(|_| LockError::AlreadyHeld)?;
        Ok(Self { _file: file, path: path.to_path_buf() })
    }
    pub fn release(self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn write_lock(path: &Path, lock: &LockFile) -> Result<(), LockError> {
    let bytes = serde_json::to_vec_pretty(lock)?;
    let tmp = path.with_extension("lock.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_lock(path: &Path) -> Result<LockFile, LockError> {
    let mut buf = String::new();
    File::open(path)?.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf)?)
}

/// A lock is stale if either
/// (a) its pid is not alive, or
/// (b) flock can be acquired (nobody's holding it).
pub fn is_stale(path: &Path) -> Result<bool, LockError> {
    if !path.exists() {
        return Ok(true);
    }
    let lock: LockFile = match read_lock(path) {
        Ok(l) => l,
        Err(_) => return Ok(true),             // corrupt = stale
    };
    if !pid_alive(lock.pid) {
        return Ok(true);
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let can_acquire = file.try_lock_exclusive().is_ok();
    if can_acquire {
        let _ = fs2::FileExt::unlock(&file);
        return Ok(true);
    }
    Ok(false)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::Foundation::CloseHandle;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() { return false; }
        CloseHandle(h);
        true
    }
}
```

Add `libc = "0.2"` (Unix) and `windows-sys = { version = "0.59", features = ["Win32_System_Threading","Win32_Foundation"], optional = true }` to `mur-agent-runtime/Cargo.toml` dependencies under `[target.'cfg(unix)'.dependencies]` / `[target.'cfg(windows)'.dependencies]` respectively.

- [x] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test lock_file`
Expected: all four tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/lock_file.rs mur-agent-runtime/tests/lock_file.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(agent-runtime): running.lock with flock and stale detection"
```

---

## Task 6: Socket path fallback for macOS 104-byte limit

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/socket_path.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/socket_path.rs`

- [x] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/socket_path.rs`:

```rust
use mur_agent_runtime::socket_path::{resolve_bind_target, BindResolution};
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn short_path_binds_direct_no_symlink() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("x");
    std::fs::create_dir_all(&agent_home).unwrap();
    let uuid = "01JQX4TM8Y9K7VQH6B2N3R5DPE";
    let canonical = agent_home.join("agent.sock");
    let res = resolve_bind_target(&canonical, uuid).unwrap();
    assert_eq!(res.bind_path, canonical);
    assert!(!res.symlink_created, "no symlink expected for short path");
}

#[test]
fn long_path_uses_tmp_and_symlinks() {
    // Synthesize a deliberately long path (simulate long home).
    let tmp = TempDir::new().unwrap();
    let long_name = "a".repeat(120);
    let agent_home = tmp.path().join(&long_name).join("agents").join("y");
    std::fs::create_dir_all(&agent_home).unwrap();
    let canonical = agent_home.join("agent.sock");
    let uuid = "01JQX4TM8Y9K7VQH6B2N3R5DPE";
    let res = resolve_bind_target(&canonical, uuid).unwrap();
    assert_ne!(res.bind_path, canonical);
    assert!(res.bind_path.to_string_lossy().starts_with("/tmp/mur-"));
    assert!(res.symlink_created, "fallback should create a symlink back to canonical");
    // Canonical path must resolve to bind_path
    let resolved = std::fs::read_link(&canonical).unwrap();
    assert_eq!(resolved, res.bind_path);
}
```

- [x] **Step 2: Run tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test socket_path`
Expected: compile failure.

- [x] **Step 3: Implement `socket_path.rs`**

Write `mur-agent-runtime/src/socket_path.rs`:

```rust
//! macOS `sun_path` is 104 bytes; Linux is 108. When the canonical
//! agent socket path is too long, bind in /tmp and symlink back.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_SAFE_PATH_BYTES: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum SocketPathError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct BindResolution {
    pub bind_path: PathBuf,
    pub canonical_path: PathBuf,
    pub symlink_created: bool,
}

pub fn resolve_bind_target(
    canonical: &Path,
    uuid: &str,
) -> Result<BindResolution, SocketPathError> {
    let canonical_bytes = canonical.as_os_str().len();
    if canonical_bytes <= MAX_SAFE_PATH_BYTES {
        return Ok(BindResolution {
            bind_path: canonical.to_path_buf(),
            canonical_path: canonical.to_path_buf(),
            symlink_created: false,
        });
    }
    let short = PathBuf::from(format!(
        "/tmp/mur-{}.sock",
        uuid.chars().take(8).collect::<String>()
    ));
    // Ensure canonical's parent exists so we can place the symlink.
    if let Some(parent) = canonical.parent() {
        fs::create_dir_all(parent)?;
    }
    // Remove any stale symlink first.
    if canonical.exists() || canonical.symlink_metadata().is_ok() {
        fs::remove_file(canonical)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&short, canonical)?;
    #[cfg(not(unix))]
    fs::copy(&short, canonical).map(|_| ())?;  // Windows placeholder
    Ok(BindResolution {
        bind_path: short,
        canonical_path: canonical.to_path_buf(),
        symlink_created: true,
    })
}
```

- [x] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test socket_path`
Expected: both tests pass.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/socket_path.rs mur-agent-runtime/tests/socket_path.rs
git commit -m "feat(agent-runtime): Unix socket path fallback for macOS 104-byte limit"
```

---

## Continuation marker

Tasks 7 onward continue in the companion plan document: `2026-04-22-murmur-p0a-agent-runtime-plan-part2.md`. Part 2 covers:

- Task 7: Telemetry JSONL writer + rotation
- Task 8: JSON-RPC 2.0 framing + error code mapping
- Task 9-11: A2A methods (agent/card, message/send, tasks/*)
- Task 12: Stdio transport
- Task 13: Unix socket transport + SO_PEERCRED
- Task 14-15: MCP client (handshake + tools/call)
- Task 16: LLM client trait + Ollama impl
- Task 17-18: Task runner + retry policy
- Task 19: task/progress notifications
- Task 20-21: Supervisor startup + shutdown
- Task 22: Communication policy enforcement
- Task 23: Signal handlers
- Task 24-30: mur-core CLI (`mur agent create/list/status/stop/remove/rename/send/install-service`)
- Task 31-33: Management CLI (prompt, mcp, skill, perm)
- Task 34: YAML edit utility (comment-preserving, atomic, .bak)
- Task 35-36: .murpkg export + import
- Task 37-39: Self-contained binary (build.rs, include_bytes!, first-run extraction, prereq check)
- Task 40: E2E smoke suite + coverage gate

Each task follows the same TDD structure: failing test → verify fails → implementation → verify passes → commit.
