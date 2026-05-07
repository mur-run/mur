# B1 Runtime Enforcement

OS-level sandboxing for mur agents. B1 upgrades B0's advisory hook layer
to kernel enforcement: write attempts and TCP connections outside the agent's
entitlements are blocked by the operating system, not just logged.

## How it works

`sandbox::apply()` fires once at supervisor startup. It translates
`profile.yaml`'s `entitlements:` block into OS-native rules:

| Platform | Mechanism | What it enforces |
|---|---|---|
| Linux 5.13+ | Landlock ABI v1–v4 | FS read/write + TCP port allowlist |
| Linux (all) | seccomp BPF | `ptrace`, `mount`, `kexec_load`, `bpf`, `unshare` denied |
| macOS | SBPL `sandbox_init` | FS write deny + network host allowlist |
| Windows | Job Object | memory cap + child breakaway disabled |
| All platforms | reqwest HostGuard | hostname-level TCP gate before DNS resolution |

B0 hooks still run first (hooks first, kernel second):

1. `B0SafetyHook::pre_tool_use` → deny with LLM-visible reason.
2. Tool executes anyway? Kernel blocks it → EACCES → `ToolError::Sandboxed`.
3. LLM receives: `"Sandboxed: write to /etc/passwd denied (B1 enforcement)"`.

## Checking sandbox status

```bash
# Tail the agent log and look for B1 lines.
mur agent logs my-agent | grep "B1"
# Expected:
# INFO B1 kernel sandbox: ENFORCING platform=linux-landlock-v4
```

If you see `NOT enforcing`, check:
- Linux: kernel ≥ 5.13, `CONFIG_SECURITY_LANDLOCK=y`, `lsm=landlock,...` boot param.
- macOS: process must not be already sandboxed (nested sandboxes not supported).

## Adjusting entitlements

```yaml
# ~/.mur/agents/my-agent/profile.yaml
entitlements:
  network:
    outbound:
      mode: restricted
      allow_hosts:
        - api.anthropic.com
        - api.openai.com
  filesystem:
    read:
      - ~/Documents/project
    write:
      - ~/Documents/project/output
    deny:
      - ~/.ssh
      - ~/.aws
  limits:
    memory_mb: 1024
```

Changes to `profile.yaml` take effect on next agent restart.

## MCP server child sandboxing

MCP server processes spawned by the supervisor inherit a tighter
birdcage sandbox: they can only read their own binary + shared libs,
write to their configured working directory, and use the same network
allowlist as the parent.

## Known limitations (v2)

- **Windows**: AppContainer (full isolation) is v3. v2 only provides
  memory cap + child breakaway prevention.
- **Linux < 5.13**: Landlock not available. seccomp BPF still applies.
- **Network host filtering**: advisory at the reqwest layer; native C
  code in MCP servers (not using reqwest) is gated by the kernel port
  rules only (no hostname filtering at kernel level until netns sidecar).
- **Nested sandboxes**: macOS rejects a second `sandbox_init` call.
  If the agent binary was already sandbox-exec'd, B1 SBPL is skipped.

## See also

- `mur-agent-runtime/src/sandbox/` — implementation
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.3 — spec
- `docs/superpowers/plans/2026-05-07-mur-agent-b1-runtime-enforcement.md` — this plan
