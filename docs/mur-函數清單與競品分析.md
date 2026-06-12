# mur — 函數清單與競品深度分析

> 初版日期：2026-04-15
> 更新日期：2026-04-18 — 新增 mur-commander 執行層對比、補充 6 個競品 (Text2Mem / memU / Hermes / claude-mem / ReMe / Karpathy LLM Wiki)、擴充戰略缺口至 10 條、加入關鍵數據點
> 範圍：`/Volumes/Firecuda4tb/Projects/mur` (Rust workspace: `mur-common` + `mur-core`) + `/Volumes/Firecuda4tb/Projects/mur-commander` (Rust workspace: 8 crates, daemon + scheduler + MCP)

---

## 第一部分 — 程式碼函數清單

mur 是一個 Rust Cargo workspace,包含兩個 crate:
- **`mur-common`** — 共享型別,無邏輯無 I/O
- **`mur-core`** — CLI 邏輯與 `mur` 二進位檔,依循四階段管線架構

核心管線:**`capture → store → retrieve → inject`**,加上橫跨所有階段的 **`evolve`** 模組。

### CLI 指令樹 (來自 `main.rs` 的 clap 定義)

| 分組 | 指令 |
|---|---|
| 管線核心 | `new`, `search`, `inject`, `run`, `sync` |
| 生命週期管理 | `pin`, `mute`, `boost`, `promote`, `deprecate`, `gc`, `feedback {helpful\|unhelpful\|auto}` |
| 學習與演化 | `learn {extract\|cross}`, `emerge`, `evolve {--consolidate\|compose\|cooccurrence}`, `links` |
| 工作流程 | `workflow {list\|show\|search\|new\|publish\|install\|schedule}` |
| 會話與情境 | `session {start\|stop\|record\|status\|list\|review\|show\|export\|push}`, `in`, `out`, `context` |
| 社群與團隊 | `community {publish\|fetch\|search\|list\|star\|report\|packs}`, `team {list\|share\|sync}`, `login`/`logout` |
| 工具類 | `init`, `doctor`, `stats`, `verify`, `import`, `exchange`, `serve`, `dashboard`, `deploy`, `gep`, `why`, `edit`, `reindex` |

---

### 階段 1 — `capture/` 擷取與信號萃取

**`noise_filter.rs`** — 將對話內容分類為信號或噪音
- `pub fn filter(text: &str) -> FilterResult` — 回傳 Pass / Noise(reason)
- `NoiseReason`: `TooShort`, `Greeting`, `SingleWord`, `EmojiOnly`, `ShortCjk`, `Boilerplate`
- 輔助: `cjk_char_count`, `is_cjk_text` (CJK 字元比例 > 50% 判定)

**`emergence.rs`** — 跨 session 浮現模式偵測
- `extract_fingerprints(transcript, session_id) -> Vec<BehaviorFingerprint>` — 從對話記錄萃取行為指紋
- `detect_emergent(threshold) -> Vec<EmergentCandidate>` — 尋找出現於 ≥ N 個 session 的模式
- `jaccard_similarity(a, b) -> f64` — 集合相似度
- `save_fingerprints` / `load_fingerprints` / `prune_fingerprints(max_age_days)` — 指紋持久化
- `generate_suggested_name(keywords)` — 候選模式自動命名

**`feedback.rs`** — session 結束後的成效分析
- `analyze_session_feedback(transcript, injected) -> Vec<SessionFeedback>`
- `write_injection_record(...)` → 寫入 `~/.mur/last_injection.json`
- 型別: `InjectedPatternRecord`, `InjectionRecord`, `SessionFeedback`, `SignalType {Reinforced|Contradicted|Ignored}`

**`starter.rs` / `import.rs`** — 種子模式;從 `.cursorrules`, `CLAUDE.md` 匯入。

---

### 階段 2 — `store/` 持久化與向量索引

**`yaml.rs` — `YamlStore`** (真實來源,atomic temp-then-rename 寫入)
- `new`, `default_store`, `list_names`, `list_all`, `get`, `save`, `delete`, `archive`, `exists`
- `pattern_assets_dir`, `copy_diagram_to_assets`, `resolve_attachment_content` — 圖表附件管理
- `default_patterns_dir() -> ~/.mur/patterns/`, `default_mur_dir() -> ~/.mur/`

**`lancedb.rs` — `VectorStore`** — LanceDB 封裝 (`SearchResult`,建立索引 / 搜尋 / upsert)

**`embedding.rs`** — `embed(text, config)`; `EmbeddingProvider {OpenAI|Local|Ollama}`

**`workflow_yaml.rs` — `WorkflowYamlStore`** — 工作流 CRUD,結構對應 `YamlStore`

**`pipeline_yaml.rs` — `PipelineYamlStore` / `PipelineDef`** — 多步驟 pipeline 儲存

**`exchange.rs`** — MKEF 格式 (MUR Knowledge Exchange Format)
- `parse_mkef`, `mkef_to_pattern`, `pattern_to_mkef`
- `import_mkef_file`, `import_mkef_dir`, `export_mkef`

**`config.rs`** — `load_config` / `save_config` (`~/.mur/config.yaml`)

**`spot_rate.rs`** — LLM 成本快照快取

---

### 階段 3 — `retrieve/` 多信號排序

**`scoring.rs`** — 混合相關性評分公式:
```
W_RELEVANCE     0.45   (向量 0.7 + BM25 0.3)
W_RECENCY       0.10
W_EFFECTIVENESS 0.15
W_IMPORTANCE    0.15
W_TIME_DECAY    0.10
W_LENGTH_NORM   0.05
```
- 分數下限 0.42、最多注入 5 個模式、~2000 tokens 上限
- `score_and_rank_hybrid(query, candidates, vector_scores)` — 主要混合評分
- `score_and_rank_hybrid_with_scope(..., scope, project_language)` — 加入使用者/平台情境
- `score_and_rank_hybrid_with_scope_and_config(..., config)` — 使用設定驅動參數
- 備援純關鍵字版本: `score_and_rank`, `_with_scope`, `_with_config`
- 型別: `ScoredPattern`, `ScopeContext`

**`gate.rs`** — `evaluate_query(query) -> GateDecision {Allow|Deny|RequireContext}` — 惡意/垃圾查詢過濾

---

### 階段 4 — `inject/` 提示詞格式化與工具擴散

**`hook.rs`** — 格式化模式供 AI 工具注入
- `detect_trigger(message) -> HookTrigger {SessionStart|OnError|OnRetry|Manual}`
- `format_for_injection(patterns, max_tokens)` / `_with_store` — 扁平格式
- `format_grouped_injection` — 依 `PatternKind` 分組
- `format_pattern_entry`, `format_workflow_entry`, `format_unified_injection`
- `record_injection(query, project, patterns)`, `record_cooccurrence_for_injection`

**`sync.rs`** — 寫入各 AI 工具設定檔
- `default_targets() -> Vec<SyncTarget>` — 自動偵測 `.cursorrules`, `.claude/instructions`, Gemini CLI 等
- `generate_sync_content(patterns, format)` — `SyncFormat {Markdown|JSON|YAML}`
- `write_sync_file(path, content, format)`

---

### 階段 5 — `evolve/` 衰退、成熟、連結、組合

- **`decay.rs`** — `calculate_decay(pattern, now) = confidence × 0.5^(days / half_life)`;`apply_decay_all`, `apply_decay_all_dry_run`
- **`lifecycle.rs`** — `evaluate_lifecycle(pattern) -> LifecycleAction {None|Promote|Deprecate|Archive}`
  - 規則:90 天無注入或成效 <0.3 → Deprecate;Deprecate 後 180 天 → Archive
  - 晉升:session → project (≥5 注入 + 0.7 成效);project → core (≥3 專案 + 0.8 成效)
  - Pinned 模式免疫於廢棄/封存,但仍可晉升
- **`maturity.rs`** — `evaluate_maturity`, `apply_maturity_all` (Draft → Emerging → Stable → Canonical)
- **`feedback.rs`** — `apply_feedback(pattern, signal)`;`FeedbackSignal {Helpful|Unhelpful|Contradicted|Ignored}`
- **`cooccurrence.rs`** — `CooccurrenceMatrix`, `PatternCluster` 共現矩陣與叢集
- **`linker.rs`** — Zettelkasten 連結
  - `discover_links`, `apply_links`;`LinkType {DependsOn|AlternativeTo|Composition|Refinement}`
  - 工作流變體: `discover_workflow_links`, `apply_workflow_links`
- **`compose.rs`** — `suggest_workflows(matrix, threshold)`, `_with_patterns` — 從共現挖掘工作流建議
- **`decompose.rs`** — `analyze_workflow_for_extraction`, `extract_pattern_from_step`
- **`consolidate.rs`** — `consolidate(store, dry_run) -> ConsolidationReport` 一次性的 decay + maturity + lifecycle 清理
- **`commander_bridge.rs`** — `workflow_exists`, `fact_to_pattern`, `pattern_to_rule`, `should_replace`
- **`mod.rs`** — `suggest_commander_workflows`

---

### 核心資料型別 — `mur-common`

- **`Pattern`** 透過 `#[serde(flatten)]` 內嵌 `KnowledgeBase`,YAML 保持扁平無 `base:` 巢狀鍵
  - 欄位: `name`, `kind`, `content`, `tier`, `importance`, `confidence`, `tags`, `applies`, `evidence`, `links`, `lifecycle`, `maturity`, `decay`
  - `Deref<Target = KnowledgeBase>` 允許直接存取 `pattern.name`
- `PatternKind {Technical|Fact|Procedure|Preference|Behavioral}`
- `Content {Code|Markdown|Url|Reference}`
- `Tier {Session|Project|Core}` — 半衰期:14 天 / 90 天 / 365 天
- `LifecycleStatus {Active|Deprecated|Archived}`
- `Evidence {success_signals, override_signals, failure_signals, injection_count, last_validated, effectiveness()}`
- `Lifecycle {status, pinned, muted, last_injected, decay_half_life, created_at, updated_at}`
- `Links {depends_on, alternatives, compositions, refinements}`, `Attachment`, `Tags`
- `Workflow {name, steps, variables, schedule, schema_version}`, `Step`, `Variable`
- `Pipeline` (執行與參數代換)

---

### 支援模組

- **`verify.rs`** — 文件驗證引擎
  - `Claim {FilePath|Command|CodeRef}`, `VerifyResult`
  - `collect_commands_from_clap(cmd)` — 執行時期從 clap 樹自動抽取已知指令
- **`server.rs`** — Axum HTTP/WebSocket 服務
  - `AppState`, `AppError`, `build_router`
  - REST endpoints: `GET/POST /patterns`, `/workflows` 等
- **`context_api.rs`** — 專案感知的情境注入 API
- **`community.rs`** — 社群中心整合;`CommunityPattern`, `sanitize_pattern` (發布前移除敏感資料)
- **`dashboard.rs`** — `render_dashboard()` ratatui TUI
- **`auth.rs`** — Device-flow OAuth
  - `AuthTokens`, `load_tokens`, `save_tokens`, `authenticated_client`
- **`session.rs`** — `SessionRecord` 紀錄與雲端推送,包含祕密清理
- **`interactive.rs`** — `dialoguer` 互動式模式建立 REPL
- **`executor/pipeline.rs`** — `PipelineExecutor` 工作流執行引擎
- **`cmd/*.rs`** — 每個指令群組的 `pub(crate)` handler 檔案
  (`pattern.rs`, `workflow.rs`, `evolve_cmd.rs`, `inject_cmd.rs`, `context.rs`, `session.rs`, `learn.rs`, `misc.rs`, `server_cmd.rs`, `sync_cmd.rs`, `community_cmd.rs`, `init.rs`, `verify.rs`, `reindex.rs`, `deploy.rs`)

---

## 第二部分 — 競品深度分析

### 1. 專門的 AI Agent 記憶框架

| 工具 | 儲存模型 | 整合方式 | 檢索方法 | 授權 | 差異化特色 |
|---|---|---|---|---|---|
| **Mem0** | 向量 + 圖 (`Mem0ᵍ`) + KV | Py/TS SDK + SaaS,21+ 框架整合 | 向量相似度 + 圖遍歷 | Apache + 託管 | 產品級 SaaS,已發表論文;準確率 +26%、token 節省 90%。⚠️ 生產環境 audit 發現 10,134 條記憶中 **97.8% 為 junk** (issue #4573) |
| **Letta** (原 MemGPT) | 分層:Core / Recall / Archival | 自架 server + REST/SDK;Agent 跑在 Letta 內部 | LLM 透過工具呼叫自行編輯記憶 | Apache + 託管 | Agent 管理記憶的典範 ("LLM-as-OS");**Sleeptime agent** 每 N 步 (預設 5) 於 idle 時觸發 compaction,主/背景 agent 可用不同模型 |
| **Zep / Graphiti** | 時序知識圖 (episodes/semantic/communities),雙時間 | 託管 + 自架 Graphiti (Neo4j/FalkorDB) | 圖遍歷 + 有效期窗口 | OSS Graphiti + SaaS | 雙時間 (bi-temporal) 時空穿梭查詢;DMR benchmark 94.8% |
| **Cognee** | 向量 + 圖 + 關聯式;內嵌 SQLite + **LanceDB** + Kuzu | Python SDK (`add/cognify/search`) | GraphRAG (向量 + 圖擴展) | Apache,種子輪 $7.5M | 本體論感知 GraphRAG;架構上最接近 mur |
| **Memobase** | SQL 結構化使用者檔案 | Py/Node/Go SDK + REST,p95 <100ms | SQL 查詢 | OSS + 託管 | 非 RAG;將記憶模型化為使用者 CRUD |
| **A-MEM** | ChromaDB + 結構化筆記 + 自動連結 | 研究倉庫 (MIT),Python | 動態筆記組織,自動連結 | OSS 研究 | 卡片盒 (Zettelkasten) 自我連結 — mur 哲學上最接近的表親;**NeurIPS 2025 錄取**;三階段 Note→Link→Evolution,新筆記觸發舊筆記更新 |
| **ReMe** (阿里 AgentScope) | Markdown 檔 + 向量索引 | Python SDK | 0.7 向量 + 0.3 BM25 (與 mur 同比) | Apache | `.reme/MEMORY.md` + `memory/YYYY-MM-DD.md`,**單檔索引**完全透明可編輯,對話壓縮自動存 `{dialog_path}/{date}.jsonl` |
| **memU** (NevaMind) | 檔案系統隱喻:類別=資料夾,item=檔,cross-link=symlink | Python SDK (PyPI `memu-py`,2026-02 釋出) | 分層 + **Intention Layer** 預測 | Apache | 為 24/7 主動 agent 設計,**預測用戶下一步**並 pre-fetch;cheap-monitor / deep-reason 雙模式 |

### 1.5 記憶操作協議層 (新興類別,mur 未涉入)

| 工具 | 定位 | 關鍵創新 |
|---|---|---|
| **Text2Mem** (arxiv 2509.11145) | **記憶操作的統一 IR**,"SQL for memory" | 12 原子動詞 (Encode/Retrieve/Update/...) × 五元 JSON 契約 (`stage`/`op`/`target`/`args`/`meta`) + Schema+Pydantic 雙層驗證 + 鎖/過期語意 |

Text2Mem 不儲存記憶,而是定義**跨後端的操作語言**。這是記憶領域的「抽象層」創新,mur 的 `pin/mute/boost/promote/deprecate/edit` 若採用類似 IR 可讓外部 agent 標準化操控。

### 1.6 主動式 / 24/7 Agent 記憶 (新興類別)

| 工具 | 主動機制 | 和 mur 的差異 |
|---|---|---|
| **memU** | Intention Layer 預測下一步類別 + 背景 agent 24/7 運作 | mur 是 trigger 被動式,無預測 |
| **Hermes Agent** (Nous Research) | 技能閉環 (任務後自創技能,使用中自改進);FTS5 跨 session 搜尋 + Honcho 用戶建模;跨 5 平台 (TG/Discord/Slack/WhatsApp/Signal) | Hermes 偏「AI agent 自我演化」,mur 偏「人類可控的 pattern 成熟度」 |
| **Letta Sleeptime** | 主 agent idle 時觸發,合併 memory blocks;可用更大/更便宜模型 | mur `evolve` 是手動/cron,未與執行層 idle 聯動 |

### 2. 程式碼助手情境系統

| 工具 | 模型 | vs. mur |
|---|---|---|
| **Claude Code `CLAUDE.md` + Skills** | 靜態永遠載入 + 名稱比對技能 | 原生、零基礎設施,但全為人工撰寫 |
| **Cursor Rules `.cursorrules`** | 靜態純文字 | 更簡單;單一工具 |
| **Continue.dev** | `config.yaml` 的 `rules` 區塊 | 靜態、人工維護 |
| **Aider / `AGENTS.md`** | 單一靜態檔案,新興跨工具標準 (Codex CLI、Aider、Gemini) | 全有或全無,無評分 |
| **claude-mem** (thedotmack) | Claude Code plugin;**5 個生命週期 hook** (SessionStart → UserPromptSubmit → PostToolUse → Stop → SessionEnd);Bun worker (:37777) + SQLite + Chroma | **漸進式披露** 3 層注入 (search 50-100 tokens → timeline → details);mur 目前一次全展,可借鑒其節 token 策略;僅支援 Claude Code 單工具 |

**共同屬性:** 皆為靜態、人工策劃、單工具、無跨 session 學習 (claude-mem 是例外:有跨 session 但也只針對 Claude Code)。

### 3. RAG 函式庫記憶

- **LangChain / LangGraph** — checkpoint 狀態 + 向量/摘要記憶區塊
- **LlamaIndex** — `VectorMemoryBlock` 可組合的對話歷史檢索

皆為基礎元件,不是產品。需要自行接線;無生命週期、無外部工具注入。

### 4. AI 強化的個人知識管理

- **Obsidian + Smart Connections** — 本地 markdown + embeddings 對話
- **Logseq** — 大綱式編輯器 + 社群 AI 外掛
- **Reor** — 桌面筆記 + 本地 LLM

以人為中心;AI *讀取* 筆記,不會 *寫回* 到程式碼 agent。

### 5. 設計哲學 / 架構模式 (非產品)

- **Karpathy LLM Wiki** (gist 442a6bf...) — 提出「LLM 當全職圖書管理員」模式,三層架構 (raw 源文件 / wiki 層 markdown / schema 規則),主張以**主動維護的結構化知識庫**取代 RAG。靈感源自 Vannevar Bush 的 Memex。mur 的 `~/.mur/patterns/*.yaml` 實際上已部分體現這個哲學,差別在 mur 的「lint」是 evolve 模組的自動化 (decay/maturity/lifecycle),而非依賴 LLM 即興判斷。
- 批評意見 (來自 gist 留言):純 LLM 維護的 Wiki 超過百頁後會失控,需要結構化資料庫與人類驗證的混合方案 — **這正是 mur 已經在做的**。

---

## mur 的獨特定位

### 獨到之處

1. **本地優先、YAML 作為真實來源**
   其他所有工具要不是 SDK+服務、就是不透明的資料庫檔案。`~/.mur/skills/` 與 `workflows/*.yaml` 可手動編輯、git 友善,LanceDB 索引可透過 `mur internals reindex` 隨時重建。僅 PKM 工具有同等透明度,但它們不注入到 agent。

2. **多工具 hook 注入**
   此列表中沒有任何工具能 *同時* 將記憶散播到 Claude Code、Cursor、Gemini CLI、Aider (透過各自原生 hook/設定)。Mem0/Zep/Letta 要求 *你的 agent* 去呼叫他們的 API;Cursor Rules/CLAUDE.md/AGENTS.md 是工具特定靜態檔。mur 是**跨工具 sidecar**。

3. **模式成熟度生命週期 + 分層半衰期**
   Draft → Emerging → Stable → Canonical;session 14 天 / project 90 天 / core 365 天。Mem0 有重要性評分、Zep 有時間有效性,但沒有任何競品建模出 *成熟度曲線* —— 模式依重複證據逐步畢業。A-MEM 最接近但是是 Python 研究原型。

4. **單一原生 Rust 二進位**
   整個競爭領域都是 Python。無執行時、無常駐服務,BM25 + 向量在程序內完成 —— 效能與安裝特性截然不同。

5. **直接從 session transcript 擷取**
   mur 的 `capture/` 直接解析 Claude/Cursor/Gemini 錄音 (噪音過濾 → 顯著性 → 浮現 → 回饋)。Mem0/Zep/Letta 只從自己 SDK 經手的流量學習。

6. **透明且已發佈的評分配方**
   權重、下限、上限、半衰期皆在程式碼與文件中。競品隱藏在服務後面。

### 重疊之處

- **Zettelkasten 連結 + 共現** → A-MEM
- **混合 BM25 + 向量** → Mem0, Cognee
- **LanceDB 後端** → Cognee 預設堆疊
- **從對話萃取模式/事實** → Mem0, Zep (概念上)
- **靜態規則檔用途** → CLAUDE.md / Cursor Rules / AGENTS.md (但 mur 是動態排序而非永久開啟)

### 競品較強之處

1. **時序推理** — Zep 的雙時間圖能回答「三月時我相信什麼?」;mur 只有指數衰退
2. **圖/關聯式推理** — Mem0ᵍ、Zep、Cognee 都有真實的實體關係圖;mur 有連結但無實體萃取或圖遍歷查詢
3. **使用者個人化** — Memobase 的結構化檔案為聊天機器人個人化量身打造;mur 並非鎖定此用途
4. **託管多租戶規模** — Mem0、Zep、Letta 都提供代管服務;mur 依設計本地優先
5. **生態系成熟度** — Mem0 單獨就有 21+ 框架整合、論文、benchmark;mur 整合面較窄
6. **基準驗證的檢索品質** — Zep (94.8% DMR)、Mem0 (已發表論文) 有硬數據;mur 尚未發表可比評測
7. **Agent 自管記憶** — Letta 的 MemGPT 迴圈 (agent 決定要記什麼) 對自主 agent 而言是嚴格更強的範式;mur 是被動的,它只觀察 session

### 定位總結

mur 佔據了沒有任何競品直接瞄準的防禦性利基:**為使用多個 AI 程式助手的人類開發者服務的本地優先、YAML 原生、跨工具模式記憶**。

- **Mem0/Zep/Letta** → 為 *你建造的 agent* 提供記憶即服務
- **Cursor Rules / Claude Skills / AGENTS.md** → 為 *單一工具* 的靜態檔案
- **A-MEM** → 哲學上最接近 (Zettelkasten、自動連結),但為 Python 研究原型
- **Cognee** → 架構上最接近 (內嵌 LanceDB + GraphRAG),但以文件為導向且 Python-SDK-first

**"Rust 二進位 + 手動可編輯 YAML + 多工具 hook 擴散 + 生命週期演化"** 這個組合在目前競爭版圖中無人複製。

---

## 第三部分 — mur-commander 執行層對比

> 範圍:`/Volumes/Firecuda4tb/Projects/mur-commander` v0.7.3,8 crates (engine / daemon / cli / gateway / chat / web / browser / code-review)

mur 生態不只是「記憶庫」。**mur-commander 是執行引擎**,與 mur CLI 形成完整閉環:

```
   記憶層              執行層                    通道層
  ┌─────┐           ┌──────────────┐        ┌─────────────┐
  │ mur │◄──pattern►│ mur-commander│◄──────►│ Slack/TG/   │
  │ CLI │  (mur-    │  (daemon +   │  SSE   │ Discord/Web │
  │     │  common   │   scheduler  │        │             │
  │YAML │  v2.2.4   │   + triggers │        └─────────────┘
  │Lance│  共享型別) │   + MCP)     │
  └─────┘           └──────────────┘
```

### 3.1 mur-commander 能力清單

| 模組 | 職責 | 關鍵技術 |
|---|---|---|
| `engine/` | workflow 執行、憲法檢查、模型路由 | Constitution (Ed25519 簽名政策)、ModelRouter、AuditStore、WorkflowRunner (runner.rs) |
| `daemon/` | 常駐、IPC、排程、觸發、SSE | Unix socket IPC、cron + NL schedule、4 類 trigger (FileChange/GitPush/ErrorPattern/Webhook) |
| `cli/` (`murc`) | 用戶介面 | `run` / `schedule` / `policy` / `machines` / `export` / `publish` / `rollback` |
| `gateway/` | 聊天平台適配 | Slack / Telegram / Discord |
| `web/` | HTTP API + 前端 | `/api/workflows` / `/api/schedules` / `/api/triggers` / `/api/executions` / webhook endpoints |
| `browser/` | Playwright agent | 多用戶 auth |
| `code-review/` | PR 分析 | 整合 LLM |

### 3.2 執行層的三層記憶 (尚未與 mur pattern tier 對齊)

`crates/engine/src/memory/` 自有三層架構:
- **Working Memory** (`conversation.rs`):最近 50 msg,直接注入 LLM context
- **Short-term store**:會話內向量搜尋,記憶體,重啟即清空
- **Long-term store**:磁碟持久化,跨 session

**重要 gap**:commander 的 long-term 目前**沒有寫回 `~/.mur/patterns/`**,沒有經過 mur 的 Evidence/Maturity 升層流程。兩邊共用 `mur-common::schedule` 但不共用記憶協議。

### 3.3 執行層獨家能力 (競品沒有)

| 能力 | mur-commander | 競品情況 |
|---|---|---|
| **Constitution** (Ed25519 簽名政策) | ✅ `ActionDecision {Allowed/NeedsApproval/Blocked}`;`forbidden`/`requires_approval`/`auto_allowed` 三類;`max_api_cost_per_run/per_day` | 無人有 |
| **Shadow mode / Breakpoint** | ✅ 乾跑預覽 + 人工驗證暫停點 + resume | 部分 CI 工具有 dry-run,AI agent 領域無人有 |
| **AutoFix** (AI 自動修失敗 step) | ✅ `autofix.rs` | 無人有 |
| **DLQ** (Dead Letter Queue) | ✅ `DlqStore` | 企業排程器有,AI agent 領域無 |
| **Multi-machine SSH 執行 + A2A relay** | ✅ `machines add/list/health` | GitHub Actions self-hosted runner 有,但非 AI agent |
| **MCP sandbox + trust system** | ✅ `SandboxExecutor` + `McpCapability` | Hermes 有類似,Letta/Mem0 無 |
| **四類 Trigger** (FileChange/GitPush/ErrorPattern/Webhook) + **NL cron** ("every weekday 9am") | ✅ daemon 常駐 + notify crate watch | n8n/Zapier 有,AI agent 領域少見 |
| **Parallel + Conditional step** | ✅ ExecutionMode + AiRole 二元組 | GitHub Actions 有,Letta function-call 不支援 |
| **聊天平台整合** (SL/TG/DC) | ✅ gateway crate | Hermes 有 5 平台,Letta/Mem0 無 |

### 3.4 mur 生態 vs 市場其他「記憶+執行」方案

| 系統 | 記憶 | 執行 | 通道 | 閉環程度 |
|---|---|---|---|---|
| **mur + mur-commander** | YAML+Lance + 3 tier | daemon + triggers + MCP + SSH | Slack/TG/Discord | **★★★★★** |
| **Hermes Agent** | MEMORY.md + FTS5 | 40+ 工具 + sub-agent | 5 平台 | ★★★★★ |
| Letta | 3 tier blocks | tool calls + sleeptime | ❌ | ★★★★ |
| Mem0 | vec+graph+KV | ❌ (SaaS 不含執行) | ❌ | ★★ |
| memU | FS 隱喻 | Intention Layer 預測 | ❌ | ★★★ |
| claude-mem | SQLite+Chroma | Claude Code 本身 | ❌ | ★★★ |

mur 生態和 **Hermes Agent** 是市場上**僅有的兩個「記憶+執行+通道」三位一體**系統。差異:
- Hermes 偏「AI agent 自我演化 + 閉環學習」,記憶與技能耦合
- mur 偏「人類可控的 pattern 成熟度 + 獨立執行引擎」,記憶與執行解耦

### 3.5 mur-commander 目前的缺口

1. **執行層記憶未寫回 mur pattern 系統** — commander 的 long-term 應可作為 Session tier 進入 mur 升層流程
2. **daemon idle 時未主動演化** — `evolve`/`gc`/`consolidate` 需手動或 cron 觸發,明明 daemon 在跑卻沒利用
3. **Pattern → Workflow 自動橋接未閉環** — `commander_bridge.rs` 存在於 mur 端但未自動觸發 commander 生成 workflow 草稿
4. **Trigger 是事件驅動,非智能驅動** — 沒有類似 Letta sleeptime 或 memU Intention 的「閒暇時思考下一步」

---

## 戰略缺口與建議 (10 條)

### 原有 5 條 (2026-04-15)

1. **發表檢索 benchmark** (即使只在 LongMemEval 或 DMR 上) — 在行銷定位上追上 Mem0/Zep
2. **實體萃取 + 輕量圖層** 建於現有連結模型之上 — 彌補 Cognee/Zep 的差距而不放棄 YAML
3. **雙時間事實失效** — 以有效期窗口儲存被取代的模式,而非單純封存;解鎖「時空穿梭」查詢
4. **AGENTS.md 相容模式** — 在現有工具特定同步之外額外產生合成的 AGENTS.md,搭上新興標準
5. **團隊同步後端** — `team` 指令表面已存在;託管 (或可自架) 同步服務能解決本地優先的擴展天花板

### 新增 5 條 (2026-04-18,來自 mur-commander 納入後的重新評估)

6. **mur ↔ commander 記憶統一協議** ⭐ — commander 的 `LongTermStore` 應寫入 `~/.mur/patterns/` 作為 Session tier,自動進入 Evidence/Maturity 升層流程;定義雙向同步格式。**這是最高槓桿的整合缺口。**
7. **Sleeptime agent 模式** — 讓 mur-commander daemon 在 idle (無 workflow 跑、無 trigger 觸發) 時自動呼叫 `mur evolve --consolidate` / `mur gc`,可用更便宜的本地模型 (ollama)。仿 Letta `sleeptime_agent_frequency` 設計。
8. **Pattern → Workflow 自動閉環** — `mur-core/src/evolve/commander_bridge.rs` 目前是「建議」而非「自動」;當 pattern 升到 Stable/Canonical 且含 action keyword (cargo/npm/docker/git) 時,自動在 commander 生成 workflow 草稿放入 pending 區,用戶一鍵啟用。這是 mur 生態獨家、競品無法複製的特色。
9. **Text2Mem 風格的記憶操作 IR** — 把 `mur pin/mute/boost/promote/deprecate/edit` 統一成 12 原子動詞 + JSON 契約。好處:(a) 外部 agent / MCP 工具可標準化操控 mur;(b) commander workflow step 可直接「記憶一件事」;(c) 可做 dry-run 與審計。
10. **漸進式注入 + Intention Layer** — 合併 claude-mem 節 token 策略 + memU 預測機制。預設注入 pattern 名稱 + 一行摘要 (~200 tokens),需要時用 `mur show <name>` 展開 (3 層披露);並用 commander daemon 的 audit log 分析最近 workflow/pattern 軌跡做 pre-fetch。

### 優先級建議 (基於影響力 × 實作難度)

| 建議 | 影響力 | 難度 | 競爭防禦性 |
|---|---|---|---|
| 6. mur↔commander 記憶統一 | 🌟🌟🌟🌟 | 中 | 獨家 |
| 7. Sleeptime agent | 🌟🌟🌟🌟 | 中低 | 追上 Letta |
| 8. Pattern→Workflow 閉環 | 🌟🌟🌟🌟 | 中 | **獨家 — 無人有「記憶→自動生成可執行代碼」** |
| 10. 漸進注入 + Intention | 🌟🌟🌟 | 中 | 追上 claude-mem/memU |
| 3. 雙時間失效 | 🌟🌟🌟 | 中 | 追上 Zep |
| 1. Benchmark 發表 | 🌟🌟🌟 行銷 | 中 | 必要防禦 |
| 9. Text2Mem IR | 🌟🌟🌟 | 中高 | 新興標準先行 |
| 2. 實體圖層 | 🌟🌟 | 中高 | 追上 Cognee |
| 5. 團隊同步後端 | 🌟🌟 | 中 | 商業化必要 |
| 4. AGENTS.md 相容 | 🌟 | 低 | 防禦性 |

---

## 關鍵數據點 / 佐證 (2026-04-18 新增)

- **Mem0 生產環境 audit** (GitHub issue #4573):10,134 條記憶中**僅 224 條乾淨**,97.8% 為 junk — 強烈佐證 mur 的 **Evidence + Maturity + Origin** 三件套策略的必要性
- **Mem0 論文發現**:「無差別記憶儲存比不用記憶更糟」,嚴格過濾提取可帶來平均 10% 效能提升 — mur 的 `noise_filter.rs` 與 Evidence 加權機制正是這個方向
- **Mem0ᵍ graph 版 token footprint 翻倍至 14k** — 支持 mur 選擇「輕量 Links」而非重圖的架構判斷
- **Letta sleeptime 細節**:`sleeptime_agent_frequency` 預設 5 步,主 agent 可用 gpt-4o-mini,sleeptime agent 可用 gpt-4 (因無延遲約束) — 可直接映射到 mur-commander daemon 的 idle 策略
- **A-Mem 錄取 NeurIPS 2025**:三階段 Note→Link→Evolution,新記憶觸發舊記憶更新;這是 mur `cooccurrence.rs` + `linker.rs` 應該演進的方向 (當前 mur 新 pattern 只 discover links,不會 update 舊 pattern 的 content)
- **memU Intention Layer** (PyPI `memu-py` 2026-02-14 釋出):FS 隱喻 + 預測式 pre-fetch — 「預測下一步」是 mur 從「學習」邁向「助手」的關鍵躍遷
- **claude-mem 漸進式披露**:第一層只回傳 50-100 tokens 的索引 — 相較 mur 目前注入 5 個 pattern 全文 (~2000 tokens),降幅可達 90%

---

## 參考來源

### 原版參考 (2026-04-15)
- [Mem0 GitHub](https://github.com/mem0ai/mem0) · [Mem0 論文](https://arxiv.org/abs/2504.19413) · [State of AI Agent Memory 2026](https://mem0.ai/blog/state-of-ai-agent-memory-2026)
- [Letta GitHub](https://github.com/letta-ai/letta) · [MemGPT 現為 Letta](https://www.letta.com/blog/memgpt-and-letta)
- [Zep 論文](https://arxiv.org/abs/2501.13956) · [Graphiti GitHub](https://github.com/getzep/graphiti)
- [Cognee GitHub](https://github.com/topoteretes/cognee) · [Cognee 首頁](https://www.cognee.ai/)
- [Memobase](https://www.memobase.io/) · [A-MEM GitHub](https://github.com/agiresearch/A-mem)
- [Claude Code Memory 文件](https://code.claude.com/docs/en/memory)
- [Aider Conventions](https://aider.chat/docs/usage/conventions.html) · [Continue.dev config](https://docs.continue.dev/reference)
- [LangGraph Memory](https://docs.langchain.com/oss/python/langgraph/memory) · [LlamaIndex Memory](https://developers.llamaindex.ai/python/examples/memory/memory/)

### 2026-04-18 新增參考
- [A-Mem paper (NeurIPS 2025)](https://arxiv.org/abs/2502.12110) · [OpenReview](https://openreview.net/forum?id=FiM0M8gcct)
- [Mem0 production junk audit — 97.8% (GitHub issue #4573)](https://github.com/mem0ai/mem0/issues/4573)
- [Letta Sleep-time Compute](https://www.letta.com/blog/sleep-time-compute) · [Letta Sleeptime Agents (DeepWiki)](https://deepwiki.com/letta-ai/letta-python/12.3-sleeptime-and-background-agents)
- [Text2Mem paper (arxiv 2509.11145)](https://arxiv.org/abs/2509.11145) · [Text2Mem GitHub](https://github.com/MemTensor/text2mem)
- [ReMe GitHub (AgentScope)](https://github.com/agentscope-ai/ReMe)
- [memU GitHub (NevaMind-AI)](https://github.com/NevaMind-AI/memU) · [memU PyPI](https://pypi.org/project/memu-py/)
- [Hermes Agent (Nous Research)](https://github.com/nousresearch/hermes-agent)
- [claude-mem (thedotmack)](https://github.com/thedotmack/claude-mem) · [claude-mem DeepWiki](https://deepwiki.com/thedotmack/claude-mem)
- [Karpathy LLM Wiki gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)
