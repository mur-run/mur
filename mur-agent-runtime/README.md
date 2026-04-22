# mur-agent-runtime

Per-agent A2A v0.3 runtime for **murmur** — the BusyBox-style supervisor
process that backs every `mur agent`. One binary, one symlink per agent
(`mur_agent_<name>` → `mur-agent-runtime`), one running.lock per agent
home.

> Spec: `docs/superpowers/specs/2026-04-22-murmur-p0a-agent-runtime-design.md`
> Implementation plan: `docs/superpowers/plans/2026-04-22-murmur-p0a-agent-runtime-plan.md`
> (+ `-part2.md`)
> Completion log: `docs/superpowers/plans/2026-04-22-murmur-p0a-agent-runtime-plan-COMPLETE.md`

## What it does

- **argv[0] dispatch.** A symlink such as `mur_agent_research_assistant`
  points at the same binary; the runtime reads its basename to pick a
  profile under `~/.mur/agents/<name>/`. Bare invocation (`mur-agent-runtime`)
  requires `--profile <name>` so the binary refuses to be spoofed
  through symlink rename.
- **Profile + entitlements.** Loads `profile.yaml` (an `AgentProfile`
  from `mur-common`), expands `{{agent_home}}`, validates the UUIDv7,
  warns on loose entitlements (unrestricted network, empty deny list,
  spawn=any), and computes a sha256 digest for the running.lock card.
- **Lifecycle.** Acquires `running.lock` (flock + pid + uuid + card
  digest), publishes the active transports, drives stdio + Unix-socket
  JSON-RPC 2.0, and on `SIGTERM/SIGINT` flushes telemetry, drops the
  lock, and exits.
- **A2A v0.3 surface.** `agent/card`, `message/send`, `tasks/get`,
  `tasks/cancel`, `tasks/list` over a shared `Arc<Dispatcher>`; the
  `task/progress` notification stream rides the same transport.
- **Telemetry.** Every event lands in
  `<agent_home>/telemetry/YYYY-MM-DD.jsonl` using OTel GenAI +
  `mur.*` field conventions and is also forwarded to the transport so
  callers can subscribe over the same pipe.
- **MCP client.** Spawn + initialize handshake + `tools/list` /
  `tools/call`, correlated by JSON-RPC id with notifications dropped.
- **LLM provider.** `LlmClient` trait + an Ollama HTTP implementation;
  `TaskRunner::with_llm(...)` plugs it into `run_sync` and emits
  `Event::LlmCall` telemetry with model, token counts, and latency.
- **Self-contained binaries.** With `--features=embedded-agent` and
  `MUR_EXPORT_AGENT_DIR=<path>` the build script tars+gzips an agent
  home into the binary; first run extracts to a content-addressed
  `~/.cache/murmur/<digest12>/` and reuses it on subsequent starts.

## Quick walkthrough

The user-facing surface lives in `mur-core` as `mur agent` subcommands;
this crate is the daemon they spawn. A typical flow:

```bash
# 1. Create an agent profile (writes ~/.mur/agents/research/profile.yaml,
#    sys_prompt.md, and a mur_agent_research symlink in ~/.local/bin).
mur agent create research --no-interactive --display-name "Research" \
    --model llama3.2:3b

# 2. Wire skills + MCPs.
mur agent skill add research path/to/web-search.md
mur agent mcp   add research crawl --command /usr/local/bin/crawl

# 3. Tighten / loosen entitlements as needed.
mur agent perm set-mode  research network.outbound restricted
mur agent perm allow-host research "*.example.com"
mur agent perm allow-spawn research /usr/local/bin/crawl

# 4. Run it. The symlink (mur_agent_research) IS the runtime; argv[0]
#    drives the profile lookup. Add --profile when invoking the bare
#    runtime binary.
mur_agent_research start

# 5. From another shell, talk to it.
mur agent card   research
mur agent send   research \
    '{"role":"user","parts":[{"kind":"text","text":"hi"}]}'
mur agent list   --json
mur agent stats  research
mur agent logs   research --tail 20

# 6. Stop and tidy.
mur agent stop   research
mur agent remove research --purge

# Optional: bundle the agent into a single distributable binary.
mur agent export research --format=bin -o /tmp/my_research_agent
```

`mur agent install-service research` generates a launchd plist (macOS)
or a systemd `--user` unit (Linux) that runs the symlink at boot.

## Layout

```
mur-agent-runtime/
├── build.rs                # embedded-agent feature: tar.gz the agent dir
├── src/
│   ├── communication_policy.rs  sends_to / accepts_from / resolve_caller_name
│   ├── entitlements.rs          warning detection + category presets
│   ├── export/
│   │   ├── pkg.rs               .murpkg tar.gz exporter (sanitises secrets)
│   │   ├── bin_embed.rs         feature-gated EMBEDDED_TAR / has_embedded_agent
│   │   ├── extract.rs           first-run extraction to ~/.cache/murmur/<sha>
│   │   └── prereq_check.rs      check_mcp_prereqs + format_missing_error
│   ├── import.rs                .murpkg → ~/.mur/agents/<name>/ (new UUIDv7)
│   ├── llm/                     LlmClient trait + Ollama provider (+ stubs)
│   ├── lock_file.rs             LockHandle (flock + pid_alive + same-pid)
│   ├── multi_call.rs            argv[0] → profile name + spoof defense
│   ├── profile.rs               YAML loader + {{agent_home}} expansion + digest
│   ├── protocol/
│   │   ├── a2a_server.rs        Dispatcher + MethodHandler + HandlerError
│   │   ├── mcp_client.rs        stdio MCP subprocess client
│   │   └── methods/             agent/card, message/send, tasks/*
│   ├── retry.rs                 fixed/linear/exp backoff per RetryPolicy
│   ├── socket_path.rs           macOS 104-byte sun_path fallback (/tmp + symlink)
│   ├── supervisor.rs            entrypoint(): startup + signal + shutdown
│   ├── task_runner.rs           TaskRunner (stub + Llm backends, sync + async)
│   ├── telemetry_writer.rs      JSONL writer + JSON-RPC notification fan-out
│   └── transport/
│       ├── stdio.rs             newline-delimited JSON-RPC over stdin/stdout
│       └── unix_socket.rs       UnixListener + SO_PEERCRED / LOCAL_PEERCRED
└── tests/                       17 integration tests covering every module
```

## Test it locally

```bash
cargo test -p mur-agent-runtime
# or the workspace E2E smoke runner:
scripts/e2e/run-all.sh
```

## Status

Tasks 0–40 of the P0a plan landed on `feat/murmur-p0a`. Remaining
roadmap items (P0b / P1) are tracked in the spec.
