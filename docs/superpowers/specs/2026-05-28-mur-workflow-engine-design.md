# MuR Workflow Engine — Session → Workflow → Replay

> **Status:** Approved design | **Date:** 2026-05-28

## Decision

**砍掉 Pattern（自動學習）整條 pipeline，聚焦 Session Recording → Workflow Extraction → Replay。**

Pattern injection 在實際使用中從未運作過（injection_count = 0 across all 42 patterns）。Emergence pipeline 在 LLM summarization 被關閉後，降級為 fingerprint counting，產出 62% 結構性垃圾。舊版 LLM summarization pipeline（Problem/Solution/Why 格式）品質較好但因 token 成本過高而在 5 月初被關閉。

Pattern 功能的核心矛盾無法解決：價值延遲（2-4 週）+ 成本立即感受（token）+ 品質不可驗證。Workflow 解決全部三個問題：價值立即（`mur run` 直接執行）、成本可控（只在「完成一件事」時才呼叫 LLM judge）、品質可觀測（exit code 判定成功/失敗）。

## Architecture: Four-Layer Pipeline

```
Session Recording ──→ Extraction ──→ Web Editor ──→ Replay/Evolution
     (Layer 1)        (Layer 2)       (Layer 3)        (Layer 4)
```

### Layer 1: Recording（強化現有 session JSONL）

在 `SessionEvent` 加入 execution metadata，讓 extraction 有足夠訊號判斷「這是一個可重覆的工作流程」。

```rust
// mur-core/src/session/mod.rs — SessionEvent 擴展
pub struct SessionEvent {
    pub timestamp: u64,
    pub event_type: String,       // "tool_call" | "tool_result" | "user_message" | "completion"
    pub tool: Option<String>,
    pub content: String,
    // ── NEW ──
    pub exit_code: Option<i32>,       // 指令成功(0)或失敗(!0)
    pub working_dir: Option<String>,  // 在哪個目錄下執行
    pub git_branch: Option<String>,   // 哪個 branch
    pub detected_vars: Option<HashMap<String, String>>, // LLM 初步標記的可變參數
}
```

### Layer 2: Extraction（LLM Judge + 手動截取）

**A. 自動提取（LLM 主動通知）**

觸發條件（三者同時滿足才呼叫 LLM，控制成本）：
1. 累積 ≥ 8 輪實質對話（排除打招呼/閒聊）
2. 偵測到「任務完成」訊號（連續 2+ tool_results 的 exit_code = 0）
3. 偵測到 ≥ 3 個相關 tool calls 構成步驟序列

LLM prompt 只做 JUDGE，不做 GENERATE：
- 判斷：這個 session 的步驟序列是否可重覆？
- 判斷：哪些值是變數？
- 如果可重覆 → 產生 Workflow draft（title + steps + variables）
- 如果不行 → 回 NOOP

通知格式：
```
💡 偵測到可重覆的 workflow: "Deploy to Fly.io"
   1. cargo build --release
   2. fly deploy --app {{app_name}}
   3. curl {{health_check_url}}
   [接受並編輯] [忽略] [稍後]
```

**B. 手動截取**

- `mur-in` 開始標記（或整個 session 已在 recording）
- 工作完成後 `mur-out` → 自動 extraction → 可選打開 Hub web editor

### Layer 3: Web Editor（Hub Workflow Editor）

Hub 內的 workflow 編輯器：
- Steps 列表（拖放排序、編輯 command/tool/timeout/on_failure）
- Variables 管理（從 extraction 標記的變數，可編輯 default value）
- Test Run 按鈕（在 sandbox 中試跑）
- 狀態：Draft / Published

### Layer 4: Replay + Evolution（執行 + 自動生命週期）

**執行：** `mur run <workflow-name>` 或 `mur workflow run <name>`

**自動狀態機（可觀測訊號驅動）：**

```
Draft ──(首次成功)──→ Active ──(10次成功)──→ Trusted ──(50次+人審)──→ Canonical
  │                      │                    │
  │                  連續失敗 3 次               │
  │                      ↓                    │
  │                   Broken ←─────────────────┘
  │                      │
  │                  30 天未修復
  │                      ↓
  └──────────────→ Trash ──(30天)──→ 永久刪除
```

| 訊號 | 來源 | 動作 |
|---|---|---|
| 首次執行成功 | `mur run` exit 0 | Draft → Active |
| 累積 10 次成功 | run history counter | Active → Trusted |
| 連續 3 次失敗 | run history（最近 3 次 exit != 0） | → Broken + 通知用戶 |
| 30 天未修復 | Broken + last_modified | Broken → Trash |
| 90 天未執行 | last_run timestamp | 降低推薦優先級（stale） |
| 累積 50 次成功 + 人審 | run history + manual flag | Trusted → Canonical（團隊共享） |

## What We Remove

- `mur-core/src/capture/` — emergence.rs, noise_filter.rs（fingerprint-based extraction）
- `mur-core/src/evolve/decay.rs` — 半衰期猜測邏輯（改為觀測驅動的 workflow 狀態機）
- `~/.mur/patterns/` — 現有 42 個 MUR schema patterns（26 個結構性垃圾）
- `~/.mur/archive/patterns/` — 24 MB 版本歷史
- `~/.mur/fingerprints.jsonl` — 4.1 MB noise log
- `mur-common/src/pattern.rs` 中的硬性 decay/maturity 邏輯

## What We Keep

- `mur-common/src/workflow.rs` — Workflow 型別（已有 Step, Variable, trigger, tools, lifecycle）
- `mur-core/src/session/` — Recording 基礎（mod.rs, scrub.rs）
- `mur-core/src/inject/hook.rs` — 改為注入推薦 workflows（而非 patterns）
- `mur-core/src/retrieve/` — 檢索 pipeline（改為匹配 workflows）
- `~/.mur/workflows/` — Workflow 儲存
- `~/.mur/session/recordings/` — Session 記錄

## Competitive Positioning

| | Archon (21K⭐) | Tembo | Sekko | **mur** |
|---|---|---|---|---|
| Session Recording | ❌ | ❌ | ✅ (browser+term) | ✅ (AI coding) |
| Workflow Extraction | ❌ (手寫YAML) | ❌ | ✅ (→markdown) | ✅ (→executable) |
| Web Editor | ✅ (DAG builder) | ❌ | ❌ | ✅ (Hub) |
| Replay/Execute | ✅ | ✅ (async sandbox) | ❌ | ✅ (mur run) |
| Evolution Engine | ❌ (Git only) | ❌ | ❌ | ✅ (觀測驅動) |

**核心差異化：Session → Workflow 自動提取。沒有人把這四步串起來。**

## Development Phases

| Phase | 內容 | 時間 |
|---|---|---|
| **P1: Clean Slate** | 砍掉 capture/ + evolve/ + 垃圾 patterns | 2-3 天 |
| **P2: Recording 強化** | SessionEvent 擴展 + mur-in/mur-out 修復 | 1 週 |
| **P3: Extraction (LLM Judge)** | 完成訊號偵測 + LLM judge prompt + 通知 | 1.5 週 |
| **P4: Workflow Evolution Engine** | 狀態機 + broken detection + trash 清理 | 1 週 |
| **P5: Hub Workflow Editor** | Web 編排器（step 編輯、variable 管理、test run） | 2 週 |
| **P6: Team Sharing** | Canonical workflow publish → team 可用 | 2 週 |
