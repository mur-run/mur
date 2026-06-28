# MUR Hub — Fleet Surface Design

**Date:** 2026-06-29  
**Status:** Approved  

## Goal

Add a fourth "Fleets" top-level surface to the MUR Hub desktop app so users can list, manage, and operate fleets without leaving the GUI.

## Non-goals

- Loop/cron configuration editor (use CLI: `mur fleet create --loop ...`)
- Real-time streaming of `fleet run` output (job status is enough for v1)
- Budget/deadline fields in the create form (use CLI)
- Team-shared fleet import/export (Phase A feature already in CLI; Hub wraps it)

---

## Architecture

Three layers, following the existing `work.rs` / `WorkView.tsx` pattern.

### 1. Tauri backend — `mur-hub-gui/src-tauri/src/fleet.rs`

New module registered in `lib.rs`. All commands are thin shims over existing `mur-core::cmd::fleet::*` functions — no new business logic in the Hub.

| Command | Signature | Wraps |
|---|---|---|
| `fleet_list` | `() → Vec<FleetSummary>` | `store::list_fleets` + `control::is_stopped` + `jobs::list_jobs` |
| `fleet_detail` | `(name: String) → FleetDetail` | `store::load_fleet` |
| `fleet_create` | `(name, goal, members, router?) → ()` | `create::cmd_fleet_create` |
| `fleet_delete` | `(name) → ()` | `delete::cmd_fleet_delete` |
| `fleet_start` | `(name) → ()` | `control::cmd_fleet_start` |
| `fleet_stop` | `(name) → ()` | `control::cmd_fleet_stop` |
| `fleet_run` | `(name) → String` | `spawn_blocking(run::cmd_fleet_run)` → emits `fleet:run_done` |
| `fleet_send` | `(name, text) → String` | `jobs::enqueue_job` |
| `fleet_jobs` | `(name, all: bool) → Vec<JobRow>` | `jobs::list_jobs` |
| `fleet_add_member` | `(name, agent) → ()` | `roster::cmd_fleet_add` |
| `fleet_remove_member` | `(name, agent) → ()` | `roster::cmd_fleet_remove` |
| `fleet_export` | `(name) → String` | `export::cmd_fleet_export` → returns output path |
| `fleet_import` | `(path: String) → String` | `import::cmd_fleet_import` → returns fleet name |

**Serializable types (Rust → TypeScript):**

```rust
#[derive(Serialize)]
pub struct FleetSummary {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub member_count: usize,
    pub active_jobs: usize,   // non-terminal job count
    pub stopped: bool,
    pub running: bool,        // any job has status == Running
}

#[derive(Serialize)]
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
}

#[derive(Serialize)]
pub struct JobRow {
    pub id: String,
    pub text: String,
    pub status: String,              // "queued" | "running" | "done" | "failed" | "canceled"
    pub created_at: String,
    pub finished_at: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}
```

**Async fleet run:**  
`fleet_run` is the only long-running command. It `tokio::task::spawn_blocking`s `cmd_fleet_run`, then emits a Tauri event `fleet:run_done { name, job_id, ok: bool }` on completion. The frontend listens for this event to refresh job status.

All other commands are synchronous reads/writes and return immediately.

### 2. UI components — `mur-hub-gui/ui/src/components/fleet/`

```
FleetView.tsx          top-level surface: state + data fetching
FleetRail.tsx          left rail: fleet list rows + "New Fleet" button
FleetDetail.tsx        right panel: header, members, send-job, jobs list
FleetCreateModal.tsx   create fleet form (name, goal, members)
```

**FleetView** owns all state and passes callbacks down. No context needed.

**Data flow:**
```
FleetView
  on mount           → fleet_list → setFleets
  on fleet select    → fleet_detail + fleet_jobs(all=false) → setDetail, setJobs
  listen("fleet:run_done") → fleet_jobs → setJobs (refresh active jobs)

  FleetRail(fleets, selectedName, onSelect, onNew)
  FleetDetail(detail, jobs, {
    onRun, onSend, onStop, onStart,
    onAddMember, onRemoveMember,
    onExport, onImport, onDelete,
    onLoadAllJobs,   // fleet_jobs(all=true)
  })
```

No polling. Refresh only on: user action or `fleet:run_done` event.

**FleetDetail sections:**

1. **Header** — display_name, goal (truncated), status badge (▶ idle / ⏸ stopped / ● running), action row: [▶ Run] [⏸ Stop / ▶ Start] [↑ Export] [↓ Import] [🗑 Delete]
2. **Members** — list of member names with [✕ Remove] per row + [+ Add member] input
3. **Send Job** — text input + [→] button → `fleet_send`; shows returned job_id in toast
4. **Jobs** — table: id (first 8 chars), status badge, text (truncated), timestamp; [Show all] toggle → `fleet_jobs(all=true)`

**FleetCreateModal** fields:
- Name (slug, required)
- Goal (text, required)  
- Members (comma-separated agent names, required)
- Router (optional, defaults to concierge)

### 3. DashboardApp.tsx changes

- Extend surface type: `"agents" | "chats" | "work" | "fleet"`
- Add 4th nav button: `{t("fleet.tab")}`
- Add `FleetView` import and render branch
- Search bar hidden on fleet surface (fleet has its own rail filter)

---

## i18n Keys

Added to both `en.ts` and `zh-TW.ts`:

```
fleet.tab            "Fleets"
fleet.empty          "No fleets yet. Create one to get started."
fleet.new            "New Fleet"
fleet.goal           "Goal"
fleet.router         "Router"
fleet.members        "Members"
fleet.addMember      "Add member…"
fleet.removeMember   "Remove"
fleet.run            "Run"
fleet.send           "Send Job"
fleet.sendPlaceholder "Describe the job…"
fleet.stop           "Stop"
fleet.start          "Start"
fleet.export         "Export"
fleet.import         "Import"
fleet.delete         "Delete"
fleet.confirmDelete  "Delete fleet \"{name}\"? This cannot be undone."
fleet.jobs           "Jobs"
fleet.showAll        "Show all"
fleet.status.queued  "queued"
fleet.status.running "running"
fleet.status.done    "done"
fleet.status.failed  "failed"
fleet.status.canceled "canceled"
fleet.runStarted     "Fleet run started"
fleet.runDone        "Fleet run completed"
fleet.runFailed      "Fleet run failed"
fleet.exported       "Exported to {path}"
fleet.imported       "Imported fleet \"{name}\""
fleet.create.name    "Fleet name"
fleet.create.goal    "Goal"
fleet.create.members "Members (comma-separated)"
fleet.create.router  "Router (optional)"
fleet.create.submit  "Create Fleet"
```

---

## Error handling

- All `invoke` calls wrapped in `.catch` → `showToast(err, 4000)` (same pattern as rest of Hub)
- Delete shows a browser `confirm()` dialog before invoking `fleet_delete`
- Remove member: no confirm (idempotent, roster shows immediately)
- `fleet_run` while stopped: Rust returns error → toast; UI does not disable the button (server-side is the authority)
- Import uses `tauri_plugin_dialog::open()` with `.fleet` filter; cancel = no-op

---

## Files changed

| File | Change |
|---|---|
| `src-tauri/src/fleet.rs` | New: all Tauri fleet commands |
| `src-tauri/src/lib.rs` | Add `pub mod fleet;` + register commands |
| `ui/src/components/fleet/FleetView.tsx` | New |
| `ui/src/components/fleet/FleetRail.tsx` | New |
| `ui/src/components/fleet/FleetDetail.tsx` | New |
| `ui/src/components/fleet/FleetCreateModal.tsx` | New |
| `ui/src/components/DashboardApp.tsx` | Add fleet surface + nav button |
| `ui/src/i18n/en.ts` | Add fleet keys |
| `ui/src/i18n/zh-TW.ts` | Add fleet keys (Traditional Chinese) |

---

## Testing

Build the Hub `.app` locally, verify with Computer Use:

1. Fleet tab appears in nav bar
2. `mur fleet create test-fleet --members pm,qa --goal "test"` → Hub shows it
3. Create fleet via Hub modal → appears in rail
4. Select fleet → detail panel shows members + empty jobs
5. Send a job → job appears in jobs table as "queued"
6. Run fleet → status changes to "running", `fleet:run_done` fires, jobs refresh
7. Stop/Start toggle → status badge updates
8. Add/remove member → members list updates
9. Export → toast with file path
10. Import → fleet appears in rail
11. Delete → confirm dialog → fleet removed from rail
