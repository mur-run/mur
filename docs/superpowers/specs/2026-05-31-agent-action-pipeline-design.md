# Agent Action Pipeline — Design Spec

> **Date**: 2026-05-31
> **Status**: Draft
> **Scope**: Agent platform enhancement — file notification + deletion safety + task queue (Block A)

## Overview

Unify three agent UX features into a single **Action Pipeline**: file/email notification with voice commands (A1), deletion confirmation with configurable delay (A2), and a user-facing task queue with pause/cancel (A3).

These are not independent features — they are phases of the same value chain: **ingest → queue → execute → notify**. The pipeline design ensures they compose naturally rather than duplicating infrastructure.

### Research Foundation

Design validated against 2025–2026 industry best practices:

- **File handling UX**: Multi-surface notification (badge + toast + OS notify), review-before-execute, structured notification metadata. Sources: [Smashing Magazine 2026](https://shop.smashingmagazine.com/2026/05/practical-interface-patterns-ai-transparency/), [Jacar](https://jacar.es/en/diseno-ui-agentes/), [Courier 2026](https://www.courier.com/blog/your-notifications-now-have-two-audiences-humans-and-ai-agents/)
- **Deletion safety**: Archive-don't-delete (Trash), two-phase protocol (never propose+execute same-turn), schema-level enforcement (batch limits, no wildcards), deterministic policy enforcement. Sources: [Conseca (Google, HOTOS '25)](https://sigops.org/s/conferences/hotos/2025/papers/hotos25-100.pdf), [agent-safe-delete](https://github.com/ckckck/agent-safe-delete), [Replit Snapshot Engine](https://blog.replit.com/inside-replits-snapshot-engine), [Safe File Deletion MCP](https://www.npmjs.com/package/@mizunashi_mana/safe-file-deletion-mcp)
- **Task queue UX**: Three-state execution row (pause/cancel/resume), dynamic checklist over progress bar, checkpoint-based pausing, AG-UI event model. Sources: [Morae (UIST '25)](https://ar5iv.labs.arxiv.org/html/2508.21456), [AgentField #243](https://github.com/Agent-Field/agentfield/issues/243), [AG-UI Protocol](https://futureagi.com/blog/agentic-ux-webinar-2025/#hero), [Autopoiesis #614](https://github.com/DavSimFel/autopoiesis/issues/614)
- **Capability declaration**: Declarative manifest with scoped permissions, mime-type filtering, structured action definitions. Sources: [Agents.md](https://www.remio.ai/post/what-is-agents-md-a-complete-guide-to-the-new-ai-coding-agent-standard-in-2025), [OpenPort Protocol](https://ar5iv.labs.arxiv.org/html/2602.20196), [Claude Code Permission Model](https://skywork.ai/blog/permission-model-claude-code-vs-code-jetbrains-cli/)

## Architecture

### Pipeline Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                    Agent Action Pipeline                          │
│                                                                  │
│  Phase 1: INGEST       Phase 2: QUEUE     Phase 3: EXEC   Phase 4: NOTIFY
│  ┌──────────────┐    ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  │ PendingStore │ ─→ │  TaskQueue   │→ │  Supervisor  │→ │  Aggregator  │
│  │              │    │              │  │  ┌──────────┐ │  │              │
│  │ file dropped │    │ prioritise   │  │  │TrashGuard│ │  │ badge update │
│  │ paste/clip   │    │ throttle     │  │  └──────────┘ │  │ OS notify    │
│  │ share URL    │    │ state mach.  │  │  dispatch     │  │ chat output  │
│  └──────────────┘    └──────────────┘  └──────────────┘  └──────────────┘
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │               Shared Ledger (JSONL)                       │    │
│  │  <agent_home>/actions/ledger/YYYY-MM-DD.jsonl            │    │
│  └──────────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────┘
```

### Design Decision: Unified Pipeline over Per-Feature Modules

Three architectural approaches were evaluated:

| Approach | Integration | Code volume | Extensibility | Chosen |
|----------|-------------|-------------|---------------|--------|
| Extend existing systems independently | Poor | Minimal | Low | |
| New subsystem per feature | Manual coordination | Highest | High | |
| **Unified Action Pipeline** | **Native** | **Medium** | **Highest** | ✅ |

The pipeline was chosen because A1, A2, A3 are phases of the same chain (ingest → queue → execute → notify), share a common ledger, and a shared state model eliminates duplication. Future features (agent roles, trigger modes, flow editor) extend by adding phases or hooks.

## Data Model

### Core Types

```rust
// ── Phase 1: Ingestion ──

struct PendingItem {
    id: Uuid,
    source: ItemSource,
    files: Vec<PendingFile>,
    created_at: DateTime<Utc>,
    status: PendingStatus,
}

enum ItemSource {
    DragDrop { paths: Vec<PathBuf> },
    Clipboard { mime_type: String },
    ShareUrl { url: String, kind: ShareKind },
}

struct PendingFile {
    path: PathBuf,
    mime_type: String,
    size_bytes: u64,
    thumbnail_path: Option<PathBuf>,
}

enum PendingStatus {
    AwaitingSelection,
    Selected { action_id: String },
    Expired,
}

// ── Phase 2: Task Queue ──

struct Task {
    id: Uuid,
    pending_item_id: Uuid,
    action: Action,
    state: TaskState,
    steps: Vec<TaskStep>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    timeout_seconds: u32,
}

struct Action {
    id: String,                   // matches capabilities.file_actions[].id
    label: String,
    user_prompt: Option<String>,  // filled for ask_me
}

enum TaskState {
    Queued,
    Running,
    Paused,
    Completed { outcome: TaskOutcome },
    Cancelled,
}

enum TaskOutcome {
    Success { outputs: Vec<ActionOutput> },
    PartialSuccess { succeeded: u32, failed: u32 },
    Failed { error: String },
}

struct ActionOutput {
    kind: OutputKind,             // file | chat_message | both
    file_path: Option<PathBuf>,
    chat_content: Option<String>,
}

struct TaskStep {
    index: u32,
    label: String,                // agent-defined: "Reading file..."
    state: StepState,             // pending | running | done | failed
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

// ── Phase 3: Deletion Safety ──

struct TrashEntry {
    id: Uuid,
    original_path: PathBuf,
    trash_path: PathBuf,          // <agent_home>/trash/{timestamp}_{filename}
    file_size_bytes: u64,
    created_by_task_id: Uuid,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,    // created_at + deletion.ttl_minutes
    status: TrashStatus,
    restore_metadata: RestoreMeta,
}

enum TrashStatus {
    Pending,
    Restored,
    PermDeleted,
    AutoDeleted,
}

struct RestoreMeta {
    owner_uid: u32,
    owner_gid: u32,
    permissions: u32,
    // extended attributes as needed
}

// ── Shared Ledger Events ──

enum ActionEvent {
    ItemIngested       { item: PendingItem },
    ItemSelected       { item_id: Uuid, action: Action },
    ItemExpired        { item_id: Uuid },
    TaskEnqueued       { task: Task },
    TaskStarted        { task_id: Uuid },
    TaskStepUpdated    { task_id: Uuid, step: TaskStep },
    TaskPaused         { task_id: Uuid, reason: String },
    TaskResumed        { task_id: Uuid },
    TaskCompleted      { task_id: Uuid, outcome: TaskOutcome },
    TaskCancelled      { task_id: Uuid },
    TrashCreated       { entry: TrashEntry },
    TrashRestored      { entry_id: Uuid },
    TrashPermDeleted   { entry_id: Uuid },
    TrashAutoDeleted   { entry_id: Uuid },
}
```

### Disk Layout

```
<agent_home>/
├── actions/
│   ├── ledger/
│   │   └── YYYY-MM-DD.jsonl     ← append-only event log
│   └── pending.json              ← current pending snapshot (rebuildable from ledger)
├── trash/
│   └── {timestamp}_{filename}/   ← trashed files
├── companion/                    ← existing
├── profile.yaml                  ← existing + new fields
└── ...
```

### Profile Schema Additions

```yaml
capabilities:
  file_actions:                   # A1: declarative action list
    - id: summarize
      label:
        en: "Summarize"
        zh-TW: "摘要"
      description: "Extract key points from the file"
      mime_types: [text/*, application/pdf, application/msword]
    - id: translate
      label:
        en: "Translate"
        zh-TW: "翻譯"
      mime_types: [text/*, application/pdf]
    - id: ask_me                  # reserved: always last, skips mime check
      label:
        en: "Ask me anything..."
        zh-TW: "自由指令..."

deletion:                          # A2: deletion safety
  trash_enabled: true
  trash_ttl_minutes: 10
  trash_max_mb: 1024
  trusted_paths: []                # paths that bypass trash

queue:                             # A3: queue settings
  max_concurrent: 3
  default_timeout_minutes: 30
  pending_item_ttl_minutes: 60     # expiry for unhandled pending items
```

### Action Buttons from Capabilities

Action buttons rendered in the selection UI are sourced from `profile.yaml → capabilities.file_actions`, filtered by the intersecting MIME types of the selected files. The `ask_me` action always appears last and accepts any file type.

**Future extension points** (not in v1): `icon`, `shortcut_key`, `requires_confirmation`, `output_kind`, MCP tool binding.

## Phase 1: INGEST — File Intake & Selection

### Flow

```
User action (drop / share / paste)
  → PendingStore::ingest()
    → MIME detect via magic bytes
    → Dedup: same path within 5 seconds = same batch
    → Merge: new files within 5 seconds of existing PendingItem = extend batch
    → Append ActionEvent::ItemIngested to ledger
    → Write pending.json snapshot
    → GUI check:
        Foreground → emit pending:updated (show selection UI directly)
        Background → OS notification + increment floating badge
                     User clicks notification/badge → GUI foreground → selection UI
```

### Selection UI

```
┌─────────────────────────────────────────────────────────┐
│  📄 Pending (3)                                         │
│                                                         │
│  ☑️ paper1.pdf  (245 KB)                               │
│  ☑️ paper2.pdf  (1.2 MB)                               │
│  ☑️ paper3.pdf  (890 KB)                               │
│                                                         │
│  What should I do?                                      │
│  ┌──────┐ ┌──────┐ ┌──────┐ ┌────────┐               │
│  │ 📝   │ │ 🌐   │ │ 📂   │ │ 🎤     │               │
│  │Summarize│Translate│Categorize│Ask... │               │
│  └──────┘ └──────┘ └──────┘ └────────┘               │
│                                                         │
│  [Cancel]                              [Run Selected]    │
└─────────────────────────────────────────────────────────┘
```

### Voice Flow

1. User taps 🎤 → STT via whisper.cpp
2. Transcribed text fills the free-input field (editable for correction)
3. User confirms → executes as `ask_me` action

### Merge & Expiry Rules

- **Merge window**: 5 seconds. New files arriving within 5s of the last file in a PendingItem are merged into the same batch (badge count increases).
- **Expiry**: `pending_item_ttl_minutes` (default 60). Unhandled items auto-expire; badge decremented.
- **No-action agent**: If the agent has no `file_actions` defined (or only `ask_me`), only the free-input field is shown. A hint appears: "This agent has no predefined actions for this file type."

### Notification Metadata

Following [Courier 2026 dual-audience guidance](https://www.courier.com/blog/your-notifications-now-have-two-audiences-humans-and-ai-agents/), notifications include structured metadata:

```json
{
  "event_type": "pending_item_ingested",
  "item_id": "<uuid>",
  "file_count": 3,
  "mime_types": ["application/pdf"],
  "urgency": "low",
  "human_readable": {
    "title": "3 files received",
    "body": "paper1.pdf, paper2.pdf, paper3.pdf"
  }
}
```

## Phase 2: QUEUE — Task Management

### Flow

```
ItemSelected from Phase 1
  → TaskQueue::enqueue(action, files)
    → Create Task { state: Queued, ... }
    → Append TaskEnqueued
    → Check max_concurrent:
        Under limit → TaskStarted → Phase 3
        At limit → stays Queued, dequeued when slot opens
    → Update GUI via bridge
```

### Queue Panel UI

```
┌─────────────────────────────────────────┐
│  📋 Tasks                               │
│                                         │
│  ⏳ Waiting (2)                          │
│  ├─ 📝 Summarize paper1-3.pdf           │
│  │   [Cancel]                           │
│  └─ 🌐 Translate contract.docx          │
│      [Cancel]                           │
│                                         │
│  🔄 Running (1)                          │
│  └─ 📂 Categorize photos (42)            │
│      ▸ Scanning files... ✓              │
│      ▸ Analyzing content... ⏳          │
│      ▸ Moving files...                  │
│      [Pause] [Cancel]                   │
│                                         │
│  ✅ Completed (12)  [Clear]              │
│  ❌ Failed (1)     [Retry All]          │
└─────────────────────────────────────────┘
```

### Pause Semantics

- **Checkpoint mode**: Agent completes the current atomic step before pausing.
- **Force timeout**: If no checkpoint reached within 30 seconds, force-interrupt.
- Tasks paused are written to ledger; survive agent restart.

### Cancel Semantics

- Cancel cascades: aborting a task aborts all sub-operations.
- **Cleanup**: Agent-created temp files are deleted (do NOT go through trash).
- **Preservation**: Original user files are never touched by cancellation.

### Step Reporting

Agent reports steps via `TaskStepUpdated` events. Steps are agent-defined — it decides what level of detail to expose. The UI renders them as a Dynamic Checklist (validated by [Smashing Magazine 2026](https://shop.smashingmagazine.com/2026/05/practical-interface-patterns-ai-transparency/)).

## Phase 3: EXEC — Execution & Deletion Safety

### TrashGuard Interception

```
Every tool call passes through TrashGuard::intercept():

1. DETECT destructive patterns:
   - Shell: rm, unlink, delete
   - Shell: mv to /dev/null
   - Python: os.remove, os.unlink, shutil.rmtree
   - MCP: delete_file tool invocations
   - A2A: delete requests to other agents
   - If path matches trusted_paths → allow (no trash)

2. SCHEMA-LEVEL enforcement:
   - Batch size ≤ 10 files per operation
   - No wildcards (*, ?) in paths
   - Path within allowed scope
   - Reject with clear error if violation

3. REWRITE destructive operations:
   rm <path> → mv <path> <agent_home>/trash/{timestamp}_{filename}/

4. CREATE TrashEntry with metadata:
   - Original path, trash path, file size
   - RestoreMetadata (owner, perms, xattrs)
   - expires_at = created_at + deletion.ttl_minutes

5. APPEND TrashCreated to ledger

6. NOTIFY Phase 4 (deletion notifications are independent and urgent)
```

### Two-Phase Protocol

Following the [Cursor/Gemini incident lesson](https://forum.cursor.com/t/gemini-deletes-destroys-code-despite-memories-forbidding-these-actions/135867): destructive actions are **never** proposed and executed in the same turn.

- Turn 1: Preview what would be deleted
- Await user confirmation
- Turn 2: Execute only after explicit consent

This is enforced at the TrashGuard level: the first time a task triggers a destructive operation, execution pauses and waits for user acknowledgment.

### Trash Timer

- Background tick every 30 seconds scans for `TrashEntry` where `expires_at < now()` and `status == Pending`
- Expired entries → `TrashAutoDeleted` → files permanently removed
- Individual timers per entry (not a single global timer)

### Trash Capacity

- Total trash size tracked. If new entry would exceed `trash_max_mb` → reject with notification "Trash full. Please empty trash before new deletions."
- No automatic eviction (FIFO or otherwise) — user must explicitly manage.

### Trusted Paths

- Exact string match only; symlinks are NOT resolved
- `~/Downloads/tmp/` as symlink → does NOT match → trash still applies
- Design rationale: prevents agent from exploiting symlinks to bypass protection

### Coverage Scope

| Path | Intercepted? | Method |
|------|-------------|--------|
| Shell `rm` | ✅ | Tool-call pattern matching |
| Python `os.remove` etc. | ✅ | Same shell tool-call path |
| MCP `delete_file` | ✅ | MCP handler layer |
| A2A delete request | ✅ | Supervisor dispatch |
| Direct syscall from C extension | ❌ | Out of scope; requires OS sandbox (Phase 2) |

## Phase 4: NOTIFY — Result Delivery

### Aggregation

- Same PendingItem results → 1 OS notification
- Format: "Agent X completed: 47 succeeded, 3 failed"
- Deletion notifications are independent (more urgent than completion)
- Badge number = pending items + running tasks (completed/failed excluded)

### Trash Notification UI

```
┌─────────────────────────────────────────┐
│  ⚠️ Agent X wants to delete 3 files      │
│                                         │
│  📄 /tmp/paper1_ocr.pdf                 │
│  📄 /tmp/paper2_ocr.pdf                 │
│  📄 /tmp/paper3_ocr.pdf                 │
│                                         │
│  Will be permanently deleted in 10 min  │
│  [Delete Now] [Restore All] [View Trash]│
└─────────────────────────────────────────┘
```

### Output Routing

Each `ActionOutput` carries a `kind` field:
- `file` → write result to disk (alongside or near original)
- `chat_message` → append to agent chat window
- `both` → do both

The agent decides which based on the action context.

### Audit Trail

Every completed task retains a "Show Work" link that replays:
- Decision logic (which action was chosen, why)
- Steps taken (from TaskStep ledger)
- Files affected (created, modified, trashed)
- Before/after diff previews where applicable

Following the [Smashing Magazine audit trail pattern](https://shop.smashingmagazine.com/2026/05/practical-interface-patterns-ai-transparency/): the presence of the link signals the system stands behind its work, even if rarely clicked.

## CLI Surface

```
# Phase 1
mur agent pending <name>              # List pending items
mur agent pending <name> --act <id>   # Execute action on pending item

# Phase 2
mur agent queue <name>                # List task queue
mur agent queue <name> --pause <id>   # Pause task
mur agent queue <name> --cancel <id>  # Cancel task
mur agent queue <name> --retry <id>   # Retry failed task

# Phase 3 (A2)
mur agent trash <name>                # List trash contents
mur agent trash <name> --restore <id> # Restore file from trash
mur agent trash <name> --empty        # Permanently empty trash
mur agent trash <name> --now <id>     # Immediately delete specific item
```

## Implementation Plan

### Phase Order & Dependencies

```
Phase 0: Shared Infrastructure
  → Phase 1: INGEST (A1 front half)
    → Phase 2: QUEUE (A3)
      → Phase 3: EXEC (A2 + A1 back half)
        → Phase 4: NOTIFY (A1 back half)
```

Each phase builds on the previous. Phase 1 can be delivered standalone (files in → notify → select action). Phase 2 adds queue. Phase 3 adds safety. Phase 4 closes the loop.

### File Structure

**mur-core/src/action_pipeline/** (8 files, ~2050 lines total, all under 800-line limit):

```
mod.rs       ← ~120 lines: public API, Pipeline struct
state.rs     ← ~350 lines: all types + serde
ledger.rs    ← ~200 lines: JSONL read/write (reuses companion pattern)
ingest.rs    ← ~300 lines: PendingStore, MIME detect, dedup, merge
queue.rs     ← ~350 lines: TaskQueue, state machine, concurrency
guard.rs     ← ~400 lines: TrashGuard, pattern matching, schema checks
notify.rs    ← ~250 lines: Aggregator, notification formatting
error.rs     ← ~80 lines:  error types
```

**mur-agent-gui/ui/src/** (new React components):

```
pending/     ← PendingPanel, FloatingBadge, FileChecklist, ActionButtons,
               VoiceInput, usePendingBridge, api, types
queue/       ← QueuePanel, TaskRow, StepList, useQueueBridge, api, types
trash/       ← TrashPanel, TrashRow, useTrashBridge, api, types
```

### Crate Impact

| Crate | New | Modified |
|-------|-----|----------|
| **mur-common** | `src/action.rs` (ActionEvent enum) | — |
| **mur-core** | `src/action_pipeline/` (8 files) | `profile.yaml` schema, CLI dispatch |
| **mur-agent-runtime** | `src/action_pipeline/` (step reporting), `trash_watcher.rs` | `supervisor.rs` (TrashGuard hook) |
| **mur-agent-gui** | `pending/` `queue/` `trash/` (~18 files) | `CompanionBridge` (add action event), `App.tsx` (new tabs) |
| **mur-daemon** | — | Optional: relocate trash timer here for long-running |

## Testing Strategy

### Unit Tests

- **state.rs**: Serialization round-trips for all types
- **ledger.rs**: Append + scan_days + state rebuild from ledger
- **ingest.rs**: MIME detect, dedup window (5s), merge window (5s), expiry (TTL exceeded)
- **queue.rs**: Enqueue below/at capacity, state machine transitions, pause at checkpoint (<30s), force-pause after timeout (≥30s), cancel cleanup, ledger rebuild (crash recovery)
- **guard.rs**: Detect all destructive patterns (shell, Python, MCP, A2A), batch size reject, wildcard reject, trusted_paths bypass, rewrite rm→mv, TrashEntry expiry, auto-delete, capacity limit, restore, symlink-in-trusted-paths edge case
- **notify.rs**: Same-batch aggregation, independent deletion notification, badge count correctness

### E2E Tests

- CLI: `mur agent pending` → list, act
- CLI: `mur agent queue` → pause, cancel, retry
- CLI: `mur agent trash` → restore, empty, now
- GUI: OS notification when backgrounded
- GUI: Badge updates on all state transitions
- GUI: File drop → notification → selection → execution flow

## Edge Cases Covered

| Edge Case | Resolution |
|-----------|-----------|
| Files dropped during active selection | Merge into existing batch (5s window) |
| Agent has no matching action for file type | Show only ask_me + hint |
| Voice STT error | Show transcription in editable field; user corrects |
| Agent crashes during task | Rebuild queue state from ledger on restart |
| Trash capacity exceeded | Reject new deletions; notify user to empty |
| Symlink in trusted_paths | Not resolved; trash still applies |
| 50 files in one batch | 1 aggregated notification |
| Task timeout | Mark failed; retain for retry |
| Pending item unhandled past TTL | Auto-expire; decrement badge |

## References

- [Smashing Magazine — Practical Interface Patterns for AI Transparency (Part 2), 2026](https://shop.smashingmagazine.com/2026/05/practical-interface-patterns-ai-transparency/)
- [Jacar — UI Design for Agents, 2025](https://jacar.es/en/diseno-ui-agentes/)
- [Courier — Notifications Have Two Audiences: Humans and AI Agents, 2026](https://www.courier.com/blog/your-notifications-now-have-two-audiences-humans-and-ai-agents/)
- [ioDroplet — Identifying Necessary Transparency Moments in Agentic AI (Part 1)](https://iodroplet.com/identifying-necessary-transparency-moments-in-agentic-ai-part-1/)
- [Conseca — Contextual Security Policies for AI Agents (Google, HOTOS '25)](https://sigops.org/s/conferences/hotos/2025/papers/hotos25-100.pdf)
- [agent-safe-delete — Open-source CLI](https://github.com/ckckck/agent-safe-delete)
- [Safe File Deletion MCP — NPM package](https://www.npmjs.com/package/@mizunashi_mana/safe-file-deletion-mcp)
- [Undisk MCP — Per-file undo](https://chat.mcp.so/server/undisk-mcp/Kiarash%20Adl)
- [Replit — Inside the Snapshot Engine, Dec 2025](https://blog.replit.com/inside-replits-snapshot-engine)
- [Morae — Proactively Pausing UI Agents (UIST '25)](https://ar5iv.labs.arxiv.org/html/2508.21456)
- [AgentField — Cancel/Pause/Resume issue #243](https://github.com/Agent-Field/agentfield/issues/243)
- [Autopoiesis — Delayed Action Buffer #614](https://github.com/DavSimFel/autopoiesis/issues/614)
- [Autopoiesis — Unified Interaction Queue #547](https://github.com/DavSimFel/autopoiesis/issues/547)
- [FutureAGI — Agentic UX in 2026: AG-UI Protocol](https://futureagi.com/blog/agentic-ux-webinar-2025/#hero)
- [Agents.md — AI Coding Agent Open Standard, 2025](https://www.remio.ai/post/what-is-agents-md-a-complete-guide-to-the-new-ai-coding-agent-standard-in-2025)
- [OpenPort Protocol — Security Governance for AI Agent Tool Access](https://ar5iv.labs.arxiv.org/html/2602.20196)
- [Claude Code Permission Model — Skywork, 2025](https://skywork.ai/blog/permission-model-claude-code-vs-code-jetbrains-cli/)
- [Cursor Forum — Gemini deletes code despite memories #135867](https://forum.cursor.com/t/gemini-deletes-destroys-code-despite-memories-forbidding-these-actions/135867)
