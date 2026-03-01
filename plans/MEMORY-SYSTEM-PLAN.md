# MUR Core v2 — Memory & Knowledge Evolution Plan

> Version: 1.0 | Date: 2026-03-01
> Author: OpenClaw + David
> Status: Proposal

## 1. Executive Summary

MUR Core 是**學習基礎設施** — 它不直接跟用戶對話，而是為各種 AI 工具（Claude Code、Commander、Gemini CLI 等）提供持續學習能力。MUR 的 pattern 系統已經很成熟（lifecycle、decay、scoring、LanceDB），但缺少：

1. **Real-time memory integration** — 當 Commander 學到新偏好，MUR 應該即時同步
2. **Cross-agent knowledge sharing** — 不同 AI 工具學到的 patterns 應該互通
3. **Memory consolidation** — 自動整理、合併、提煉 patterns
4. **Context engineering API** — 讓消費者（Commander 等）能按需請求最佳 context

本計劃定義 MUR 在記憶生態系統中的角色，以及與 Commander 的互通協議。

### MUR vs Commander 的定位

```
MUR Core = 知識引擎 (Knowledge Engine)
├── 儲存 patterns (YAML + LanceDB)
├── 評分 & 排序 (multi-signal scoring)
├── 生命週期管理 (lifecycle, decay)
├── 跨工具同步 (inject hooks, sync)
└── 知識進化 (compose, decompose, emergence)

Commander = 應用層 (Application Layer)
├── 用戶互動 (chat, commands)
├── 即時記憶 (session memory, preferences)
├── 行為執行 (workflows, schedules)
└── 回饋生成 (success/failure signals)
```

**Commander 是 MUR 最大的 client。**
Commander 產生 raw memories → MUR 提煉為 patterns → Commander 消費 patterns。

---

## 2. Current State Analysis

### 已有（可直接利用）

| Module | Path | 功能 | 成熟度 |
|--------|------|------|--------|
| `store/yaml.rs` | patterns/ dir | YAML pattern CRUD | ✅ 穩定 |
| `store/lancedb.rs` | LanceDB index | Vector search | ✅ 穩定 |
| `store/embedding.rs` | Ollama/OpenAI | Embedding generation | ✅ 穩定 |
| `retrieve/scoring.rs` | Multi-signal | 6-factor scoring pipeline | ✅ 穩定 |
| `retrieve/gate.rs` | Token gating | Budget-aware retrieval | ✅ 穩定 |
| `evolve/lifecycle.rs` | Promote/deprecate/archive | Pattern lifecycle | ✅ 穩定 |
| `evolve/decay.rs` | Time-based decay | Half-life per tier | ✅ 穩定 |
| `evolve/feedback.rs` | Success/override signals | Effectiveness tracking | ✅ 穩定 |
| `evolve/compose.rs` | Pattern composition | Merge related patterns | 🔧 基本 |
| `evolve/cooccurrence.rs` | Co-occurrence matrix | Find related patterns | 🔧 基本 |
| `evolve/commander_bridge.rs` | Bridge to Commander | 🚧 Stub | ❌ 未實作 |
| `inject/hook.rs` | Format for injection | CLAUDE.md style output | ✅ 穩定 |
| `inject/sync.rs` | Sync to native skills | Write to .claude/rules/ | ✅ 穩定 |

### 缺失

1. **Commander Bridge** — `commander_bridge.rs` 是 stub
2. **Real-time sync** — 沒有即時通知機制
3. **Memory consolidation daemon** — 沒有定期整理
4. **Cross-agent protocol** — 沒有標準化的 knowledge exchange format
5. **Context API** — Commander 無法 programmatically 請求「給我最相關的 5 條 patterns for this query」

---

## 3. Architecture

### 3.1 MUR as Knowledge Service

```
┌─────────────────────────────────────────────────┐
│                  MUR Core                         │
│                                                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────────┐   │
│  │  Store   │  │ Retrieve │  │   Evolve     │   │
│  │ YAML+DB  │  │ Score+   │  │ Lifecycle+   │   │
│  │          │  │ Gate     │  │ Compose+     │   │
│  │          │  │          │  │ Decay        │   │
│  └────┬─────┘  └────┬─────┘  └──────┬───────┘   │
│       │              │               │            │
│  ┌────┴──────────────┴───────────────┴────────┐  │
│  │           Knowledge Bus (new)               │  │
│  │  - Pattern CRUD events                      │  │
│  │  - Score cache                              │  │
│  │  - Cross-tool notifications                 │  │
│  └────────────────────┬───────────────────────┘  │
│                       │                           │
│  ┌────────────────────┴───────────────────────┐  │
│  │         Context API (new)                   │  │
│  │  retrieve(query, budget, scope) → patterns  │  │
│  │  ingest(fact, source, metadata) → pattern   │  │
│  │  feedback(pattern_id, signal) → updated     │  │
│  └────────────────────────────────────────────┘  │
└──────────────────┬────────────────────────────────┘
                   │ IPC (Unix socket / gRPC / HTTP)
      ┌────────────┼────────────┐
      ▼            ▼            ▼
┌──────────┐ ┌──────────┐ ┌──────────┐
│Commander │ │Claude    │ │Gemini    │
│(Slack,TG)│ │Code      │ │CLI       │
└──────────┘ └──────────┘ └──────────┘
```

### 3.2 Context API

MUR 對消費者提供的核心 API：

```rust
/// Request context-optimized patterns for a specific query.
pub struct ContextRequest {
    /// The query/message to find relevant patterns for
    pub query: String,
    /// Maximum token budget for returned patterns
    pub token_budget: usize,  // default: 2000
    /// Scope filter
    pub scope: ContextScope,
    /// Which tool is requesting (for feedback tracking)
    pub source: String,  // "commander", "claude-code", etc.
}

pub struct ContextScope {
    /// User identifier (for personalized retrieval)
    pub user: Option<String>,
    /// Project/workspace (for project-scoped patterns)
    pub project: Option<String>,
    /// Intent/task type (for task-relevant patterns)
    pub task: Option<String>,
    /// Platform (for platform-specific patterns)  
    pub platform: Option<String>,
}

pub struct ContextResponse {
    /// Scored and budget-fitted patterns
    pub patterns: Vec<ScoredPattern>,
    /// Total tokens used
    pub tokens_used: usize,
    /// Injection-ready formatted text
    pub formatted: String,
    /// Pattern IDs for feedback tracking
    pub injection_ids: Vec<String>,
}

// Usage from Commander:
let context = mur.retrieve(ContextRequest {
    query: "用戶問怎麼部署",
    token_budget: 800,
    scope: ContextScope {
        user: Some("david"),
        task: Some("chat"),
        platform: Some("slack"),
        ..Default::default()
    },
    source: "commander".into(),
}).await;

// Inject into LLM prompt
let system_prompt = format!("{}\n\n{}", base_prompt, context.formatted);
```

### 3.3 Ingest API

Commander（或其他工具）向 MUR 提交新知識：

```rust
pub struct IngestRequest {
    /// What the user said or what was learned
    pub content: String,
    /// Categorization
    pub category: IngestCategory,
    /// Source tool
    pub source: String,
    /// User context
    pub user: Option<String>,
    /// Related pattern names (for linking)
    pub related: Vec<String>,
}

pub enum IngestCategory {
    /// User preference ("回覆要簡短")
    Preference,
    /// Factual knowledge ("SSH server is X")
    Fact,
    /// Behavioral rule ("不回 @別人")
    Rule,
    /// Procedural knowledge ("部署前先跑測試")
    Procedure,
    /// Correction ("不是 A，是 B")
    Correction,
}

pub struct IngestResponse {
    /// Created or updated pattern ID
    pub pattern_id: String,
    /// Was this a new pattern or update to existing?
    pub action: IngestAction,
    /// Similar existing patterns (for dedup review)
    pub similar: Vec<String>,
}

// Usage from Commander:
let result = mur.ingest(IngestRequest {
    content: "David prefers concise responses in Traditional Chinese",
    category: IngestCategory::Preference,
    source: "commander".into(),
    user: Some("david".into()),
    related: vec![],
}).await;
```

### 3.4 Feedback API

```rust
pub struct FeedbackRequest {
    /// Pattern ID
    pub pattern_id: String,
    /// Signal type
    pub signal: FeedbackSignal,
    /// Source tool
    pub source: String,
}

pub enum FeedbackSignal {
    /// Pattern was injected and the response was good
    Success,
    /// Pattern was injected but user corrected the response
    Override,
    /// Pattern was explicitly referenced by user
    Referenced,
    /// User said "forget this" or "this is wrong"
    Rejected,
}
```

---

## 4. Commander Bridge

### 4.1 Commander → MUR (Write Path)

```
Commander: 用戶說「記住：部署前要先跑測試」

1. Commander stores locally (for immediate use):
   ~/.mur/commander/memory/rules/global.md
   → append: "部署前要先跑測試"

2. Commander calls MUR Ingest API:
   mur.ingest({
     content: "部署前要先跑測試",
     category: Procedure,
     source: "commander",
     user: "david"
   })

3. MUR creates/updates pattern:
   ~/.mur/patterns/deploy-run-tests-first.yaml
   → tier: session
   → tags: [procedure, deployment]
   → applies.tools: [commander, claude-code]

4. MUR notifies other consumers (optional):
   → Claude Code: sync to .claude/rules/
   → Other Commander instances: invalidate cache
```

### 4.2 MUR → Commander (Read Path)

```
Commander: 用戶說「部署 production」

1. Commander calls MUR Context API:
   mur.retrieve({
     query: "deploy production",
     token_budget: 800,
     scope: { user: "david", task: "run" }
   })

2. MUR returns:
   - "部署前要先跑測試" (score: 0.92)
   - "production 部署需要 approval" (score: 0.85)
   - "部署後通知 #ops channel" (score: 0.78)

3. Commander injects into LLM context:
   [User Preferences]
   1. 部署前要先跑測試
   2. production 部署需要 approval
   3. 部署後通知 #ops channel

4. After execution, Commander sends feedback:
   mur.feedback({ pattern_id: "deploy-run-tests-first", signal: Success })
```

### 4.3 Real-time Sync

兩個選項：

**Option A: File Watcher**
```
MUR watches: ~/.mur/patterns/**/*.yaml
Commander watches: ~/.mur/commander/memory/**/*.md
Changes → re-index LanceDB → invalidate cache
```

**Option B: Unix Socket IPC** (preferred for daemon mode)
```
mur-daemon listens on: ~/.mur/mur.sock
Commander connects and subscribes to pattern events
Events: PatternCreated, PatternUpdated, PatternArchived
```

---

## 5. Knowledge Categories for MUR

### 5.1 Existing Pattern Tiers (keep)

```
Session → Project → Core
```

### 5.2 New: Knowledge Origin Tags

```yaml
# Pattern metadata
origin:
  source: commander          # which tool created this
  trigger: user_explicit     # how it was created
  user: david                # who it's about
  platform: slack            # where it happened
  confidence: 0.9            # extraction confidence
```

`trigger` values:
- `user_explicit` — 用戶主動說「記住」
- `user_correction` — 用戶糾正 bot
- `agent_inferred` — agent 從行為模式推斷
- `community_shared` — 來自社群分享

### 5.3 New: Memory-type Patterns

MUR 目前的 patterns 偏向 technical knowledge。新增支援：

| 類型 | 現有? | 範例 |
|------|-------|------|
| Technical | ✅ | Swift testing macros |
| Preference | 🆕 | 用繁體中文回覆 |
| Fact | 🆕 | SSH server 是 X |
| Procedure | 🆕 | 部署前跑測試 |
| Behavioral | 🆕 | 不回 @別人的訊息 |

在 `Pattern` struct 加 `kind` field：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatternKind {
    Technical,    // existing
    Preference,   // user preference
    Fact,         // factual knowledge
    Procedure,    // how-to
    Behavioral,   // behavior rule
}
```

---

## 6. Memory Consolidation

### 6.1 Periodic Review (mur evolve)

```bash
mur evolve --consolidate
```

流程：
1. **Dedup** — Find semantically similar patterns (cosine > 0.90), merge them
2. **Promote** — Frequently-used session patterns → project tier
3. **Decay** — Score all patterns, mark stale ones
4. **Archive** — Move deprecated + 180d patterns to archive
5. **Rebuild** — Rebuild LanceDB index
6. **Report** — Output summary of changes

### 6.2 Sleep-time Processing

受 Letta 啟發，MUR 可以在「低活動」時期進行深度整理：

```rust
/// Run during idle periods (e.g., Commander heartbeat)
async fn sleep_time_consolidation(store: &mut YamlStore) {
    // 1. Review recent facts for potential pattern extraction
    let recent_facts = store.recent_facts(days=7);
    for cluster in cluster_by_topic(recent_facts) {
        if cluster.len() >= 3 {
            // Multiple facts about same topic → synthesize into pattern
            let pattern = synthesize_pattern(cluster).await;
            store.add(pattern);
        }
    }
    
    // 2. Check for contradictions
    let conflicts = find_contradictions(store.all_active());
    for (a, b) in conflicts {
        // Mark lower-scored one as deprecated
        if a.score() < b.score() {
            store.deprecate(&a.name);
        }
    }
    
    // 3. Update co-occurrence matrix
    update_cooccurrence(store).await;
}
```

---

## 7. Cross-Agent Protocol

### 7.1 Standard: MUR Knowledge Exchange Format (MKEF)

```yaml
# ~/.mur/exchange/{id}.yaml
mkef_version: 1
id: "pref-concise-response-david"
kind: preference
content:
  technical: "User David prefers concise responses (2-3 sentences max)"
  principle: "Brevity improves user satisfaction for chat interactions"
origin:
  source: commander
  trigger: user_explicit
  user: david
  timestamp: "2026-03-01T14:00:00Z"
scope:
  tools: [commander, claude-code, gemini-cli]
  users: [david]
  platforms: [slack, telegram, discord]
lifecycle:
  status: active
  confidence: 0.95
  use_count: 12
  last_used: "2026-03-01T13:55:00Z"
```

### 7.2 Sync Protocol

```
Tool A learns something
  → writes to ~/.mur/exchange/{id}.yaml
  → calls: mur ingest --exchange {id}.yaml
  → MUR indexes + notifies other tools
  → Tool B picks up via: mur retrieve --for {tool_b}
```

### 7.3 Privacy & Isolation

```
~/.mur/patterns/              ← shared patterns (all tools)
~/.mur/patterns/private/      ← private patterns (per-user, per-tool)
~/.mur/commander/memory/      ← Commander-only fast cache

Privacy rules:
- Facts with user PII → private/
- Preferences → shared (unless marked private)
- Behavioral rules → shared
- Technical patterns → shared
```

---

## 8. Implementation Phases

### Phase 1: Context API (2 weeks)

**mur-core 改動：**
- [ ] `pub mod context_api` in mur-core — `ContextRequest`, `ContextResponse`
- [ ] `pub fn retrieve(req: ContextRequest) -> ContextResponse` — wraps scoring + gating
- [ ] `pub fn ingest(req: IngestRequest) -> IngestResponse` — wraps store + embed
- [ ] `pub fn feedback(req: FeedbackRequest)` — wraps evolve/feedback
- [ ] HTTP server endpoint: `POST /api/v1/context`, `POST /api/v1/ingest`, `POST /api/v1/feedback`
- [ ] CLI: `mur context --query "..." --budget 800 --scope user=david`
- [ ] Tests: 20+ for API logic

### Phase 2: Commander Bridge (1 week)

**mur-core 改動：**
- [ ] Implement `evolve/commander_bridge.rs`
- [ ] Commander fact → MUR pattern conversion
- [ ] MUR pattern → Commander rule/fact conversion
- [ ] File watcher for bi-directional sync

**Commander 改動：**
- [ ] `MemoryStore` calls MUR Context API instead of local-only search
- [ ] On "記住" → also call MUR Ingest API
- [ ] On successful interaction → call MUR Feedback API

### Phase 3: Pattern Kind Extension (1 week)

- [ ] Add `PatternKind` enum to `mur-common/pattern.rs`
- [ ] Update scoring to weight differently per kind
- [ ] Update `inject/hook.rs` to format preferences differently from technical
- [ ] Update `inject/sync.rs` to write preferences to `.claude/rules/` as well
- [ ] Backward compatibility: existing patterns default to `Technical`

### Phase 4: Sleep-time Consolidation (1 week)

- [ ] `mur evolve --consolidate` command
- [ ] Dedup via semantic similarity
- [ ] Contradiction detection
- [ ] Cluster-based pattern synthesis
- [ ] Scheduled via cron or daemon idle loop

### Phase 5: Cross-Agent Protocol (2 weeks)

- [ ] MKEF format definition and parser
- [ ] `~/.mur/exchange/` directory and watcher
- [ ] `mur sync --tool commander` / `mur sync --tool claude-code`
- [ ] Privacy controls (private vs shared patterns)
- [ ] Community sharing opt-in (`mur share --pattern {name}`)

---

## 9. Compatibility Matrix

| Feature | MUR owns | Commander owns | Shared |
|---------|----------|---------------|--------|
| Pattern storage | ✅ | | |
| Pattern scoring | ✅ | | |
| Pattern lifecycle | ✅ | | |
| Embedding generation | | | ✅ (Ollama) |
| LanceDB indexing | ✅ (patterns) | ✅ (intents) | Schema |
| User preferences | | ✅ (fast cache) | MUR backup |
| Session memory | | ✅ | |
| Chat context assembly | | ✅ | MUR retrieval |
| Cross-tool sync | ✅ | | |
| Community sharing | ✅ | | |

---

## 10. Key Design Decisions

### Q: Why not put all memory in MUR and have Commander just call the API?

**A:** Latency. Commander needs sub-100ms response for chat. Local fast cache (core memory + scoped rules) eliminates the API round trip for common cases. MUR API is for deeper retrieval and long-term storage.

### Q: Why not have Commander manage its own patterns independently?

**A:** Silos. If Commander learns "David prefers Chinese" but Claude Code doesn't know, the user has to teach every tool separately. MUR as central knowledge store eliminates this.

### Q: Why support both file-based sync and API?

**A:** Different tools have different integration capabilities. Claude Code can read files (`.claude/rules/`) but can't call HTTP APIs during prompt assembly. Commander can call APIs. File-based sync is the lowest-common-denominator.

### Q: How to handle conflicting knowledge from different tools?

**A:** Provenance + recency + confidence. If Commander says "user likes short responses" (confidence 0.9, 2 days ago) and Claude Code says "user wants detailed explanations" (confidence 0.7, 30 days ago), Commander's version wins. User can also explicitly resolve via `mur resolve --conflict {id}`.
