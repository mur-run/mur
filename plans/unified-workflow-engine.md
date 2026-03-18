# Design Doc: Unified Workflow & Pipeline Engine

**Status:** Proposal  
**Author:** David + Claude  
**Date:** 2026-03-18  
**Affects:** mur, mur-commander, mur-server, mur-web (dashboard)

---

## Problem Statement

MUR ecosystem 目前有兩套重複的 workflow 系統：

| | mur (Rust) | Commander / murc (Rust) |
|---|---|---|
| Workflow 存儲 | `~/.mur/workflows/` | `~/.mur/commander/workflows/` |
| Step 格式 | `Step { order, description, command, tool }` | `StepYaml { name, action, step_type, breakpoint, retry }` |
| 執行引擎 | `PipelineExecutor` — pipe, sequential, parallel | `WorkflowRunner` — 獨立實作 |
| Pipeline | `mur run "w1 \| w2 && w3"` | `murc run w1 \| w2` |
| 特殊功能 | 語意搜尋 + LanceDB | Shadow mode, breakpoints, Slack 通知 |

**造成的問題：**
1. 用戶困惑：兩個 `run` 命令、兩個 workflow 目錄
2. Extract Workflow (LLM) 產出的格式兩邊都不完全兼容
3. Dashboard Save Workflow 存的格式 Commander 解析不了
4. 功能重複，維護成本 2x

---

## Solution: mur 是引擎，Commander 是上層

```
┌─────────────────────────────────────────────┐
│           Commander (murc)                   │
│  增值層 — 不做執行引擎                        │
│                                             │
│  • Daemon 常駐 + Slack/Telegram/Discord 整合 │
│  • 排程 (cron) + 觸發器                      │
│  • 多用戶 + 權限 (PathGuard, Operators)      │
│  • Shadow mode + Breakpoints                │
│  • Web dashboard relay agent                │
│  • 審計日誌 (Audit log)                      │
│                                             │
│  workflow 執行 → 委託給 mur                   │
└──────────────────┬──────────────────────────┘
                   │ 呼叫 mur CLI 或 mur-core lib
┌──────────────────▼──────────────────────────┐
│           mur (核心引擎)                      │
│  基礎層 — 所有知識管理 + 執行                   │
│                                             │
│  • Workflow CRUD + YAML 存儲                 │
│  • PipelineExecutor (pipe, seq, parallel)    │
│  • Step 執行 (shell command, tool, prompt)    │
│  • Pattern 學習 + 注入 + 演化                 │
│  • Session 錄製 + LLM extraction             │
│  • 語意搜尋 (LanceDB)                        │
│  • Cloud sync (mur-server)                   │
│                                             │
│  單一存儲: ~/.mur/                            │
└─────────────────────────────────────────────┘
```

---

## Phase 1: 統一 Workflow 存儲 + 格式

### 1.1 單一存儲目錄
- **所有 workflows 存在 `~/.mur/workflows/`**
- **所有 pipelines 存在 `~/.mur/pipelines/`**
- 移除 `~/.mur/commander/workflows/`（廢棄）
- Commander 讀寫 `~/.mur/workflows/`（跟 mur 共用）

### 1.2 統一 Step 格式（mur 的格式為主）

mur 的 `Step` struct 為基礎，加 optional 擴展欄位：

```yaml
# ~/.mur/workflows/get-usd-spot-rate.yaml
name: get-usd-spot-rate
description: "Fetch USD spot rate from bank website"
steps:
  - order: 1
    description: "Navigate to currency billboard page"
    command: null          # 沒有 command = 描述性 step（由 AI 解釋執行）
    tool: "agent-browser"
  - order: 2
    description: "Extract USD spot buy rate"
    command: null
    tool: "agent-browser"
  - order: 3
    description: "Send rate to user"
    command: "echo '{{rate}}'"  # 有 command = 可直接執行
    tool: null
variables:
  - name: url
    type: url
    default_value: "https://..."
    description: "Target URL"
tools: ["agent-browser"]
trigger: "on user request"
```

### 1.3 Commander Step 擴展欄位

Commander 需要的額外欄位透過 **serde optional 欄位** 處理：

```rust
// mur-common/src/workflow.rs
pub struct Step {
    // 基礎欄位（mur + Commander 共用）
    pub order: u32,
    pub description: String,
    pub command: Option<String>,
    pub tool: Option<String>,
    pub needs_approval: bool,
    pub on_failure: FailureAction,
    // Commander 擴展（mur 忽略，Commander 讀取）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakpoint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}
```

**好處：** mur 的 YAML 不受影響（optional 欄位不出現），Commander 讀同一個檔案但能用額外功能。

---

## Phase 2: Commander 委託 mur 執行

### 2.1 `murc run` → thin wrapper

```
murc run deploy-app
  = mur run deploy-app  +  Slack 通知  +  audit log  +  權限檢查
```

```rust
// Commander 的 handle_run 改為：
async fn handle_run(&self, workflow: &str, user_id: &str) -> IntentResult {
    // 1. 權限檢查（PathGuard, allowed_workflows）
    self.check_permission(user_id, workflow)?;
    
    // 2. 委託給 mur
    let output = Command::new("mur")
        .args(["run", workflow])
        .output()
        .await?;
    
    // 3. Commander 負責增值
    self.notify_slack(user_id, &output).await;
    self.audit_log(user_id, workflow, &output).await;
    
    IntentResult::ok(format_output(&output))
}
```

### 2.2 Pipeline 也統一

```
用戶在終端:    mur run "w1 | w2 && w3"     → mur 的 PipelineExecutor
用戶在 Slack:  @murc run "w1 | w2 && w3"   → 委託 mur run + 通知 + audit
```

### 2.3 移除 Commander 的重複程式碼
- `crates/core/src/workflow/runner.rs` → deprecated，改為 thin wrapper 呼叫 mur
- `crates/core/src/workflow/parser.rs` → 移除（用 mur-common 的）

---

## Phase 3: Cloud Sync + License Gating

### 3.1 mur-server Workflow 表（統一）

合併現有 `commander_workflows` 為 `workflows`：

```sql
CREATE TABLE workflows (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),     -- owner
    team_id UUID REFERENCES teams(id),               -- null = personal
    name TEXT NOT NULL,
    yaml_content TEXT NOT NULL,
    source TEXT DEFAULT 'manual',                     -- manual | extract | sync
    visibility TEXT DEFAULT 'personal',               -- personal | team
    version INT DEFAULT 1,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);
```

### 3.2 Sync 流程

```
mur sync (patterns + workflows + sessions)
  ├── patterns: push to /api/v1/core/sync/push (已有)
  ├── workflows: push to /api/v1/workflows (新增)
  └── sessions: push to /api/v1/core/sessions (已有)

Commander Extract → Save
  ├── 存本地 ~/.mur/workflows/
  └── push to cloud (如有 license)
```

### 3.3 License 功能矩陣

```
┌─────────────────────────────────────────────────┐
│  功能               │ Free  │ Pro    │ Team     │
├─────────────────────┼───────┼────────┼──────────┤
│ 本地 workflow CRUD  │  ✅   │  ✅   │  ✅     │
│ mur run (本地)      │  ✅   │  ✅   │  ✅     │
│ mur serve (local)   │  ✅   │  ✅   │  ✅     │
│ Session recording   │  ✅   │  ✅   │  ✅     │
│ LLM extract         │  ✅   │  ✅   │  ✅     │
│ ────────────────── │ ───── │ ────── │ ──────── │
│ Cloud sync          │  ❌   │  ✅   │  ✅     │
│ Dashboard cloud     │  ❌   │  ✅   │  ✅     │
│ 排程 (cron)         │  ❌   │  ✅   │  ✅     │
│ ────────────────── │ ───── │ ────── │ ──────── │
│ Team sharing        │  ❌   │  ❌   │  ✅     │
│ Multi-user          │  ❌   │  ❌   │  ✅     │
│ Approval workflow   │  ❌   │  ❌   │  ✅     │
│ RBAC               │  ❌   │  ❌   │  ✅     │
│ Audit log          │  ❌   │  ❌   │  ✅     │
└─────────────────────┴───────┴────────┴──────────┘
```

### 3.4 License 檢查點（四層防禦）

1. **mur CLI** — `mur sync` / `mur session push` / `mur workflow publish`
   → 檢查 `~/.mur/auth.json` 裡的 user plan
   → Free 用戶: `"Cloud sync requires Pro plan. Run mur upgrade"`

2. **mur-server API** — 後端二次驗證
   → JWT 裡帶 user_id → 查 DB `user.plan`
   → Free 用戶的 sync/push request → 403

3. **Commander** — `murc run` / relay commands
   → 檢查 `License::load()` 的 plan
   → Free plan 限制 workflow 數量

4. **Dashboard** — 前端提示
   → Free 用戶顯示 upgrade 提示，隱藏 sync 按鈕

### 3.5 Offline-First 原則

- 本地功能**永遠可用**，不管有沒有 license 或網路
- `mur run`、`mur workflow list`、`mur session record` = **免費**
- Cloud sync、team sharing、dashboard cloud = **付費功能**
- 斷網時所有本地功能正常

---

## Phase 4: Dashboard 整合

### 4.1 Free vs Paid Dashboard 體驗

```
Free 用戶:
  mur serve → http://localhost:3847
  Dashboard 自動偵測 → Local 模式
  所有功能可用（workflow, pipeline, session review, extract）
  只差不能 cloud sync / team share

Pro/Team 用戶:
  dashboard.mur.run → Cloud 模式
  多裝置同步 + team 分享
  離線時 fallback 到 localhost
```

### 4.2 Dashboard Workflow 來源

| Dashboard 模式 | Workflow 來源 | Pipeline 來源 |
|---------------|-------------|--------------|
| Local (Free) | `mur serve` API → `~/.mur/workflows/` | `mur serve` API → `~/.mur/pipelines/` |
| Cloud (Pro+) | mur-server API → DB | mur-server API → DB |
| Cloud + Commander | Relay → 本機 `~/.mur/` | Relay → 本機 `~/.mur/` |

### 4.3 Extract Workflow 格式修正
LLM extract 輸出改為 mur Step 格式（有 order + description + optional command/tool）。
Dashboard Save Workflow → 存到 `~/.mur/workflows/`（透過 relay 或直接 API）。

---

## Phase 5: 多用戶 Workflow 管理

### 5.1 存儲架構（不改檔案系統）

```
~/.mur/workflows/          ← 本地公共 workflows（所有用戶可用）
mur-server DB              ← Cloud 存儲
├── personal workflows     ← per-user (user_id, visibility=personal)
└── team workflows         ← per-team (team_id, visibility=team)
```

### 5.2 Workflow 可見性規則

```
mur workflow list
→ 顯示：本地 + 個人 cloud + team shared

mur run <id>
→ 搜尋順序：本地 → 個人 cloud → team shared
→ 第一個匹配的執行
```

### 5.3 權限矩陣

| 來源 | 看 | 跑 | 建/改 | 刪 | 發佈給 team |
|------|----|----|-------|-----|-----------|
| 本地 | ✅ all | ✅ all | ✅ all | ✅ all | ✅ Pro+ |
| 個人 cloud | ✅ owner | ✅ owner | ✅ owner | ✅ owner | ✅ owner |
| Team shared | ✅ members | ✅ members | ✅ creator | ✅ admin | — |

### 5.4 分享流程

```bash
# 發佈到 team (requires Team plan)
mur workflow publish get-usd-spot-rate --team my-team

# 從 team 安裝到本地
mur workflow install get-usd-spot-rate --from team

# 在 Commander (Slack) 裡
@murc run get-usd-spot-rate   # 從 local/team 找到並執行
```

### 5.5 Commander 權限整合

```toml
# config.toml
[channels.users.U08E594M9GT]
permission = "admin"
allowed_paths = ["/Users/david", "/Volumes/..."]
allowed_workflows = ["*"]  # admin 可以跑所有

[channels.users.U_GUEST]
permission = "standard"
allowed_workflows = ["deploy-*", "check-*"]  # 只能跑特定前綴
```

---

## Migration Plan

| 階段 | 工作 | 影響 | 時間 |
|------|------|------|------|
| **M1** | mur-common: Step 加 optional Commander 擴展欄位 | mur, commander | 1h |
| **M2** | Commander: 讀 `~/.mur/workflows/` 取代自己的目錄 | commander | 1h |
| **M3** | Commander: `murc run` 委託 `mur run` | commander | 2h |
| **M4** | Dashboard: Save Workflow 存 mur 格式 | mur-web, relay | 1h |
| **M5** | mur-server: 統一 workflows 表 (personal + team) | mur-server | 2h |
| **M6** | mur: `workflow publish/install` + sync push workflows | mur | 2h |
| **M7** | mur-server + mur: License gating for cloud sync | mur-server, mur | 1h |
| **M8** | Commander: `allowed_workflows` 權限 | commander | 1h |
| **M9** | 移除 Commander 的 WorkflowRunner + parser | commander | 1h |

**總計：~12 小時**

---

## Decision Log

| 決策 | 理由 |
|------|------|
| mur 格式為主 | mur 的 Step 更通用，Commander 的擴展透過 optional 欄位 |
| 不用檔案系統多用戶 | Slack 用戶沒有 OS 帳號，用 cloud DB per-user |
| Commander 不做執行引擎 | 避免重複造車，mur 已有完整 PipelineExecutor |
| 保留 `murc` CLI | 用戶習慣 + daemon 管理需要獨立命令 |
| Offline-first | 本地功能免費永遠可用，cloud sync 是付費增值 |
| `mur serve` 給 Free 用戶 | 完整 dashboard 體驗，不需 cloud |
| 漸進遷移 | 9 個階段可獨立做，不需一次全改 |
| Pipeline 也統一 | 跟 workflow 同理，避免兩套系統 |
