# Hub: Agent Permissions, Read-Only

**Status**: Designed, not started.
**Scope**: sub-project A of two. B (browsing the agent's own on-disk files under
`~/.mur/agents/<name>/`) is deliberately not designed here.

## Problem

An agent's entitlements are invisible in the Hub. `CapabilitiesTab` renders a
`PermissionsTab` that is 41 lines of stub: a row of `capabilities` strings, then
the *counts* of MCP servers and installed skills, then a hint. Nothing from
`profile.entitlements` reaches it — `AgentDetail` has no field for it. So the
network mode, the allow-hosts, the filesystem read/write/deny grants, the spawn
policy, the per-tool allow/ask/deny rules, the LLM entitlement and
`fail_closed_on_sandbox_error` are visible only from `mur agent perm`.

The Hub is where a user goes to understand an agent. Today it answers "what can
this agent reach?" with a count of MCP servers.

## Constraints the design must respect

Two pieces of domain knowledge already live in `mur-core/src/cmd/agent/perm.rs`
and must not be re-derived, because a second derivation is a second thing to
keep true:

1. **Runtime traffic and MCP-server traffic are different subjects with
   different enforcement.** `allow_hosts` is an in-process guard on the
   runtime's own HTTP client plus the B0 gate on `network.*` tools. A spawned
   MCP server runs neither. A server in `inherit` mode is bounded only by the
   OS sandbox, which restricts ports, not hosts. `outbound_picture()` says this
   in prose; its doc comment records that showing only one subject is exactly
   how a user comes to believe `perm allow-host` scopes their MCP servers.
2. **Configured grants are not enforced grants.** `paths_picture()` reads the
   running agent's `LockFile` sandbox seal and leads with the difference: not
   running means nothing is enforced; running with no recorded seal means what
   took effect is unknown; running with `enforcing: false` means the agent can
   reach *more* than the list shows. A UI that renders the configured list as
   a permissions table, with no seal state, asserts a safety property that may
   be false.

## Design

### §1 One derivation — `mur-core/src/cmd/agent/perm_view.rs` (new)

A serde view struct over `(&AgentProfile, Option<&LockFile>)`:

```rust
pub struct PermissionsView {
    pub enforcement: Enforcement,      // NotRunning | SealUnknown | Advisory | Enforcing
    pub runtime_outbound: OutboundView, // mode, allow_hosts, model-host note
    pub mcp_servers: Vec<McpNetView>,   // name, mode, detail, bounded_by_allow_hosts
    pub filesystem: PathsView,          // read, write, deny
    pub processes: ProcessesView,       // spawn mode + allowlist
    pub tools: Vec<ToolRuleView>,       // pattern -> allow/ask/deny
    pub llm: LlmView,
    pub limits: LimitsView,
    pub fail_closed_on_sandbox_error: bool,
}

pub fn permissions_view(profile: &AgentProfile, lock: Option<&LockFile>) -> PermissionsView;
```

`outbound_picture()` and `paths_picture()` are rewritten to render *from* this
view. Their text output does not change; their existing unit tests are the
guard that it did not drift. Both stay in `perm.rs` — only the derivation moves.

Incidental benefit: `perm.rs` is 859 lines today, over the repository's 800-line
rule. Extracting the derivation brings it back under.

### §2 DTO — `AgentDetail.permissions: PermissionsView`

One new field in `mur-hub-gui/src-tauri/src/detail.rs`, populated where the
profile is already loaded, plus the agent's `running.lock` when present. There
is no write path for permissions, so the narrow-DTO-roundtrip hazard (#957 —
`insert(key, T { ..Default })` silently dropping fields the DTO does not model)
does not apply here; if editing is added later, it will.

### §3 UI — replace the stub

Stays in Capabilities -> Permissions. No new tab.

1. **The enforcement banner is first**, above any list. "Not running — these are
   the grants it would ask for; nothing is enforced until it starts" is more
   honest than a well-formatted permissions table.
2. **Runtime traffic and MCP servers are two separate blocks.** Servers in
   `inherit` are marked, visually, as not bounded by the allow-hosts above.
   Constraint 1 becomes layout, not a footnote.
3. **Filesystem, processes and tools** use the existing badge-row and list
   vocabulary from the other detail sections.
4. **Each block carries its `mur agent perm ...` command**, copyable. Editing is
   out of scope, but a user who can see a grant should not have to go find how
   to change it.

### §4 Testing

- `mur-core`: derivation tests for `permissions_view`, covering all four
  enforcement states and an `inherit` MCP server (the case where
  `bounded_by_allow_hosts` is false while `allow_hosts` is non-empty).
- `mur-core`: the existing `outbound_picture` / `paths_picture` tests keep
  passing unchanged — that is the no-drift proof.
- `mur-hub-gui`: one `detail.rs` test that a profile carrying entitlements
  projects them into `AgentDetail`.

### §5 Explicitly not in scope

- **Editing.** Mutation stays in `mur agent perm`. Adding it later means
  handling the running-agent restart semantics (`warn_if_running`) and the
  write-path hazard named in §2.
- **The agent's own files** (`profile.yaml`, prompt, `skills/`, keys, logs).
  Separate sub-project, separate spec.
