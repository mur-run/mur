# Hub Phase 2 Design: HITL Cards, Notification Budget, Memory Transparency

**Date:** 2026-06-08
**Builds on:** Hub Phase 1 (PR #367 — two-way chat + streaming), Hub UI polish (PR #369)

---

## Goals

Three features shipped in one PR:

1. **HITL action cards** — when the agent runtime's `pre_tool_use` hook returns `Decision::AskUser`, the agent pauses and an inline approval card appears in the Hub chat. User clicks Allow or Deny; runtime continues or reports failure.
2. **Notification budget** — expose `ProactiveConfig` (`daily_cap`, `quiet_hours`, `enabled`) in the Hub's DetailPanel Behavior tab. Backend already exists; this is pure UI.
3. **Memory transparency** — new Memory tab in DetailPanel showing the agent's `relationship.json` as editable cards (relationship type, formality, first-memory text) plus `sys_prompt.md` behind an Advanced toggle.

---

## Architecture

All three features follow the same Hub pattern established in Phase 1:

- Rust Tauri commands read/write agent files under `~/.mur/agents/<name>/`
- React components call `invoke()` to read on mount, `invoke()` to write on save
- Tauri events used for real-time push only (HITL: `hitl-approval-needed` event; countdown timer in React)

### New files

| File | Purpose |
|---|---|
| `mur-hub-gui/src-tauri/src/hitl.rs` | `agent_hitl_respond` Tauri command |
| `mur-hub-gui/src-tauri/src/notif.rs` | `agent_get_notif_config`, `agent_set_notif_config` |
| `mur-hub-gui/src-tauri/src/memory.rs` | `agent_get_memory`, `agent_set_memory` |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Inline approval card rendered in ChatTab |
| `mur-hub-gui/ui/src/components/MemoryTab.tsx` | Memory tab for DetailPanel |

### Modified files

| File | Change |
|---|---|
| `mur-agent-runtime/src/supervisor_runner.rs` | `pending_approvals` map; handle `Decision::AskUser` from hook chain; handle `tool/hitl_respond` method |
| `mur-agent-runtime/src/protocol/methods/` | New `tool/hitl_respond` JSON-RPC handler |
| `mur-core/src/a2a_dial.rs` | Extend streaming callback to deliver `tool/approval_needed` events |
| `mur-hub-gui/src-tauri/src/chat.rs` | Forward `tool/approval_needed` notification as Tauri event |
| `mur-hub-gui/src-tauri/src/lib.rs` | Register new commands and modules |
| `mur-hub-gui/ui/src/components/ChatTab.tsx` | Listen for `hitl-approval-needed`; render `HitlCard` inline |
| `mur-hub-gui/ui/src/components/DetailPanel.tsx` | Add Memory tab; add Notifications section to Behavior tab |
| `mur-hub-gui/ui/src/types.ts` | New types: `HitlRequest`, `NotifConfig`, `MemoryView`, `MemoryPatch` |

---

## Feature 1: HITL Action Cards

### Runtime side

When the hook chain's `pre_tool_use` returns `Decision::AskUser` (b0.rs already emits this; no change to b0.rs needed), the caller in `supervisor_runner.rs`:

1. Generate `hitl_id` (UUID v4).
2. Create `oneshot::channel::<bool>()`. Store the sender in `pending_approvals: Arc<DashMap<String, oneshot::Sender<bool>>>` keyed by `hitl_id`.
3. Emit a JSON-RPC notification on the agent socket:
   ```json
   {
     "jsonrpc": "2.0",
     "method": "tool/approval_needed",
     "params": {
       "hitl_id": "<uuid>",
       "tool_name": "<name>",
       "tool_input": { ... },
       "prompt": "<human-readable description>",
       "timeout_ms": 300000
     }
   }
   ```
4. `tokio::time::timeout(Duration::from_secs(300), receiver).await`:
   - `Ok(true)` → return `Decision::Allow`
   - `Ok(false)` → return `Decision::Deny { reason: "user denied" }`
   - `Err(_)` (timeout) → return `Decision::Deny { reason: "approval timed out" }`

**New JSON-RPC method `tool/hitl_respond`** (handled in the agent socket server):
```json
{ "hitl_id": "<uuid>", "allow": true }
```
Looks up `pending_approvals[hitl_id]`, sends on the oneshot. Returns `{}` on success, error if `hitl_id` unknown (already resolved/timed out).

**Timeout config:** stored as `hitl.timeout_secs: u32` (default `300`) in `profile.yaml`. New `HitlConfig` struct in `mur-common`. A value of `0` means wait indefinitely (expert mode; not exposed in UI v1).

### Hub side (chat.rs)

`dial_message_streaming` callback extended with a third case: when the notification type is `tool/approval_needed`, fire Tauri event `hitl-approval-needed` with the `HitlRequest` payload.

**New Tauri command `agent_hitl_respond(name, hitl_id, allow)`** in `hitl.rs`:
- Calls `dial_method(home, name, "tool/hitl_respond", json!({hitl_id, allow}), DialMode::RequireRunning)`
- Returns `Result<(), String>`

### Hub side (ChatTab.tsx)

On mount, listen for `hitl-approval-needed` events filtered to `agentName`. When received:

- Insert a `HitlCard` message into the messages list (role: `"hitl"`, not `"user"` or `"agent"`).
- `HitlCard` shows: tool name, tool input summary, countdown timer (5:00 counting down in 1s intervals via `setInterval`).
- Allow / Deny buttons invoke `agent_hitl_respond`.
- After response (or on timeout event): card updates to show outcome (`"Allowed"` / `"Denied — timed out"`). Buttons disabled.

**Timeout display:** The countdown is purely cosmetic in React (started when the card renders, `timeout_ms` from the event payload). The actual enforcement is in the runtime. If the user responds after timeout, `agent_hitl_respond` returns an error; the card shows "Too late — already timed out."

---

## Feature 2: Notification Budget

### Backend (notif.rs)

```rust
pub struct NotifConfig {
    pub enabled: bool,
    pub daily_cap: u8,
    pub quiet_hours_enabled: bool,
    pub quiet_start: String,   // "HH:MM", e.g. "22:00"
    pub quiet_end: String,     // "HH:MM", e.g. "08:00"
}

pub struct NotifPatch {
    pub enabled: Option<bool>,
    pub daily_cap: Option<u8>,
    pub quiet_hours_enabled: Option<bool>,
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
}

#[tauri::command]
pub fn agent_get_notif_config(name: String) -> Result<NotifConfig, String>

#[tauri::command]
pub fn agent_set_notif_config(name: String, patch: NotifPatch) -> Result<NotifConfig, String>
```

Reads/writes `companion.proactive` in `profile.yaml`. The `quiet_hours_enabled` flag maps to whether `quiet_hours: Option<QuietHours>` is `Some` or `None`. Default times when first enabled: `{ start: "22:00", end: "08:00" }`.

### UI (DetailPanel Behavior tab)

New "Notifications" section appended after existing Behavior presets:

- **Proactive messages** toggle → `enabled`
- **Daily cap** slider (0–20, integer) → `daily_cap`. Label shows current value numerically. 0 = effectively silent.
- **Quiet hours** toggle → `quiet_hours_enabled`
- **From / Until** time pickers (HTML `<input type="time">`) → `quiet_start` / `quiet_end`. Greyed out when quiet hours toggle is off.
- Auto-save on every change (no explicit Save button needed — each control fires `agent_set_notif_config` with a single-field patch).

---

## Feature 3: Memory Transparency

### Backend (memory.rs)

```rust
pub struct MemoryView {
    pub relationship: String,     // "friend" | "coach" | "accountability_buddy" | "mentor"
    pub formality: String,        // "casual" | "neutral" | "formal"
    pub first_memory: String,     // free text; empty string if none
    pub sys_prompt: String,       // full contents of sys_prompt.md
}

pub struct MemoryPatch {
    pub relationship: Option<String>,
    pub formality: Option<String>,
    pub first_memory: Option<String>,
    pub sys_prompt: Option<String>,  // only set when user explicitly edits advanced
}

#[tauri::command]
pub fn agent_get_memory(name: String) -> Result<MemoryView, String>

#[tauri::command]
pub fn agent_set_memory(name: String, patch: MemoryPatch) -> Result<MemoryView, String>

#[tauri::command]
pub fn agent_reset_sys_prompt(name: String) -> Result<String, String>
```

`agent_get_memory` reads:
- `~/.mur/agents/<name>/companion/relationship.json` for relationship/formality/first_memory
- `~/.mur/agents/<name>/sys_prompt.md` for sys_prompt

`agent_set_memory` writes:
- Relationship fields → serialize back to `relationship.json`
- `sys_prompt` (if present) → overwrite `sys_prompt.md`

`agent_reset_sys_prompt` → rebuilds `sys_prompt.md` from the persona fields in `profile.yaml` using the same deterministic Handlebars/mustache template used at `mur agent create` time (no LLM call; template lives in `mur-common` or `mur-core`). Overwrites `sys_prompt.md`, returns new content.

### UI (MemoryTab.tsx — new tab in DetailPanel)

**Relationship section** (always visible):
- Dropdown: relationship type (Friend / Coach / Accountability buddy / Mentor)
- Dropdown: formality (Casual / Neutral / Formal)
- Textarea: "What it knows about you" (`first_memory` text). Placeholder: "Nothing recorded yet."
- Save button — writes all three fields via `agent_set_memory`.

**System Prompt section** (collapsible, collapsed by default):
- Header row: "System Prompt (Advanced)" + collapse arrow
- When expanded: `<textarea>` with full sys_prompt content (monospace font, min 8 rows)
- Buttons: Save (writes via `agent_set_memory`) + Reset to default (calls `agent_reset_sys_prompt` with confirmation dialog)
- Warning banner: "Editing the system prompt directly can break this agent's behaviour."

---

## Error Handling

- All Tauri commands return `Result<T, String>`. Errors shown as inline error text below the relevant control (same pattern as existing DetailPanel).
- `agent_hitl_respond` called after timeout: show "Too late — request already timed out" in the card.
- `relationship.json` missing (agent not companion-initialised): `agent_get_memory` returns empty strings for relationship fields; Memory tab shows a "Not initialised" notice with no editable fields (editing relationship state for a non-companion agent is out of scope for v1).

---

## Testing

- Unit: `agent_get_notif_config` / `agent_set_notif_config` round-trip with a temp `profile.yaml`.
- Unit: `agent_get_memory` / `agent_set_memory` round-trip with temp files.
- Unit: HITL oneshot timeout fires `Decision::Deny` after configured duration.
- Unit: `tool/hitl_respond` with unknown `hitl_id` returns JSON-RPC error (not a panic).
- Manual: end-to-end HITL flow — start an agent, trigger a tool that hits AskUser, verify card appears, click Allow, verify agent continues.

---

## Out of Scope (v1)

- HITL outside of an active chat session (no persistent socket subscription yet — Phase 3).
- Pattern cache (`patterns_cache/`) visibility — deferred; not user-facing "memory".
- Editing `relationship.json` for non-companion agents.
- Configuring HITL timeout in the Hub UI (hardcoded 5 min in v1; `hitl.timeout_secs` in schema for future).
- "Allow always" shortcut from HITL card directly to grant store (deferred — requires GrantStore write from Hub side).
