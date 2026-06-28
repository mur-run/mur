# Hub Fleet Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fourth "Fleets" top-level surface to MUR Hub so users can list, manage, and operate agent fleets from the GUI.

**Architecture:** Thin Tauri command layer (`fleet.rs`) wraps existing `mur-core::cmd::fleet::*` functions; React components (`FleetView` / `FleetRail` / `FleetDetail` / `FleetCreateModal`) follow the `WorkView` pattern; `DashboardApp.tsx` gains a 4th nav surface.

**Tech Stack:** Rust / Tauri 2, React + TypeScript (no extra libraries), `tauri-plugin-dialog` (already installed) for file picker, `chrono` + `dirs` (both already in Cargo.toml).

## Global Constraints

- Brand: "MUR" (uppercase) in all user-visible strings; `mur` (lowercase) in code identifiers.
- `mur_home_path()` — already defined as `pub(crate)` in `mur-hub-gui/src-tauri/src/lib.rs:657`; import it via `use crate::mur_home_path`.
- `mur-core` is already a workspace dep in `mur-hub-gui/src-tauri/Cargo.toml`.
- No new dependencies — use only what is already in Cargo.toml / package.json.
- Rust edition 2024; let-chains stable.
- All `#[tauri::command]` functions return `Result<T, String>` — map errors with `.map_err(|e| e.to_string())`.
- i18n: add keys to BOTH `ui/src/i18n/en.ts` AND `ui/src/i18n/zh-TW.ts`.
- Files ≤ 800 lines. Split if approaching limit.
- Build the Hub .app with `npm run build` (inside `mur-hub-gui/ui`) then `cargo tauri build` (inside `mur-hub-gui/src-tauri`) for final verify. For quick iteration use `cargo check`.

---

## File Map

| File | Change |
|---|---|
| `mur-hub-gui/src-tauri/src/fleet.rs` | **Create** — all Tauri fleet commands + Serialize structs |
| `mur-hub-gui/src-tauri/src/lib.rs` | **Modify** — add `pub mod fleet;` + register 13 commands |
| `mur-hub-gui/ui/src/components/fleet/types.ts` | **Create** — TypeScript interfaces matching Rust structs |
| `mur-hub-gui/ui/src/components/fleet/FleetRail.tsx` | **Create** — left rail list |
| `mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx` | **Create** — create fleet form |
| `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` | **Create** — detail panel (members + jobs + actions) |
| `mur-hub-gui/ui/src/components/fleet/FleetView.tsx` | **Create** — surface root, state + data fetching |
| `mur-hub-gui/ui/src/components/DashboardApp.tsx` | **Modify** — add "fleet" surface type + nav button + render |
| `mur-hub-gui/ui/src/i18n/en.ts` | **Modify** — add fleet.* keys |
| `mur-hub-gui/ui/src/i18n/zh-TW.ts` | **Modify** — add fleet.* keys (Traditional Chinese) |

---

### Task 1: Rust fleet.rs — Tauri commands

**Files:**
- Create: `mur-hub-gui/src-tauri/src/fleet.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `fleet_list`, `fleet_detail`, `fleet_create`, `fleet_delete`, `fleet_start`, `fleet_stop`, `fleet_run`, `fleet_send`, `fleet_jobs`, `fleet_add_member`, `fleet_remove_member`, `fleet_export`, `fleet_import` — all registered as Tauri commands.
- Produces: `FleetSummary`, `FleetDetail`, `JobRow` — serializable Rust structs; Tasks 2+ reference their field names in TypeScript.

- [ ] **Step 1: Write the unit test first**

Create `mur-hub-gui/src-tauri/src/fleet.rs` with only the structs + helpers + tests (no Tauri commands yet):

```rust
//! Fleet management Tauri commands for MUR Hub.

use std::path::PathBuf;

use mur_common::fleet::{Job, JobStatus};
use mur_core::cmd::fleet::{control, create, delete, export, import, jobs, roster, run, store};
use serde::Serialize;

use crate::mur_home_path;

#[derive(Serialize, Clone)]
pub struct FleetSummary {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub member_count: usize,
    pub active_jobs: usize,
    pub stopped: bool,
    pub running: bool,
}

#[derive(Serialize, Clone)]
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
}

#[derive(Serialize, Clone)]
pub struct JobRow {
    pub id: String,
    pub text: String,
    pub status: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}

pub(crate) fn job_to_row(j: Job) -> JobRow {
    JobRow {
        id: j.id,
        text: j.text,
        status: j.status.to_string(),
        created_at: j.created_at,
        finished_at: j.finished_at,
        result: j.result,
        error: j.error,
    }
}

pub(crate) fn display(name: &str, display_name: &str) -> String {
    if display_name.is_empty() {
        name.to_string()
    } else {
        display_name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_job(id: &str, status: JobStatus) -> Job {
        Job {
            id: id.to_string(),
            text: "test task".to_string(),
            source: "test".to_string(),
            status,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn job_to_row_maps_status_to_string() {
        assert_eq!(job_to_row(make_job("a", JobStatus::Running)).status, "running");
        assert_eq!(job_to_row(make_job("b", JobStatus::Done)).status, "done");
        assert_eq!(job_to_row(make_job("c", JobStatus::Failed)).status, "failed");
        assert_eq!(job_to_row(make_job("d", JobStatus::Queued)).status, "queued");
        assert_eq!(job_to_row(make_job("e", JobStatus::Canceled)).status, "canceled");
    }

    #[test]
    fn display_falls_back_to_name() {
        assert_eq!(display("dev", ""), "dev");
        assert_eq!(display("dev", "Dev Squad"), "Dev Squad");
    }
}
```

- [ ] **Step 2: Run tests — expect PASS**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download \
  cargo test -p mur-hub-gui-lib fleet 2>&1 | tail -20
```

Expected: `test fleet::tests::job_to_row_maps_status_to_string ... ok` and `test fleet::tests::display_falls_back_to_name ... ok`

Note: the lib crate name is `mur_hub_gui_lib` (from `[lib] name = "mur_hub_gui_lib"` in Cargo.toml). Use `-p mur-hub-gui` if the above doesn't resolve; alternatively use `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml fleet`.

- [ ] **Step 3: Add all Tauri commands to fleet.rs**

Append below the `#[cfg(test)]` block (or add before it):

```rust
// ─── Tauri commands ───────────────────────────────────────────────────────

#[tauri::command]
pub fn fleet_list() -> Result<Vec<FleetSummary>, String> {
    let home = mur_home_path();
    let names = store::list_fleets(&home).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(names.len());
    for name in &names {
        let f = store::load_fleet(&home, name).map_err(|e| e.to_string())?;
        let job_list = jobs::list_jobs(&home, name).unwrap_or_default();
        let active = job_list.iter().filter(|j| !j.status.is_terminal()).count();
        let running = job_list.iter().any(|j| j.status == JobStatus::Running);
        out.push(FleetSummary {
            display_name: display(&f.name, &f.display_name),
            name: f.name.clone(),
            goal: f.goal.clone(),
            member_count: f.members.len(),
            active_jobs: active,
            stopped: control::is_stopped(&home, name),
            running,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn fleet_detail(name: String) -> Result<FleetDetail, String> {
    let home = mur_home_path();
    let f = store::load_fleet(&home, &name).map_err(|e| e.to_string())?;
    Ok(FleetDetail {
        display_name: display(&f.name, &f.display_name),
        name: f.name.clone(),
        goal: f.goal.clone(),
        router: f.router_or_concierge().to_string(),
        members: f.members.clone(),
        channel_id: f.channel_id.clone(),
        stopped: control::is_stopped(&home, &name),
    })
}

#[tauri::command]
pub fn fleet_create(
    name: String,
    goal: String,
    members: Vec<String>,
    router: Option<String>,
) -> Result<(), String> {
    let home = mur_home_path();
    create::cmd_fleet_create(&home, &name, members, router, Some(goal))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_delete(name: String) -> Result<(), String> {
    let home = mur_home_path();
    // yes: true — Hub already confirmed with the user via JS confirm()
    delete::cmd_fleet_delete(&home, &name, true).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_stop(name: String) -> Result<(), String> {
    let home = mur_home_path();
    control::cmd_fleet_stop(&home, &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_start(name: String) -> Result<(), String> {
    let home = mur_home_path();
    control::cmd_fleet_start(&home, &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn fleet_run(name: String, app: tauri::AppHandle) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    // cmd_fleet_run is async and long-running; spawn so the command returns immediately.
    tokio::spawn(async move {
        let ok = run::cmd_fleet_run(&home, &fleet_name, None).await.is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}

#[tauri::command]
pub fn fleet_send(name: String, text: String) -> Result<String, String> {
    let home = mur_home_path();
    let job = jobs::enqueue_job(&home, &name, &text, "hub").map_err(|e| e.to_string())?;
    Ok(job.id)
}

#[tauri::command]
pub fn fleet_jobs(name: String, all: bool) -> Result<Vec<JobRow>, String> {
    let home = mur_home_path();
    let job_list = jobs::list_jobs(&home, &name).map_err(|e| e.to_string())?;
    let filtered: Vec<_> = if all {
        job_list
    } else {
        // ponytail: active = non-terminal only; callers pass all=true for full history
        job_list.into_iter().filter(|j| !j.status.is_terminal()).collect()
    };
    Ok(filtered.into_iter().map(job_to_row).collect())
}

#[tauri::command]
pub fn fleet_add_member(name: String, agent: String) -> Result<(), String> {
    let home = mur_home_path();
    roster::cmd_fleet_add(&home, &name, vec![agent]).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_remove_member(name: String, agent: String) -> Result<(), String> {
    let home = mur_home_path();
    roster::cmd_fleet_remove(&home, &name, vec![agent]).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_export(name: String) -> Result<String, String> {
    let home = mur_home_path();
    let out_path = dirs::desktop_dir()
        .unwrap_or_else(|| home.join("exports"))
        .join(format!("{name}.fleet"));
    let now = chrono::Utc::now().to_rfc3339();
    export::cmd_fleet_export(&home, &name, false, Some(out_path.clone()), &now)
        .map_err(|e| e.to_string())?;
    Ok(out_path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn fleet_import(path: String) -> Result<String, String> {
    let home = mur_home_path();
    let file = PathBuf::from(&path);
    // Extract fleet name from filename stem: "dev-squad.fleet" → "dev-squad"
    let fleet_name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    import::cmd_fleet_import(
        &home,
        &file,
        import::ImportOpts { force: false, no_members: false, yes: true },
    )
    .map_err(|e| e.to_string())?;
    Ok(fleet_name)
}
```

- [ ] **Step 4: Register in lib.rs**

In `mur-hub-gui/src-tauri/src/lib.rs`, find the line `pub mod work;` and add after it:

```rust
pub mod fleet;
```

Then find the `invoke_handler` call (it contains `tauri::generate_handler![...]`) and add the 13 new commands. The existing handler looks like:

```rust
.invoke_handler(tauri::generate_handler![
    list_agents,
    // ... existing commands ...
])
```

Add to the list:

```rust
    fleet::fleet_list,
    fleet::fleet_detail,
    fleet::fleet_create,
    fleet::fleet_delete,
    fleet::fleet_stop,
    fleet::fleet_start,
    fleet::fleet_run,
    fleet::fleet_send,
    fleet::fleet_jobs,
    fleet::fleet_add_member,
    fleet::fleet_remove_member,
    fleet::fleet_export,
    fleet::fleet_import,
```

- [ ] **Step 5: cargo check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download \
  cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml 2>&1 | tail -30
```

Expected: `Finished` with no errors. Fix any type mismatches before continuing.

- [ ] **Step 6: Run tests again — still PASS**

```bash
MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download \
  cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml fleet 2>&1 | tail -10
```

Expected: 2 tests pass.

- [ ] **Step 7: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/src-tauri/src/fleet.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): fleet Tauri commands — fleet_list/detail/create/delete/run/send/jobs/roster/export/import"
```

---

### Task 2: i18n keys + TypeScript types

**Files:**
- Create: `mur-hub-gui/ui/src/components/fleet/types.ts`
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`
- Modify: `mur-hub-gui/ui/src/i18n/zh-TW.ts`

**Interfaces:**
- Produces: `FleetSummary`, `FleetDetail`, `JobRow` TypeScript interfaces — consumed by Tasks 3–6.
- Produces: i18n keys `fleet.*` — consumed by Tasks 3–6 via `useT()`.

- [ ] **Step 1: Create types.ts**

```typescript
// mur-hub-gui/ui/src/components/fleet/types.ts

export interface FleetSummary {
  name: string;
  display_name: string;
  goal: string;
  member_count: number;
  active_jobs: number;
  stopped: boolean;
  running: boolean;
}

export interface FleetDetail {
  name: string;
  display_name: string;
  goal: string;
  router: string;
  members: string[];
  channel_id: string;
  stopped: boolean;
}

export interface JobRow {
  id: string;
  text: string;
  status: "queued" | "running" | "done" | "failed" | "canceled";
  created_at: string;
  finished_at?: string;
  result?: string;
  error?: string;
}
```

- [ ] **Step 2: Add fleet keys to en.ts**

Open `mur-hub-gui/ui/src/i18n/en.ts`. Find the closing `} as const;` and insert before it:

```typescript
  // ─── Fleet surface ──────────────────────────────────────────────────────
  "fleet.tab": "Fleets",
  "fleet.empty": "No fleets yet. Create one to get started.",
  "fleet.new": "New Fleet",
  "fleet.goal": "Goal",
  "fleet.router": "Router",
  "fleet.members": "Members",
  "fleet.addMember": "Add member…",
  "fleet.removeMember": "Remove",
  "fleet.run": "Run",
  "fleet.send": "Send Job",
  "fleet.sendPlaceholder": "Describe the job…",
  "fleet.stop": "Stop",
  "fleet.start": "Start",
  "fleet.export": "Export",
  "fleet.import": "Import",
  "fleet.delete": "Delete",
  "fleet.confirmDelete": "Delete fleet \"{name}\"? This cannot be undone.",
  "fleet.jobs": "Jobs",
  "fleet.showAll": "Show all",
  "fleet.status.queued": "queued",
  "fleet.status.running": "running",
  "fleet.status.done": "done",
  "fleet.status.failed": "failed",
  "fleet.status.canceled": "canceled",
  "fleet.runStarted": "Fleet run started",
  "fleet.runDone": "Fleet run completed",
  "fleet.runFailed": "Fleet run failed",
  "fleet.exported": "Exported to {path}",
  "fleet.imported": "Imported fleet \"{name}\"",
  "fleet.create.name": "Fleet name",
  "fleet.create.goal": "Goal",
  "fleet.create.members": "Members (comma-separated agent names)",
  "fleet.create.router": "Router (optional, defaults to concierge)",
  "fleet.create.submit": "Create Fleet",
```

- [ ] **Step 3: Add fleet keys to zh-TW.ts**

Open `mur-hub-gui/ui/src/i18n/zh-TW.ts`. Find the closing `} as const;` and insert before it:

```typescript
  // ─── Fleet surface ──────────────────────────────────────────────────────
  "fleet.tab": "機群",
  "fleet.empty": "尚無機群。建立一個以開始使用。",
  "fleet.new": "新增機群",
  "fleet.goal": "目標",
  "fleet.router": "路由器",
  "fleet.members": "成員",
  "fleet.addMember": "新增成員…",
  "fleet.removeMember": "移除",
  "fleet.run": "執行",
  "fleet.send": "傳送任務",
  "fleet.sendPlaceholder": "描述任務…",
  "fleet.stop": "停止",
  "fleet.start": "啟動",
  "fleet.export": "匯出",
  "fleet.import": "匯入",
  "fleet.delete": "刪除",
  "fleet.confirmDelete": "確定刪除機群「{name}」？此操作無法復原。",
  "fleet.jobs": "任務",
  "fleet.showAll": "顯示全部",
  "fleet.status.queued": "排隊中",
  "fleet.status.running": "執行中",
  "fleet.status.done": "完成",
  "fleet.status.failed": "失敗",
  "fleet.status.canceled": "已取消",
  "fleet.runStarted": "機群已開始執行",
  "fleet.runDone": "機群執行完成",
  "fleet.runFailed": "機群執行失敗",
  "fleet.exported": "已匯出至 {path}",
  "fleet.imported": "已匯入機群「{name}」",
  "fleet.create.name": "機群名稱",
  "fleet.create.goal": "目標",
  "fleet.create.members": "成員（以逗號分隔的 Agent 名稱）",
  "fleet.create.router": "路由器（選填，預設為 concierge）",
  "fleet.create.submit": "建立機群",
```

- [ ] **Step 4: TypeScript compile check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npx tsc --noEmit 2>&1 | head -20
```

Expected: no errors. If `useT` type inference complains about missing keys, verify the key names match exactly between en.ts and zh-TW.ts.

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/ui/src/components/fleet/types.ts \
        mur-hub-gui/ui/src/i18n/en.ts \
        mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): fleet i18n keys (en + zh-TW) + TypeScript types"
```

---

### Task 3: FleetRail component

**Files:**
- Create: `mur-hub-gui/ui/src/components/fleet/FleetRail.tsx`

**Interfaces:**
- Consumes: `FleetSummary` from `./types`
- Produces: `<FleetRail>` — rendered by `FleetView` (Task 6)

- [ ] **Step 1: Create FleetRail.tsx**

```tsx
// mur-hub-gui/ui/src/components/fleet/FleetRail.tsx
import { useT } from "../../i18n";
import type { FleetSummary } from "./types";

interface Props {
  fleets: FleetSummary[];
  selectedName: string | null;
  onSelect: (name: string) => void;
  onNew: () => void;
}

function statusBadge(f: FleetSummary): string {
  if (f.stopped) return "⏸";
  if (f.running) return "▶";
  return "●";
}

function statusClass(f: FleetSummary): string {
  if (f.stopped) return "fleet-rail__status--stopped";
  if (f.running) return "fleet-rail__status--running";
  return "fleet-rail__status--idle";
}

export function FleetRail({ fleets, selectedName, onSelect, onNew }: Props) {
  const t = useT();
  return (
    <aside className="fleet-rail">
      <button className="fleet-rail__new toolbar-btn toolbar-btn--primary" onClick={onNew}>
        + {t("fleet.new")}
      </button>
      {fleets.length === 0 && (
        <p className="fleet-rail__empty">{t("fleet.empty")}</p>
      )}
      <ul className="fleet-rail__list">
        {fleets.map((f) => (
          <li
            key={f.name}
            className={`fleet-rail__item${selectedName === f.name ? " is-selected" : ""}`}
            onClick={() => onSelect(f.name)}
          >
            <span className={`fleet-rail__status ${statusClass(f)}`}>
              {statusBadge(f)}
            </span>
            <span className="fleet-rail__name">{f.display_name}</span>
            {f.active_jobs > 0 && (
              <span className="fleet-rail__jobs">{f.active_jobs}</span>
            )}
          </li>
        ))}
      </ul>
    </aside>
  );
}
```

- [ ] **Step 2: TypeScript check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npx tsc --noEmit 2>&1 | head -20
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/ui/src/components/fleet/FleetRail.tsx
git commit -m "feat(hub): FleetRail — fleet list left rail"
```

---

### Task 4: FleetCreateModal component

**Files:**
- Create: `mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx`

**Interfaces:**
- Consumes: `invoke("fleet_create", { name, goal, members, router? })` → `Promise<void>`
- Produces: `<FleetCreateModal>` — rendered by `FleetView` (Task 6)

- [ ] **Step 1: Create FleetCreateModal.tsx**

```tsx
// mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";

interface Props {
  onCreated: (name: string) => void;
  onClose: () => void;
}

export function FleetCreateModal({ onCreated, onClose }: Props) {
  const t = useT();
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [members, setMembers] = useState("");
  const [router, setRouter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setBusy(true);
    const memberList = members
      .split(",")
      .map((m) => m.trim())
      .filter(Boolean);
    try {
      await invoke("fleet_create", {
        name: name.trim(),
        goal: goal.trim(),
        members: memberList,
        router: router.trim() || null,
      });
      onCreated(name.trim());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-card" onClick={(e) => e.stopPropagation()}>
        <h2>{t("fleet.new")}</h2>
        <form onSubmit={handleSubmit}>
          <label className="field">
            <span>{t("fleet.create.name")}</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="dev-squad"
              required
              pattern="[a-z0-9_-]+"
              title="Lowercase letters, digits, - or _"
              autoFocus
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.goal")}</span>
            <input
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="Ship the v3 release"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.members")}</span>
            <input
              value={members}
              onChange={(e) => setMembers(e.target.value)}
              placeholder="pm, qa, dev"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.router")}</span>
            <input
              value={router}
              onChange={(e) => setRouter(e.target.value)}
              placeholder="mur"
            />
          </label>
          {error && <p className="field-error">{error}</p>}
          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              {t("app.cancel") ?? "Cancel"}
            </button>
            <button
              type="submit"
              className="toolbar-btn toolbar-btn--primary"
              disabled={busy}
            >
              {busy ? "…" : t("fleet.create.submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
```

Note: `t("app.cancel")` — check if this key exists in en.ts; if not use the string literal `"Cancel"` / `"取消"`. The `?? "Cancel"` fallback handles either case.

- [ ] **Step 2: TypeScript check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npx tsc --noEmit 2>&1 | head -20
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx
git commit -m "feat(hub): FleetCreateModal — create fleet form"
```

---

### Task 5: FleetDetail component

**Files:**
- Create: `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx`

**Interfaces:**
- Consumes: `FleetDetail`, `JobRow` from `./types`
- Consumes: invoke calls: `fleet_stop`, `fleet_start`, `fleet_run`, `fleet_send`, `fleet_add_member`, `fleet_remove_member`, `fleet_export`, `fleet_import`, `fleet_delete`, `fleet_jobs`
- Produces: `<FleetDetail>` — rendered by `FleetView` (Task 6); calls `onRefresh`, `onDelete` callbacks to signal `FleetView` to reload state.

- [ ] **Step 1: Create FleetDetail.tsx**

```tsx
// mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../../i18n";
import type { FleetDetail as Detail, JobRow } from "./types";

interface Props {
  detail: Detail;
  jobs: JobRow[];
  onRefresh: () => void;
  onDelete: () => void;
}

function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

function statusBadge(d: Detail): string {
  if (d.stopped) return "⏸ stopped";
  return "● idle";
}

function jobStatusClass(status: JobRow["status"]): string {
  return `fleet-job__status fleet-job__status--${status}`;
}

export function FleetDetail({ detail, jobs, onRefresh, onDelete }: Props) {
  const t = useT();
  const [addInput, setAddInput] = useState("");
  const [sendInput, setSendInput] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [busy, setBusy] = useState<string | null>(null); // which action is in progress

  async function call(action: string, args: Record<string, unknown>) {
    setBusy(action);
    try {
      await invoke(action, args);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleRun() {
    setBusy("fleet_run");
    try {
      await invoke("fleet_run", { name: detail.name });
      showToast(t("fleet.runStarted"));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleSend() {
    if (!sendInput.trim()) return;
    setBusy("fleet_send");
    try {
      const jobId = await invoke<string>("fleet_send", {
        name: detail.name,
        text: sendInput.trim(),
      });
      setSendInput("");
      showToast(`Job queued: ${jobId.slice(0, 8)}`);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleAddMember() {
    const agent = addInput.trim();
    if (!agent) return;
    await call("fleet_add_member", { name: detail.name, agent });
    setAddInput("");
  }

  async function handleExport() {
    setBusy("fleet_export");
    try {
      const path = await invoke<string>("fleet_export", { name: detail.name });
      showToast(t("fleet.exported").replace("{path}", path), 4000);
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleImport() {
    const selected = await open({ filters: [{ name: "Fleet", extensions: ["fleet"] }] });
    if (!selected) return;
    const filePath = typeof selected === "string" ? selected : selected[0];
    if (!filePath) return;
    setBusy("fleet_import");
    try {
      const name = await invoke<string>("fleet_import", { path: filePath });
      showToast(t("fleet.imported").replace("{name}", name));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleDelete() {
    const msg = t("fleet.confirmDelete").replace("{name}", detail.display_name);
    if (!window.confirm(msg)) return;
    setBusy("fleet_delete");
    try {
      await invoke("fleet_delete", { name: detail.name });
      onDelete();
    } catch (err) {
      showToast(String(err), 4000);
      setBusy(null);
    }
  }

  async function handleShowAll() {
    if (showAll) {
      setShowAll(false);
      onRefresh(); // resets to active-only
      return;
    }
    setBusy("fleet_jobs");
    try {
      await invoke<JobRow[]>("fleet_jobs", { name: detail.name, all: true });
      setShowAll(true);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="fleet-detail">
      {/* Header */}
      <header className="fleet-detail__header">
        <div>
          <h2 className="fleet-detail__name">{detail.display_name}</h2>
          <p className="fleet-detail__goal">{detail.goal}</p>
          <span className="fleet-detail__status">{statusBadge(detail)}</span>
        </div>
        <div className="fleet-detail__actions">
          <button
            className="toolbar-btn toolbar-btn--primary"
            onClick={handleRun}
            disabled={busy !== null}
          >
            {t("fleet.run")}
          </button>
          {detail.stopped ? (
            <button onClick={() => call("fleet_start", { name: detail.name })} disabled={busy !== null}>
              {t("fleet.start")}
            </button>
          ) : (
            <button onClick={() => call("fleet_stop", { name: detail.name })} disabled={busy !== null}>
              {t("fleet.stop")}
            </button>
          )}
          <button onClick={handleExport} disabled={busy !== null}>{t("fleet.export")}</button>
          <button onClick={handleImport} disabled={busy !== null}>{t("fleet.import")}</button>
          <button className="toolbar-btn toolbar-btn--danger" onClick={handleDelete} disabled={busy !== null}>
            {t("fleet.delete")}
          </button>
        </div>
      </header>

      {/* Members */}
      <section className="fleet-detail__section">
        <h3>{t("fleet.members")}</h3>
        <ul className="fleet-members">
          {detail.members.map((m) => (
            <li key={m} className="fleet-members__row">
              <span>{m}</span>
              <button
                className="fleet-members__remove"
                onClick={() => call("fleet_remove_member", { name: detail.name, agent: m })}
                disabled={busy !== null}
              >
                {t("fleet.removeMember")}
              </button>
            </li>
          ))}
        </ul>
        <div className="fleet-members__add">
          <input
            value={addInput}
            onChange={(e) => setAddInput(e.target.value)}
            placeholder={t("fleet.addMember")}
            onKeyDown={(e) => e.key === "Enter" && handleAddMember()}
          />
          <button onClick={handleAddMember} disabled={busy !== null || !addInput.trim()}>
            +
          </button>
        </div>
      </section>

      {/* Send Job */}
      <section className="fleet-detail__section">
        <div className="fleet-send">
          <input
            value={sendInput}
            onChange={(e) => setSendInput(e.target.value)}
            placeholder={t("fleet.sendPlaceholder")}
            onKeyDown={(e) => e.key === "Enter" && handleSend()}
          />
          <button
            className="toolbar-btn toolbar-btn--primary"
            onClick={handleSend}
            disabled={busy !== null || !sendInput.trim()}
          >
            {t("fleet.send")}
          </button>
        </div>
      </section>

      {/* Jobs */}
      <section className="fleet-detail__section">
        <h3>{t("fleet.jobs")}</h3>
        {jobs.length === 0 ? (
          <p className="fleet-jobs__empty">—</p>
        ) : (
          <ul className="fleet-jobs">
            {jobs.map((j) => (
              <li key={j.id} className="fleet-jobs__row">
                <code className="fleet-jobs__id">{j.id.slice(0, 8)}</code>
                <span className={jobStatusClass(j.status)}>
                  {t(`fleet.status.${j.status}` as Parameters<typeof t>[0])}
                </span>
                <span className="fleet-jobs__text" title={j.text}>
                  {j.text.length > 60 ? j.text.slice(0, 59) + "…" : j.text}
                </span>
              </li>
            ))}
          </ul>
        )}
        <button className="fleet-jobs__show-all" onClick={handleShowAll}>
          {t("fleet.showAll")}
        </button>
      </section>
    </section>
  );
}
```

- [ ] **Step 2: TypeScript check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npx tsc --noEmit 2>&1 | head -30
```

Expected: no errors. If `t(\`fleet.status.${j.status}\`)` fails type inference, replace with `t(("fleet.status." + j.status) as Parameters<typeof t>[0])`.

- [ ] **Step 3: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx
git commit -m "feat(hub): FleetDetail — members, jobs, run/stop/send/export/import/delete actions"
```

---

### Task 6: FleetView + DashboardApp integration

**Files:**
- Create: `mur-hub-gui/ui/src/components/fleet/FleetView.tsx`
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx`

**Interfaces:**
- Consumes: `FleetRail`, `FleetDetail`, `FleetCreateModal` from previous tasks
- Consumes: `invoke("fleet_list")`, `invoke("fleet_detail")`, `invoke("fleet_jobs")`, `listen("fleet:run_done")`
- Produces: `<FleetView>` — wired into `DashboardApp` as the 4th surface

- [ ] **Step 1: Create FleetView.tsx**

```tsx
// mur-hub-gui/ui/src/components/fleet/FleetView.tsx
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../i18n";
import type { FleetSummary, FleetDetail as Detail, JobRow } from "./types";
import { FleetRail } from "./FleetRail";
import { FleetDetail } from "./FleetDetail";
import { FleetCreateModal } from "./FleetCreateModal";

function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

export function FleetView() {
  const t = useT();
  const [fleets, setFleets] = useState<FleetSummary[]>([]);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [showCreate, setShowCreate] = useState(false);
  const selectedRef = useRef<string | null>(null);

  useEffect(() => {
    selectedRef.current = selectedName;
  }, [selectedName]);

  async function loadList() {
    try {
      const rows = await invoke<FleetSummary[]>("fleet_list");
      setFleets(rows);
      // Auto-select first on initial load
      if (selectedRef.current === null && rows.length > 0) {
        setSelectedName(rows[0].name);
      }
    } catch (err) {
      showToast(String(err), 4000);
    }
  }

  async function loadDetail(name: string) {
    try {
      const [d, j] = await Promise.all([
        invoke<Detail>("fleet_detail", { name }),
        invoke<JobRow[]>("fleet_jobs", { name, all: false }),
      ]);
      if (selectedRef.current !== name) return; // stale
      setDetail(d);
      setJobs(j);
    } catch (err) {
      showToast(String(err), 4000);
    }
  }

  // Initial list load
  useEffect(() => {
    void loadList();
  }, []);

  // Load detail whenever selection changes
  useEffect(() => {
    if (selectedName) {
      void loadDetail(selectedName);
    } else {
      setDetail(null);
      setJobs([]);
    }
  }, [selectedName]);

  // Refresh jobs when a fleet run completes
  useEffect(() => {
    const unlisten = listen<{ name: string; ok: boolean }>(
      "fleet:run_done",
      (event) => {
        const { name, ok } = event.payload;
        showToast(ok ? t("fleet.runDone") : t("fleet.runFailed"), 3000);
        void loadList();
        if (selectedRef.current === name) void loadDetail(name);
      }
    );
    return () => { void unlisten.then((fn) => fn()); };
  }, []);

  function handleSelect(name: string) {
    setSelectedName(name);
  }

  function handleRefresh() {
    void loadList();
    if (selectedName) void loadDetail(selectedName);
  }

  function handleDelete() {
    setSelectedName(null);
    setDetail(null);
    setJobs([]);
    void loadList();
  }

  function handleCreated(name: string) {
    setShowCreate(false);
    void loadList().then(() => setSelectedName(name));
  }

  return (
    <div className="fleet-view">
      <FleetRail
        fleets={fleets}
        selectedName={selectedName}
        onSelect={handleSelect}
        onNew={() => setShowCreate(true)}
      />
      <main className="fleet-view__main">
        {detail ? (
          <FleetDetail
            detail={detail}
            jobs={jobs}
            onRefresh={handleRefresh}
            onDelete={handleDelete}
          />
        ) : (
          <div className="fleet-view__empty">
            <p>{t("fleet.empty")}</p>
          </div>
        )}
      </main>
      {showCreate && (
        <FleetCreateModal
          onCreated={handleCreated}
          onClose={() => setShowCreate(false)}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 2: Wire into DashboardApp.tsx**

In `mur-hub-gui/ui/src/components/DashboardApp.tsx`:

**2a.** Add the import near the top with the other view imports:
```tsx
import { FleetView } from "./fleet/FleetView";
```

**2b.** Find the surface state declaration (line ~393):
```tsx
const [surface, setSurface] = useState<"agents" | "chats" | "work">("agents");
```
Change to:
```tsx
const [surface, setSurface] = useState<"agents" | "chats" | "work" | "fleet">("agents");
```

**2c.** Find the `<nav className="surface-toggle dashboard__bar-nav">` block and add the 4th button after the Work button:
```tsx
<button
  className={surface === "fleet" ? "is-active" : ""}
  onClick={() => setSurface("fleet")}
>
  {t("fleet.tab")}
</button>
```

**2d.** Find the render block that conditionally shows `WorkView`/`ChatsView`/agent grid. It currently looks like:
```tsx
{surface === "work" ? (
  <WorkView agents={agents} />
) : surface === "chats" ? (
  <ChatsView ... />
) : (
  // agents grid
)}
```
Add the fleet branch:
```tsx
{surface === "fleet" ? (
  <FleetView />
) : surface === "work" ? (
  <WorkView agents={agents} />
) : surface === "chats" ? (
  <ChatsView ... />
) : (
  // agents grid — leave unchanged
)}
```

- [ ] **Step 3: TypeScript check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npx tsc --noEmit 2>&1 | head -30
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-hub-gui/ui/src/components/fleet/FleetView.tsx \
        mur-hub-gui/ui/src/components/DashboardApp.tsx
git commit -m "feat(hub): wire FleetView as 4th surface in DashboardApp"
```

---

### Task 7: Build .app and verify with Computer Use

**Files:** No new files — build and test only.

**Goal:** Confirm every user-visible feature from the spec works end-to-end.

- [ ] **Step 1: Build UI**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
npm run build 2>&1 | tail -20
```

Expected: `✓ built in Xs` with no errors.

- [ ] **Step 2: Build Tauri .app**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/src-tauri
MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download \
  cargo tauri build 2>&1 | tail -40
```

Expected: `Finished 1 bundle` pointing to a `.app`. Ignore exit code 1 from the updater-sign step (no signing key in dev) — the .app is still usable.

- [ ] **Step 3: Ad-hoc sign + install**

```bash
# Sign so macOS allows it to run
APP=$(find /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/src-tauri/target -name "MUR Hub.app" -not -path "*/deps/*" | head -1)
codesign --force --deep --sign - "$APP"
cp -R "$APP" /Applications/
```

- [ ] **Step 4: Create a test fleet via CLI**

```bash
mur fleet create test-fleet --members pm,qa --goal "Hub integration test"
```

- [ ] **Step 5: Launch Hub and verify with Computer Use**

Take a screenshot, then verify each of the following:

1. "Fleets" tab appears in the Hub nav bar (4th button after Agents/Chats/Activity)
2. Fleet tab shows `test-fleet` in the left rail with ● status
3. Clicking `test-fleet` shows: name, goal, members (pm, qa), action buttons
4. "New Fleet" button opens the create modal — fill in and submit → fleet appears in rail
5. Send Job input → type a job → press Enter → job appears in Jobs section as "queued"
6. Run button → `fleet.runStarted` toast appears
7. Stop button → status changes to ⏸; Start button reverses it
8. Export button → toast shows Desktop path (`~/Desktop/test-fleet.fleet`)
9. Import button → file picker opens; pick the exported `.fleet` → imported toast
10. Remove member button → member disappears from list
11. Add member input → type `dev` → click + → `dev` appears in members list
12. Delete button → confirm dialog → fleet removed from rail

- [ ] **Step 6: Fix any issues found, then final commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add -A
git commit -m "feat(hub): Hub Fleet surface — verified end-to-end"
```
