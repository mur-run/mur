# Hub agent permissions — P2 item-by-item editing — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-07-hub-agent-permissions-design.md` §P2. Stacked on the P1 branch `feat/hub-permissions-p1-view` (head `c24fec00`). One PR, three tasks, each commit builds.

## Goal

Every row the P1 Permissions section shows can be changed from the Hub: outbound mode and hosts, filesystem read / write / deny folders (picked with the native folder dialog), spawn mode, spawn programs and build-lane directories, tool rules — through the CLI's own write functions, with the CLI's own validation, and with "takes effect on restart" said out loud.

## Architecture

mur-core gains the one write the CLI lacked (removing a filesystem grant) and one parser gap (`proxy_only` outbound mode). A new Tauri module `perm_admin.rs` exposes twelve thin commands that each call one `cmd_perm_*` and return a fresh `AgentDetail`, exactly like `mcp_skills::agent_mcp_add`. The UI adds editors beside each P1 block in a new `PermissionEditors.tsx`; `PermissionsTab.tsx` composes them. No form state survives a tab switch, so no dirty guard.

## Tech stack

Same as P1. Native pickers via `@tauri-apps/plugin-dialog` `open()` (already used by `PluginsTab`; `dialog:allow-open` already granted to the dashboard window).

## Global Constraints

Copied from the spec and `CLAUDE.md`. Every task includes all of them.

1. **Write path = load whole `AgentProfile`, mutate one field, save whole.** Every Hub write calls a `mur_core::cmd::agent::cmd_perm_*` function. No Tauri command constructs or serialises an `AgentProfile`, and none writes back any DTO (#957).
2. **Validation is the CLI's.** `reject_ungrantable`, `reject_dead_grant`, `validate_host_pattern` run because the CLI functions run; their error text reaches the UI verbatim via `format!("{e:#}")`.
3. **Restart is said, not implied.** After a write to a running agent the block shows `perm.restartHint`. (Deviation from spec §P2 "with a Restart action": the Hub has `start_agent` / `stop_agent` and no restart command; the header already offers Stop and Run. The hint names them. A restart command is out of scope.)
4. `limits`, `llm`, `fail_closed_on_sandbox_error` stay read-only.
5. Brand name is uppercase **MUR** in every user-visible string.
6. Single source file ≤ 800 lines. `lib.rs` is 905 already — this plan adds only its twelve `invoke_handler` lines there; everything else lives in `perm_admin.rs`. `PermissionsTab.tsx` (212) + `PermissionEditors.tsx` (new) each stay under.
7. Every new user-visible string lands in both `en.ts` and `zh-TW.ts` in the same commit.
8. Components reference only semantic tokens; no raw hex.
9. Tests never touch the DOM: pure functions only. Rust tests never set `MUR_HOME` (env races under `cargo test`, see `project_mur_core_flaky_tests`): mutations are tested on `&mut` structs, the `cmd_*` wrappers only load/save.
10. Every commit is gated on the real exit code: `set -o pipefail; <cmd> 2>&1 | tail -n 20`.

## Working agreement

Identical to the P1 plan (env exports, test commands, `npm` commands from `mur-hub-gui/ui/`). Branch: `feat/hub-permissions-p2-edit` off `feat/hub-permissions-p1-view`.

## File structure

| File | Responsibility |
|---|---|
| `mur-core/src/cmd/agent/perm.rs` (modify) | `remove_path()` + `cmd_perm_remove_path()`; `parse_outbound_mode()` accepting `proxy_only`; tests |
| `mur-core/src/cmd/agent/mod.rs` (modify) | export `cmd_perm_remove_path` |
| `mur-hub-gui/src-tauri/src/perm_admin.rs` (new) | twelve `agent_perm_*` commands; `PathVerb` |
| `mur-hub-gui/src-tauri/src/lib.rs` (modify) | `mod perm_admin;` + twelve handler lines |
| `mur-hub-gui/ui/src/components/inspector/tabs/permissionsModel.ts` (+ `.test.ts`) (modify) | `OUTBOUND_MODES`, `SPAWN_MODES`, `TOOL_POLICIES`, `afterWriteHint` |
| `mur-hub-gui/ui/src/components/inspector/tabs/PermissionEditors.tsx` (new) | `usePermWrite`, `ModeSelect`, `AddHost`, `AddFolder`, `AddProgram`, `AddDir`, `AddRule`, `RemoveBtn`, `PolicySelect` |
| `mur-hub-gui/ui/src/components/inspector/tabs/PermissionsTab.tsx` (modify) | wires editors into the blocks; takes `isRunning` |
| `mur-hub-gui/ui/src/components/detail/agent/CapabilitiesTab.tsx`, `AgentDetail.tsx` (modify) | pass `isRunning` |
| `mur-hub-gui/ui/src/styles/components/permissions.css` (modify) | `.perm__row-x`, `.perm__add`, `.perm__select`, `.perm__hint` |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` (modify) | `perm.*` editing keys |

---

### Task 1 — mur-core: the missing remove, and `proxy_only`

**Interfaces.** Produces `mur_core::cmd::agent::cmd_perm_remove_path(name: &str, verb: &str, path: &str) -> Result<()>` (verb ∈ `read|write|deny`) and `cmd_perm_set_mode(name, "network.outbound", "proxy_only")` accepted. Task 2 consumes both.

- [ ] `perm.rs`: below `cmd_perm_deny_path` add:
  ```rust
  /// Drop one path from one grant list. `deny_path` ADDS to the deny list; this
  /// is the only way to take a grant back short of editing profile.yaml.
  pub fn remove_path(
      fs: &mut mur_common::agent::FilesystemEntitlement,
      verb: &str,
      path_arg: &str,
  ) -> Result<bool> {
      let list = match verb {
          "read" => &mut fs.read,
          "write" => &mut fs.write,
          "deny" => &mut fs.deny,
          other => bail!("remove-path: unknown list '{other}' (read, write, deny)"),
      };
      let before = list.len();
      list.retain(|p| p != path_arg);
      Ok(list.len() != before)
  }

  pub fn cmd_perm_remove_path(name: &str, verb: &str, path_arg: &str) -> Result<()> {
      let (path, mut profile) = load_profile_for_edit(name)?;
      if !remove_path(&mut profile.entitlements.filesystem, verb, path_arg)? {
          bail!("'{path_arg}' is not in the {verb} list of '{name}'");
      }
      save_profile(&path, &mut profile)?;
      warn_if_running(name);
      Ok(())
  }
  ```
- [ ] `perm.rs`: in `cmd_perm_set_mode`, replace the inline `"network.outbound"` match with a call to a new pure parser placed just above the function:
  ```rust
  /// The wire names plus the two spellings people type for the fourth mode.
  /// `ProxyOnly` was unreachable from the CLI until now.
  fn parse_outbound_mode(value: &str) -> Result<NetworkOutboundMode> {
      Ok(match value {
          "restricted" => NetworkOutboundMode::Restricted,
          "unrestricted" => NetworkOutboundMode::Unrestricted,
          "proxy_only" | "proxy-only" | "proxyonly" => NetworkOutboundMode::ProxyOnly,
          "off" => NetworkOutboundMode::Off,
          other => bail!("invalid outbound mode '{other}' (restricted, unrestricted, proxy_only, off)"),
      })
  }
  ```
  and in the match arm: `let mode = parse_outbound_mode(value)?;`.
- [ ] `mod.rs` line 99 export list: add `cmd_perm_remove_path`.
- [ ] `perm.rs` tests module, after `inert_patterns_are_refused_with_guidance`:
  ```rust
      #[test]
      fn remove_path_takes_one_grant_back_and_reports_whether_it_was_there() {
          let mut fs = mur_common::agent::FilesystemEntitlement {
              read: vec!["/a".into()],
              write: vec!["/b".into(), "/c".into()],
              deny: vec![],
          };
          assert!(remove_path(&mut fs, "write", "/b").unwrap());
          assert_eq!(fs.write, vec!["/c"]);
          assert!(!remove_path(&mut fs, "write", "/b").unwrap(), "already gone");
          assert_eq!(fs.read, vec!["/a"], "other lists untouched");
          assert!(remove_path(&mut fs, "exec", "/a").is_err());
      }

      #[test]
      fn proxy_only_is_now_reachable_from_the_cli() {
          use mur_common::agent::NetworkOutboundMode as M;
          assert_eq!(parse_outbound_mode("proxy_only").unwrap(), M::ProxyOnly);
          assert_eq!(parse_outbound_mode("proxy-only").unwrap(), M::ProxyOnly);
          assert_eq!(parse_outbound_mode("off").unwrap(), M::Off);
          assert!(parse_outbound_mode("open").is_err());
      }
  ```
  Add `parse_outbound_mode, remove_path` to the test module's `use super::…` line.
- [ ] `set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib cmd::agent::perm 2>&1 | tail -n 6` → `17 tests run: 17 passed`. `cargo clippy -p mur-core --all-targets -- -D warnings` → 0. `cargo fmt -p mur-core`.
- [ ] Commit: `feat(core): perm remove-path, and proxy_only reachable from set-mode`

---

### Task 2 — Hub: twelve write commands

**Interfaces.** Consumes Task 1. Produces Tauri commands (all `(name, …) -> Result<AgentDetail, String>`): `agent_perm_set_outbound_mode(name, mode)`, `agent_perm_allow_host(name, host)`, `agent_perm_deny_host(name, host)`, `agent_perm_grant_path(name, verb, path)`, `agent_perm_remove_path(name, verb, path)`, `agent_perm_set_spawn_mode(name, mode)`, `agent_perm_allow_spawn(name, program)`, `agent_perm_deny_spawn(name, program)`, `agent_perm_allow_spawn_dir(name, dir)`, `agent_perm_deny_spawn_dir(name, dir)`, `agent_perm_set_tool(name, pattern, policy)`, `agent_perm_clear_tool(name, pattern)`. Task 3 invokes them by these names with camelCase args.

- [ ] Create `mur-hub-gui/src-tauri/src/perm_admin.rs`:
  ```rust
  //! Permission writes from the Hub (spec 2026-09-07 §P2). Every command is one
  //! `cmd_perm_*` call and a re-read: the CLI function loads the whole profile,
  //! changes one field, validates with its own rules, and saves the whole
  //! profile — so nothing here can drop a field the DTO does not model (#957),
  //! and the error text the CLI would print is what the UI shows.

  use crate::detail::{AgentDetail, get_agent_detail};
  use mur_common::agent::ToolPolicy;
  use mur_core::cmd::agent::{
      cmd_perm_allow_host, cmd_perm_allow_read, cmd_perm_allow_spawn, cmd_perm_allow_spawn_dir,
      cmd_perm_allow_write, cmd_perm_clear_tool, cmd_perm_deny_host, cmd_perm_deny_path,
      cmd_perm_deny_spawn, cmd_perm_deny_spawn_dir, cmd_perm_remove_path, cmd_perm_set_mode,
      cmd_perm_set_tool,
  };

  fn err(e: anyhow::Error) -> String {
      format!("{e:#}")
  }

  #[tauri::command]
  pub fn agent_perm_set_outbound_mode(name: String, mode: String) -> Result<AgentDetail, String> {
      cmd_perm_set_mode(&name, "network.outbound", &mode).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_allow_host(name: String, host: String) -> Result<AgentDetail, String> {
      cmd_perm_allow_host(&name, host.trim()).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_deny_host(name: String, host: String) -> Result<AgentDetail, String> {
      cmd_perm_deny_host(&name, &host).map_err(err)?;
      get_agent_detail(name)
  }

  /// `verb` is the list name the P1 view uses: read | write | deny.
  #[tauri::command]
  pub fn agent_perm_grant_path(name: String, verb: String, path: String) -> Result<AgentDetail, String> {
      let grant = grant_for(&verb)?;
      grant(&name, &path).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_remove_path(name: String, verb: String, path: String) -> Result<AgentDetail, String> {
      cmd_perm_remove_path(&name, &verb, &path).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_set_spawn_mode(name: String, mode: String) -> Result<AgentDetail, String> {
      cmd_perm_set_mode(&name, "processes.spawn", &mode).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_allow_spawn(name: String, program: String) -> Result<AgentDetail, String> {
      cmd_perm_allow_spawn(&name, program.trim()).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_deny_spawn(name: String, program: String) -> Result<AgentDetail, String> {
      cmd_perm_deny_spawn(&name, &program).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_allow_spawn_dir(name: String, dir: String) -> Result<AgentDetail, String> {
      cmd_perm_allow_spawn_dir(&name, dir.trim()).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_deny_spawn_dir(name: String, dir: String) -> Result<AgentDetail, String> {
      cmd_perm_deny_spawn_dir(&name, &dir).map_err(err)?;
      get_agent_detail(name)
  }

  /// `policy` arrives as the serde name (`allow` / `ask` / `deny`), which is
  /// also what the P1 view emits — no second spelling to keep in step.
  #[tauri::command]
  pub fn agent_perm_set_tool(name: String, pattern: String, policy: ToolPolicy) -> Result<AgentDetail, String> {
      let pattern = pattern.trim();
      if pattern.is_empty() {
          return Err("tool pattern must not be empty".into());
      }
      cmd_perm_set_tool(&name, policy, pattern).map_err(err)?;
      get_agent_detail(name)
  }

  #[tauri::command]
  pub fn agent_perm_clear_tool(name: String, pattern: String) -> Result<AgentDetail, String> {
      cmd_perm_clear_tool(&name, &pattern).map_err(err)?;
      get_agent_detail(name)
  }

  type Grant = fn(&str, &str) -> anyhow::Result<()>;

  fn grant_for(verb: &str) -> Result<Grant, String> {
      Ok(match verb {
          "read" => cmd_perm_allow_read,
          "write" => cmd_perm_allow_write,
          "deny" => cmd_perm_deny_path,
          other => return Err(format!("unknown grant list '{other}' (read, write, deny)")),
      })
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      /// The three list names the view emits map to the three CLI grants;
      /// anything else is refused before any profile is touched.
      #[test]
      fn grant_verbs_are_the_view_list_names() {
          for v in ["read", "write", "deny"] {
              assert!(grant_for(v).is_ok(), "{v}");
          }
          assert!(grant_for("exec").is_err());
          assert!(grant_for("").is_err());
      }
  }
  ```
- [ ] `lib.rs`: add `mod perm_admin;` beside `mod mcp_skills;` (grep `^mod mcp_skills`), and in `invoke_handler` after `mcp_skills::agent_mcp_remove,` add the twelve `perm_admin::agent_perm_*` lines in the order above.
- [ ] `set -o pipefail; cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib perm_admin:: 2>&1 | tail -n 5` → 1 passed. `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all-targets -- -D warnings` → 0. `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
- [ ] Commit: `feat(hub): permission write commands, each one CLI call and a re-read`

---

### Task 3 — UI: editors in every block

**Interfaces.** Consumes Task 2's command names. Produces nothing later tasks consume.

- [ ] `permissionsModel.test.ts`: append
  ```ts
  import { OUTBOUND_MODES, SPAWN_MODES, TOOL_POLICIES, afterWriteHint } from "./permissionsModel";

  describe("editing model", () => {
    it("offers every mode the CLI accepts, in the view's spelling", () => {
      expect(OUTBOUND_MODES).toEqual(["restricted", "unrestricted", "proxy_only", "off"]);
      expect(SPAWN_MODES).toEqual(["allowlist", "any", "none", "strict"]);
      expect(TOOL_POLICIES).toEqual(["allow", "ask", "deny"]);
    });
    it("says restart only when the agent is running", () => {
      expect(afterWriteHint(true)).toBe("perm.restartHint");
      expect(afterWriteHint(false)).toBe("perm.saved");
    });
  });
  ```
  `npm test -- src/components/inspector/tabs/permissionsModel.test.ts` → fails.
- [ ] `permissionsModel.ts`: append
  ```ts
  /** What the CLI's set-mode accepts, spelled as the view/serde names. The
   *  outbound value goes to `cmd_perm_set_mode` verbatim; `proxy_only` is the
   *  CLI spelling (the DTO shows `proxyonly`, serde's lowercase of the variant). */
  export const OUTBOUND_MODES = ["restricted", "unrestricted", "proxy_only", "off"] as const;
  export const SPAWN_MODES = ["allowlist", "any", "none", "strict"] as const;
  export const TOOL_POLICIES = ["allow", "ask", "deny"] as const;

  /** Spec §P2: restart is said, not implied. Only a running agent needs it. */
  export function afterWriteHint(isRunning: boolean): "perm.restartHint" | "perm.saved" {
    return isRunning ? "perm.restartHint" : "perm.saved";
  }

  /** The DTO's outbound spelling → the CLI's, for the select's current value. */
  export function outboundModeForCli(dto: string): (typeof OUTBOUND_MODES)[number] {
    return dto === "proxyonly" ? "proxy_only" : (dto as (typeof OUTBOUND_MODES)[number]);
  }
  ```
  Test passes.
- [ ] i18n `en.ts` after `"perm.copied"`:
  ```ts
    "perm.saved": "Saved.",
    "perm.restartHint": "Saved — takes effect when the agent restarts (Stop, then Run, in the header).",
    "perm.addFolder": "Add folder…",
    "perm.addHost": "Add host",
    "perm.hostPlaceholder": "api.example.com or *.example.com",
    "perm.addProgram": "Add program…",
    "perm.addDir": "Add build lane…",
    "perm.addRule": "Add rule",
    "perm.patternPlaceholder": "tool name, or prefix*",
    "perm.pickFolder": "Choose a folder this agent may {verb}",
    "perm.pickProgram": "Choose a program this agent may run",
    "perm.pickDir": "Choose a directory whose executables this agent may run",
    "perm.mode.restricted": "restricted — listed hosts only",
    "perm.mode.unrestricted": "unrestricted — any host, any port",
    "perm.mode.proxy_only": "proxy only — loopback proxies, hosts still guarded",
    "perm.mode.off": "off — no outbound",
    "perm.spawnMode.allowlist": "allowlist — listed programs plus system paths",
    "perm.spawnMode.any": "any",
    "perm.spawnMode.none": "none",
    "perm.spawnMode.strict": "strict — listed programs only, no system paths",
  ```
  `zh-TW.ts`:
  ```ts
    "perm.saved": "已儲存。",
    "perm.restartHint": "已儲存 — 待 agent 重新啟動後生效（標題列先 Stop 再 Run）。",
    "perm.addFolder": "新增資料夾…",
    "perm.addHost": "新增主機",
    "perm.hostPlaceholder": "api.example.com 或 *.example.com",
    "perm.addProgram": "新增程式…",
    "perm.addDir": "新增 build lane…",
    "perm.addRule": "新增規則",
    "perm.patternPlaceholder": "工具名稱，或前綴*",
    "perm.pickFolder": "選擇這個 agent 可以{verb}的資料夾",
    "perm.pickProgram": "選擇這個 agent 可以執行的程式",
    "perm.pickDir": "選擇一個目錄，其中的執行檔這個 agent 都可以執行",
    "perm.mode.restricted": "restricted — 只有列出的主機",
    "perm.mode.unrestricted": "unrestricted — 任何主機、任何埠",
    "perm.mode.proxy_only": "proxy only — 只走 loopback proxy，主機仍受守衛",
    "perm.mode.off": "off — 無對外連線",
    "perm.spawnMode.allowlist": "allowlist — 列出的程式加系統路徑",
    "perm.spawnMode.any": "any",
    "perm.spawnMode.none": "none",
    "perm.spawnMode.strict": "strict — 只有列出的程式，不含系統路徑",
  ```
- [ ] Create `PermissionEditors.tsx` — `usePermWrite(detail, onSaved, isRunning)` returning `{ busy, error, hint, run(cmd, args) }` (mirrors `McpTab.addServer`: `setError(null); setBusy(true); try { onSaved(await invoke<AgentDetail>(cmd, { name: detail.agent_name, ...args })); setHint(afterWriteHint(isRunning)) } catch (e) { setError(String(e)) } finally { setBusy(false) }`), plus: `ModeSelect` (`<select className="perm__select">` over a given list, `onChange` → `run`), `RemoveBtn` (`<button className="perm__row-x" title={t("detail.remove")}>×</button>`), `AddHost` (input + `detail.add` button → `agent_perm_allow_host`), `AddFolder({verb})` (`open({ directory: true, title: t("perm.pickFolder", { verb: t(`perm.fs.${verb}`) }) })` → `agent_perm_grant_path`), `AddProgram` (`open({ multiple: false, title })` → `agent_perm_allow_spawn`), `AddDir` (`open({ directory: true, title })` → `agent_perm_allow_spawn_dir`), `AddRule` (pattern input + `PolicySelect` + `detail.add` → `agent_perm_set_tool`), `PolicySelect` (select over `TOOL_POLICIES`). Each editor takes the `write` object from the hook. `≤ 220` lines.
- [ ] `PermissionsTab.tsx`: accept `isRunning: boolean`; create `const write = usePermWrite(detail, onSaved, isRunning)` once at the top; render `write.error` as `<p className="save-error">` and `write.hint` as `<p className="perm__hint field-muted">` directly under the enforcement banner (one place, not per block); then per block:
  - runtime: `<ModeSelect value={outboundModeForCli(v.runtime_outbound.mode)} options={OUTBOUND_MODES} labelKey="perm.mode" cmd="agent_perm_set_outbound_mode" />` replaces the plain `perm__mode` line; each allow-host row gets `<RemoveBtn onClick={() => write.run("agent_perm_deny_host", { host })} />`; `<AddHost />` after the list. (Mode `off`/`unrestricted` still hide the host list, as P1 does.)
  - filesystem: each of Read / Write / Deny sub-lists gets `<AddFolder verb="read" />` etc. under it (shown even when the list is empty — replace the `perm.fs.none` early-return with the three `Grants` + three `AddFolder`s); each row gets `RemoveBtn` → `agent_perm_remove_path { verb, path: g.raw }`.
  - processes: `<ModeSelect value={v.processes.spawn_mode} options={SPAWN_MODES} labelKey="perm.spawnMode" cmd="agent_perm_set_spawn_mode" />`; program rows get `RemoveBtn` → `agent_perm_deny_spawn`; `<AddProgram />`; build-lane rows get `RemoveBtn` → `agent_perm_deny_spawn_dir`; `<AddDir />`.
  - tools: each rule's policy text becomes `<PolicySelect value={r.policy} onChange={(policy) => write.run("agent_perm_set_tool", { pattern: r.pattern, policy })} />`; `RemoveBtn` → `agent_perm_clear_tool`; `<AddRule />` after the list (also when empty).
  - limits block unchanged (Constraint 4).
  - The `CopyCmd` lines stay.
- [ ] `CapabilitiesTab.tsx`: prop `isRunning: boolean` threaded to `<PermissionsTab detail={detail} onSaved={onSaved} isRunning={isRunning} />`. `AgentDetail.tsx` line 255: `<CapabilitiesTab detail={detail} onSaved={setDetail} isRunning={isRunning} />` (`isRunning` is already computed at line 142).
- [ ] `permissions.css` append:
  ```css
  .perm__select { font: inherit; font-size: var(--text-sm); font-family: var(--font-mono); color: var(--text-primary); background: var(--surface-secondary); border: 1px solid var(--border-line); border-radius: var(--radius-sm); padding: 3px 6px; margin: 2px 0; }
  .perm__row-x { margin-left: auto; font: inherit; line-height: 1; color: var(--text-tertiary); background: none; border: 0; cursor: pointer; padding: 0 4px; }
  .perm__row-x:hover { color: var(--status-stopped); }
  .perm__add { display: flex; gap: 6px; align-items: center; margin: 4px 0 0; }
  .perm__add .input { flex: 1; min-width: 0; max-width: 320px; }
  .perm__hint { font-size: var(--text-xs); margin: 0 0 var(--space-3); }
  ```
- [ ] `set -o pipefail; npm test 2>&1 | tail -n 4; npm run build 2>&1 | tail -n 2; npm run lint 2>&1 | tail -n 2` → all green, 0 lint errors.
- [ ] Manual acceptance in the real Hub, on a **running** agent: (1) Filesystem → Write → "Add folder…" → pick an existing folder → row appears with `·`/✓ and the hint says restart; (2) pick `~` (home) → the CLI's `reject_ungrantable` message appears verbatim in the error line, nothing added; (3) `×` on the new row → gone; (4) outbound select → `proxy_only` → view shows `proxyonly`; (5) tools → add `bash` `deny` → red DENY row; change its select to `allow` → green; `×` → gone; (6) `mur agent perm show <name>` in a terminal agrees with every change. Then stop the agent and confirm the hint reads "Saved." without the restart clause.
- [ ] Commit: `feat(hub): edit permissions in place — folders via the native picker, hosts, spawn, tool rules`

## Self-review

- Spec §P2 coverage: write path (Constraint 1 → Task 2 only calls `cmd_perm_*`), validation (Constraint 2), restart said (Constraint 3, with the recorded deviation), read-only leftovers (Constraint 4) — all in Task 3's block list.
- Cross-task names: `agent_perm_*` twelve names identical in Task 2 (Rust) and Task 3 (invoke strings); verbs `read|write|deny` identical in `remove_path`, `grant_for`, `AddFolder`, i18n `perm.fs.<verb>`; outbound `proxy_only` (CLI) vs `proxyonly` (DTO) bridged by `outboundModeForCli` and documented in its comment.
- No placeholders; every `<…>` is inside a user-facing CLI template or a picker title.
