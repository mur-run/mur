# Plan 010: Dashboard & Workflow Architecture

## Problem

mur 和 mur-commander 的 dashboard/workflow 功能邊界模糊：

1. **Dashboard 混合** — mur-web 是 mur-core 的 dashboard，但硬塞了 Commander 的 nav items（Commander / Cmd Workflows / Audit Log），即使使用者沒裝 commander
2. **兩套 Workflow 系統** — mur-core 有簡單的 `~/.mur/workflows/`（知識型），commander 有完整的執行引擎（可排程、有 audit），兩者 Workflow struct 不同
3. **Session → Workflow 斷裂** — `mur session review` 打開 dashboard 但只能看 timeline，無法提取成 workflow
4. **安全邊界不明** — commander sync 時哪些 workflow 該推、哪些不該推，沒有定義

## 產品定位

```
┌─────────────────────────────┐
│         mur (base)          │  ← 每個 AI CLI 使用者都裝
│  學習引擎：記憶 + 注入       │
│  patterns / workflows /     │
│  sessions / sync            │
├─────────────────────────────┤
│    mur-commander (add-on)   │  ← 需要自動化的進階用戶
│  自動化引擎：執行 + 排程     │
│  workflow run / schedule /  │
│  chat / audit / multi-model │
└─────────────────────────────┘
```

- **mur** 必須完全獨立運作，不需要 commander
- **commander** 是 mur 的擴充，繼承 mur 的 patterns & workflows

## Design

### 1. Dashboard：Runtime Feature Detection（不拆 codebase）

保持 mur-web 單一 codebase，用 runtime detection 決定顯示範圍：

```
mur serve (port 3847)
├── 偵測 commander (port 3939)
│   ├── 有 → sidebar 顯示 Commander 區塊
│   └── 沒有 → sidebar 只顯示 mur 功能（乾淨）
```

**改動：**
- Commander nav items 用 `{#if commanderAvailable}` 包起來（目前是永遠顯示）
- Commander header 也條件化
- 當 commander 沒有偵測到時，完全不出現 commander 相關 UI

**理由：** 不需要兩個 build target。mur-web 的 commander 程式碼很小（3 個頁面 + 1 個 API client），gzip 後不到 5KB。多數使用者不會注意到。

### 2. mur-core Workflow：完整的「學習 → 編輯 → 存檔」流程

mur-core 的 workflow 定位：**學到的工作流程**（reusable knowledge sequence），不是可執行的自動化。

**Session Review → Extract Workflow 流程：**

```
Session Review 頁面
  ↓ 點 "Extract Workflow" 按鈕
提取 tool_call events 作為步驟
  ↓ 自動跳到 Workflow Editor
編輯（add/remove/reorder steps, 改 name/description）
  ↓ 點 Save
存到 ~/.mur/workflows/{name}.yaml
  ↓ （可選）如果 commander 有跑
  "Push to Commander" 按鈕
```

**Workflow Editor UI（在 mur-web）：**
- 已有 `Workflows.svelte` 的 inline edit（create/edit/delete/reorder steps）
- 加一個 `/#/workflows/{id}/edit` 獨立編輯頁面
- Session Review 頁面加 "Extract Workflow" 按鈕

**API endpoint（mur-core server.rs）：**
- `POST /api/v1/workflows/extract-from-session/{session_id}` — 從 session events 提取 workflow 草稿
  - 提取所有 `tool_call` events 作為 steps
  - 回傳 Workflow object（未存檔，讓前端 preview/edit）
  - 前端編輯後用 `POST /api/v1/workflows` 存檔

### 3. Commander 資料存取邊界

**原則：同一台機器上，Commander 直接讀 mur 的資料，不複製。**

```
~/.mur/
├── patterns/        ← mur owns (read/write), commander reads
├── workflows/       ← mur owns (read/write), commander reads
├── session/         ← mur owns (read/write), commander reads
├── config.yaml      ← mur config
└── commander/       ← commander owns (read/write), mur doesn't touch
    ├── config.toml
    ├── schedules/   ← cron/trigger definitions per workflow
    ├── executions/  ← execution history & results
    └── audit/       ← hash-chained audit log
```

**存取規則：**
- Commander **讀取** `~/.mur/workflows/` 和 `~/.mur/patterns/`（唯讀）
- Commander **寫入** `~/.mur/commander/`（排程、執行記錄、audit）
- Commander 不修改 mur 的 YAML files
- 不需要 sync 機制或 `commander_sync` 欄位 — 同機器直接讀

**Commander 擴充欄位存在哪？**
- Commander 對 workflow 加的 schedule/notify/permissions 存在 `~/.mur/commander/schedules/{workflow-name}.yaml`
- 不汙染 mur 的 workflow YAML

**未來跨機器場景：**
- 如果需要跨機器（mur on laptop, commander on server），再設計 publish/subscribe 機制
- Commander 的雲端同步只同步 `commander/` 目錄，不動 mur 的原始資料

### 4. Workflow 類型轉換

mur-core Workflow → commander Workflow 轉換：

```
mur Step {                      commander Step {
  order: u32,                     name: String,         ← description
  description: String,            step_type: Execute,   ← 預設
  command: Option<String>,        action: String,       ← command.unwrap_or(description)
  tool: Option<String>,           on_failure: Abort,    ← 預設
}                               }
```

轉換是 **lossy**（commander 有更多欄位），但足以建立可執行的草稿。使用者在 commander 端可以進一步編輯加上 schedule、retry、breakpoint 等。

## Implementation Order

1. **Dashboard cleanup** — Commander nav items 條件化（30 min）
2. **Extract from Session API** — `POST /api/v1/workflows/extract-from-session/{id}`（1-2 hr）
3. **Session Review UI** — 加 "Extract Workflow" 按鈕 + preview（1-2 hr）
4. **Workflow Editor 頁面** — `/#/workflows/{id}/edit` 獨立編輯頁（1-2 hr）
5. **Commander sync** — `mur sync` 推 workflow 到 commander（1 hr）
6. **Rebuild & release** — build.sh + brew tap update

## Not Doing

- 不拆 mur-web codebase（不值得維護兩份程式碼）
- 不把 commander 的執行引擎搬進 mur-core（職責不同）
- 不做 commander → mur 反向 sync（避免衝突）
