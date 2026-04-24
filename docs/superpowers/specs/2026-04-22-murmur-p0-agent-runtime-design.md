# murmur — P0 Agent Runtime Foundation (Design)

**Status:** Draft — spec under review.
**Date:** 2026-04-22
**Authors:** David + Claude.
**Sibling spec (follow-up):** P0b enforcement delta, to be written after P0a lands.
**Out of scope (separate spec cycles):** P1 Registry + Messaging · P2 Notifications wiring · P3 Agent Workflow (graph-of-agents) · P4 Factory.

---

## 0. Executive Summary

Introduce **murmur** — a sub-agent runtime where each agent is its own OS-native executable, completely independent of the existing `mur` CLI and MUR Commander. An agent is created with `mur agent create agent_a`, which produces a `mur_agent_a` executable (symlink to a shared multi-call runtime binary) plus a profile directory at `~/.mur/agents/agent_a/`. Agents start/stop like any other Unix daemon (`mur_agent_a start`, launchd/systemd integration, double-click), speak A2A v0.3 (JSON-RPC 2.0) over stdio or Unix domain socket, consume MCP servers internally for tools, and declare fine-grained entitlements that P0b will enforce via platform-native sandboxing.

This spec covers **P0a**: the runtime, protocol, profile, discovery, and telemetry foundation. P0b — TCP transport, sandbox enforcement, `message/stream`, push notifications, Agent Card signing — is previewed in §13 and will be a sibling spec.

Agents can be shared two ways: `.murpkg` archive (team sharing among mur users) or a self-contained single binary with profile + sys_prompt + skills embedded (for recipients who do not have mur installed). Profile is manageable via `mur agent {prompt,mcp,skill,perm}` subcommands without hand-editing YAML.

Estimated P0a scope: ~3830 LOC + ~1100 LOC tests (~2 weeks). P0b: ~3150 LOC (~1.5-2 weeks).

---

## 1. Background & Motivation

The existing mur system manages pattern memory for AI assistants. Users increasingly want sub-agents — autonomous, LLM-backed processes with per-agent skills, MCP servers, system prompts, personas, and network policies — that can be launched like apps, composed into workflows, and notify on task completion. These sub-agents must be independent of mur itself (mur can be uninstalled, agents keep running) and must feel OS-native (launchable by launchd/systemd, discoverable by standard Unix means, constrained by standard permission primitives).

2026 literature (MAESTRO, AgentSight, Orchestration of Multi-Agent Systems) converges on three truths:

1. **Process-per-agent beats agent-as-function** for CLI/coding tools (Claude Agent SDK, OpenCode, Cursor Composer).
2. **JSON-RPC 2.0 over stdio and HTTP is the winning IPC**; MCP (tools) + A2A v0.3 (agents) is the dominant protocol split.
3. **Fine-grained declared entitlements, enforced by the OS**, is the modern meaning of "OS executable" (App Sandbox, Flatpak, Snap, AppContainer).

murmur follows all three.

## 2. Goals / Non-goals

### P0a Goals

- **G1.** `mur agent create <name>` produces a standalone executable `mur_agent_<name>` and profile directory `~/.mur/agents/<name>/`.
- **G2.** The created executable can be invoked directly (`mur_agent_a start`), double-clicked (opens Terminal and runs), or managed by launchd/systemd; no dependency on mur or MUR Commander.
- **G3.** Each agent speaks A2A v0.3 (subset) over stdio (default) or Unix domain socket; each is a client of its declared MCP servers.
- **G4.** Each agent emits structured telemetry (OpenTelemetry GenAI conventions) for every LLM call, tool call, error, and heartbeat.
- **G5.** Profile YAML declares identity (name, UUIDv7, persona), transports, model, MCP servers, skills, capabilities (A2A extension keys), communication policy, notifications, and entitlements.
- **G6.** Agents discover each other via filesystem registry (`running.lock` per agent); peer-to-peer A2A calls work without any central daemon.
- **G7.** Communication is opt-in: sender declares `sends_to`, receiver declares `accepts_from`; receiver is authoritative.
- **G8.** An agent can be exported for sharing in two formats: a `.murpkg` archive (profile + sys_prompt + skills for mur users) or a self-contained single executable (runtime + all assets embedded for non-mur recipients). Imports round-trip.
- **G9.** Per-agent `sys_prompt`, MCP servers, skills, and entitlements are manageable via `mur agent <subcommand>` without hand-editing profile.yaml; running agents receive restart warnings when settings change.

### P0a Non-goals

- Sandbox enforcement (declaration only; enforcement is P0b).
- TCP transport (Unix-socket available in P0a; TCP binding is P0b, where it forces Bearer auth).
- `message/stream` A2A method (P0a uses ad-hoc `task/progress` notifications instead).
- Agent Card signing (P0b).
- `tasks/pushNotificationConfig/set` (P0b — P0a declares notification config, delivery is P0b).
- Orchestrator daemon (P1).
- Agent workflow / factory composition (P3/P4).
- Windows full support (P1; P0a builds on Windows but sandbox + daemonization are partial).
- Platform-native export bundles — macOS `.app`, Linux AppImage, Windows installer (P1; `.murpkg` and self-contained binary cover P0a sharing needs).
- Embedding MCP server binaries inside exports (documented as prerequisite for the recipient; embedding is rejected as too large and version-locked).

---

## 3. Locked Design Decisions

Six axial decisions plus three refinements, locked through brainstorming:

| # | Axis | Decision | Rationale |
|---|---|---|---|
| D1 | Phase scope | P0 Agent Runtime Foundation, split into P0a and P0b | Reduces protocol-design risk; ships working system in ~2 weeks (P0a includes export/import + management CLI) |
| D2 | Binary model | Each agent = its own executable; created by mur; profile-driven | User's "像 OS 執行檔" requirement; strongest isolation; MCP-server-style distribution |
| D3 | Binary mechanism | Multi-call symlink pattern (BusyBox/git-style); single `mur-agent-runtime` binary, one symlink per agent | One-update-fits-all; minimal disk; no Rust toolchain on user machine |
| D4 | Protocol | A2A v0.3 subset outbound, MCP client inbound | 2026 cross-vendor standards; forward-compatible |
| D5 | LLM | Agent-internal; mandatory OTel-style telemetry back out | Autonomy + observability; closes MAESTRO's "silent information consumption" gap |
| D6 | Topology | Default peer-to-peer via filesystem registry; optional orchestrator in P1 for star routing | More Unix-native; no mandatory daemon |
| D7 | Network policy | Declarative entitlements + platform-native sandbox (declared in P0a, enforced in P0b) | Matches App Sandbox / Flatpak pattern |
| D8 | Streaming | P0a: `task/progress` JSON-RPC notifications (fire-and-forget) during execution; P0b: full A2A `message/stream` with SSE | 90% UX at 10% cost; clean upgrade path |
| D9 | Telemetry naming | OpenTelemetry GenAI semantic conventions (`gen_ai.*`) + `mur.*` prefix for project-specific fields | OTel ecosystem portability |
| D10 | HTTP auth | Unix domain socket default (file-mode 0600 = auth); opt-in TCP + Bearer token | Unix-native; zero token management in common case |
| D11 | Communication policy enforcement | Receiver is authoritative (`accepts_from`); sender-side `sends_to` is intent/perf/observability filter, not a security boundary | Avoids false sense of security from sender-only checks |
| D12 | macOS 104-byte socket path | Symlink fallback when direct path > 100 bytes: bind at `/tmp/mur-<uuid8>.sock`, symlink `agent_home/agent.sock → short path` | Single discovery path for clients |
| D13 | Organization | Same Cargo workspace, new crate `mur-agent-runtime`; `mur-core` gets thin `cmd/agent.rs` for create/list | Keeps distribution simple; shared `mur-common` types |
| D14 | `mur agent list` UX | 7-column dense table by default (`docker ps` style) + rich `mur agent status <name>` detail (`systemctl status` style) | Glance-friendly + drill-down |
| D15 | Export formats | Both `.murpkg` (tar.gz: profile + sys_prompt + skills; recipient needs `mur-agent-runtime`) and self-contained single-binary (runtime + all assets embedded via `include_bytes!`); MCP server deps are prerequisites checked at startup with actionable error messages | Covers "team share" and "share-with-outsider" scenarios in one phase; rejects MCP-embedding as too heavy |
| D16 | Management CLI surface | `mur agent {prompt,mcp,skill,perm} <subverb>` mutates profile.yaml + sibling files with schema validation; running agents get a restart-required warning on change | Avoids making profile.yaml hand-editing the only management path |

---

## 4. Architecture Overview

```
Install layout:
  /opt/homebrew/bin/              (or ~/.cargo/bin, or $MUR_AGENT_BIN_DIR)
    mur                           ← existing CLI (unchanged behaviour)
    mur-agent-runtime             ← NEW: multi-call binary
    mur_agent_a → mur-agent-runtime   (symlink, one per agent)
    mur_agent_b → mur-agent-runtime
    ...

Workspace layout (same Cargo workspace):
  ~/Projects/mur/
    mur-common/                   (unchanged; add a2a/telemetry/agent types)
    mur-core/                     (unchanged; add cmd/agent.rs — thin create/list)
    mur-agent-runtime/            ← NEW crate (the multi-call binary)

~/.mur/ layout (additive):
  ~/.mur/
    agents/                       ← NEW
      agent_a/
        profile.yaml              ← source of truth (Agent Card + config)
        sys_prompt.md
        skills/
        workdir/
        telemetry/YYYY-MM-DD.jsonl
        logs/stderr.log
        running.lock              ← present while running; auto-cleaned if stale
        agent.sock                ← Unix socket (or symlink to /tmp/mur-*.sock)
    patterns/                     (unchanged)
    workflows/                    (unchanged)
    conversations/                (unchanged)
    config.yaml                   (add [agents] section)

Runtime process model (P0a):
  ┌─────────────────────────────────────────────────────────────────┐
  │  mur_agent_a (= mur-agent-runtime, argv[0] dispatch)            │
  │                                                                  │
  │  ┌───────────────┐  ┌───────────────┐  ┌──────────────────────┐ │
  │  │ A2A server    │  │ LLM client    │  │ MCP clients          │ │
  │  │ (stdio / unix │  │ (internal,    │  │ (one per profile     │ │
  │  │  socket)      │  │ profile.model)│  │  .mcp_servers entry) │ │
  │  └───────┬───────┘  └──────┬────────┘  └──────┬───────────────┘ │
  │          │                 │                  │                  │
  │  ┌───────┴─────────────────┴──────────────────┴───────────────┐ │
  │  │                 Task orchestrator                          │ │
  │  │   (receives message/send, runs LLM + tool calls,           │ │
  │  │    emits task/progress + telemetry/* notifications)        │ │
  │  └────────────────────────────────────────────────────────────┘ │
  │                             │                                    │
  │                             ▼                                    │
  │  ┌─────────────────────────────────────────────────────────┐   │
  │  │   Telemetry writer (JSONL per day + OTel field names)    │   │
  │  └─────────────────────────────────────────────────────────┘   │
  └─────────────────────────────────────────────────────────────────┘
        ▲                                                     ▲
        │ A2A peer calls                                     │ MCP stdio
        │                                                     │ (subprocess)
   another agent                                         browser, filesystem,
   or CLI caller                                         custom MCP servers
```

## 5. Profile Schema + Directory Layout

Single source of truth: `~/.mur/agents/<name>/profile.yaml`.

```yaml
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE          # UUIDv7 (time-sortable)
name: agent_a                            # unique; matches mur_agent_<name>
display_name: "Price Hunter"
version: "0.1.0"

persona:
  category: research                     # research | automation | monitor | notify | commerce | custom
  description: "Finds and compares product prices across Taiwan e-commerce sites"
  traits:
    tone: concise                        # concise | friendly | formal | playful
    risk: cautious                       # cautious | balanced | bold
    verbosity: low                       # low | medium | high

sys_prompt_file: "sys_prompt.md"

model:
  provider: ollama                       # ollama | anthropic | openai | openrouter | custom
  name: "llama3.2:3b"
  params: { temperature: 0.2, max_tokens: 4096 }

mcp_servers:
  - name: filesystem
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "{{agent_home}}/workdir"]
  - name: browser
    command: "agent-browser"
    args: ["mcp"]

skills:
  - "skills/price-extraction.md"
  - "skills/tw-retailer-map.md"

transport:
  stdio: true                            # permit stdio transport (foreground / spawned mode)
  socket:
    enabled: true                        # permit socket transport (detached / daemon mode)
    bind: "unix:///Users/david/.mur/agents/agent_a/agent.sock"   # unix:// available in P0a
    # P0b also supports TCP binding; opt-in forces auth:
    # bind: "tcp://127.0.0.1:8080"
    # auth: { scheme: bearer, token_file: "{{agent_home}}/token" }

communication:
  accepts_from: ["*"]                    # glob; receiver enforces (authoritative)
  sends_to: ["notify_a"]                 # intent/perf filter; NOT a security boundary

capabilities:
  - "a2a.message.send"
  - "a2a.tasks"
  # optional, when relevant:
  # - "commerce.ucp"

entitlements:
  network:
    inbound:
      ports: []
    outbound:
      mode: "restricted"                 # unrestricted | restricted | off
      allow_hosts:
        - "api.anthropic.com"
        - "*.pchome.com.tw"
        - "localhost:11434"
      protocols: ["tcp"]
      resolve_dns:
        mode: "system"                   # system | proxy | static | off
  filesystem:
    read:
      - "{{agent_home}}"
    write:
      - "{{agent_home}}/workdir"
      - "{{agent_home}}/telemetry"
      - "{{agent_home}}/logs"
    deny:
      - "~/.ssh"
      - "~/.aws"
      - "~/.gnupg"
      - "~/.config/gh"
  processes:
    spawn:
      mode: "allowlist"                  # allowlist | any | none
      allowed:                            # MCP commands auto-merged at load time
        - "agent-browser"
        - "npx"
  syscalls:
    mode: "default"                      # default | strict (P0b strict)
  limits:
    memory_mb: 512
    file_descriptors: 1024
    processes: 32

notifications:
  on_task_complete:
    - target: agent                      # agent | commander | email | slack | webpush | webhook
      name: notify_a
    - target: email
      address: "david@twdd.com.tw"
  on_error:
    - target: slack
      webhook_url_env: SLACK_ALERT_WEBHOOK
  on_shutdown: []

retry:
  llm:
    max_retries: 3
    backoff: exponential                 # linear | exponential | fixed
    initial_delay_ms: 1000
    max_delay_ms: 30000
    retry_on: [rate_limit, timeout, connection_error]
  tool:
    max_retries: 1
    backoff: fixed
    initial_delay_ms: 500

lifecycle:
  restart: on_failure                    # never | on_failure | always
  max_restarts: 3
  restart_window_secs: 600               # restart counter reset window
  stop_timeout_secs: 15
  mcp_required: true                     # fail to start if any MCP server fails

created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
```

**`{{agent_home}}` template variable** resolves to `~/.mur/agents/<name>/`. All relative paths in the profile resolve against this root, so an agent directory is relocatable by copy.

**Category presets** (injected by `mur agent create` based on `persona.category`):

| category | network.outbound | filesystem extras | processes allowed |
|---|---|---|---|
| `research` | restricted + prompt for hosts | agent_home only | MCP-derived + `agent-browser` |
| `commerce` | restricted + common retailers | agent_home + `~/Downloads/receipts` | MCP-derived |
| `notify` | unrestricted (needs slack/email) | agent_home only | MCP-derived |
| `monitor` | restricted + common monitoring endpoints | agent_home + `/var/log` (read) | MCP-derived |
| `automation` | restricted empty (user fills) | agent_home | MCP-derived |
| `custom` | restricted empty | agent_home only | MCP-derived |

## 6. Binary Creation & Multi-call Dispatch

### 6.1 `mur agent create`

```bash
$ mur agent create agent_a
? Display name: Price Hunter
? Category [research|automation|monitor|notify|commerce|custom]: research
? One-line description: Finds and compares prices across TW e-commerce sites
? LLM provider: ollama
? Model: llama3.2:3b
? Start with which skills? price-extraction
? Network access hosts (comma-separated): *.pchome.com.tw,*.books.com.tw

✓ Created ~/.mur/agents/agent_a/
✓ Wrote profile.yaml (with category:research preset)
✓ Generated sys_prompt.md template
✓ Created symlink /opt/homebrew/bin/mur_agent_a → mur-agent-runtime
✓ UUID: 01JQX4TM8Y9K7VQH6B2N3R5DPE

Next steps:
  edit ~/.mur/agents/agent_a/sys_prompt.md
  mur_agent_a start          # foreground
  mur_agent_a card            # inspect Agent Card
  mur agent list              # see all agents
```

Non-interactive: `mur agent create agent_a --no-interactive --display-name="..." --model=... [--from template.yaml]`.

### 6.2 Symlink install directory

Resolution order (first writable wins):

1. `$MUR_AGENT_BIN_DIR`
2. Same directory as `mur-agent-runtime` (Homebrew: `/opt/homebrew/bin`; cargo-install: `~/.cargo/bin`)
3. `~/.local/bin`
4. `~/.mur/bin` (last resort; emits a warning instructing user to export PATH)

### 6.3 Multi-call dispatch

```rust
// mur-agent-runtime/src/main.rs (sketch)
fn main() {
    let argv0 = std::env::args_os().next().unwrap();
    let basename = Path::new(&argv0).file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("mur-agent-runtime");

    let profile_name = match basename.strip_prefix("mur_agent_") {
        Some(name) => name.to_string(),
        None if basename == "mur-agent-runtime" => parse_profile_from_args(),
        None => bail("unknown invocation"),
    };

    let agent_home = PathBuf::from(env::var("MUR_HOME").unwrap_or_else(|_| expand("~/.mur")))
        .join("agents").join(&profile_name);

    let profile = Profile::load(&agent_home.join("profile.yaml"))?;
    if profile.name != profile_name { bail("binary name / profile name mismatch"); }

    run(profile, agent_home, parse_subcommand()).await
}
```

**Spoof defense:** runtime cross-checks `argv[0]`'s derived name against `profile.name`. Mismatch → refuse start.

### 6.4 Subcommands (every `mur_agent_<name>` supports)

```
mur_agent_a start [--http :PORT] [--foreground|--detach]
mur_agent_a stop
mur_agent_a status
mur_agent_a card
mur_agent_a send '<json>'
mur_agent_a logs [--tail]
mur_agent_a stats
mur_agent_a --help
mur_agent_a --version
```

`mur agent <cmd>` on the main mur CLI is a thin shell that invokes these for one or all agents.

### 6.5 Cross-platform symlink / hardlink

| Platform | Mechanism | API |
|---|---|---|
| macOS | symlink | `std::os::unix::fs::symlink` |
| Linux | symlink | `std::os::unix::fs::symlink` |
| Windows | hardlink (no admin needed; Win10 1607+) | `std::fs::hard_link` |

### 6.6 Remove / rename

```
mur agent remove agent_a           # delete symlink; keep ~/.mur/agents/agent_a/
mur agent remove agent_a --purge   # also delete the directory
mur agent rename agent_a a_new     # stop → rename dir → update profile.name → update symlink
```

UUID is immutable across rename; only `name` and filesystem path change.

### 6.7 Export & Import (sharing)

Two supported export formats (D15). MCP server binaries are **not** embedded — they are the recipient's prerequisites, checked at startup.

#### 6.7.1 `.murpkg` format (for mur users)

```bash
mur agent export agent_a --format=pkg -o agent_a.murpkg
```

The `.murpkg` file is a gzip'd tarball with this structure:

```
agent_a.murpkg (tar.gz)
├── manifest.yaml              # mur-agent package manifest v1
├── profile.yaml               # sanitized profile (absolute paths rewritten to {{agent_home}})
├── sys_prompt.md
├── skills/*.md
└── README.md                  # auto-generated — MCP prerequisites, sample commands
```

`manifest.yaml`:

```yaml
schema: "mur-agent-package/1"
exported_at: "2026-04-22T10:00:00+08:00"
exported_by: "david@twdd.com.tw"
source_runtime_version: "mur-agent-runtime 0.1.0"
min_runtime_version: "0.1.0"
original_uuid: "01JQX4TM8Y9K7VQH6B2N3R5DPE"
sanitized:
  removed_fields: ["notifications.on_error[0].webhook_url_env"]  # secrets filtered
prerequisites:
  mcp_servers:
    - name: filesystem
      command_basename: "npx"
      hint: "npm install -g npx"
    - name: browser
      command_basename: "agent-browser"
      hint: "brew install agent-browser"
```

Import:

```bash
mur agent import agent_a.murpkg                # uses packaged name
mur agent import agent_a.murpkg --as other_a   # rename on import
```

On import:

1. Validate manifest schema + runtime version compatibility.
2. Check `prerequisites.mcp_servers[].command_basename` available in `$PATH`; warn each missing.
3. Generate **new** UUID (v7) — never reuse the original (traceability + avoid collision if both sides run the same agent).
4. Write to `~/.mur/agents/<name>/` and create symlink.

Secrets handling: fields known to carry credentials (`*.webhook_url`, `*.token`, `*.api_key`, env-referenced values) are stripped on export and `manifest.sanitized.removed_fields` records their names. Import prompts for replacements.

#### 6.7.2 Self-contained binary (for non-mur recipients)

```bash
mur agent export agent_a --format=bin -o my_agent            # host platform
mur agent export agent_a --format=bin --target=x86_64-linux -o my_agent.linux
```

Produces a single executable with profile + sys_prompt + skills embedded via Rust `include_bytes!`. Uses `cargo` cross-compilation (via `cross` or native toolchain) for `--target=`.

Runtime behavior:

```
1. Detect embedded-assets signature (magic bytes + version byte at known offset)
2. If $MUR_AGENT_EXTERNAL_PROFILE is set: use external (dev/test override)
   Else: use embedded profile
3. On first run per machine: extract embedded assets to
   $XDG_CACHE_HOME/murmur/<original_uuid>/  (or ~/.cache/murmur/<uuid>/)
   — idempotent; re-extract only if embedded digest changes
4. Treat the cache dir as {{agent_home}}; write running.lock, logs, telemetry, workdir there
5. Check MCP prerequisites from embedded manifest; print actionable errors if missing
6. Continue with standard §7 startup sequence
```

Recipient experience:

```bash
# Linux/macOS
$ ./my_agent start
$ ./my_agent card
$ ./my_agent --help

# Windows
> my_agent.exe start
```

Size envelope: ~6-8 MB compressed (`upx` or `--profile=release-strip`); ~18 MB uncompressed. MCP server binaries are NOT included — recipient must install them separately (listed in `my_agent --help`).

Build plumbing: exported binary is built via a `build.rs` in the `mur-agent-runtime` crate that reads `$MUR_EXPORT_AGENT_DIR` at compile time and emits the embed block. `mur agent export --format=bin` invokes `cargo build --release --features=embedded-agent --target=<target>` with the env var set.

### 6.8 Management CLI (D16)

Each subcommand mutates profile.yaml (or sibling files) with schema validation; if the agent is running, a restart-required warning is printed.

#### System Prompt

```
mur agent prompt <name>               # print sys_prompt.md
mur agent prompt <name> edit          # $EDITOR opens sys_prompt.md
mur agent prompt <name> set "text"    # replace sys_prompt.md
mur agent prompt <name> set -f file   # replace from file
```

#### MCP Servers

```
mur agent mcp list <name>
mur agent mcp add <name> <server_name> <command> [-- args...]
mur agent mcp remove <name> <server_name>
mur agent mcp rename <name> <old> <new>
```

Example:

```bash
mur agent mcp add agent_a playwright npx -- -y @playwright/mcp@latest
mur agent mcp list agent_a
```

Adds/removes entries under `profile.mcp_servers[]` and auto-updates `entitlements.processes.spawn.allowed` with the new command basename.

#### Skills

```
mur agent skill list <name>
mur agent skill add <name> <skill_file>       # copies file into skills/; adds to profile.skills[]
mur agent skill remove <name> <skill_id>      # removes from list; deletes file if orphaned
mur agent skill show <name> <skill_id>
```

#### Permissions / Entitlements

```
mur agent perm show <name>                            # print entire entitlements block
mur agent perm show <name> network                    # print just network

# Network
mur agent perm set-mode <name> network.outbound <unrestricted|restricted|off>
mur agent perm allow-host <name> <glob>
mur agent perm deny-host <name> <glob>
mur agent perm list-hosts <name>

# Filesystem
mur agent perm allow-read <name> <path>
mur agent perm allow-write <name> <path>
mur agent perm deny-path <name> <path>

# Processes
mur agent perm allow-spawn <name> <binary_basename>
mur agent perm deny-spawn <name> <binary_basename>

# Limits
mur agent perm set-limit <name> memory_mb <N>
mur agent perm set-limit <name> file_descriptors <N>
```

All perm mutations validate the resulting entitlement block against schema before writing; reject mutations that would introduce inconsistencies (e.g., `allow-read` of non-existent absolute path).

**Write discipline:** every mutation command uses atomic write (temp file + rename) + preserves YAML comments via `serde_yaml_ng` roundtrip. A timestamped backup is saved to `~/.mur/agents/<name>/.profile.yaml.bak` before each change.

## 7. Process Lifecycle

### 7.1 Launch modes

| Mode | Invocation | Use case |
|---|---|---|
| Foreground | `mur_agent_a start` | Interactive debugging |
| Detached | `mur_agent_a start --detach` | Manual background; fork + setsid + log redirect |
| HTTP daemon | `mur_agent_a start --http :8080 --detach` | A2A HTTP server (TCP requires profile auth; Unix socket allowed in P0a) |
| OS-integrated | launchd/systemd/Task Scheduler | Auto-start at login; crash-restart by OS |

Double-click UX:

- macOS: symlink double-click opens Terminal running `mur_agent_a start`. `.app` bundles optional in P0b.
- Linux: generated `.desktop` entry (`Exec=mur_agent_a start`, `Terminal=true`) for GNOME/KDE.
- Windows: `.lnk` shortcut with `--detach`.

### 7.2 Startup sequence

```
1. Parse argv[0] → profile_name; validate against profile.name (refuse on mismatch)
2. Acquire flock on ~/.mur/agents/<name>/running.lock (LOCK_EX | LOCK_NB; fail → already running)
3. Load + validate profile.yaml; expand {{agent_home}}
4. Warn on loose entitlements (unrestricted / empty deny / spawn.mode=any)
5. Init tracing → stderr + logs/stderr.log (rotate at 10 MB, keep 3)
6. Init telemetry writer → telemetry/YYYY-MM-DD.jsonl
7. Spawn MCP servers from profile.mcp_servers[]; init MCP handshakes
   └─ On failure: if lifecycle.mcp_required → exit(2); else warn and disable that server
8. Init LLM client (or skip if provider=custom)
9. Open A2A server endpoints:
   ├─ transport.stdio=true (default) → frame newline-delimited JSON-RPC 2.0 on stdin/stdout
   └─ transport.socket.enabled + bind=unix://... → UnixListener (with § 9.3 symlink fallback; TCP bind is P0b-only)
10. Probe ~/.mur/orchestrator.sock; register if present (P1); else run standalone
11. Write running.lock with { pid, uuid, started_at, transports, capabilities, card_digest, version }
12. Install SIGTERM/SIGINT handlers (graceful) — SIGKILL not interceptable
13. Event loop: accept requests, dispatch, emit telemetry
```

### 7.3 Shutdown sequence (SIGTERM)

```
1. Set shutting_down flag; A2A server returns 503 to new requests
2. Wait for in-flight tasks up to profile.lifecycle.stop_timeout_secs
3. Cancel timed-out tasks (state=cancelled); emit task_cancelled telemetry
4. Dispatch profile.notifications.on_shutdown
5. Unregister from orchestrator (if registered)
6. Send MCP shutdown RPCs; wait; kill stragglers (5s cap)
7. Flush telemetry writer
8. Remove running.lock
9. exit(0)
```

If the internal timeout is hit before clean exit, the process force-exits with SIGKILL semantics. flock auto-releases even on kill -9, so stale lock is impossible.

### 7.4 Crash / restart

`profile.lifecycle.restart`:

- `never` — exit = die.
- `on_failure` — exit != 0 triggers restart, up to `max_restarts` within `restart_window_secs`.
- `always` — any exit triggers restart.

**Agent never self-respawns.** Restart is the responsibility of the OS supervisor (launchd/systemd) or the P1 orchestrator. This avoids PID confusion and state leakage. `mur agent install-service <name>` generates the platform-specific service file (macOS `.plist` or Linux `systemd --user` unit).

### 7.5 Health check

`agent/card` doubles as health probe: `mur agent status <name>` reads running.lock → dials endpoint → sends `agent/card` → expects response within 5s with matching UUID. On three consecutive failures, the OS supervisor/orchestrator triggers restart (if `restart=on_failure`).

## 8. Protocol Surface

### 8.1 Framing

**stdio:** newline-delimited JSON. Each line is exactly one JSON-RPC 2.0 request, response, or notification. Matches MCP stdio transport rules.

**Unix socket:** HTTP/1.1 framing; JSON-RPC 2.0 in request body for `POST /jsonrpc`.

JSON-RPC 2.0 semantic discrimination:

- `{ jsonrpc, id, method, params }` — request (expects response)
- `{ jsonrpc, id, result }` / `{ jsonrpc, id, error }` — response
- `{ jsonrpc, method, params }` (no `id`) — notification (fire-and-forget; used for telemetry and `task/progress`)

### 8.2 Methods implemented in P0a

| Method | Direction | Purpose | P0a | P0b |
|---|---|---|---|---|
| `agent/card` | inbound | Return Agent Card JSON | ✓ | ✓ |
| `message/send` | inbound | Sync task | ✓ | ✓ |
| `message/stream` | inbound | Streaming task (SSE) | — | ✓ |
| `tasks/get` | inbound | Query task state | ✓ | ✓ |
| `tasks/cancel` | inbound | Cancel running task | ✓ | ✓ |
| `tasks/list` | inbound | List task history | ✓ | ✓ |
| `tasks/pushNotificationConfig/set` | inbound | Configure push on completion | — | ✓ |
| `peer/handshake` | inbound | Upgrade to direct peer link | — | ✓ |
| `task/progress` | outbound (notification) | Ad-hoc progress during sync task | ✓ | ✓ |
| `telemetry/llm_call` | outbound (notification) | LLM call summary | ✓ | ✓ |
| `telemetry/tool_call` | outbound (notification) | MCP tool call summary | ✓ | ✓ |
| `telemetry/error` | outbound (notification) | Runtime error | ✓ | ✓ |
| `telemetry/heartbeat` | outbound (notification) | Every 30 s | ✓ | ✓ |
| `telemetry/warning` | outbound (notification) | Degraded state | ✓ | ✓ |

### 8.3 Task state machine

```
submitted → working → { completed | failed | cancelled }
```

Transitions are one-way. `tasks/get` returns current state; `tasks/cancel` moves working → cancelled.

### 8.4 Example: `agent/card`

```json
{"jsonrpc":"2.0","id":1,"result":{
  "protocolVersion":"a2a/0.3",
  "name":"agent_a",
  "id":"01JQX4TM8Y9K7VQH6B2N3R5DPE",
  "displayName":"Price Hunter",
  "version":"0.1.0",
  "description":"Finds and compares prices across TW e-commerce sites",
  "capabilities":["a2a.message.send","a2a.tasks"],
  "transports":["stdio","unix-socket"],
  "endpoints":{
    "stdio":"pipe://self",
    "unix-socket":"unix:///Users/david/.mur/agents/agent_a/agent.sock"
  },
  "persona":{"category":"research","traits":{"tone":"concise","risk":"cautious","verbosity":"low"}},
  "skills":[{"id":"price-extraction"},{"id":"tw-retailer-map"}],
  "entitlements":{ /* full entitlements block — declaration, not enforcement (P0a) */ }
}}
```

### 8.5 Example: `message/send` + `task/progress`

```json
// request
{"jsonrpc":"2.0","id":2,"method":"message/send","params":{
  "message":{"role":"user","parts":[{"kind":"text","text":"Find AirPods Pro 3 prices on PChome 24h"}]}
}}

// agent emits during execution (notifications; no id)
{"jsonrpc":"2.0","method":"task/progress","params":{"task_id":"task-01JQXT…","stage":"llm_reasoning","percent":20}}
{"jsonrpc":"2.0","method":"task/progress","params":{"task_id":"task-01JQXT…","stage":"tool_call","message":"agent-browser.snapshot","percent":60}}
{"jsonrpc":"2.0","method":"telemetry/tool_call","params":{"mcp_server":"browser","tool":"snapshot","duration_ms":820,"ok":true,"mur.agent.name":"agent_a"}}

// final response
{"jsonrpc":"2.0","id":2,"result":{
  "id":"task-01JQXT…","state":"completed","messages":[…],
  "createdAt":"2026-04-22T08:05:17Z","completedAt":"2026-04-22T08:05:23Z",
  "usage":{"gen_ai.usage.input_tokens":412,"gen_ai.usage.output_tokens":587}
}}
```

### 8.6 Telemetry payload shape

```json
{"jsonrpc":"2.0","method":"telemetry/llm_call","params":{
  "trace_id":"…","span_id":"…","task_id":"…",
  "gen_ai.system":"ollama",
  "gen_ai.request.model":"llama3.2:3b",
  "gen_ai.usage.input_tokens":412,
  "gen_ai.usage.output_tokens":587,
  "latency_ms":3840,
  "cost_usd":0.0,
  "mur.agent.uuid":"01JQX4…",
  "mur.agent.name":"agent_a",
  "mur.task.id":"task-01JQXT…",
  "ts":"2026-04-22T08:05:23Z"
}}
```

Field naming: OpenTelemetry GenAI semantic conventions for LLM standard fields; `mur.*` namespace for project-specific fields (agent identity, task correlation, MCP server tagging, entitlement violations in P0b).

### 8.7 MCP client behavior

On startup, for each entry in `profile.mcp_servers[]`:

1. `Command::new(cmd).args(args).spawn()` as subprocess, wired to stdio.
2. Run MCP `initialize` handshake; negotiate protocol version and capabilities.
3. Enumerate `tools/list`, `resources/list`, `prompts/list`; merge into agent's internal tool registry.

During task execution, the agent's LLM emits `tool_use`; agent maps tool name to MCP server; sends `tools/call` RPC; returns result as observation.

On MCP failure: if `lifecycle.mcp_required=true`, task fails and agent shuts down; else the specific server is marked unavailable and task proceeds with remaining tools.

### 8.8 Error codes

| Code | Meaning |
|---|---|
| `-32700` | Parse error (invalid JSON) |
| `-32600` | Invalid request |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32603` | Internal error |
| `-32000` | A2A: task not found |
| `-32001` | A2A: task already completed |
| `-32002` | A2A: task cancelled |
| `-32010` | murmur: capability not supported (e.g. P0b-only method called on P0a) |
| `-32011` | murmur: communication denied (caller not in `accepts_from`) |

### 8.9 Authentication (P0a)

- stdio: implicit — trust follows from OS process ownership (whoever spawned the runtime).
- Unix socket: implicit — `chmod 0600`, so only the owning user can connect. Identity of caller resolved via `SO_PEERCRED` (Linux) / `LOCAL_PEERCRED` (macOS).
- TCP: not available in P0a; when opted-in in P0b, requires Bearer token from `{{agent_home}}/token`.

## 9. Discovery & Registry

### 9.1 Filesystem as registry

No central daemon in P0a. Each running agent writes `~/.mur/agents/<name>/running.lock`:

```json
{
  "schema": 1,
  "uuid": "01JQX4TM8Y9K7VQH6B2N3R5DPE",
  "name": "agent_a",
  "pid": 48921,
  "ppid": 48900,
  "started_at": "2026-04-22T08:05:17Z",
  "binary_version": "mur-agent-runtime 0.1.0",
  "transports": {
    "stdio": false,
    "unix_socket": "/Users/david/.mur/agents/agent_a/agent.sock",
    "tcp": null
  },
  "card_digest": "sha256:b9f2…",
  "capabilities": ["a2a.message.send","a2a.tasks"]
}
```

### 9.2 `mur agent list` implementation

Walks `~/.mur/agents/*/running.lock`. For each lock: if PID is alive AND flock cannot be acquired, agent is running; else the lock is stale and is cleaned. Output is the 7-column dense table (D14).

```
NAME       STATUS    UPTIME   PID     TASKS   MEM     CATEGORY
agent_a    running   2h14m    48921   1       180M    research
agent_b    running   2h14m    48935   0       95M     commerce
notify_a   stopped   —        —       —       —       notify
```

`mur agent list --json` for scripts; `--capability=...` / `--running` for filtering; `--verbose` adds ENDPOINT + CAPABILITIES columns.

`mur agent status <name>` provides the `systemctl status`-style detail view (active time, PID, active/total tasks, memory, endpoint, capabilities, MCP server health, last task).

### 9.3 Unix socket path convention

Default: `~/.mur/agents/<name>/agent.sock` at mode 0600.

**macOS 104-byte fallback:** if the expanded path exceeds 100 bytes, runtime binds at `/tmp/mur-<uuid8>.sock` and creates a symlink `agent_home/agent.sock → /tmp/mur-<uuid8>.sock`. Clients always use the canonical `agent_home/agent.sock` path (kernel follows the symlink). Lock file still records `agent_home/agent.sock` as the canonical discovery path. Stale `/tmp/mur-*.sock` entries are cleaned by the same scan pass that removes stale running.lock files.

### 9.4 Peer discovery and calls

```rust
async fn send_to_peer(peer_name: &str, msg: Message) -> Result<TaskResult> {
    // 1. Sender-side intent filter (perf + observability, NOT security)
    if !profile().communication.sends_to.allows(peer_name) {
        return Err(Error::CommunicationDenied);
    }
    // 2. Discovery via filesystem
    let lock = LockFile::read(&agents_dir().join(peer_name).join("running.lock"))?;
    // 3. Connect over Unix socket (canonical path; symlink if needed)
    let stream = UnixStream::connect(lock.transports.unix_socket).await?;
    // 4. Send A2A message/send
    A2aClient::over_stream(stream).message_send(msg).await
}
```

Receiver-side verification runs on every inbound request: resolves caller from `SO_PEERCRED` pid → reverse-look-up against running.lock files → if caller agent name is not in `accepts_from`, return `-32011`. Policies record denials via `telemetry/error {kind:CommDenied}`.

### 9.5 Orchestrator upgrade path (P1)

On startup, agent probes `~/.mur/orchestrator.sock`. If present:

1. POST `/agents/register { name, uuid, lock_path }`.
2. Route peer calls via orchestrator (star); audit log is centralized.
3. Subscribe to policy updates (white-list changes propagate without agent restart).

If the socket disappears mid-run, agent reconnects with exponential backoff and operates peer-to-peer in the interim.

## 10. Entitlement Declaration

### 10.1 P0a: declaration only

The `entitlements` block (§5) is loaded, validated, exposed in Agent Card, and recorded in OTel telemetry at startup. **No actual enforcement.** The runtime runs with the host user's full privileges.

Effects of a declaration in P0a:

1. Schema validation on load (hard error for enum violation, non-existent absolute read paths, missing processes.spawn entries for MCP servers, etc.).
2. Warnings for loose settings (`unrestricted`, empty `deny`, `spawn.mode=any`, `memory_mb > 2048`); `mur agent list` flags these with ⚠.
3. `agent/card` response includes the full entitlements block so callers/orchestrators can reason about intent.
4. `MUR_ENTITLEMENTS_JSON` environment variable is set when spawning the runtime so third-party agent implementations that opt into self-limiting can read and honor it.
5. OTel span `agent.entitlements.loaded` at startup for audit.

### 10.2 P0b: enforcement

Same schema, no changes. Backend translates per platform:

| Field | macOS (sandbox-exec) | Linux (bwrap + nftables) | Windows (AppContainer + WFP, P1) |
|---|---|---|---|
| `network.outbound.mode=off` | `(deny network*)` | `--unshare-net` | WFP block-all |
| `network.outbound.mode=restricted` | `(deny network*) (allow network* (remote tcp "..."))` | `bwrap --unshare-net` + netns + nftables allowlist | WFP per-host rule |
| `filesystem.read` | `(allow file-read* (subpath "..."))` | `--ro-bind` | AppContainer ACL |
| `filesystem.write` | `(allow file-write* (subpath "..."))` | `--bind` | AppContainer ACL |
| `filesystem.deny` | `(deny file-read* (subpath "..."))` | Not bound = invisible | ACL deny |
| `processes.spawn.allowed` | `(deny process-exec) + (allow process-exec (literal "..."))` | Filtered PATH + cap-drop | Job Object CREATE_SUSPENDED check |
| `limits.memory_mb` | `ulimit` pre-exec | `prlimit` / cgroup | Job Object mem limit |

Runtime re-execs itself under sandbox (Chromium-style, detected via `MUR_SANDBOXED` env var) to self-apply. DNS handling requires either static pre-resolution or an in-sandbox resolver pointed at a supervisor-side DNS proxy; profile's `resolve_dns.mode` selects which.

### 10.3 Honest documentation requirement

P0a release notes and `mur agent list`'s entitlement display must explicitly state: **"Entitlements are declared but enforcement lands in P0b. Do not rely on sandboxing for security in P0a."** This matches Apple's historical App Sandbox rollout pattern (API stable from day one, enforcement tightened over years).

## 11. Error Handling & Failure Modes

Errors categorized by phase; each has agent behavior, user-visible output, and telemetry expectations.

### 11.1 Load phase (exit 1, does not enter event loop)

| Error | Recovery |
|---|---|
| profile.yaml missing | `error: profile not found at <path>` |
| profile schema invalid | `error: profile.yaml:<line>: <field>: expected X, got Y` |
| argv[0] / profile.name mismatch | `error: binary name '...' does not match profile '...'` |
| UUID not UUIDv7 | `error: profile.id must be UUIDv7` |
| filesystem.read absolute path does not exist | `error: entitlements.filesystem.read: path '...' does not exist` |

### 11.2 Startup phase (exit 2, partial init)

| Error | Recovery |
|---|---|
| running.lock held by live PID | `error: already running (pid=N)` |
| Unix socket bind failure | `error: cannot bind unix socket: <reason>`; telemetry `kind:SocketBind` |
| MCP server spawn failure + `mcp_required=true` | `error: required MCP server '...' failed to start`; telemetry `kind:McpStart` |
| MCP server spawn failure + `mcp_required=false` | Warning on stderr; telemetry `kind:McpUnavailable`; continue with reduced tool set |
| LLM provider unreachable (e.g. Ollama not running) | Startup succeeds; first task fails; telemetry `warning` at startup, `error` at task |

### 11.3 Runtime phase (agent alive, task-level failures)

| Error | Recovery |
|---|---|
| LLM call fails (429/timeout/network) | Task `failed`; retry per profile.retry.llm; telemetry `kind:LLMRateLimit/LLMTimeout` |
| MCP tool call fails | LLM receives error observation; may recover; telemetry `tool_call {ok:false}` |
| Tool call timeout (default 60s, profile-configurable) | Kill tool call; return timeout to LLM; telemetry `kind:Timeout` |
| `tasks/cancel` received | Abort LLM/tool call; state=cancelled; telemetry `task_cancelled` |
| Communication denied (caller not in `accepts_from`) | JSON-RPC `-32011`; never enters task; telemetry `kind:CommDenied, caller:…` |
| Malformed JSON-RPC request | JSON-RPC `-32700` or `-32600`; telemetry `kind:BadRequest` |
| Method not found (P0b-only method on P0a) | JSON-RPC `-32010`; telemetry `kind:UnsupportedMethod` |
| MCP server crashes mid-task | Required: task fails + agent shuts down; Optional: server marked unavailable; telemetry `kind:McpCrash` |
| Memory limit exceeded (P0a monitors; P0b enforces) | P0a: `warning`; P0b: OS kill → lifecycle.restart |

### 11.4 Shutdown phase

| Error | Recovery |
|---|---|
| Graceful (SIGTERM) | Runs §7.3; exit 0; telemetry `shutdown {reason:sigterm}` |
| stop_timeout exceeded | Internal timeout → SIGKILL children → exit 130; telemetry `reason:timeout` |
| Cannot flush telemetry (disk full) | Drop pending notifications + stderr warning |

### 11.5 Infrastructure

| Error | Recovery |
|---|---|
| Orchestrator socket unreachable | 5 retries → fallback to standalone; telemetry warning |
| Orchestrator disconnect mid-run | Exponential backoff reconnect (1s/2s/4s/8s/16s cap); standalone mode during outage |
| Profile modified on disk (file watcher) | No hot reload in P0a — requires restart; `mur agent status` shows "profile drift detected"; telemetry `kind:ProfileDrift` |

### 11.6 Notification trigger rules

`on_error` fires for:

- ✓ Load/startup failures (#11.1, #11.2)
- ✓ Task failures (#11.3: LLM final failure, required MCP crash)
- ✓ Three consecutive LLM retry failures
- ✓ lifecycle.restart triggers

Does not fire for:

- ✗ Single tool call failure (too noisy)
- ✗ Single LLM rate limit (will retry)
- ✗ Communication denied (expected policy behavior)

### 11.7 Task error envelope

```json
{"id":"task-…","state":"failed","messages":[…],
 "error":{
   "code":"llm_rate_limit",
   "message":"Anthropic API returned 429 after 3 retries",
   "recoverable":true,
   "details":{"retries":3,"total_latency_ms":48230}
 }}
```

Error code enum (defined in `mur-common`):
`profile_invalid` / `mcp_start_failed` / `llm_unavailable` / `llm_rate_limit` / `llm_timeout` / `llm_invalid_response` / `tool_timeout` / `tool_failed` / `communication_denied` / `cancelled` / `internal_error` / `capability_not_supported` / `entitlement_violation` (P0b).

## 12. Testing Strategy

### 12.1 Layers

| Layer | Tool | Coverage | Location |
|---|---|---|---|
| Unit | `cargo test` | ≥85% per crate (hard gate via `cargo-llvm-cov`) | `mur-agent-runtime/src/**/mod.rs#[cfg(test)]` |
| Integration | `cargo test --test '*'` | Behavior checklist (all items listed below must exist) | `mur-agent-runtime/tests/*.rs` |
| E2E | Rust helpers + shell | Golden paths + lifecycle | `mur-agent-runtime/tests/e2e/` + `scripts/e2e/` |

### 12.2 Unit coverage priorities

- `profile`: schema validation, defaults, `{{agent_home}}` expansion, UUIDv7 check, warning detection, digest.
- `multi_call`: argv[0] parsing, prefix stripping, spoof defense, direct-invocation `--profile` flag.
- `entitlements`: glob matching, deny-over-allow priority, category preset generation, schema validation.
- `lock_file`: serialize/deserialize, stale detection, symlink-fallback path logic, length > 100 byte detection.
- `communication_policy`: sender filter, receiver verify, glob matching, deny-wins semantics, error codes.
- `telemetry`: OTel field mapping, `mur.*` prefix, JSONL write with rotation, notification assembly.
- `retry_policy`: backoff calculation, `retry_on` matching, max_retries termination.
- `error_codes`: enum → JSON-RPC code mapping, human message format.

### 12.3 Integration tests (all required)

- `integration_profile_load_and_card`
- `integration_stdio_jsonrpc` (with mock LLM HTTP server)
- `integration_unix_socket`
- `integration_unix_socket_fallback` (synthetic long home path)
- `integration_mcp_client` (with mock MCP subprocess)
- `integration_lock_file_lifecycle`
- `integration_graceful_shutdown`
- `integration_communication_policy` (two real subprocesses)
- `integration_orchestrator_optional`
- `integration_retry_policy`
- `integration_communication_peer_cred`
- `integration_export_pkg_roundtrip` — export agent_a → `.murpkg` → import as agent_b → verify all fields + new UUID + stripped secrets
- `integration_export_bin_asset_embed` — build self-contained binary via `build.rs`; verify embedded manifest + asset digest; run it under isolated cache dir; verify Agent Card identical to source
- `integration_export_bin_prereq_missing` — remove MCP command from `PATH`; run exported binary; verify actionable error message
- `integration_mgmt_cli_prompt_edit` — `mur agent prompt set` round-trip; restart-required warning when agent running
- `integration_mgmt_cli_mcp_add_updates_spawn_allowlist` — `mur agent mcp add` also inserts basename into `entitlements.processes.spawn.allowed`
- `integration_mgmt_cli_perm_schema_rejects` — malformed `allow-host` glob rejected with schema error; profile untouched
- `integration_yaml_edit_preserves_comments` — edit via CLI; re-open file; comments intact
- `integration_yaml_edit_atomic_and_backup` — simulate write crash mid-edit; original file still readable; `.bak` present

### 12.4 E2E tests

- `e2e_create_and_launch`
- `e2e_roundtrip_send`
- `e2e_remove_purge`
- `e2e_argv0_spoofing`
- `e2e_list_filters`
- `e2e_export_import_murpkg` — `mur agent export --format=pkg` then `import`; run imported agent; verify card differs only by UUID
- `e2e_export_bin_run_standalone` — `mur agent export --format=bin`; copy binary to a fresh tmpdir with only `PATH` set; run `./my_agent card` — must work without mur installed
- `e2e_mgmt_cli_suite` — run each of `prompt/mcp/skill/perm` subcommands on a test agent; verify profile.yaml state after each

All E2E runs under a temporary `MUR_HOME`; test fixture isolates filesystem state from real `~/.mur/`.

### 12.5 Mock strategy

| Dependency | Mock |
|---|---|
| LLM | Local HTTP stub (`axum` test server); response sequence pre-configured |
| MCP server | Small Rust binary in `tests/fixtures/mock_mcp/` — stub `tools/list` + `tools/call` |
| OS sandbox (P0b) | Skip-if-unavailable; `#[ignore = "requires sandbox"]` otherwise |
| Orchestrator | Local `UnixListener` mock |
| Clock | `tokio::time::pause()` + `advance()` |

### 12.6 CI matrix

| Job | Runners | Scope |
|---|---|---|
| test-unit | Ubuntu + macOS | `cargo test --workspace --lib` |
| test-integration | Ubuntu + macOS | `cargo test --workspace --test '*'` |
| test-e2e | Ubuntu + macOS | `scripts/e2e/run-all.sh` |
| clippy | Ubuntu | `cargo clippy --workspace -- -D warnings` |
| fmt | Ubuntu | `cargo fmt --check` |
| smoke-macos-104byte | macOS | Synthetic long home path → symlink fallback |
| smoke-windows | Windows | Unit + argv[0] + hard-link only (full P1) |

LLM provider integration tests require API keys and run nightly/manually, not in PR CI.

### 12.7 TDD expectation

Every module lands test-first: failing test → implementation → green → refactor. Protocol boundary methods (`agent/card`, `message/send`, `tasks/*`) require 100% line coverage.

## 13. P0b Delta (Preview)

P0b extends P0a without breaking changes. Profile schema, symlinks, registry, and protocol surface remain identical; P0b adds the following:

| Block | Content | Est LOC |
|---|---|---|
| TCP transport (A2A streamable HTTP) | TCP bind with forced Bearer auth; SSE streaming; CORS; body framing (Unix-socket HTTP already in P0a) | ~400 |
| Sandbox enforcement | macOS `sandbox-exec` + Linux `bwrap` + netns + nftables + DNS handling + Chromium-style re-exec | ~700 |
| Peer handshake / direct upgrade | `peer/handshake` method; token exchange; post-handshake direct calls; orchestrator audit | ~300 |
| `message/stream` (full SSE) | Streaming method; cancellation mid-stream; partial-state in `tasks/get` | ~250 |
| `tasks/pushNotificationConfig/set` | Webhook/email/slack delivery; retry queue (at-least-once) | ~350 |
| Agent Card signing | Ed25519 per-install keypair; signed Card responses; optional external CA | ~200 |
| Windows partial | AppContainer basic (file + network ACL); HTTP proxy fallback where sandbox unavailable | ~500 |
| Platform bundles (moved from P0a non-goals) | macOS `.app`, Linux AppImage, Windows NSIS — wraps self-contained binary with platform metadata + signing | ~450 |

**Total P0b:** ~3150 LOC, ~1.5-2 weeks focused.

**Breaking-change guarantee:** none. The transition is additive:

- Existing symlinks work unchanged.
- Existing profile.yaml works unchanged.
- Existing `entitlements` blocks start being enforced; `unrestricted` profiles continue to run permissively; `restricted` profiles start actually blocking.
- Existing stdio tests pass unchanged.
- Methods that returned `-32010 capability not supported` in P0a now return success.

The only behavior change is that entitlement violations convert from warning to block. Release notes will explicitly enumerate this transition.

## 14. Implementation Scope Estimate

### 14.1 P0a modules (new crate `mur-agent-runtime`)

| Module | LOC | Responsibility |
|---|---|---|
| `main.rs` + `multi_call.rs` | 150 | argv[0] dispatch, spoof defense, subcommand routing |
| `profile.rs` | 300 | YAML schema, validation, defaults, `{{agent_home}}` expansion, digest |
| `entitlements.rs` | 200 | Schema, glob matching, category presets, warning detection |
| `supervisor.rs` | 350 | Startup/shutdown sequences, signal handlers, lock file, running state |
| `protocol/a2a.rs` | 400 | JSON-RPC 2.0 framing, method dispatch, error mapping, Agent Card projection |
| `protocol/mcp_client.rs` | 250 | MCP subprocess management, handshake, tool registry |
| `transport/stdio.rs` | 100 | Newline-delimited JSON over stdin/stdout |
| `transport/unix_socket.rs` | 150 | UnixListener, SO_PEERCRED, 104-byte fallback |
| `llm/mod.rs` + per-provider | 200 | LLM client abstraction; Ollama/Anthropic/OpenAI/OpenRouter |
| `task_runner.rs` | 300 | Task state machine, LLM + tool call orchestration, progress notifications |
| `telemetry.rs` | 250 | OTel GenAI + `mur.*` fields, JSONL writer, rotation, notification assembly |
| `communication_policy.rs` | 120 | `sends_to` / `accepts_from` evaluation, caller resolution via peer cred |
| `retry.rs` | 80 | Backoff, max_retries, retry_on matching |
| `export/pkg.rs` | 200 | `.murpkg` tar.gz pack; manifest generation; secret sanitization |
| `export/bin_embed.rs` | 150 | `include_bytes!` manifest; build.rs glue; asset-digest check at runtime |
| `export/extract.rs` | 120 | First-run extraction of embedded assets to `$XDG_CACHE_HOME/murmur/<uuid>/` |
| `export/prereq_check.rs` | 100 | MCP command-in-`PATH` check at startup; actionable error output |
| `import.rs` | 180 | `.murpkg` validate + unpack + UUID regeneration + prereq scan |

### 14.2 `mur-core` additions

| Module | LOC | Responsibility |
|---|---|---|
| `cmd/agent.rs` | 300 | `mur agent create/list/status/remove/rename/send/install-service/export/import` — thin shell |
| `cmd/agent_prompt.rs` | 80 | `mur agent prompt {show,edit,set}` |
| `cmd/agent_mcp.rs` | 150 | `mur agent mcp {list,add,remove,rename}`; auto-updates `entitlements.processes.spawn.allowed` |
| `cmd/agent_skill.rs` | 100 | `mur agent skill {list,add,remove,show}`; file copy + cleanup |
| `cmd/agent_perm.rs` | 220 | `mur agent perm {show,set-mode,allow-host,deny-host,allow-read,allow-write,deny-path,allow-spawn,deny-spawn,set-limit}` |
| `yaml_edit.rs` | 120 | Comment-preserving YAML roundtrip helpers; atomic write + `.bak` backup |
| `running_warn.rs` | 40 | Detect running agent + print restart-required warning on profile mutation |
| (tests) | 200 | `mur agent` command unit tests |

### 14.3 `mur-common` additions

| Module | LOC | Responsibility |
|---|---|---|
| `agent.rs` | 150 | `AgentProfile`, `Persona`, `Entitlements`, `AgentCard` structs |
| `a2a.rs` | 100 | A2A message, task, request/response envelope types |
| `telemetry.rs` | 80 | OTel field constants, notification builder |

### 14.4 Totals

**P0a production code:** ~3830 LOC (~2680 runtime + ~750 export/import + ~400 management CLI). **P0a tests:** ~1100 LOC (unit + integration + E2E). **P0b:** ~3150 LOC + ~1000 LOC tests.

**Timeline:** P0a ~2 weeks (includes export/import + management CLI), P0b ~1.5-2 weeks, total P0 ~3.5-4 weeks focused work.

## 15. References

Key 2025-2026 literature and specifications that informed this design:

- Multi-Agent LLM Orchestration (arXiv:2511.15755) — multi-agent achieves 100% vs 1.7% actionable recommendation rate in incident response.
- The Orchestration of Multi-Agent Systems (arXiv:2601.13671) — canonical 2026 survey; topologies and framework mapping.
- AgentOrchestra (arXiv:2506.12508) — hierarchical planner/specialist pattern.
- AgentSight eBPF Observability (arXiv:2508.02736) — intent-to-syscall correlation.
- MAESTRO Evaluation Suite (arXiv:2601.00481) — "silent information consumption" telemetry gap.
- Benchmarking Financial MAS (arXiv:2603.22651) — supervisor-worker wins cost/accuracy.
- A2A Protocol v0.3 — [`https://a2a-protocol.org/latest/specification/`](https://a2a-protocol.org/latest/specification/) — JSON-RPC 2.0 Agent-to-Agent standard (Linux Foundation).
- MCP Specification 2025-11-25 — [`https://modelcontextprotocol.io/specification/2025-11-25`](https://modelcontextprotocol.io/specification/2025-11-25) — Model Context Protocol.
- Claude Agent SDK — [`https://platform.claude.com/docs/en/agent-sdk/overview`](https://platform.claude.com/docs/en/agent-sdk/overview) — process-per-subagent patterns.
- LangGraph 1.0 + `langgraph-supervisor` — supervisor-worker reference implementation.
- Cognition: Don't Build Multi-Agents (Jun 2025) — shared-context failure mode analysis.
- OpenTelemetry GenAI semantic conventions — [`https://opentelemetry.io/docs/specs/semconv/gen-ai/`](https://opentelemetry.io/docs/specs/semconv/gen-ai/) — cross-vendor LLM telemetry fields.
- Google Universal Commerce Protocol (UCP) partner preview — noted as future optional capability, not in P0 scope.
- BusyBox multi-call binary pattern — reference for `argv[0]`-based subcommand dispatch.
