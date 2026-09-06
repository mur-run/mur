# Hub agent permissions — P1 read-only view — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-07-hub-agent-permissions-design.md` (§1.x references point there). One PR, three tasks, each commit builds.

## Goal

The Hub's Capabilities → Permissions section shows an agent's complete entitlements — enforcement state first, runtime traffic and MCP servers as two blocks, filesystem / processes / tools / LLM / limits — from the same derivation the CLI's `mur agent perm list-hosts` and `list-paths` render.

## Architecture

`mur-core/src/cmd/agent/perm_view.rs` (new) turns `(&AgentProfile, Option<&LockFile>)` into a serde `PermissionsView`. The two text renderers in `perm.rs` are rewritten to print *from* the view; their existing tests are the no-drift proof. `mur-hub-gui/src-tauri/src/detail.rs` adds the view to `AgentDetail` (reading `running.lock` when the agent home is known). `PermissionsTab.tsx` renders it; a pure `permissionsModel.ts` holds the two decisions worth unit-testing.

## Tech stack

Rust 2024 (`mur-core`, `mur-hub-gui/src-tauri`), serde; React 18 + TypeScript 5.5 + Vite 5, plain CSS on semantic tokens, Vitest 4 without jsdom, the lightweight i18n (`t(key, vars)`).

## Global Constraints

Copied from the spec and `CLAUDE.md`. Every task includes all of them.

1. **Runtime traffic is not MCP-server traffic** (spec Constraint 1). The view carries them as two fields; the UI renders two blocks; an `inherit` server is marked as bounded by ports, not hosts.
2. **A configured grant is not an enforced grant** (spec Constraint 2). `enforcement` is the first field and the first thing rendered.
3. `outbound_picture()` and `paths_picture()` text output does not change; their tests in `perm.rs` are not edited.
4. No editing in this PR: no Tauri command writes a profile, the UI has no inputs.
5. Brand name is uppercase **MUR** in every user-visible string.
6. Single source file ≤ 800 lines. `perm.rs` is 859 today and must end ≤ 800; `detail-panel.css` (880) is not touched — new CSS goes in a new file.
   **Executed deviation (Task 1):** extracting only the derivation left `perm.rs` at 843. The two text renderers and their 8 tests moved into `perm_view.rs` too — pure movement, no text change, tests unedited. Result: `perm.rs` 513, `perm_view.rs` 711. The plan's "both stay in `perm.rs`" no longer holds; `perm.rs` calls `super::perm_view::{outbound_picture, paths_picture}`.
7. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit.
8. Components reference only semantic tokens (`--status-attention`, `--status-running`, `--text-secondary`, …; note `--status-ok` is NOT a token — `wizard.css` uses it with a fallback, which is the anti-pattern); no raw hex in component CSS or TSX.
9. Tests never touch the DOM: pure functions only.
10. Every commit is gated on the real exit code: `set -o pipefail; <cmd> 2>&1 | tail -n 20` — never on grep's.

## Working agreement

- Line numbers cite `main` at `1100c820` (2026-09-07); re-check with `grep -n` before cutting.
- Rust env, exported once per shell before any cargo command on `mur-core` (the crate does not compile without it):
  ```bash
  export MUR_WEB_DIST="$HOME/Projects/mur-web/dist"   # must exist (build.sh builds it)
  export ORT_STRATEGY=download
  export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
  ```
- `mur-core` tests: `RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib cmd::agent::perm` (nextest, not `cargo test`; the substring after `--lib` filters test names).
- Hub Rust tests: `cd mur-hub-gui/ui && npm run build` once (the Tauri crate embeds `ui/dist`), then `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib detail::`.
- Hub UI: commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint` (6 pre-existing warnings in untouched files; 0 errors is the bar).
- Commit after every task with the message given. Branch: `feat/hub-permissions-p1-view` off `main`.

## File structure

| File | Responsibility |
|---|---|
| `mur-core/src/cmd/agent/perm_view.rs` (new) | `PermissionsView` and sub-types; `permissions_view()`; derivation tests |
| `mur-core/src/cmd/agent/perm.rs` (modify) | `outbound_picture` / `paths_picture` render from the view; nothing else changes |
| `mur-core/src/cmd/agent/mod.rs` (modify) | `pub mod perm_view;` |
| `mur-hub-gui/src-tauri/src/detail.rs` (modify) | `AgentDetail.permissions`; `running.lock` read; one projection test |
| `mur-hub-gui/ui/src/types.ts` (modify) | TS mirror of `PermissionsView` |
| `mur-hub-gui/ui/src/components/inspector/tabs/permissionsModel.ts` (+ `.test.ts`) (new) | `enforcementTone`, `mcpScope`, `permCommands` |
| `mur-hub-gui/ui/src/components/inspector/tabs/PermissionsTab.tsx` (rewrite) | the section |
| `mur-hub-gui/ui/src/styles/components/permissions.css` (new), `src/styles/index.css` (modify) | `.perm-*` rules |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` (modify) | `perm.*` keys; `detail.permissionsHint` removed |

---

### Task 1 — `perm_view.rs`: the derivation, and the CLI renders from it

**Interfaces.** Produces (Task 2 and 3 consume by name):

```rust
// mur-core/src/cmd/agent/perm_view.rs — all pub, all #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Enforcement { NotRunning, SealUnknown, Advisory, Enforcing }   // serde snake_case
pub enum GrantStatus { Unverified, Effective, Dropped { reason: String } } // serde tag = "status", snake_case
pub enum McpScope { Unbounded, OwnHosts, AllAudited, Off }               // serde snake_case
pub struct PathGrantView { pub raw: String, pub expanded: String, pub status: GrantStatus }
pub struct PathsView { pub read: Vec<PathGrantView>, pub write: Vec<PathGrantView>, pub deny: Vec<PathGrantView> }
pub struct OutboundView { pub mode: NetworkOutboundMode, pub allow_hosts: Vec<String>, pub model_host_always_allowed: bool }
pub struct McpNetView { pub name: String, pub mode: McpNetMode, pub scope: McpScope, pub allow_hosts: Vec<String>, pub deny_hosts: Vec<String> }
pub struct ProcessesView { pub spawn_mode: SpawnMode, pub allowed: Vec<String>, pub allowed_dirs: Vec<String> }
pub struct ToolRuleView { pub pattern: String, pub policy: ToolPolicy, pub risk: Option<RiskTier> }
pub struct LimitsView { pub cpu_seconds: Option<u64>, pub memory_mb: u64, pub file_descriptors: u32, pub processes: u32 }
pub struct PermissionsView {
    pub enforcement: Enforcement,
    pub sandbox_mode: Option<String>,
    pub grants_drifted: bool,
    pub runtime_outbound: OutboundView,
    pub mcp_servers: Vec<McpNetView>,
    pub filesystem: PathsView,
    pub processes: ProcessesView,
    pub tools: Vec<ToolRuleView>,
    pub llm: LlmMode,
    pub limits: LimitsView,
    pub fail_closed_on_sandbox_error: bool,
}
pub fn permissions_view(profile: &AgentProfile, lock: Option<&LockFile>) -> PermissionsView;
```

The spec's `bounded_by_allow_hosts: bool` is expressed as `scope: McpScope`: that boolean is false for *every* server (no MCP server is ever bounded by the agent's `allow_hosts`), so a column of it says nothing. `Unbounded` is the `inherit` case the UI must call out.

- [x] Create `mur-core/src/cmd/agent/perm_view.rs`:
  ```rust
  //! The one derivation of "what may this agent reach, and is that enforced".
  //!
  //! `mur agent perm list-hosts` / `list-paths` render text from this; the Hub
  //! serialises it into `AgentDetail`. One derivation, two surfaces, so the two
  //! facts that matter cannot drift between them: runtime traffic is not
  //! MCP-server traffic, and a configured grant is not an enforced one.

  use mur_common::LockFile;
  use mur_common::agent::{
      McpNetMode, NetworkOutboundMode, SpawnMode, ToolPolicy, filesystem_grants_digest,
  };
  use mur_common::bridge::llm_entitlement::LlmMode;
  use mur_common::hitl::RiskTier;
  use serde::{Deserialize, Serialize};

  /// What the running agent's sandbox seal says — derived from `running.lock`,
  /// never from the profile, because the profile is a request.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum Enforcement {
      /// No lock: nothing is enforced until the agent starts.
      NotRunning,
      /// A lock with no seal record: an older runtime; what took effect is unknown.
      SealUnknown,
      /// Sealed with `enforcing: false`: only advisory hooks; the agent can reach
      /// MORE than the grants list.
      Advisory,
      Enforcing,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(tag = "status", rename_all = "snake_case")]
  pub enum GrantStatus {
      /// No seal to check against (not running, or seal unknown).
      Unverified,
      Effective,
      /// The sandbox discarded this grant when sealing.
      Dropped { reason: String },
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct PathGrantView {
      pub raw: String,
      pub expanded: String,
      pub status: GrantStatus,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
  pub struct PathsView {
      pub read: Vec<PathGrantView>,
      pub write: Vec<PathGrantView>,
      pub deny: Vec<PathGrantView>,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct OutboundView {
      pub mode: NetworkOutboundMode,
      pub allow_hosts: Vec<String>,
      /// In `restricted` / `proxy_only` the configured model's own host is
      /// reachable whether or not it is listed.
      pub model_host_always_allowed: bool,
  }

  /// What bounds an MCP server's traffic. NEVER the agent's `allow_hosts` —
  /// that list guards the runtime's own HTTP client, and a spawned server does
  /// not run it.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum McpScope {
      /// `inherit`: only the OS sandbox, which restricts ports, not hosts.
      Unbounded,
      /// `restricted`: the server's own `allow_hosts`, via the egress proxy.
      OwnHosts,
      /// `broad_audited`: all hosts (minus `deny_hosts`), audited.
      AllAudited,
      Off,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct McpNetView {
      pub name: String,
      pub mode: McpNetMode,
      pub scope: McpScope,
      pub allow_hosts: Vec<String>,
      pub deny_hosts: Vec<String>,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct ProcessesView {
      pub spawn_mode: SpawnMode,
      pub allowed: Vec<String>,
      pub allowed_dirs: Vec<String>,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct ToolRuleView {
      pub pattern: String,
      pub policy: ToolPolicy,
      pub risk: Option<RiskTier>,
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct LimitsView {
      pub cpu_seconds: Option<u64>,
      pub memory_mb: u64,
      pub file_descriptors: u32,
      pub processes: u32,
  }

  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct PermissionsView {
      pub enforcement: Enforcement,
      /// `SandboxRecord.mode` when a seal exists (`"macos-sbpl"`, `"advisory-only"`, …).
      pub sandbox_mode: Option<String>,
      /// Filesystem grants were edited after the seal: `Effective` rows describe
      /// the profile as it is now, not as it was enforced.
      pub grants_drifted: bool,
      pub runtime_outbound: OutboundView,
      pub mcp_servers: Vec<McpNetView>,
      pub filesystem: PathsView,
      pub processes: ProcessesView,
      pub tools: Vec<ToolRuleView>,
      pub llm: LlmMode,
      pub limits: LimitsView,
      pub fail_closed_on_sandbox_error: bool,
  }

  pub fn permissions_view(
      profile: &mur_common::AgentProfile,
      lock: Option<&LockFile>,
  ) -> PermissionsView {
      let ent = &profile.entitlements;
      let sandbox = lock.and_then(|l| l.sandbox.as_ref());
      let enforcement = match (lock, sandbox) {
          (None, _) => Enforcement::NotRunning,
          (Some(_), None) => Enforcement::SealUnknown,
          (Some(_), Some(sb)) if !sb.enforcing => Enforcement::Advisory,
          (Some(_), Some(_)) => Enforcement::Enforcing,
      };
      let grants_drifted =
          sandbox.is_some_and(|sb| sb.granted_digest != filesystem_grants_digest(&ent.filesystem));

      let grants = |verb: &str, list: &[String]| -> Vec<PathGrantView> {
          list.iter()
              .map(|raw| {
                  let expanded = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw)
                      .display()
                      .to_string();
                  let status = match sandbox {
                      None => GrantStatus::Unverified,
                      Some(sb) => match sb
                          .dropped
                          .iter()
                          .find(|d| d.verb == verb && d.path == expanded)
                      {
                          Some(d) => GrantStatus::Dropped {
                              reason: d.reason.clone(),
                          },
                          None => GrantStatus::Effective,
                      },
                  };
                  PathGrantView {
                      raw: raw.clone(),
                      expanded,
                      status,
                  }
              })
              .collect()
      };

      let out = &ent.network.outbound;
      let model_host_always_allowed = !matches!(
          out.mode,
          NetworkOutboundMode::Off | NetworkOutboundMode::Unrestricted
      );

      let mcp_servers = profile
          .mcp_servers
          .iter()
          .map(|m| {
              let mode = m.network.as_ref().map(|n| n.mode).unwrap_or_default();
              let scope = match mode {
                  McpNetMode::Inherit => McpScope::Unbounded,
                  McpNetMode::Restricted => McpScope::OwnHosts,
                  McpNetMode::BroadAudited => McpScope::AllAudited,
                  McpNetMode::Off => McpScope::Off,
              };
              McpNetView {
                  name: m.name.clone(),
                  mode,
                  scope,
                  allow_hosts: m.network.as_ref().map(|n| n.allow_hosts.clone()).unwrap_or_default(),
                  deny_hosts: m.network.as_ref().map(|n| n.deny_hosts.clone()).unwrap_or_default(),
              }
          })
          .collect();

      PermissionsView {
          enforcement,
          sandbox_mode: sandbox.map(|sb| sb.mode.clone()),
          grants_drifted,
          runtime_outbound: OutboundView {
              mode: out.mode,
              allow_hosts: out.allow_hosts.clone(),
              model_host_always_allowed,
          },
          mcp_servers,
          filesystem: PathsView {
              read: grants("read", &ent.filesystem.read),
              write: grants("write", &ent.filesystem.write),
              deny: grants("deny", &ent.filesystem.deny),
          },
          processes: ProcessesView {
              spawn_mode: ent.processes.spawn.mode,
              allowed: ent.processes.spawn.allowed.clone(),
              allowed_dirs: ent.processes.spawn.allowed_dirs.clone(),
          },
          tools: ent
              .tools
              .iter()
              .map(|r| ToolRuleView {
                  pattern: r.pattern.clone(),
                  policy: r.policy,
                  risk: r.risk,
              })
              .collect(),
          llm: ent.llm.mode,
          limits: LimitsView {
              cpu_seconds: ent.limits.cpu_seconds,
              memory_mb: ent.limits.memory_mb,
              file_descriptors: ent.limits.file_descriptors,
              processes: ent.limits.processes,
          },
          fail_closed_on_sandbox_error: ent.fail_closed_on_sandbox_error,
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use mur_common::agent::{DroppedGrant, McpServerEntry, McpServerNetwork, SandboxRecord};

      fn profile() -> mur_common::AgentProfile {
          let mut p = mur_common::AgentProfile::default_for_tests();
          p.entitlements.filesystem.write = vec!["/tmp/x".into()];
          p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
          p
      }

      fn lock(sandbox: Option<SandboxRecord>) -> LockFile {
          LockFile {
              schema: 1,
              uuid: "u".into(),
              name: "mur".into(),
              pid: 1,
              ppid: 0,
              started_at: "t".into(),
              binary_version: "v".into(),
              transports: mur_common::agent::LockTransports {
                  stdio: true,
                  unix_socket: None,
                  tcp: None,
                  webhook: None,
              },
              card_digest: "d".into(),
              capabilities: vec![],
              build_sha: String::new(),
              proto_version: 0,
              sandbox,
          }
      }

      fn sealed(p: &mur_common::AgentProfile, enforcing: bool) -> SandboxRecord {
          SandboxRecord {
              enforcing,
              mode: if enforcing { "macos-sbpl" } else { "advisory-only" }.into(),
              granted_digest: filesystem_grants_digest(&p.entitlements.filesystem),
              dropped: vec![],
          }
      }

      #[test]
      fn enforcement_covers_all_four_states() {
          let p = profile();
          assert_eq!(permissions_view(&p, None).enforcement, Enforcement::NotRunning);
          assert_eq!(
              permissions_view(&p, Some(&lock(None))).enforcement,
              Enforcement::SealUnknown
          );
          assert_eq!(
              permissions_view(&p, Some(&lock(Some(sealed(&p, false))))).enforcement,
              Enforcement::Advisory
          );
          assert_eq!(
              permissions_view(&p, Some(&lock(Some(sealed(&p, true))))).enforcement,
              Enforcement::Enforcing
          );
      }

      /// Not running means no grant is verified — the view must not tick rows.
      #[test]
      fn a_stopped_agent_has_only_unverified_grants() {
          let v = permissions_view(&profile(), None);
          assert_eq!(v.filesystem.write.len(), 1);
          assert_eq!(v.filesystem.write[0].raw, "/tmp/x");
          assert_eq!(v.filesystem.write[0].status, GrantStatus::Unverified);
          assert!(!v.grants_drifted);
      }

      #[test]
      fn a_dropped_grant_carries_its_reason_and_drift_is_detected() {
          let mut p = profile();
          p.entitlements.filesystem.write.push("/tmp/gone".into());
          let mut rec = sealed(&p, true);
          rec.dropped.push(DroppedGrant {
              path: "/tmp/gone".into(),
              verb: "write".into(),
              reason: "path does not exist on disk".into(),
          });
          let l = lock(Some(rec));
          let v = permissions_view(&p, Some(&l));
          assert_eq!(v.filesystem.write[0].status, GrantStatus::Effective);
          assert_eq!(
              v.filesystem.write[1].status,
              GrantStatus::Dropped {
                  reason: "path does not exist on disk".into()
              }
          );
          assert!(!v.grants_drifted);

          p.entitlements.filesystem.write.push("/tmp/added-later".into());
          assert!(permissions_view(&p, Some(&l)).grants_drifted);
      }

      /// The load-bearing case from the spec: an `inherit` server is Unbounded
      /// even though the agent's own allow_hosts is non-empty.
      #[test]
      fn inherit_server_is_unbounded_while_agent_allow_hosts_is_set() {
          let mut p = profile();
          p.mcp_servers.push(McpServerEntry {
              name: "media".into(),
              command: "npx".into(),
              network: None,
              ..Default::default()
          });
          p.mcp_servers.push(McpServerEntry {
              name: "db".into(),
              command: "npx".into(),
              network: Some(McpServerNetwork {
                  mode: McpNetMode::Restricted,
                  allow_hosts: vec!["db.internal".into()],
                  deny_hosts: vec![],
                  authorization: None,
              }),
              ..Default::default()
          });
          let v = permissions_view(&p, None);
          assert_eq!(v.runtime_outbound.allow_hosts, vec!["api.example.com"]);
          assert!(v.runtime_outbound.model_host_always_allowed);
          assert_eq!(v.mcp_servers[0].scope, McpScope::Unbounded);
          assert_eq!(v.mcp_servers[1].scope, McpScope::OwnHosts);
          assert_eq!(v.mcp_servers[1].allow_hosts, vec!["db.internal"]);
      }

      /// The Hub deserialises what mur-core serialises; a private field or a
      /// non-string enum tag would break at runtime, not compile time.
      #[test]
      fn round_trips_through_json() {
          let p = profile();
          let v = permissions_view(&p, Some(&lock(Some(sealed(&p, true)))));
          let s = serde_json::to_string(&v).unwrap();
          let back: PermissionsView = serde_json::from_str(&s).unwrap();
          assert_eq!(back, v);
          assert!(s.contains("\"enforcement\":\"enforcing\""), "{s}");
          assert!(s.contains("\"status\":\"effective\""), "{s}");
      }
  }
  ```
- [x] `mur-core/src/cmd/agent/mod.rs`: after line 51 (`mod perm;`) add `pub mod perm_view;`.
- [x] Run: `set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib cmd::agent::perm_view 2>&1 | tail -n 20` → expect `5 tests run: 5 passed`.
- [x] `mur-core/src/cmd/agent/perm.rs`: replace the body of `outbound_picture` (lines 47–128) so it renders from the view. Delete the `use mur_common::agent::{McpNetMode, NetworkOutboundMode};` line inside it and write:
  ```rust
  /// Testable core of [`print_outbound_picture`].
  fn outbound_picture(profile: &mur_common::AgentProfile) -> String {
      use super::perm_view::{McpScope, permissions_view};
      use mur_common::agent::NetworkOutboundMode;
      use std::fmt::Write as _;

      let v = permissions_view(profile, None);
      let out = &v.runtime_outbound;
      let mut o = String::new();

      let _ = writeln!(o, "runtime's own traffic — {:?}", out.mode);
      let _ = writeln!(
          o,
          "  (in-process DNS guard + the B0 gate on `network.*` tools)"
      );
      match out.mode {
          NetworkOutboundMode::Off => {
              let _ = writeln!(o, "  no outbound");
          }
          NetworkOutboundMode::Unrestricted => {
              let _ = writeln!(o, "  any host, any port");
          }
          _ => {
              if out.allow_hosts.is_empty() {
                  let _ = writeln!(
                      o,
                      "  allow_hosts: (none — only the configured model's host)"
                  );
              } else {
                  let _ = writeln!(o, "  allow_hosts:");
                  for h in &out.allow_hosts {
                      let _ = writeln!(o, "    {h}");
                  }
                  let _ = writeln!(o, "  plus the configured model's own host, always allowed");
              }
          }
      }

      if v.mcp_servers.is_empty() {
          return o;
      }
      let _ = writeln!(o);
      let _ = writeln!(o, "MCP servers — {}", v.mcp_servers.len());
      let mut any_inherit = false;
      for m in &v.mcp_servers {
          let detail = match m.scope {
              // The load-bearing line: `inherit` does NOT pick up allow_hosts.
              McpScope::Unbounded => {
                  any_inherit = true;
                  "NOT bounded by allow_hosts above — only by the OS sandbox, which \
                   restricts ports, not hosts"
                      .to_string()
              }
              McpScope::OwnHosts => {
                  if m.allow_hosts.is_empty() {
                      "via the egress proxy; no hosts allowed (denies all)".to_string()
                  } else {
                      format!("via the egress proxy; allows {}", m.allow_hosts.join(", "))
                  }
              }
              McpScope::AllAudited => {
                  if m.deny_hosts.is_empty() {
                      "via the egress proxy; ALL hosts, audited".to_string()
                  } else {
                      format!(
                          "via the egress proxy; all hosts except {}, audited",
                          m.deny_hosts.join(", ")
                      )
                  }
              }
              McpScope::Off => "no outbound".to_string(),
          };
          let label = format!("{:?}", m.mode).to_lowercase();
          let _ = writeln!(o, "  {:<20} {:<11} {detail}", m.name, label);
      }
      if any_inherit {
          let _ = writeln!(o);
          let _ = writeln!(
              o,
              "  Bound a server by host: mur agent mcp set-network <agent> <server> --allow-host <host>"
          );
      }
      o
  }
  ```
- [x] `perm.rs`: replace the body of `paths_picture` (lines 308–390) so it renders from the view:
  ```rust
  /// Testable core of [`cmd_perm_list_paths`].
  fn paths_picture(
      name: &str,
      profile: &mur_common::AgentProfile,
      lock: Option<&LockFile>,
  ) -> String {
      use super::perm_view::{Enforcement, GrantStatus, permissions_view};
      use std::fmt::Write as _;

      let v = permissions_view(profile, lock);
      let mut o = String::new();
      let mode = v.sandbox_mode.as_deref().unwrap_or_default();

      // The header can subsume everything below it, so it goes first.
      match v.enforcement {
          Enforcement::NotRunning => {
              let _ = writeln!(
                  o,
                  "agent '{name}' is not running — these are the grants it would ask for; \
                   nothing is enforced until it starts."
              );
          }
          Enforcement::SealUnknown => {
              let _ = writeln!(
                  o,
                  "agent '{name}' was started by a runtime that did not record its seal, \
                   so what actually took effect is unknown. Restart it to find out."
              );
          }
          Enforcement::Advisory => {
              let _ = writeln!(
                  o,
                  "agent '{name}' is running WITHOUT a kernel sandbox ({mode}). Only advisory \
                   hooks apply, so it can reach MORE than the grants below — restart it to \
                   try sealing again."
              );
          }
          Enforcement::Enforcing => {
              let _ = writeln!(o, "agent '{name}' — sandbox enforcing ({mode})");
          }
      }

      let fs = &v.filesystem;
      for (label, list) in [("read", &fs.read), ("write", &fs.write), ("deny", &fs.deny)] {
          if list.is_empty() {
              continue;
          }
          let _ = writeln!(o, "\n{}", label.to_uppercase());
          for g in list {
              match &g.status {
                  GrantStatus::Dropped { reason } => {
                      let _ = writeln!(o, "  ✗ {}\n      dropped — {reason}", g.raw);
                  }
                  GrantStatus::Unverified => {
                      let _ = writeln!(o, "  · {}", g.raw);
                  }
                  GrantStatus::Effective => {
                      let _ = writeln!(o, "  ✓ {}", g.raw);
                  }
              }
          }
      }

      if v.grants_drifted {
          let _ = writeln!(
              o,
              "\nGrants have changed since this agent sealed, so the ✓ rows describe the \
               profile as it is now, not as it was enforced:\n    mur agent restart {name}"
          );
      }
      o
  }
  ```
  Then run `cargo check -p mur-core 2>&1 | tail -n 5` and delete any import the compiler now reports unused at the top of `perm.rs` (`LockFile` stays: `cmd_perm_list_paths` still parses it).
- [x] Run: `set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib cmd::agent::perm 2>&1 | tail -n 25` → expect every `perm::tests::*` and `perm_view::tests::*` test passing (`15 tests run: 15 passed` — 10 existing + 5 new; the plan said 9, the measured baseline was 10). The 9 existing tests are not edited (Global Constraint 3).
- [x] `wc -l mur-core/src/cmd/agent/perm.rs` → must print ≤ 800. If it does not, the two function bodies above were not replaced but appended — re-check.
- [x] `cargo clippy -p mur-core --all-targets -- -D warnings 2>&1 | tail -n 5` → exit 0; `cargo fmt -p mur-core`.
- [x] Commit: `feat(core): perm_view — one derivation for agent permissions, CLI renders from it`

---

### Task 2 — Hub DTO: `AgentDetail.permissions`

**Interfaces.** Consumes `mur_core::cmd::agent::perm_view::{PermissionsView, permissions_view, Enforcement, GrantStatus}` (Task 1). Produces `AgentDetail.permissions: PermissionsView` on the wire — the JSON shape Task 3's `types.ts` mirrors.

- [ ] `mur-hub-gui/src-tauri/src/detail.rs`: in the `AgentDetail` struct, after `pub capabilities: Vec<String>,` (line 29) add:
  ```rust
      /// Spec 2026-09-07 §1.2: the same derivation `mur agent perm list-paths`
      /// prints. Read-only in P1.
      pub permissions: mur_core::cmd::agent::perm_view::PermissionsView,
  ```
- [ ] In `build_agent_detail` (line 261), before `let model_id = …` add:
  ```rust
      // The seal lives in `running.lock`; without an agent home (tests) there
      // is none, which the view reports as NotRunning — nothing enforced.
      let lock: Option<mur_common::LockFile> = agent_home
          .map(|h| h.join("running.lock"))
          .and_then(|p| std::fs::read(p).ok())
          .and_then(|b| serde_json::from_slice(&b).ok());
      let permissions = mur_core::cmd::agent::perm_view::permissions_view(&profile, lock.as_ref());
  ```
  and in the struct literal after `capabilities: profile.capabilities.clone(),` add `permissions,`.
- [ ] In `mod tests` (after `a_model_without_reasoning_control_offers_no_levels`, line ~512) add:
  ```rust
      /// Spec 2026-09-07 §1.4: entitlements reach the DTO through the shared
      /// derivation, and with no agent home the state is NotRunning.
      #[test]
      fn entitlements_project_into_the_detail() {
          use mur_core::cmd::agent::perm_view::{Enforcement, GrantStatus};
          let mut p = mur_common::AgentProfile::default_for_tests();
          p.entitlements.filesystem.write = vec!["/tmp/x".into()];
          p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
          p.entitlements.tools.push(mur_common::agent::ToolRule {
              pattern: "bash".into(),
              policy: mur_common::agent::ToolPolicy::Allow,
              risk: None,
          });

          let d = build_agent_detail(p, None);
          assert_eq!(d.permissions.enforcement, Enforcement::NotRunning);
          assert_eq!(d.permissions.filesystem.write[0].raw, "/tmp/x");
          assert_eq!(d.permissions.filesystem.write[0].status, GrantStatus::Unverified);
          assert_eq!(d.permissions.runtime_outbound.allow_hosts, vec!["api.example.com"]);
          assert_eq!(d.permissions.tools.len(), 1);
          assert_eq!(d.permissions.tools[0].pattern, "bash");
      }
  ```
- [ ] `cd mur-hub-gui/ui && npm run build` (once), then: `set -o pipefail; cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib detail:: 2>&1 | tail -n 15` → expect `test result: ok` with the new test listed.
- [ ] `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings 2>&1 | tail -n 5` → exit 0; `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
- [ ] Commit: `feat(hub): AgentDetail carries the permissions view`

---

### Task 3 — UI: the Permissions section

**Interfaces.** Consumes the JSON shape of `PermissionsView` (Task 2). Produces nothing later tasks consume.

- [ ] `mur-hub-gui/ui/src/types.ts`: before `export interface AgentDetail` (line 104) add:
  ```ts
  /** Mirror of mur-core `perm_view::PermissionsView` (spec 2026-09-07 §1.1). */
  export type Enforcement = "not_running" | "seal_unknown" | "advisory" | "enforcing";
  export type GrantStatus =
    | { status: "unverified" }
    | { status: "effective" }
    | { status: "dropped"; reason: string };
  export type McpScope = "unbounded" | "own_hosts" | "all_audited" | "off";
  export type ToolPolicy = "allow" | "ask" | "deny";
  export interface PathGrantView { raw: string; expanded: string; status: GrantStatus }
  export interface PathsView { read: PathGrantView[]; write: PathGrantView[]; deny: PathGrantView[] }
  export interface OutboundView {
    mode: "unrestricted" | "restricted" | "proxyonly" | "off";
    allow_hosts: string[];
    model_host_always_allowed: boolean;
  }
  export interface McpNetView {
    name: string;
    mode: "inherit" | "restricted" | "broad_audited" | "off";
    scope: McpScope;
    allow_hosts: string[];
    deny_hosts: string[];
  }
  export interface ProcessesView {
    spawn_mode: "allowlist" | "any" | "none" | "strict";
    allowed: string[];
    allowed_dirs: string[];
  }
  export interface ToolRuleView { pattern: string; policy: ToolPolicy; risk: string | null }
  export interface LimitsView {
    cpu_seconds: number | null;
    memory_mb: number;
    file_descriptors: number;
    processes: number;
  }
  export interface PermissionsView {
    enforcement: Enforcement;
    sandbox_mode: string | null;
    grants_drifted: boolean;
    runtime_outbound: OutboundView;
    mcp_servers: McpNetView[];
    filesystem: PathsView;
    processes: ProcessesView;
    tools: ToolRuleView[];
    llm: "allowed" | "off";
    limits: LimitsView;
    fail_closed_on_sandbox_error: boolean;
  }
  ```
  and in `AgentDetail` after `capabilities: string[];` add `permissions: PermissionsView;`.
  (`NetworkOutboundMode` serialises `lowercase`, so `ProxyOnly` is `"proxyonly"`; `McpNetMode` is `snake_case`, so `"broad_audited"`.)
- [ ] Create `src/components/inspector/tabs/permissionsModel.test.ts`:
  ```ts
  import { describe, expect, it } from "vitest";
  import { enforcementTone, permCommands } from "./permissionsModel";

  describe("enforcementTone", () => {
    it("only a sealed, enforcing sandbox is ok; advisory is attention; the rest muted", () => {
      expect(enforcementTone("enforcing")).toBe("ok");
      expect(enforcementTone("advisory")).toBe("attention");
      expect(enforcementTone("not_running")).toBe("muted");
      expect(enforcementTone("seal_unknown")).toBe("muted");
    });
  });

  describe("permCommands", () => {
    it("names the agent in every command and covers each block", () => {
      const c = permCommands("aura");
      expect(c.hosts).toBe("mur agent perm allow-host aura <host>");
      expect(c.paths).toBe("mur agent perm allow-write aura <path>");
      expect(c.spawn).toBe("mur agent perm allow-spawn aura <program>");
      expect(c.tools).toBe("mur agent perm set-tool aura <tool> allow|ask|deny");
      expect(c.mcp).toBe("mur agent mcp set-network aura <server> --allow-host <host>");
    });
  });
  ```
  `npm test -- src/components/inspector/tabs/permissionsModel.test.ts` → fails (module missing).
- [ ] Create `src/components/inspector/tabs/permissionsModel.ts`:
  ```ts
  import type { Enforcement } from "../../../types";

  export type Tone = "ok" | "attention" | "muted";

  /** Spec 2026-09-07 §1.3.1: the banner's tone. Advisory is the loud one —
   *  the agent can reach MORE than the list, the opposite of every other state. */
  export function enforcementTone(e: Enforcement): Tone {
    if (e === "enforcing") return "ok";
    if (e === "advisory") return "attention";
    return "muted";
  }

  /** Spec §1.3.4: each block carries the CLI command that changes it. P1 has
   *  no editing, so a user who can see a grant is told how to change it. */
  export function permCommands(agent: string) {
    return {
      hosts: `mur agent perm allow-host ${agent} <host>`,
      paths: `mur agent perm allow-write ${agent} <path>`,
      spawn: `mur agent perm allow-spawn ${agent} <program>`,
      tools: `mur agent perm set-tool ${agent} <tool> allow|ask|deny`,
      mcp: `mur agent mcp set-network ${agent} <server> --allow-host <host>`,
    };
  }
  ```
  `npm test -- src/components/inspector/tabs/permissionsModel.test.ts` → 2 passed.
- [ ] `src/i18n/en.ts`: delete the `"detail.permissionsHint": …` line (289) and, after `"detail.noCaps": …` (177), add:
  ```ts
    // ── Permissions (spec 2026-09-07 §1.3) ──
    "perm.enforcement.not_running": "Not running — these are the grants it would ask for; nothing is enforced until it starts.",
    "perm.enforcement.seal_unknown": "Started by a runtime that did not record its seal, so what actually took effect is unknown. Restart it to find out.",
    "perm.enforcement.advisory": "Running WITHOUT a kernel sandbox ({mode}). Only advisory hooks apply, so it can reach MORE than the grants below — restart it to try sealing again.",
    "perm.enforcement.enforcing": "Sandbox enforcing ({mode}).",
    "perm.drifted": "Grants have changed since this agent sealed: ✓ rows describe the profile as it is now, not as it was enforced. Restart to apply.",
    "perm.runtime": "Runtime's own traffic",
    "perm.runtime.note": "In-process DNS guard + the B0 gate on network.* tools",
    "perm.outbound.off": "No outbound",
    "perm.outbound.unrestricted": "Any host, any port",
    "perm.outbound.onlyModel": "No allow-hosts — only the configured model's host",
    "perm.outbound.plusModel": "Plus the configured model's own host, always allowed",
    "perm.mcp": "MCP servers",
    "perm.mcp.none": "No MCP servers — only one traffic policy applies.",
    "perm.mcp.unbounded": "NOT bounded by the allow-hosts above — only by the OS sandbox, which restricts ports, not hosts",
    "perm.mcp.own_hosts": "Via the egress proxy; allows {hosts}",
    "perm.mcp.own_hosts_none": "Via the egress proxy; no hosts allowed (denies all)",
    "perm.mcp.all_audited": "Via the egress proxy; ALL hosts, audited",
    "perm.mcp.all_audited_except": "Via the egress proxy; all hosts except {hosts}, audited",
    "perm.mcp.off": "No outbound",
    "perm.filesystem": "Filesystem",
    "perm.fs.read": "Read",
    "perm.fs.write": "Write",
    "perm.fs.deny": "Deny",
    "perm.fs.none": "No filesystem grants.",
    "perm.fs.dropped": "dropped — {reason}",
    "perm.processes": "Processes",
    "perm.spawn.mode": "Spawn: {mode}",
    "perm.spawn.dirs": "Build lanes",
    "perm.tools": "Tool rules",
    "perm.tools.none": "No tool rules — every tool asks (the default).",
    "perm.llm": "LLM calls: {mode}",
    "perm.limits": "Limits",
    "perm.limits.value": "memory {memory} MB · {fds} fds · {procs} processes",
    "perm.limits.cpu": " · cpu {cpu} s",
    "perm.failClosed.on": "Refuses to start if the sandbox cannot be applied.",
    "perm.failClosed.off": "Runs unconfined if the sandbox cannot be applied.",
    "perm.cmdHint": "Change from the CLI:",
  ```
- [ ] `src/i18n/zh-TW.ts`: delete the `"detail.permissionsHint": …` line (291) and, after `"detail.noCaps": …` (179), add:
  ```ts
    // ── Permissions (spec 2026-09-07 §1.3) ──
    "perm.enforcement.not_running": "未執行中 — 以下是它啟動時會索取的授權；在它啟動前沒有任何一項被強制。",
    "perm.enforcement.seal_unknown": "啟動它的 runtime 沒有記錄封印，實際生效的內容未知。重新啟動即可得知。",
    "perm.enforcement.advisory": "執行中但沒有核心沙盒（{mode}）。只有 advisory hook 生效，所以它能碰到的比下列授權更多 — 重新啟動以重試封印。",
    "perm.enforcement.enforcing": "沙盒強制中（{mode}）。",
    "perm.drifted": "授權在封印後有變動：✓ 列描述的是目前的 profile，不是實際強制的內容。重新啟動以套用。",
    "perm.runtime": "Runtime 自己的流量",
    "perm.runtime.note": "行程內 DNS 守衛＋network.* 工具的 B0 閘門",
    "perm.outbound.off": "無對外連線",
    "perm.outbound.unrestricted": "任何主機、任何埠",
    "perm.outbound.onlyModel": "沒有 allow-hosts — 只有設定的模型主機",
    "perm.outbound.plusModel": "另加設定的模型主機，永遠放行",
    "perm.mcp": "MCP 伺服器",
    "perm.mcp.none": "沒有 MCP 伺服器 — 只有一套流量政策。",
    "perm.mcp.unbounded": "不受上方 allow-hosts 限制 — 只受 OS 沙盒限制，而沙盒限制的是埠，不是主機",
    "perm.mcp.own_hosts": "經 egress proxy；允許 {hosts}",
    "perm.mcp.own_hosts_none": "經 egress proxy；未允許任何主機（全部拒絕）",
    "perm.mcp.all_audited": "經 egress proxy；所有主機，有稽核",
    "perm.mcp.all_audited_except": "經 egress proxy；除 {hosts} 外所有主機，有稽核",
    "perm.mcp.off": "無對外連線",
    "perm.filesystem": "檔案系統",
    "perm.fs.read": "讀取",
    "perm.fs.write": "寫入",
    "perm.fs.deny": "拒絕",
    "perm.fs.none": "沒有檔案系統授權。",
    "perm.fs.dropped": "已捨棄 — {reason}",
    "perm.processes": "行程",
    "perm.spawn.mode": "Spawn：{mode}",
    "perm.spawn.dirs": "Build lanes",
    "perm.tools": "工具規則",
    "perm.tools.none": "沒有工具規則 — 每個工具都會詢問（預設）。",
    "perm.llm": "LLM 呼叫：{mode}",
    "perm.limits": "限制",
    "perm.limits.value": "記憶體 {memory} MB · {fds} fds · {procs} 個行程",
    "perm.limits.cpu": " · cpu {cpu} 秒",
    "perm.failClosed.on": "沙盒無法套用時拒絕啟動。",
    "perm.failClosed.off": "沙盒無法套用時不受限制地執行。",
    "perm.cmdHint": "從 CLI 修改：",
  ```
- [ ] Replace `src/components/inspector/tabs/PermissionsTab.tsx` entirely:
  ```tsx
  import type { AgentDetail, McpNetView, PathGrantView, PermissionsView } from "../../../types";
  import { useT } from "../../../i18n";
  import { enforcementTone, permCommands } from "./permissionsModel";

  /** Spec 2026-09-07 §1.3: enforcement first, runtime traffic and MCP servers
   *  as two blocks, then filesystem / processes / tools / LLM / limits. Read-only. */
  export function PermissionsTab({ detail }: { detail: AgentDetail }) {
    const { t } = useT();
    const v = detail.permissions;
    const cmd = permCommands(detail.agent_name);
    return (
      <div className="tab-form perm">
        <EnforcementBanner v={v} />

        <label className="field-label">{t("detail.capabilities")}</label>
        {detail.capabilities.length === 0 ? (
          <p className="field-muted perm__muted">{t("detail.noCaps")}</p>
        ) : (
          <div className="badge-row">
            {detail.capabilities.map((c) => (
              <span key={c} className="cap-tag"><span className="cap-dot" />{c}</span>
            ))}
          </div>
        )}

        <Block title={t("perm.runtime")} cmd={cmd.hosts}>
          <p className="field-muted perm__muted">{t("perm.runtime.note")}</p>
          <p className="perm__mode">{v.runtime_outbound.mode}</p>
          <Outbound v={v} />
        </Block>

        <Block title={t("perm.mcp")} cmd={v.mcp_servers.length ? cmd.mcp : undefined}>
          {v.mcp_servers.length === 0 ? (
            <p className="field-muted perm__muted">{t("perm.mcp.none")}</p>
          ) : (
            <ul className="perm__list">
              {v.mcp_servers.map((m) => <McpRow key={m.name} m={m} />)}
            </ul>
          )}
        </Block>

        <Block title={t("perm.filesystem")} cmd={cmd.paths}>
          {v.filesystem.read.length + v.filesystem.write.length + v.filesystem.deny.length === 0 ? (
            <p className="field-muted perm__muted">{t("perm.fs.none")}</p>
          ) : (
            <>
              <Grants label={t("perm.fs.read")} list={v.filesystem.read} />
              <Grants label={t("perm.fs.write")} list={v.filesystem.write} />
              <Grants label={t("perm.fs.deny")} list={v.filesystem.deny} />
              {v.grants_drifted && <p className="perm__note perm__note--attention">{t("perm.drifted")}</p>}
            </>
          )}
        </Block>

        <Block title={t("perm.processes")} cmd={cmd.spawn}>
          <p className="perm__mode">{t("perm.spawn.mode", { mode: v.processes.spawn_mode })}</p>
          {v.processes.allowed.length > 0 && <Paths list={v.processes.allowed} />}
          {v.processes.allowed_dirs.length > 0 && (
            <>
              <p className="field-muted perm__muted">{t("perm.spawn.dirs")}</p>
              <Paths list={v.processes.allowed_dirs} />
            </>
          )}
        </Block>

        <Block title={t("perm.tools")} cmd={cmd.tools}>
          {v.tools.length === 0 ? (
            <p className="field-muted perm__muted">{t("perm.tools.none")}</p>
          ) : (
            <ul className="perm__list">
              {v.tools.map((r) => (
                <li key={r.pattern} className="perm__row">
                  <code>{r.pattern}</code>
                  <span className={`perm__policy perm__policy--${r.policy}`}>{r.policy}</span>
                  {r.risk && <span className="badge-sm">{r.risk}</span>}
                </li>
              ))}
            </ul>
          )}
        </Block>

        <Block title={t("perm.limits")}>
          <p className="perm__mode">{t("perm.llm", { mode: v.llm })}</p>
          <p className="field-muted perm__muted">
            {t("perm.limits.value", {
              memory: v.limits.memory_mb,
              fds: v.limits.file_descriptors,
              procs: v.limits.processes,
            })}
            {v.limits.cpu_seconds != null && t("perm.limits.cpu", { cpu: v.limits.cpu_seconds })}
          </p>
          <p className="field-muted perm__muted">
            {v.fail_closed_on_sandbox_error ? t("perm.failClosed.on") : t("perm.failClosed.off")}
          </p>
        </Block>
      </div>
    );
  }

  function EnforcementBanner({ v }: { v: PermissionsView }) {
    const { t } = useT();
    const tone = enforcementTone(v.enforcement);
    return (
      <p className={`perm__banner perm__banner--${tone}`} role="status">
        {t(`perm.enforcement.${v.enforcement}`, { mode: v.sandbox_mode ?? "" })}
      </p>
    );
  }

  function Block({ title, cmd, children }: { title: string; cmd?: string; children: React.ReactNode }) {
    const { t } = useT();
    return (
      <section className="perm__block">
        <label className="field-label">{title}</label>
        {children}
        {cmd && (
          <p className="perm__cmd">
            <span className="field-muted">{t("perm.cmdHint")}</span> <code>{cmd}</code>
          </p>
        )}
      </section>
    );
  }

  function Outbound({ v }: { v: PermissionsView }) {
    const { t } = useT();
    const o = v.runtime_outbound;
    if (o.mode === "off") return <p className="field-muted perm__muted">{t("perm.outbound.off")}</p>;
    if (o.mode === "unrestricted") return <p className="field-muted perm__muted">{t("perm.outbound.unrestricted")}</p>;
    if (o.allow_hosts.length === 0) return <p className="field-muted perm__muted">{t("perm.outbound.onlyModel")}</p>;
    return (
      <>
        <Paths list={o.allow_hosts} />
        {o.model_host_always_allowed && <p className="field-muted perm__muted">{t("perm.outbound.plusModel")}</p>}
      </>
    );
  }

  function McpRow({ m }: { m: McpNetView }) {
    const { t } = useT();
    const detail =
      m.scope === "unbounded" ? t("perm.mcp.unbounded")
      : m.scope === "own_hosts" ? (m.allow_hosts.length ? t("perm.mcp.own_hosts", { hosts: m.allow_hosts.join(", ") }) : t("perm.mcp.own_hosts_none"))
      : m.scope === "all_audited" ? (m.deny_hosts.length ? t("perm.mcp.all_audited_except", { hosts: m.deny_hosts.join(", ") }) : t("perm.mcp.all_audited"))
      : t("perm.mcp.off");
    return (
      <li className={`perm__row${m.scope === "unbounded" ? " perm__row--attention" : ""}`}>
        <code>{m.name}</code>
        <span className="badge-sm">{m.mode}</span>
        <span className="perm__detail">{detail}</span>
      </li>
    );
  }

  function Grants({ label, list }: { label: string; list: PathGrantView[] }) {
    const { t } = useT();
    if (list.length === 0) return null;
    return (
      <>
        <p className="field-muted perm__muted">{label}</p>
        <ul className="perm__list">
          {list.map((g) => (
            <li key={g.raw} className={`perm__row perm__row--${g.status.status}`} title={g.expanded}>
              <span className="perm__glyph" aria-hidden>
                {g.status.status === "effective" ? "✓" : g.status.status === "dropped" ? "✗" : "·"}
              </span>
              <code>{g.raw}</code>
              {g.status.status === "dropped" && (
                <span className="perm__detail">{t("perm.fs.dropped", { reason: g.status.reason })}</span>
              )}
            </li>
          ))}
        </ul>
      </>
    );
  }

  function Paths({ list }: { list: string[] }) {
    return (
      <ul className="perm__list">
        {list.map((p) => <li key={p} className="perm__row"><code>{p}</code></li>)}
      </ul>
    );
  }
  ```
  `import type React from "react";` is needed at the top for `React.ReactNode` — add it as the first line.
- [ ] Create `src/styles/components/permissions.css`:
  ```css
  /* Permissions section (spec 2026-09-07 §1.3). Read-only. */
  .perm__banner { margin: 0 0 var(--space-5); padding: var(--space-3) var(--space-4); border-radius: var(--radius-md); font-size: var(--text-sm); border: 1px solid var(--border-line); }
  .perm__banner--ok { color: var(--text-primary); }
  .perm__banner--muted { color: var(--text-secondary); }
  .perm__banner--attention { color: var(--text-on-attention); background: var(--status-attention); border-color: transparent; }
  .perm__block { margin-top: var(--space-5); }
  .perm__muted { font-size: var(--text-xs); margin: 2px 0; }
  .perm__mode { margin: 2px 0; font-size: var(--text-sm); font-family: var(--font-mono); }
  .perm__list { list-style: none; margin: 4px 0 0; padding: 0; }
  .perm__row { display: flex; align-items: baseline; gap: var(--space-2); padding: 2px 0; font-size: var(--text-sm); }
  .perm__row code { font-family: var(--font-mono); font-size: var(--text-xs); }
  .perm__row--attention .perm__detail { color: var(--status-attention); }
  .perm__row--dropped code { text-decoration: line-through; color: var(--text-secondary); }
  .perm__glyph { width: 1em; text-align: center; color: var(--text-secondary); }
  .perm__row--effective .perm__glyph { color: var(--status-running); }
  .perm__row--dropped .perm__glyph { color: var(--status-stopped); }
  .perm__detail { color: var(--text-secondary); font-size: var(--text-xs); }
  .perm__policy { font-size: var(--text-xs); text-transform: uppercase; letter-spacing: .04em; }
  .perm__policy--allow { color: var(--status-running); }
  .perm__policy--deny { color: var(--status-stopped); }
  .perm__policy--ask { color: var(--text-secondary); }
  .perm__note { margin: var(--space-3) 0 0; font-size: var(--text-xs); }
  .perm__note--attention { color: var(--status-attention); }
  .perm__cmd { margin: var(--space-2) 0 0; font-size: var(--text-xs); user-select: text; }
  .perm__cmd code { font-family: var(--font-mono); user-select: all; }
  ```
  If `--font-mono`, `--text-xs`, `--radius-md`, or `--space-N` is not defined in `src/styles/tokens/primitives.css` (check with `grep -c "<name>" …`), use the name the file does define for the same role — the CSS may not introduce a raw value.
- [ ] `src/styles/index.css`: after line 8 (`@import "./components/detail-panel.css";`) add `@import "./components/permissions.css";`.
- [ ] Run, from `mur-hub-gui/ui/`: `set -o pipefail; npm test 2>&1 | tail -n 8` → all files passing; `npm run build 2>&1 | tail -n 3` → `✓ built`; `npm run lint 2>&1 | tail -n 3` → 0 errors.
- [ ] `grep -rn "permissionsHint" src/` → no matches (the removed key has no remaining user).
- [ ] Manual acceptance (real Hub, `gotcha_hub_local_app_build_recipe`): open an agent that is **stopped** → Capabilities → Permissions shows the muted "Not running" banner first and `·` glyphs, no ✓. Start it → banner reads "Sandbox enforcing (macos-sbpl)", ✓ rows. Agent with an `inherit` MCP server → its row is marked and reads "NOT bounded by the allow-hosts above". Each block ends with a selectable `mur agent perm …` line.
- [ ] Commit: `feat(hub): Permissions section shows the full entitlements view, read-only`

---

## Self-review

- **Spec coverage.** §1.1 → Task 1 (`perm_view.rs`, both pictures rewritten, `perm.rs` ≤ 800). §1.2 → Task 2. §1.3.1 banner-first → `EnforcementBanner` is the first child. §1.3.2 two blocks + `inherit` marked → `Block(perm.runtime)` / `Block(perm.mcp)`, `perm__row--attention`. §1.3.3 vocabulary → `badge-row`, `cap-tag`, `badge-sm`, `field-label`, `field-muted`. §1.3.4 commands → `permCommands` per block. §1.4 → the five `perm_view` tests (four states + inherit + JSON round-trip), existing picture tests untouched, one `detail.rs` test.
- **Placeholder scan.** The only `<host>` / `<path>` strings are inside CLI command templates shown to the user, by design.
- **Cross-task names.** `Enforcement` variants `not_running | seal_unknown | advisory | enforcing` (Rust snake_case ↔ TS union ↔ i18n keys `perm.enforcement.<variant>`); `GrantStatus` tagged `status` ↔ TS discriminant `status` ↔ CSS `perm__row--<status>`; `McpScope` `unbounded | own_hosts | all_audited | off` ↔ i18n `perm.mcp.<scope>`.
- **Deviation from the spec, recorded:** `bounded_by_allow_hosts: bool` became `scope: McpScope` (Task 1 rationale). The spec's `LlmView` / `limits` are the raw `LlmMode` and a flat `LimitsView`; nothing more was needed.
