# Pipeline Workflow Execution Design

> MUR CLI — Unix-style workflow composition
> Date: 2026-03-10

## 概念

讓用戶用一行指令組合多個 workflow，靈感來自 Unix philosophy：

```bash
mur run workflow1 | workflow2        # Pipeline: w1 的輸出餵給 w2
mur run workflow1 && workflow2       # Sequential: w1 成功才跑 w2
mur run workflow1, workflow2         # Parallel: 同時跑 w1 和 w2
```

## 設計原則

### 1. Pipe (`|`) — 資料流傳遞

**語意**：`workflow1` 的輸出（結構化結果）作為 `workflow2` 的輸入上下文。

**不是 Unix stdin/stdout pipe**。AI workflow 的輸出是結構化的（JSON/YAML/text），不是 byte stream。我們定義自己的 pipe 語意：

```
w1.output → 注入為 w2 的 context/input
```

**實作方式**：
- 每個 workflow 執行完產生一個 `PipelineOutput`（JSON 結構）
- Pipe 運算子把前一個的 output 注入到下一個的 `{{input}}` 模板變數
- AI step 可以讀到 `{{input}}` 作為上下文

```yaml
# workflow2.yaml
steps:
  - name: process-data
    prompt: |
      根據以下資料進行分析：
      {{input}}
      
      請產出摘要報告。
```

**PipelineOutput 結構**：
```rust
pub struct PipelineOutput {
    pub workflow_id: String,
    pub status: PipelineStatus,      // Success | Failed | Skipped
    pub output_text: Option<String>,  // 人類可讀的輸出
    pub output_data: Option<serde_json::Value>,  // 結構化資料
    pub exit_code: i32,
    pub duration_ms: u64,
}
```

### 2. Sequential (`&&`) — 成功才繼續

**語意**：跟 shell 完全一樣。`w1` exit code 0 才跑 `w2`。

**差異 vs pipe**：
- `&&` 不傳遞資料，只看成功/失敗
- `|` 傳遞資料（前一個的 output 是後一個的 input）

### 3. Parallel (`,`) — 同時執行

**語意**：`w1` 和 `w2` 同時啟動，各自獨立跑。

**注意**：逗號語意可能跟 pipe 混淆。考慮其他選項：
- `mur run workflow1, workflow2` — 平行
- `mur run workflow1 + workflow2` — 平行（更直覺）

**等待策略**：全部完成才回傳，或 `--fail-fast` 任一失敗就停止。

### 4. 混合組合

```bash
# w1 的輸出餵給 w2，w2 成功後平行跑 w3 和 w4
mur run w1 | w2 && w3, w4

# 解析優先順序：| > && > ,
# 等同於：(w1 | w2) && (w3, w4)
```

## CLI Parser 設計

```rust
/// Pipeline expression AST
pub enum PipelineExpr {
    /// Single workflow
    Single(String),
    /// w1 | w2 — pipe output
    Pipe(Box<PipelineExpr>, Box<PipelineExpr>),
    /// w1 && w2 — sequential (success-gated)
    Sequential(Box<PipelineExpr>, Box<PipelineExpr>),
    /// w1, w2 — parallel
    Parallel(Vec<PipelineExpr>),
}
```

**解析規則**（優先順序高到低）：
1. `|` Pipe（最高優先）
2. `&&` Sequential
3. `,` Parallel（最低優先）

**範例解析**：
```
"w1 | w2 && w3, w4"
→ Parallel([
    Sequential(
      Pipe(Single("w1"), Single("w2")),
      Single("w3")
    ),
    Single("w4")
  ])
```

等等，這不對。重新想：

```
"w1 | w2 && w3, w4"
→ 按照 `,` 分割：["w1 | w2 && w3", "w4"]
→ "w1 | w2 && w3" 按 `&&` 分割：["w1 | w2", "w3"]
→ "w1 | w2" 按 `|` 分割：["w1", "w2"]

結果：
Parallel([
  Sequential(
    Pipe(Single("w1"), Single("w2")),
    Single("w3")
  ),
  Single("w4")
])
```

## Pipeline Executor 設計

```rust
pub struct PipelineExecutor {
    workflow_store: WorkflowYamlStore,
    // Future: LLM client for AI-powered steps
}

impl PipelineExecutor {
    /// Execute a pipeline expression, returning the final output.
    pub async fn execute(
        &self,
        expr: &PipelineExpr,
        input: Option<PipelineOutput>,
    ) -> Result<PipelineOutput> {
        match expr {
            PipelineExpr::Single(id) => {
                self.run_single(id, input).await
            }
            PipelineExpr::Pipe(left, right) => {
                let left_output = self.execute(left, input).await?;
                if left_output.status != PipelineStatus::Success {
                    return Ok(left_output); // Don't pipe to right if left failed
                }
                self.execute(right, Some(left_output)).await
            }
            PipelineExpr::Sequential(left, right) => {
                let left_output = self.execute(left, input).await?;
                if left_output.exit_code != 0 {
                    return Ok(left_output); // && semantics: stop on failure
                }
                self.execute(right, None).await // No data transfer
            }
            PipelineExpr::Parallel(exprs) => {
                let handles: Vec<_> = exprs.iter().map(|e| {
                    let executor = self.clone();
                    let expr = e.clone();
                    tokio::spawn(async move {
                        executor.execute(&expr, None).await
                    })
                }).collect();
                
                let results = futures::future::join_all(handles).await;
                // Merge results...
                todo!()
            }
        }
    }
}
```

## Web UI 編排（localhost:3847/#/workflows）

在 Web UI 加入 **Pipeline Builder**：

### 方案：Visual Pipeline Editor

```
┌─────────┐     ┌─────────┐     ┌─────────┐
│   w1    │──|──│   w2    │──&&─│   w3    │
└─────────┘     └─────────┘     └─────────┘
                                     │
                                     && 
                                     │
                                ┌─────────┐
                                │   w4    │
                                └─────────┘
```

- 拖拉 workflow 卡片到畫布
- 用連接線選擇關係（pipe / sequential / parallel）
- 自動生成 pipeline 指令
- 可儲存為 "meta-workflow"

### API

```
POST /api/pipelines
{
  "id": "my-pipeline",
  "expression": "w1 | w2 && w3",
  "description": "..."
}

POST /api/pipelines/:id/run
→ 回傳 execution ID，可透過 WebSocket 追蹤進度
```

## 實作計劃

### Phase 1: Core Pipeline Parser + Executor（mur-core v2）
**目標**：`mur run w1 | w2 && w3` 可以執行

1. `mur-common/src/pipeline.rs` — PipelineExpr AST + Parser
2. `mur-core/src/executor/pipeline.rs` — PipelineExecutor
3. `mur-core/src/cmd/workflow.rs` — 擴充 `cmd_workflow_run` 支援 pipeline 語法
4. 測試

### Phase 2: PipelineOutput + Input Injection
**目標**：`|` 真正傳遞資料

1. 定義 PipelineOutput 結構
2. Workflow YAML 支援 `{{input}}` 模板變數
3. Executor 注入 input context

### Phase 3: Parallel Execution
**目標**：`,` 平行執行

1. tokio::spawn 平行化
2. 輸出合併策略
3. `--fail-fast` flag

### Phase 4: Web UI Pipeline Builder
**目標**：視覺化編排

1. Pipeline Builder 元件（React）
2. Pipeline CRUD API
3. 即時執行追蹤（WebSocket）

### Phase 5: Commander Integration
**目標**：在 Slack/Telegram 直接打 pipeline

1. Intent 辨識支援 pipeline 語法
2. 即時進度回報（用 TypingIndicator）

## ⚠️ 重要 Note

**mur-core v2 是 Rust**（`~/Projects/mur`），不是 Go。
之前 MUR Commander 的 agentic loop 搞錯語言寫了 Go 代碼，已清除。
所有實作必須是 Rust，在 workspace crates 裡。
