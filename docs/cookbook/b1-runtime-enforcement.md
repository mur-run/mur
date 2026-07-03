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

## Process-spawn enforcement

Process spawning (the `bash` tool, MCP server children, any `exec`) is
gated at two layers:

1. **Hook coarse gate (B0)** — `B0SafetyHook`'s bash pre-tool-use check
   (`hooks/b0.rs`) does a coarse allow/deny pass before the OS ever sees
   the exec: it blocks obviously-dangerous command strings and, when
   `spawn_mode: allowlist` is set, rejects tool calls that name a binary
   outside `spawn_allowed_paths` up front. This is advisory only — an LLM
   that shells out via an unanticipated code path bypasses it.
2. **OS fine gate (B1)** — the generated sandbox profile enforces the
   real, kernel-level exec restriction:
   - **macOS**: SBPL denies `process-exec` by default and re-allows only
     `spawn_allowed_paths` plus `sandbox::macos::MACOS_SYSTEM_EXEC_PATHS`
     (`/bin`, `/usr/bin`, `/usr/lib`) — the standard binary roots the shell
     interpreter and coreutils need to keep `bash` usable. **This is a
     deliberate exemption, not a gap**: any binary under those three
     system roots execs regardless of the allowlist; the allowlist instead
     bounds the real threat surface — downloaded, Homebrew-installed, and
     project-local binaries outside the system roots.

     A stricter shell-only `Strict` spawn mode is shipped for callers who
     want the system exec roots fenced too: `process-exec` is denied for
     everything under `/bin`, `/usr/bin`, and `/usr/lib` except the resolved
     shell binary the `bash` tool itself spawns — that shell path is seeded
     into `spawn_allowed_paths` automatically by
     `SandboxPolicy::from_entitlements`, so the `bash` tool keeps working
     with no manual configuration — plus whatever the profile lists in
     `spawn_allowed_paths`/`spawn_allowed_prefixes`. No other system binary
     (coreutils, `git`, etc.) execs unless explicitly allowlisted. Enable it
     with:
     ```
     mur agent perm set-mode <agent> processes.spawn strict
     ```
   - **Linux**: seccomp BPF denies dangerous syscalls (`ptrace`,
     `mount`, `kexec_load`, `bpf`, `unshare`) but does **not** currently
     enforce a per-binary exec allowlist — `spawn_allowed_paths` is
     applied at the B0 hook layer only. A hijacked LLM that reaches
     `exec` through a path the hook doesn't intercept is not blocked by
     the kernel on Linux today. Landlock's `LANDLOCK_ACCESS_FS_EXECUTE`
     (ABI v3+) is the intended v3 mechanism; not wired up yet.
   - **Windows**: the Job Object only caps memory and disables child
     breakaway; there is no exec-path allowlist enforcement at the OS
     layer. Same residual gap as Linux — hook-layer only.

See `mur-agent-runtime/tests/b1_spawn_allowlist_enforce.rs` for the
macOS `sandbox-exec` end-to-end proof (gated behind `MUR_TEST_SANDBOX=1`)
that the allowlist really is kernel-enforced for non-system binaries, and
that the system-path exemption really does let coreutils like
`/bin/mkdir` keep running.

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
- **Process-spawn exec allowlist**: kernel-enforced today on macOS only
  (SBPL `process-exec` deny + `spawn_allowed_paths`/system-path exemption
  in `allowlist` mode, or the fully-fenced `strict` mode — see
  "Process-spawn enforcement" above). Linux and Windows enforce
  `spawn_allowed_paths` at the B0 hook layer only — no kernel-level
  per-binary exec allowlist yet.

## See also

- `mur-agent-runtime/src/sandbox/` — implementation
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.3 — spec
- `docs/superpowers/plans/2026-05-07-mur-agent-b1-runtime-enforcement.md` — this plan
