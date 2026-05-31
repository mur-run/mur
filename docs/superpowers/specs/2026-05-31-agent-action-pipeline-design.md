# Agent Action Pipeline — Design Spec

> **Date**: 2026-05-31
> **Status**: Ready for review (revised 2026-05-31 — reconciled against codebase: P0b dependency, Hook-based guard, `file_actions` schema, `mur-hub-gui` target; **red-team revision 2026-05-31 — deletion model changed from timed auto-permanent-delete → cancel-window + long retention with NO auto-hard-delete; Roadmap Alignment section added**)
> **Scope**: Agent platform enhancement — file notification + deletion safety + task queue (Block A)

## Overview

Unify three agent UX features into a single **Action Pipeline**: file/email notification with voice commands (A1), deletion confirmation with configurable delay (A2), and a user-facing task queue with pause/cancel (A3).

These are not independent features — they are phases of the same value chain: **ingest → queue → execute → notify**. The pipeline design ensures they compose naturally rather than duplicating infrastructure.

### Research Foundation

Design validated against 2025–2026 industry best practices:

- **File handling UX**: Multi-surface notification (badge + toast + OS notify), review-before-execute, structured notification metadata. Sources: [Smashing Magazine 2026](https://shop.smashingmagazine.com/2026/05/practical-interface-patterns-ai-transparency/), [Jacar](https://jacar.es/en/diseno-ui-agentes/), [Courier 2026](https://www.courier.com/blog/your-notifications-now-have-two-audiences-humans-and-ai-agents/)
- **Deletion safety**: Archive-don't-delete (Trash) with a **long retention window and NO time-triggered permanent deletion** (industry norm: Notion keeps until manually emptied, Instagram ~30 days); a short **cancel/undo window before the destructive op executes** (Gmail-undo style); two-phase protocol (never propose+execute same-turn); **read-after-write verification** (the exact check missing in the Gemini-CLI incident); schema-level enforcement (batch limits, no wildcards). Sources: [Conseca (Google, HOTOS '25)](https://sigops.org/s/conferences/hotos/2025/papers/hotos25-100.pdf), [Replit DB-wipe incident (Fortune)](https://fortune.com/2025/07/23/ai-coding-tool-replit-wiped-database-called-it-a-catastrophic-failure/), [Gemini CLI file-deletion incident](https://winbuzzer.com/2025/07/26/googles-gemini-cli-deletes-user-files-confesses-catastrophic-failure-xcxwbn/), [soft-delete anti-pattern (Cultured Systems)](https://www.cultured.systems/2024/04/24/Soft-delete/), [delete-UX best practice (DesignMonks)](https://www.designmonks.co/blog/delete-button-ui)
- **Task queue UX**: Three-state execution row (pause/cancel/resume), dynamic checklist over progress bar, checkpoint-based pausing (interrupt-and-resume), AG-UI event model. Sources: [Morae (UIST '25)](https://ar5iv.labs.arxiv.org/html/2508.21456), [LangChain human-in-the-loop docs](https://docs.langchain.com/oss/python/langchain/human-in-the-loop), [AG-UI Protocol](https://futureagi.com/blog/agentic-ux-webinar-2025/#hero), [HITL approvals 2026 (getclaw)](https://getclaw.sh/blog/human-in-the-loop-ai-agents-approvals-2026)
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

The pipeline was chosen because A1, A2, A3 are phases of the same chain (ingest → queue → execute → notify), share a common ledger, and a shared state model eliminates duplication. Future features (agent roles, trigger modes) extend by adding phases or hooks. A visual DAG/flow editor is explicitly **not** a planned extension — see Roadmap Alignment below.

### Roadmap Alignment (red-team 2026-05-31)

This spec is **Block A** of a larger agent-platform roadmap. A red-team review (ultra-deep web research + competitive scan) bounded that roadmap against MUR's stated moat — local-first + ≈0 marginal cost + accumulating memory + cryptographic governance (`docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md`). The constraints that touch this spec:

- **Deletion never auto-hard-deletes on a timer** (this revision; see Phase 3). A background process that destroys user files on a short TTL is itself a data-loss vector and contradicts the "archive-don't-delete" principle cited above and the Replit/Gemini incidents.
- **No visual DAG/flow editor.** The pipeline's extension surface is phases/hooks plus the `mur run "w1 | w2 && w3"` expression — not a graph canvas. Authoring for non-technical users, if ever needed, is NL→pipeline-expression generation; at most a **read-only** run visualization for trust/audit.
- **Agent roles ship narrow, not broad.** Prefer 2–3 deep roles/teams that exploit the moat (local-first, always-on at ≈0 cost, governed) over a wide catalog of thin vertical agents.
- **Agent Teams** (when built) are **first-party, curated, signed bundles** gated to a paid tier — **not** an open third-party marketplace (keeps the catalog off the GPT-Store / MCP supply-chain risk surface). A one-click team load is a large capability grant and must pass the same gate + sandbox + audit as any single agent.
- **Result delivery** prefers push + the existing Slack bridge over a net-new native mobile app until demand is validated.

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
    id: String,                   // matches file_actions[].id
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
    trash_path: Option<PathBuf>,  // None while PendingDelete (file not moved yet); set on move-to-trash
    file_size_bytes: u64,
    created_by_task_id: Uuid,
    created_at: DateTime<Utc>,
    execute_at: DateTime<Utc>,    // created_at + deletion.cancel_window_minutes; move-to-trash fires at/after this
    retention_until: Option<DateTime<Utc>>, // set on move = moved_at + deletion.trash_retention_days; marks
                                  //   eviction-ELIGIBILITY, never auto-deletion
    status: TrashStatus,
    restore_metadata: RestoreMeta,
}

enum TrashStatus {
    PendingDelete,  // consented; within cancel window; file NOT yet moved (Undo drops it, lossless)
    Retained,       // moved to trash; within retention window; recoverable
    Expired,        // past retention; eligible for capacity eviction but STILL on disk + recoverable
    Restored,       // user restored to original location
    PermDeleted,    // gone for good — ONLY via explicit empty/now, or capacity eviction (oldest-Expired-first)
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
    DeletionPending    { entry: TrashEntry },        // consented; cancel window open, file untouched
    DeletionCancelled  { entry_id: Uuid },           // Undo within cancel window
    TrashCreated       { entry: TrashEntry },         // cancel window elapsed → moved to trash
    TrashRestored      { entry_id: Uuid },
    TrashExpired       { entry_id: Uuid },            // past retention; eviction-eligible (NOT deleted)
    TrashPermDeleted   { entry_id: Uuid, reason: PermDeleteReason }, // explicit empty/now | CapacityEviction
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
├── profile.yaml                  ← existing + new top-level keys (file_actions, action_pipeline)
└── ...
```

> **Ledger writer model.** Three concurrent writers exist — CLI control ops,
> the runtime executor, and the daemon expiry tick — so the ledger uses the
> **flock pattern** (`mur-common/src/multimodal/ledger.rs::ProvenanceLedger`),
> **not** the single-writer companion `durable::Ledger`. Sink the shared
> daily-rotated abstraction into `mur-common` first. `pending.json` is a
> whole-state snapshot (temp+rename) rebuilt by replaying the day's ledger on
> crash — no existing ledger reconstructs in-memory state today, so this is new
> work, not reuse.

### Profile Schema Additions

> **Do NOT nest under `capabilities:`.** That field is `Vec<String>` (`mur-common/src/agent.rs:72`) and is serialized into three surfaces — the A2A agent card (`protocol/methods/card.rs:65` → feeds `LockFile.card_digest`), `running.lock` (`supervisor.rs:377`), and the `.muragent` export (`muragent/writer.rs:188`). It also already denotes **trust/security** capabilities (`skill/capability.rs`). Add new **top-level** sections instead; all fields are `#[serde(default)]` so existing profiles load unchanged.

```yaml
# A1: declarative UI action list (NEW top-level key — not under capabilities)
file_actions:
  - id: summarize
    label: { en: "Summarize", zh-TW: "摘要" }
    description: "Extract key points from the file"
    mime_types: [text/*, application/pdf, application/msword]
  - id: translate
    label: { en: "Translate", zh-TW: "翻譯" }
    mime_types: [text/*, application/pdf]
  - id: ask_me                  # reserved: always last; empty mime_types ⇒ any
    label: { en: "Ask me anything...", zh-TW: "自由指令..." }

# A2 + A3: namespaced under one section
action_pipeline:
  deletion:
    trash_enabled: true
    cancel_window_minutes: 10    # undo window BEFORE the delete executes (Gmail-undo); 0 = execute on consent
    trash_retention_days: 30     # how long trashed files stay recoverable; expiry = eviction-eligibility, NOT auto-delete
    trash_max_mb: 1024           # hard cap; capacity pressure evicts oldest-EXPIRED-first (never unexpired)
    max_batch: 50                # reject a destructive op touching more than this many paths
    auto_permanent_delete: false # MUST stay false: no daemon ever hard-deletes user files on a timer
    trusted_paths: []            # canonicalized; symlinks resolved (see Phase 3)
  queue:
    max_concurrent: 3
    default_timeout_minutes: 30
    pending_item_ttl_minutes: 60
```

`label` is a BCP-47 → text map. Render-time locale selection reuses `CompanionConfig.locale` / `default_locale()` (from `LANG`), falling back exact (`zh-TW`) → language prefix (`zh`) → `en` → first entry.

```rust
struct FileAction {
    id: String,
    #[serde(default)] label: BTreeMap<String, String>,
    #[serde(default)] description: Option<String>,
    #[serde(default)] mime_types: Vec<String>,   // empty ⇒ matches any
}
```

### Action Buttons from file_actions

Action buttons rendered in the selection UI are sourced from `profile.yaml → file_actions` (top-level), filtered by the intersecting MIME types of the selected files. The `ask_me` action always appears last and accepts any file type.

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

### TrashGuard: a Hook gate + a dispatcher executor (NOT a new layer)

"Every tool call passes through TrashGuard" **already describes the existing frozen hook chain** (`HOOK_SCHEMA_VERSION = 2`, `hooks/mod.rs`). TrashGuard is not new infrastructure — it splits into two pieces that plug into existing seams.

**(a) Gate — a `Hook` impl, registered via the A1 handler-picker.** Runs in `pre_tool_use` *after* `B0SafetyHook`. The chain short-circuits on the first non-`Allow` (`hooks/chain.rs:49-65`), and B0 already owns the out-of-home AskUser gate (Rule 1 matches `fs.write|fs.delete|fs.append|fs.create`, `b0.rs:403-431`). The gate therefore adds only the checks B0 lacks:

```
DETECT destructive patterns (shell rm/unlink, mv→/dev/null, os.remove,
   os.unlink, shutil.rmtree, MCP delete_file, A2A delete intent)
SCHEMA-LEVEL enforcement → Decision::Deny on violation:
   - batch size ≤ deletion.max_batch          (config, not hardcoded)
   - reject wildcards (*, ?) in paths
   - path within allowed scope (canonicalized)
TWO-PHASE → first destructive op in a task → Decision::AskUser (default Deny),
   reusing the existing AskUser + GrantStore plumbing.
```

**(b) Executor — in the P0b tool dispatcher, not a hook.** `pre_tool_use` is a *gate* (Allow/Deny/AskUser) and **cannot mutate call args**, so the rm→mv rewrite cannot live there. Once the gate returns `Allow`, the dispatcher routes the op through a trash executor:

```
DEFER    record PendingDelete { original_path, size, RestoreMeta,
            execute_at = now + deletion.cancel_window_minutes }
APPEND   DeletionPending to ledger; surface Undo (Phase 4) — file NOT touched yet
─ later, when cancel window elapses (daemon tick, no Undo) ─
REWRITE  rm <path> → move <path> → <agent_home>/trash/{ts}_{filename}/
SET      trash_path; retention_until = now + deletion.trash_retention_days; status → Retained
APPEND   TrashCreated to ledger
NOTIFY   Phase 4 (deletion notifications are independent and urgent)
```

This split is why (b) is blocked on P0b (it needs the tool loop) while (a) can be written and unit-tested ahead of it.

### Relationship to the B1 OS sandbox (defense in depth, not redundancy)

The kernel sandbox (Landlock on Linux, SBPL on macOS — `sandbox/policy.rs`) is the **outer** confinement; TrashGuard is **userspace policy within writable scope**. Document the consequence explicitly: on Linux, deleting a path outside `fs_write` is **blocked by the kernel before TrashGuard sees it** — the op never reaches userspace, so move-to-trash neither does nor can apply there.

### Cross-filesystem trash (EXDEV)

The trash dir is inside `<agent_home>` (e.g. `~/.mur/...`), but sources may sit on another volume. `rename(2)` across filesystems returns **EXDEV**, so the executor falls back to **copy + unlink**, and a batch is **per-file all-or-nothing**, reporting mixed results as `TaskOutcome::PartialSuccess`.

### Two-Phase Protocol

Following the [Cursor/Gemini incident lesson](https://forum.cursor.com/t/gemini-deletes-destroys-code-despite-memories-forbidding-these-actions/135867): destructive actions are **never** proposed and executed in the same turn.

- Turn 1: Preview what would be deleted
- Await user confirmation
- Turn 2: Execute only after explicit consent

This maps onto the **existing** `Decision::AskUser` + `GrantStore` flow: the first destructive op returns `AskUser` (default `Deny`); execution proceeds on a later turn once the user grants. Note this is a *between-turn* grant, not mid-turn suspend/resume — true in-flight pausing is a separate P0b runtime capability and is out of scope for the guard.

### Cancel Window (pre-execution undo)

After consent, a destructive op is **not executed immediately**. The executor records a `PendingDelete` entry with `execute_at = now + deletion.cancel_window_minutes` (default 10; set 0 to execute on consent) and surfaces an **Undo** affordance (Phase 4). The file is **not touched** during this window — Undo simply drops the entry (`DeletionCancelled`), instantly and losslessly. The daemon performs the actual move-to-trash only after the window elapses. This is the correct reading of the original "defer deletion 10 minutes" intent: a reversible grace period *before* anything is destroyed, distinct from (and layered on top of) the long trash retention *after* the move.

### Trash Timer (deferred executor — never an auto-deleter)

A single background tick (every 30 s) does **two** things, and **never permanently deletes user files**:

1. **Execute deferred deletes.** For `status == PendingDelete` where `execute_at < now()` (cancel window elapsed, no Undo): perform rm→move-to-trash, set `trash_path` + `retention_until`, transition → `Retained`, append `TrashCreated`.
2. **Mark expirations.** For `status == Retained` where `retention_until < now()`: transition → `Expired` and append `TrashExpired`. **The file stays on disk and remains recoverable** — `Expired` only makes it *eligible* for capacity eviction (see Trash Capacity). No file is removed here.

- **Owner = `mur-daemon`, not the runtime.** The runtime is frequently ephemeral (spawn → serve one request → exit, `a2a_dial.rs`), so a deferred delete or retention sweep would otherwise never fire. The daemon's existing loop (`main.rs`) hosts one scan across all agents.
- No per-entry timers — they don't survive restart and contradict the established 30 s-scan pattern (`idle_scheduler.rs`).
- **There is no time-triggered permanent deletion** (red-team reversal, 2026-05-31). Files leave disk only via explicit user action (`trash --empty` / `trash --now`) or capacity-pressure eviction. A background process that destroys user data on a timer is itself a data-loss vector and contradicts the archive-don't-delete principle.

### Trash Capacity (the only automatic permanent removal)

- Total trash size tracked. If a new entry would exceed `trash_max_mb`, evict **`Expired` entries oldest-first** (those past `retention_until`) until under budget; each eviction appends `TrashPermDeleted { reason: CapacityEviction }`. This is the **only** path by which the system removes a file without explicit user action — and it touches **only** already-expired entries.
- If reclaiming every `Expired` entry still leaves the new op over budget → reject the rewrite, but **do not hard-block the agent**: fall back to B0's `AskUser` (delete-without-trash on explicit consent) and raise an urgent "Trash full" notification with a one-tap **Empty expired / Empty all**. This avoids a deadlock where a full trash silently prevents every routine delete.
- **Never evict `Retained` (unexpired) entries** (FIFO or otherwise) — the user stays in control of those.

### Trusted Paths

- Paths are **canonicalized** (`std::fs::canonicalize()`), matching B0's `path_confined_to` (`b0_helpers.rs:17-36`). The two safety layers must not disagree on whether a symlink escapes confinement.
- A `trusted_paths` entry bypasses trash only if the canonical target matches; a symlink whose canonical target lies outside the trusted set does not bypass.

### Coverage Scope

All tool-level interception requires the **P0b agentic loop** (the gate fires from `HookChain::pre_tool_use`, which has no caller until P0b). Syscall-level deletion is covered by the **already-shipped B1 OS sandbox**, not a future phase.

| Path | Gated by guard? | Method |
|------|-------------|--------|
| Shell `rm` | ✅ (P0b) | Tool-call pattern matching in `pre_tool_use` |
| Python `os.remove` etc. | ✅ (P0b) | Same shell tool-call path |
| MCP `delete_file` | ✅ (P0b) | Same `pre_tool_use` gate before `tools/call` |
| A2A delete intent | ✅ (P0b) | Supervisor dispatch |
| Out-of-`fs_write` path | n/a | **B1 kernel sandbox denies first** (guard never sees it) |
| Direct syscall from C extension | — | Covered by B1 sandbox (Landlock/SBPL), already shipped |

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
│  Moving to Trash in 10 min · recoverable │
│  for 30 days   [Undo] [Move Now] [Trash] │
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

**Conventions.** The flag forms above are shorthand. House style for multi-op subcommands is **nested-action enums** (`AgentPendingAction` / `AgentQueueAction` / `AgentTrashAction`), mirroring `AgentScheduleAction` — e.g. `mur agent queue <name> pause <id>`, not `--pause`.

**Control plane.** Read-only listing reads the ledger / `pending.json` snapshot directly. **Mutating a live task** (pause / cancel) dials the running agent via `dial_method()` A2A over its socket (`a2a_dial.rs`); if the agent is offline, the op is appended to the ledger and reconciled on next start. `trash restore|empty|now` mutate the trash dir + ledger directly (no live agent needed).

## Implementation Plan

### Phase Order & Dependencies

**Hard dependency:** Phase 3 (EXEC / trash *executor*) and per-step reporting depend on the **P0b agentic tool-call loop**, which is NOT yet implemented. Today `task_runner.rs::run_llm()` only performs LLM↔text round-trips and never parses or dispatches tool calls, so `HookChain::pre_tool_use()` has **no caller** (`HOOKS.md`: "lights up with Track A/D MCP integration"). There is no dispatch point to host trash rewriting until P0b lands.

```
Phase 0  Shared infra (ledger, state, profile schema)   — buildable now
Phase 1  INGEST  (file intake + selection UI)           — buildable now
Phase 2  QUEUE   (task lifecycle, CLI)                   — buildable now
Phase 4a NOTIFY  (completion aggregation)                — after Phase 2
Phase 3  EXEC    (TrashGuard gate + trash executor)      — BLOCKED on P0b
Phase 4b NOTIFY  (per-step reporting)                    — BLOCKED on P0b
```

**v1 deliverable = Phases 0–2 + 4a.** Gate Phase 3 / 4b behind P0b. The TrashGuard *gate* (detection + schema limits + two-phase AskUser) can be written and unit-tested as a `Hook` ahead of P0b; only its *executor* (the actual rm→mv) needs the loop.

### File Structure

**mur-core/src/action_pipeline/** (8 files, ~2050 lines total, all under 800-line limit):

```
mod.rs       ← ~120 lines: public API, Pipeline struct
state.rs     ← ~350 lines: all types + serde
ledger.rs    ← ~200 lines: JSONL read/write (flock multi-writer; sink Ledger<E> to mur-common)
ingest.rs    ← ~300 lines: PendingStore, MIME detect, dedup, merge
queue.rs     ← ~350 lines: TaskQueue, state machine, concurrency
guard.rs     ← ~400 lines: TrashGuard, pattern matching, schema checks
notify.rs    ← ~250 lines: Aggregator, notification formatting
error.rs     ← ~80 lines:  error types
```

**mur-hub-gui/ui/src/** (new React components — NOT `mur-agent-gui`, deprecated M-h8 / removed in v2):

```
pending/     ← PendingPanel, FloatingBadge, FileChecklist, ActionButtons,
               VoiceInput, usePendingBridge, api, types
queue/       ← QueuePanel, TaskRow, StepList, useQueueBridge, api, types
trash/       ← TrashPanel, TrashRow, useTrashBridge, api, types
```

### Crate Impact

| Crate | New | Modified |
|-------|-----|----------|
| **mur-common** | `src/action.rs` (`ActionEvent`, `FileAction`); **sink the generic `Ledger<E>` here** (today in `mur-agent-runtime/src/durable/ledger.rs`) | — |
| **mur-core** | `src/action_pipeline/` (ingest, queue, state, error) + CLI | profile schema, CLI dispatch |
| **mur-agent-runtime** | TrashGuard **gate** (`Hook`) + trash **executor** in the P0b dispatcher; `trash_watcher.rs` | register hook via **A1 handler-picker** (`HooksConfig`) |
| **mur-daemon** | trash + pending TTL **expiry tick** (30 s scan, all agents) | main loop |
| **mur-gui-core** | action bridge (shared); extract VoiceInput from legacy app | `companion_bridge` |
| **mur-hub-gui** | `components/` + `hooks/` panels (~18 files) | `App.tsx` (new tabs) |
| ~~mur-agent-gui~~ | — *(deprecated M-h8; do not target)* | — |

## Testing Strategy

### Unit Tests

- **state.rs**: Serialization round-trips for all types
- **ledger.rs**: Append + scan_days + state rebuild from ledger
- **ingest.rs**: MIME detect, dedup window (5s), merge window (5s), expiry (TTL exceeded)
- **queue.rs**: Enqueue below/at capacity, state machine transitions, pause at checkpoint (<30s), force-pause after timeout (≥30s), cancel cleanup, ledger rebuild (crash recovery)
- **guard.rs**: Detect all destructive patterns (shell, Python, MCP, A2A), batch size reject, wildcard reject, trusted_paths bypass, rewrite rm→mv, cancel-window deferral + Undo, deferred-execute after window elapses, retention→Expired (no deletion), capacity eviction (oldest-Expired-first only), restore, symlink-in-trusted-paths edge case
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
| Trash capacity exceeded | Evict oldest EXPIRED entries first; if still over, AskUser (delete-without-trash) + "Trash full" notify — never evict unexpired |
| User hits Undo in cancel window | Drop PendingDelete; file never touched (lossless) |
| Daemon down during cancel window | Delete simply does not execute (fail-safe); reconciled on next daemon start |
| Trash retention elapsed | Mark Expired (still on disk, recoverable); NEVER auto-deleted |
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
- [Replit AI wiped production database — Fortune, Jul 2025](https://fortune.com/2025/07/23/ai-coding-tool-replit-wiped-database-called-it-a-catastrophic-failure/)
- [Google Gemini CLI deletes user files — Winbuzzer, Jul 2025](https://winbuzzer.com/2025/07/26/googles-gemini-cli-deletes-user-files-confesses-catastrophic-failure-xcxwbn/)
- [Avoiding the soft-delete anti-pattern — Cultured Systems, 2024](https://www.cultured.systems/2024/04/24/Soft-delete/)
- [Delete Button UI best practice — DesignMonks](https://www.designmonks.co/blog/delete-button-ui)
- [Human-in-the-loop — LangChain docs](https://docs.langchain.com/oss/python/langchain/human-in-the-loop)
- [Human-in-the-loop AI agent approvals 2026 — getclaw](https://getclaw.sh/blog/human-in-the-loop-ai-agents-approvals-2026)
- [Morae — Proactively Pausing UI Agents (UIST '25)](https://ar5iv.labs.arxiv.org/html/2508.21456)
- [AgentField — Cancel/Pause/Resume issue #243](https://github.com/Agent-Field/agentfield/issues/243)
- [Autopoiesis — Delayed Action Buffer #614](https://github.com/DavSimFel/autopoiesis/issues/614)
- [Autopoiesis — Unified Interaction Queue #547](https://github.com/DavSimFel/autopoiesis/issues/547)
- [FutureAGI — Agentic UX in 2026: AG-UI Protocol](https://futureagi.com/blog/agentic-ux-webinar-2025/#hero)
- [Agents.md — AI Coding Agent Open Standard, 2025](https://www.remio.ai/post/what-is-agents-md-a-complete-guide-to-the-new-ai-coding-agent-standard-in-2025)
- [OpenPort Protocol — Security Governance for AI Agent Tool Access](https://ar5iv.labs.arxiv.org/html/2602.20196)
- [Claude Code Permission Model — Skywork, 2025](https://skywork.ai/blog/permission-model-claude-code-vs-code-jetbrains-cli/)
- [Cursor Forum — Gemini deletes code despite memories #135867](https://forum.cursor.com/t/gemini-deletes-destroys-code-despite-memories-forbidding-these-actions/135867)
