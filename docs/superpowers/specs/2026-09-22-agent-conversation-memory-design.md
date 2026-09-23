# MUR Agent 對話記憶 — 三層設計（Working set · Episodes · Digest）

**Date:** 2026-09-22
**Status:** Draft for review
**Scope:** `mur-agent-runtime`（`task_runner.rs` ConversationStore、`turn_ledger.rs`、`llm/mod.rs`）、`mur-core/src/conversations`（新 ingester）、`mur-common`
**依據研究:** `~/.mur/artifacts/deep-research/20260921-final-agent-memory-best-practices.md`（下稱「研究定稿」；證據等級沿用 ✅ 🟡 ⚠️ 標記）
**上游規格:** `2026-09-19-turn-ledger-memory-design.md`、`2026-08-04-unified-memory-federation.md`、`2026-04-19-mur-conversations-design.md`

---

## 0. 邊界：這份規格管什麼、不管什麼

MUR 已有三種「記憶」，這份規格只重新設計第一種：

| 記憶 | 內容 | 擁有者 | 本規格 |
|---|---|---|---|
| **對話記憶（episodic）** | 這段對話說了什麼、做了什麼、決定了什麼 | runtime 每個 agent 各自一份 | **重新設計** |
| notes / `remember` | 使用者的規則與環境事實（蒸餾物） | `~/.mur` 中央管線，七態生命週期 | 不動；只在 §6 定義單向的「提案」介面 |
| skills / patterns | 可重用的做法 | 同上 | 不動 |

Federation 規格原則 1 直接適用：「Episodic stays local; distillates federate.」（`2026-08-04-unified-memory-federation.md:37`）。對話記憶永遠留在 agent 家目錄；跨出去的只有蒸餾物，而且必須走既有的 memory-proposal（Draft、公告、可撤銷）。

---

## 1. 問題

### 1.1 現況

`ConversationStore`（`mur-agent-runtime/src/task_runner.rs:228`）把一段對話存成
`[user Text, agent Text, TurnLedger]*`，容量是模型視窗的四分之一：

```rust
/// Share of the model's context window that stored history may occupy: the
/// window also has to hold the system prompt, injected skills, the tool
/// inventory, this turn's tool traffic and the reply, so history gets a quarter.
const CONV_BUDGET_DIVISOR: u64 = 4;                       // task_runner.rs:121
```

超過預算時整輪丟掉，最新一輪永遠保留（`remember`，`task_runner.rs:365-372`）。丟掉就是丟掉——沒有摘要、沒有歸檔、沒有任何可回查的痕跡。

### 1.2 三個具體缺口（以本機資料佐證，2026-09-22）

1. **純截斷。** 研究定稿 §2.1 把「截斷」列為四種手法中最弱的一種；業界收斂在「滾動摘要 + 保護頭尾 + 增量更新」。一段 30 輪的除錯對話，第 31 輪時模型已經不記得第 1–8 輪決定的方向，於是重做、翻案、或編造。
2. **每輪整段重寫。** CLI／Hub 把 `context.task_id` 設成上一則回覆的 id，store 就以「產生它的那一輪 id」為 key（`task_runner.rs:216-218`）。結果是 `~/.mur/agents/mur/conversations/` 有 **257 個檔、只對應 101 段對話**（5.7 MB），每一輪都把整段歷史再存一份；`sweep_stale_files`（`task_runner.rs:329`）只按 mtime 砍到 256 個，砍掉的可能正是一段活對話的中段 key。
3. **agent 對話從未進入可檢索層。** `mur chat` 管線（LanceDB、抽取＋摘要、週／月 rollup、三道保護 retention）成熟但 `conversations.enabled: false`，且 ingester 只有 `claude_code / cursor / gemini / aider`（`mur-core/src/conversations/ingest/`）。Channel 有 FTS5（`mur-channel/src/index.rs:164`）但 agent 不會回頭查。「上次我們怎麼解這個 EDEADLK 的？」今天沒有任何路徑能回答。

### 1.3 已經做對、必須保留的

- **TurnLedger 是工具流量的壓縮形式**，且「trimming never separates a ledger from its turn」（turn-ledger 規格 §2 目標 5）。本規格的摘要層必須把 ledger 的誠實訊號一起帶過去，否則就是把 2026-09-18 的捏造事故重新引進來。
- **Runtime 歸屬的固定標頭**：ledger 不當敘事寫進 assistant 文字（同規格目標 4）。摘要也一樣——它是 runtime 的記錄，不是 agent 的自述。
- **持久化 best-effort、永不讓一輪失敗**（`persist`，`task_runner.rs:300`）。

---

## 2. 目標與非目標

目標：

1. 一段對話不論多長，模型永遠看得到：**目前任務、已做決定、未了事項、環境事實、最近幾輪逐字**——在同一個四分之一預算內。
2. 被摺疊掉的內容**不消失**：先落到 agent 本機的 episodes 日誌，再從視窗移除（三道保護，與 `retention.rs` 同型）。
3. agent 能**回查自己過去的對話**（工具驅動優先，自動注入為選配）。
4. `mur chat ask` 也能回答關於 MUR agent 對話的問題——靠**新增一個 ingester，不是再造一條管線**。
5. 每一項行為都可離線、可用 stub LLM 測試；LLM 摘要失敗時退回到今天的截斷行為，不會更糟。

非目標：

- 把 `ToolUse` / `ToolResults` 原樣存進跨輪記憶（turn-ledger 規格已否決，理由不變）。
- 自動寫入 notes／`remember`。§6 只做提案。
- 改 Hub／TUI 的呈現。ConversationState 對使用者要不要可見是 UI 規格的事（§9 開放問題）。
- 跨 agent 共享對話記憶。scope 是預先過濾器（federation 原則 4），不是事後合併。

---

## 3. 模型：三層，對應 MemGPT ✅（arXiv 2310.08560）

```
┌──────────────────────── context window ────────────────────────┐
│ system prompt · notes/skills 注入（既有）                         │
│ ┌ Tier 0 · Working set（本規格）───────────────────────────────┐ │
│ │ [ConversationState]   ← 結構化摘要，runtime 標頭，放頭部        │ │
│ │ [user][agent][ledger] ← 尾窗：最近 N 輪逐字                    │ │
│ │ [user][agent][ledger]                                         │ │
│ │ [user]                ← 本輪，永不摺疊（active task anchoring） │ │
│ └───────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
          │ fold（§4.3）                       ▲ recall_conversation（§5）
          ▼                                   │
 Tier 1 · Episodes（agent 本機，append-only JSONL，FTS5 + 向量）
          │ 既有 mur chat 管線：summarize → rollup → retention
          ▼
 Tier 2 · Digest（日／週／月摘要，永久保留；raw 依 retention_days）
```

對應研究定稿 §4 第 1 條：「核心區塊（always-in-context、有大小上限）、可搜尋對話史、冷歸檔」。MUR 的「核心區塊」已經由 notes 注入承擔（身分、規則、事實）；本規格新增的是**對話層的**核心區塊——ConversationState——它只描述這段對話，不描述使用者。

---

## 4. Tier 0 · Working set

### 4.1 一輪一個 frame，用父指標串起來（修 §1.2 缺口 2）

**一輪一個檔，檔裡指向上一輪。** 沒有索引、沒有整段重寫、沒有「一段對話只有一條直線」的假設。

`remember_turn` 的簽章已經把需要的東西送進來了（`task_runner.rs:730`）：

```rust
fn remember_turn(&self, key: &str, ctx: Option<&str>, input: &Message, reply: &Message)
//                      ^ 本輪 id           ^ 上一輪 id = 父指標
```

今天 `ctx` 只被用來 `store.prior(ctx)` 讀出整段歷史、接兩則、再以 `key` 整份寫回。改成只寫本輪：

```
~/.mur/agents/<name>/conversations/<turn-id>.json
```

```json
{
  "v": 1,
  "parent": "<上一輪 id>",        // null = 對話起點
  "root": "<起點 id>",            // 由父 frame 複製；自己是起點時等於自己
  "depth": 42,                    // = parent.depth + 1；只給人看與排序，不是 key（見下）
  "messages": [ /* 只有這一輪：user、agent、TurnLedger */ ],
  "state": null,                  // 這一輪做了 fold 時才有（§4.2）
  "state_covers": null,           // [from_id, to_id]：被摺的最舊／最新 frame id，與 state 同進同出
  "token_ratio": null             // {model, ratio, samples}：估計器漂移校準（§4.3.0(b)）；每輪由父 frame 複製再更新
}
```

**輪號不是身分。** 分岔（兩個 frame 同一個 `parent`）之後，兩支的 `depth` 會重疊：A 支的第 7 輪與 B 支的第 7 輪是不同的檔、不同的內容。所以所有「指向某一輪」的欄位一律記 **frame id**（檔名），不記輪號：`state_covers`、`ConversationState.from_id/to_id`（§4.2）、episodes 的 `fold` 事件（§4.5）。`depth` 只用來給 Hub 排序與給摘要器寫「第幾輪決定的」這種人話；任何以 `depth` 做 lookup 的程式碼都是 bug。

**讀：`prior(ctx)` 走鏈。** 從 `ctx` 起沿 `parent` 往回收集 frame，直到下列任一條件，然後反轉：

| 停止條件 | 理由 |
|---|---|
| `parent == null` | 到起點 |
| frame 有 `state` | 它已經蓋住所有更舊的輪次，再往回是浪費（fold 的天然邊界） |
| 檔案不存在或解不開 | 鏈被 sweep 截斷或壞檔；記一次 warn，用已收集到的，標 `truncated` |
| 深度 > `MAX_CHAIN_WALK`（預設 512） | 防呆，不是預算控制；預算由 §4.3 管 |

組出來的 `Vec<RichMessage>` 存進記憶體 map（key = `ctx`），所以只有冷讀（重啟後、或 map 被換掉）才會走檔案。一段 30 輪對話冷讀 = 30 個小檔，都在同一個目錄、同一個頁快取裡。

**寫：`persist` 變成 O(1)。** 每輪寫一個只含本輪的檔，不碰任何既有檔案。原子性沿用今天的 `write tmp → rename`（`task_runner.rs:314-316`），不變。

**這樣修掉的四件事**

1. **寫放大。** 今天第 N 輪要寫 N 輪份量，一段對話的總寫入是 O(N²)；現在是 O(N)。§1.2 的 257 檔／101 對話／5.7 MB 會變成 257 檔／同樣的對話／約 1/20 的位元組。
2. **分岔是免費的。** 兩輪可以有同一個 `parent`（使用者編輯重送、Hub 開新分支），各自寫各自的檔，誰也不覆蓋誰。root + 索引做不到這點：索引是 turn → root 的多對一，重送時第二個分支會把第一個分支的輪次接在同一個線性檔尾巴，下一輪的歷史裡就會多出使用者沒看過的一輪。
3. **不需要重建。** 沒有索引就沒有「索引與檔案不同步」這個狀態，§7 少一列。
4. **fold 有了乾淨的邊界。** fold 就是在本輪的 frame 填 `state` + `state_covers`；走鏈遇到它自然停下，被蓋住的舊 frame 不再被讀（但還在磁碟上，逐字內容也已在 episodes，§4.5 保護 1）。

**sweep。** `sweep_stale_files` 的上限改成 frame 數（`MAX_FRAMES`，預設 4096——frame 比整段歷史小得多，可以留久一點），刪除順序**只有 mtime LRU 一段**，與今天 `task_runner.rs:329-355` 同型。刪到活鏈的中段時，`prior` 會在斷點停住 —— 退化結果是「這段對話看起來比較短」，正是今天 miss 的行為，不會更差。

> **為什麼不先刪「已被 `state_covers` 蓋住」的 frame。** 「蓋住」是**每支鏈各自**的事實：A 支在第 17 輪 fold 了 3..17，B 支從第 10 輪分岔出去、還沒 fold，那 11..17 對 A 支已無作用，對 B 支的 10 卻是活祖先。sweep 沒有走鏈就沒辦法判斷一個 frame 是不是「所有後代都蓋住了它」——要判斷就得對整個目錄反向建圖，那就是本節第 3 點剛拿掉的索引。LRU 對兩支一視同仁，錯了也只是「看起來短一點」；「先刪被蓋住的」錯了會**精準砍掉兄弟分支的活祖先**。留 `state_covers` 是給 §4.5 保護 2 對帳與 Hub 顯示用，不給 sweep 用。

**移轉。** 舊檔是一個裸 `Vec<RichMessage>`（沒有 `v`／`parent`）。`load` 先試 frame，失敗再試裸陣列；讀到裸陣列就當成「一個 `parent: null`、已含完整歷史」的 frame，照常可用。不做批次搬遷，舊檔被 sweep 自然淘汰。

### 4.2 新的訊息變體 `ConversationState`

```rust
// mur-agent-runtime/src/llm/mod.rs
RichMessage::ConversationState {
    /// 最後一次已套用 fold 的冪等鍵（§4.5）；用來完成 crash recovery，不是內容 digest。
    fold_id: String,
    /// 目前 state 累積覆蓋的 frame 範圍（含）：最舊／最新 frame id（§4.1「輪號不是身分」）。
    from_id: String,
    to_id: String,
    /// 目前 state 累積覆蓋的 frame 數，只供渲染成人話（"turns=15"）；不做 lookup。
    count: u32,
    state: ConversationState,
}

// mur-agent-runtime/src/conversation_state.rs
pub struct ConversationState {
    pub task: String,                 // 目前在做什麼（一句）
    pub decisions: Vec<Decision>,     // 已做決定；被推翻的標 superseded，不刪
    pub open: Vec<String>,            // 未了事項、等待中的問題
    pub facts: Vec<String>,           // 這段對話裡出現的環境事實：路徑、分支、PR 號、錯誤碼
    pub ledger_digest: LedgerDigest,  // 被摺疊輪次的工具統計 + 失敗逐字（≤200 chars 各）
    pub artifacts: Vec<String>,       // 產出的檔案路徑
    // 注意：token_ratio **不在這裡**。校準是每一輪都要更新的量，而 state 只在 fold 時才誕生；
    // 第一次 fold 前沒有 state 可放，放進來就等於前 N 輪永遠沒有校準。它住在 frame 上（§4.1、§4.3.0(b)）。
}
pub struct Decision { pub text: String, pub turn: u32, pub superseded_by: Option<u32> }
// `turn` / `superseded_by` 是 depth，不是 frame id——這裡可以用輪號，因為一份 state 只屬於一支鏈，
// 鏈內 depth 唯一；它們只給模型讀（「第 12 輪改成 merge」），從不拿去 lookup 檔案。
```

渲染規則與 `TurnLedger` 相同：固定 runtime 標頭、`<conversation_state turns="15">…</conversation_state>`（`turns` 是 `count`，不是輪號區間——分岔後輪號沒有唯一意義），**永遠是 history 的第一則**（Lost-in-the-middle ✅ arXiv 2307.03172：頭尾記得最清楚；本輪使用者訊息在尾）。不摺進 system prompt——system prompt 屬於 agent 設定，這是對話的事。

`superseded_by` 是 Zep 雙時間模型 ✅ 的最小版本（arXiv 2501.13956）：**推翻用失效，不用刪除**。模型看得到「原本要 rebase，第 12 輪改成 merge」，就不會在第 30 輪又提 rebase。

### 4.3 Fold：何時、摺什麼、怎麼摺

#### 4.3.0 計量：閥門的分母必須先站得住

預算沿用今天的 `budget_tokens`（視窗 ÷ 4；未知視窗 8k，見 `task_runner.rs:116-125`）。但**分子**今天是 `estimated_tokens()`，它把 UTF-8 位元組除以 4：

```rust
// mur-agent-runtime/src/task_runner.rs:113-116
/// Deliberately crude: the alternative is tokenizing every stored turn on every
/// send, and the budget leaves enough headroom that a 25% error costs nothing.
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;
```

那句註解在今天成立（唯一消費者是 `drop_oldest_turn` 的裁切迴圈，`task_runner.rs:366`，估低只是少丟幾輪）。**本規格讓它不成立**：50/85 是要在超窗前搶跑的閥門，估低 = 閥門遲到 = 強制 fold 在真實超窗之後才觸發，而那正是它要防的事。

本機實測（`~/.mur/agents/mur/conversations`，258 檔 / 1,692,379 字）：

| 量測 | 值 |
|---|---|
| CJK 佔全體字元 | 16.8% |
| 位元組 ÷ 4 相對字形感知估計，低估中位數 | 1.085× |
| 最糟單檔（CJK 37%） | 1.16× |
| 純中文段落的理論上限 | ~1.27× |

三段處置：

**(a) 字形感知估計器**（P1，取代常數除法）。CJK 字元約 1 token，其餘按 4 字元 1 token：

```rust
fn estimated_tokens_of(text: &str) -> u64 {
    let (mut cjk, mut rest) = (0usize, 0usize);
    for c in text.chars() {
        // CJK 統一表意文字 + 全形標點 + 假名 + 諺文
        if matches!(c, '\u{3000}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}' | '\u{f900}'..='\u{faff}' | '\u{ff00}'..='\u{ffef}') { cjk += 1 } else { rest += 1 }
    }
    (cjk as u64) + (rest / CHARS_PER_TOKEN_ESTIMATE) as u64
}
```

`estimated_tokens()` 改為逐 `RichMessage` 呼叫它，走 `chars()` 而非 `len()`。純 Rust、離線、確定性，無新依賴；不引 `tiktoken-rs`（要載詞表、每輪 tokenize，且各 provider 詞表不同，準確度換不到閥門需要的那一位數）。

**(b) 漂移校準**（P2，讓估計器對上真實計費）。每次 `LlmResponse` 都帶真的 prompt 大小：

```rust
// mur-agent-runtime/src/llm/mod.rs:270
pub input_tokens: u64,
```

回覆落地時算 `ratio = input_tokens / estimated_prompt_tokens`，以 EWMA（α = 0.3）更新 **frame 上的 `token_ratio`**（§4.1；`{model, ratio, samples}`），**clamp 在 [0.5, 3.0]**，每輪由父 frame 複製後再更新，隨鏈持久化。下一輪的閥門用 `estimated × ratio` 比預算。它**不放** `ConversationState`：state 只在 fold 時才誕生，第一次 fold 前沒有地方放，前 N 輪就永遠沒有校準——而閥門最需要準的恰恰是第一次 fold 之前。

**什麼才算一個樣本。** 一筆回覆要同時滿足下列四條才進 EWMA，否則丟棄不計、`samples` 不加：

1. `input_tokens > 0`。今天三個地方會給 0：Ollama 抓不到 `prompt_eval_count`（`ollama.rs:140` / `ollama.rs:259` 都是 `unwrap_or(0)`）、stub 的 `joined.len() / 4`（`stub.rs:76`，那是位元組除以 4，正是本節要修掉的東西，不能拿來當真相）、fallback 走 `estimate_input_tokens` 自估的路徑。0 進 EWMA 會把 ratio 拖向 0.5 的下限，閥門從此永遠遲到。
2. `LlmResponse.model` 與 frame 記的 `token_ratio.model` 相同。不同 provider 詞表不同，同一段中文在 Claude 與 Qwen 上差 1.5× 是常態；模型換了就**整個重置**為 `{model: 新模型, ratio: 1.0, samples: 0}`，不做跨模型平均。
3. 這一筆的 prompt 是**完整 prompt**（見下方估計器定義），不是 fold 摘要器那種只送幾輪的子請求——摘要器的呼叫不進校準。
4. 沒有被 fallback 換過 provider（`fallback/mod.rs` 換路時 `model` 會變，由第 2 條擋掉；這條是提醒實作者不要在換路前先讀 model）。

`samples < 3` 時 `ratio` 視為 None，閥門走 §4.3.0 末段的保守係數。

**Ollama 只往上校準。** Ollama 的 `prompt_eval_count` 是**這次真的算過的 prompt token 數**，KV cache 命中的前綴不計在內；連續對話下它常常只回報本輪新增的幾百 token，而非整個 prompt。這與 `llm/mod.rs:263-270` 對 `input_tokens` 的定義（「Everything the model read for this call, cached or not」）**不一致**，但修 adapter 是另一件事，本規格不假設它會修。處置：來源是 Ollama 時，樣本只在 `ratio_sample > 目前 ratio` 時才進 EWMA（只允許把估計往上推，不允許往下拉）。方向性理由與末段相同：估計器高估頂多早 fold 幾輪，低估會讓 §4.3.1 的 85% 警戒線失效。其他 provider（Anthropic、OpenAI 相容）回報的是完整 prompt 大小，雙向校準。

**估計器估的是什麼。** `estimated_prompt_tokens` 必須與 `input_tokens` 量的是同一個東西——**送出的整個 `LlmRequest`**，定義為：

```rust
fn estimated_prompt_tokens(req: &LlmRequest) -> u64 {
    // 系統提示（含 skill 注入、TurnLedger 標頭）已是 messages[0] 的 role="system" Text
    // （task_runner.rs:704-708），不是獨立欄位；所以 messages 一路加總就涵蓋它。
    let mut n = req.messages.iter().map(estimated_tokens_of_message).sum::<u64>();
    // 工具 schema：name + description + input_schema JSON，這塊今天完全沒被估。
    n += req.tools.iter().map(|t| estimated_tokens_of(&t.render_for_estimate())).sum::<u64>();
    n
}
```

今天的 `estimated_tokens(history)`（`task_runner.rs:144-158`）只在 `drop_oldest_turn` 的裁切迴圈對 history 呼叫（`task_runner.rs:366`），那份 history 不含系統提示（`seed_history` 才把它 push 進 `h`，`task_runner.rs:697-708`），工具清單則從頭到尾不在內；若拿它當分母，ratio 會把那兩塊固定量吃進來變成常態高估（工具清單動輒 2-4k token，在 8k 視窗下是 30-50% 的常態偏差）。所以：**分母是送出那一刻的 `estimated_prompt_tokens(req)`，閥門比的也是它**——不是存檔前 history 單獨的估計。`cache_read_input_tokens` 已由 Anthropic adapter 併入 `input_tokens`（見該欄位註解），不另計。ratio 只校準閥門，**不進入路由計費**（`mur-core/src/route/` 那個 `estimated_tokens` 是另一條路，本規格不碰）。

**(c) 閥門的分母寫明**。下表每一格都以 **(a) 的估計 × (b) 的 ratio** 為準，與 `budget_tokens` 同一把尺；規格裡不再出現「位元組」這個字：

| 區塊 | 上限 | 說明 |
|---|---|---|
| ConversationState | 15% | 以校準後估計計；超過就要求摘要器再壓（§4.4） |
| 尾窗（逐字） | 其餘 | 至少 2 輪，永遠含本輪 |
| 警戒線 | 85% | 到達時強制 fold，不等背景工作 |

估計器只可**高估**不可低估的方向性：若 ratio 樣本不足（`samples < 3`，含剛換模型後重置）而對話 CJK 比例 > 30%，閥門額外乘 1.15 的保守係數，直到有效樣本進來為止。「樣本不足」看的是 frame 上 `token_ratio.samples`，被丟棄的無效樣本（(b) 四條）不計數。

#### 4.3.1 觸發

觸發（研究定稿 §2.5 ⚠️ Hermes 50/85，**數字為 MUR 選擇，可調**）：

- **50%**：本輪回覆送出後，背景排一個 fold job（`RequestIntent::Background(BackgroundKind::Maintenance)`；模型由下方「fold 走哪個模型」決定）。
- **85%**：下一輪開始前 fold 尚未完成 → 同步做**抽取式 fold**（§4.4 第 0 級，不需 LLM），保證不超預算。
- **訊息數 > 400**：與 85% 同等，觸發同步 fold（見下方「400 上限」）。
- 任何時候 fold 失敗 → 退回今天的 `drop_oldest_turn`。不會比現在差。

**誰跑背景 fold。** 是 `TaskRunner`，不是 `IdleScheduler`。`idle_scheduler.rs` 是「閒置 N 秒後注入一則訊息」的 30 秒 tick 迴圈（`idle_scheduler.rs:1-9`、`:93-107`），它的觸發條件是「使用者不在」，而 fold 要的恰恰相反：回覆一落地就開始，趕在下一輪之前做完。所以 fold job 在 `remember_turn`（`task_runner.rs:730`）寫完本輪 frame 之後由 runner 直接 `tokio::spawn`，handle 存在 runner 上的 `in_flight_folds: HashMap<conv_key, JoinHandle>`，runner drop 時 abort。

**fold 結果落在哪（pending fold）。** frame 是 append-only（§4.1），fold 是在本輪 frame 寫完之後才算出來的，所以**不能改寫**排程它的那個 frame。結果先寫成 sidecar `<conv>/pending/<based_on>.json`：

```jsonc
{ "fold_id": "<uuid>", "based_on": "<排程 fold 時的 frame id>", "state": { …ConversationState… }, "state_covers": ["<累積 from_id>", "<本批 to_id>"], "batch": { "from_id": "<本批 from_id>", "to_id": "<本批 to_id>", "count": 15, "digest": "sha256:…", "level": 0 } }
```

`state`／`state_covers` 是 fold 後的**累積狀態與 coverage**；`batch` 只描述這次新摺掉的區段，供 §4.5 的 fold event 與 provenance 驗證。digest 不放進 `ConversationState`，否則第二次增量 fold 後無法分辨它是累積 state 的摘要還是本批逐字的 hash。

套用時機是**下一輪寫 frame 時**：新 frame 的 `parent == based_on` → 依 §4.5 的提交順序把 `state`／`state_covers`／`fold_id` 搬進新 frame；`parent != based_on`（使用者編輯重送、分岔到別支）→ sidecar 直接丟掉，不記事件，因為它摘要的是另一支的歷史。sidecar 也吃 §4.1 的 mtime LRU sweep，但獨立計數，不佔 frame 名額。

**單飛（single-flight）。** 每個 conversation key 同時最多一個 fold 在跑：50% 觸發時若 `in_flight_folds` 已有該 key，不再排第二個。85%／400 的同步 fold 走到時若背景那個還沒回來，同步 fold 先做，並 abort 背景 handle、刪掉可能已落地的 sidecar——兩份 state 疊在同一支上是非法狀態，同步那份永遠贏，因為它已經進了 frame。

**fold 走哪個模型。** 今天的路由事實：`RequestIntent::Background(_)` **只有在** `smart.enabled` 時才會把 cheap 模型排到前面（`fallback/mod.rs:169-181`），而 `DEFAULT_SMART_ENABLED: bool = false`（`mur-common/src/config.rs:21`）。也就是說預設安裝下，標 `Maintenance` 的 fold 會跟使用者對話走**同一條主模型鏈**——不是「既有便宜模型路由」，那條路由預設是關的。規格因此新增 `conversations.memory.fold_model: Option<String>`：

| `fold_model` | `smart.enabled` | fold 實際打到 |
|---|---|---|
| 指定 | 任意 | 指定的那個 ref（不合格則 warn 一次，退到下一列） |
| 未指定 | true | `smart.cheap` 或 `autopick_cheap`，後面接主鏈 |
| 未指定 | false | 主鏈，與對話同模型 |

第三列是預設，規格接受它——本機 Ollama 上主模型就是最便宜的模型；只有雲端主模型的使用者才需要設 `fold_model`。

**400 上限。** `MAX_CONV_MESSAGES: usize = 400`（`task_runner.rs:111`）是第二道、按**則數**而非 token 的閘，今天在 `:369` 整輪整輪砍最舊。這道閘保留，但改在 fold 之後才檢查：沿鏈組出的視窗若仍超過 400 則，先觸發同步 fold（等同 85%），fold 完還超才 `drop_oldest_turn`。理由跟 token 閥門一樣——砍是最後手段，不是第一步。上限值不動；含 ledger 訊息的一輪通常是 3 則，400 則 ≈ 130 輪，在 8k 視窗下 token 閥門一定先到，這道閘實際只在 200k 視窗才會被摸到。

摺哪些：從最舊的一輪起，摺到尾窗放得下為止；**本輪與前一整輪永不摺疊**（active task anchoring：「the latest user message must stay outside the summary」，研究定稿 §2.1）。

### 4.4 摘要器：兩級

**第 0 級 · 抽取式（純 Rust，離線，確定性）** — P1 就上，永遠是後備：

- `task` ← 最近一則使用者訊息的前 120 字。
- `facts` ← 正則抓路徑、`#NNNN`、分支名、錯誤碼（`E[A-Z]+`、`os error N`）。
- `ledger_digest` ← 直接由被摺輪次的 `TurnMemory` 合併：工具計數、`Failed(..)` 逐字保留、`narrative_only` 輪數。
- `decisions` / `open` ← 空（誠實留白，不硬湊）。

**第 1 級 · LLM 增量更新** — P2：

輸入 = 舊 `ConversationState` + 被摺輪次逐字（含 ledger 渲染）；輸出 = 新 `ConversationState`。**增量更新舊摘要而非重生**（研究定稿 §2.1，Hermes；亦見 Mem0 的 UPDATE 語意 ✅ arXiv 2504.19413）。

**Structured output 契約。** 今天的 `LlmRequest`（`mur-agent-runtime/src/llm/mod.rs:236-257`）沒有 JSON schema／structured-output 欄位，所以「JSON schema 強制」不是現況。P2 新增 `structured_output: Option<JsonSchema>` 與 provider capability：支援原生 schema 的 provider 直接送 schema；不支援者仍要求只回 JSON，runtime 再做嚴格 JSON parse、schema 驗證與未知欄位拒絕。任一解析／驗證失敗都丟棄模型輸出、降第 0 級，絕不把半份 state 接進鏈。

Prompt 硬規則：

1. 只能寫被摺輪次裡出現的事；不知道就留空。
2. 決定被推翻時填 `superseded_by`，不得刪除舊決定。
3. `ledger_digest` 不由 LLM 產生——runtime 先算好塞進去，LLM 不可改（誠實訊號不經過模型）。
4. 新 state 的總預算是該模型 context window 的 15%，以 §4.3.0 的校準估計衡量；超過就要求「再壓一次」，最多兩次。仍超標時由 runtime 做確定性裁切並且**每一步重估**，順序固定為：`facts` 尾端 → 已 superseded 的 `decisions`（最舊先）→ 其餘 `decisions`（最舊先）→ `open` 尾端 → `artifacts` 尾端 → `task` 尾端。每個被裁欄位保留一個 `…[truncated N]` 標記；空間不足時標記取代最後一項，不可悄悄消失。
5. `ledger_digest` 不參與上述一般裁切：先限制工具統計為每工具一列、每則 `Failed(..)` 逐字最多 200 chars；若整份 state 到最後仍超 15%，才從**最舊**失敗開始裁，但每則保留 `…[truncated N]`，且至少保留最新一則失敗。`task`、各向量欄與 `ledger_digest` 的序列化大小都要受 runtime 上限約束；不能因某一欄異常膨脹而突破總預算。

### 4.5 三道保護

> 與 `mur-core/src/conversations/retention.rs:1-8` 同為「三道守衛」的防誤設計，但守衛內容與方向相反：`retention.rs` 的三道是「年齡 / summary 存在 / 先寫 audit 再刪」，保護的是**刪除**；本節保護的是**摺疊**。只借精神，不可直接套用。

被摺輪次從視窗移除前：

1. **Episodes 已寫入**：該輪次的 `user / agent / ledger` 已 append 到 Tier 1 檔（§5.1）並 fsync。
2. **本批 provenance 對得上**：sidecar `batch.digest` = sha256(本批實際被摺輪次的渲染文字)。digest **只涵蓋本批**，不屬於累積的 `ConversationState`；若因 §7 的鏈中斷／frame 解不開而未讀到某些輪次，那些輪次不在 digest 內。驗收時以 fold event 的 `from_id..to_id` 沿 `parent` 走鏈重算，不要拿整段 episodes 重算，也不要拿輪號切片。
3. **可冪等提交**：每次 fold 先產生唯一 `fold_id`；episodes 的事件為 `{"kind":"fold","fold_id":"<uuid>","from_id":"<本批 frame id>","to_id":"<本批 frame id>","count":15,"digest":"…","level":0|1}`，`fold_id` 是去重鍵。

**跨檔提交順序固定如下：**

1. sidecar 寫暫存檔、`fsync`，再原子 rename 成 `<conv>/pending/<based_on>.json`，並 `fsync` 目錄。
2. episodes append fold event 並 `fsync`；若同一 `fold_id` 已存在，視為成功，不重複追加。
3. 寫含 `fold_id`／`state`／`state_covers` 的新 frame 暫存檔，`fsync` 後原子 rename，並 `fsync` 目錄。只有到這一步，被摺輪次才從讀取視窗消失。
4. 刪 sidecar，並 `fsync` pending 目錄。

**Crash recovery。** 啟動或下一輪套用前檢查 sidecar：沒有事件就從步驟 2 重播；有事件但沒有含同一 `fold_id` 的 frame 就從步驟 3 補完；frame 已存在就只做步驟 4。事件已存在時必須比對 `from_id/to_id/count/digest/level`，同 `fold_id` 但內容不同視為損毀、停止套用並 warn。孤兒 fold event 在 frame 尚未提交前不代表視窗已摺疊，但可由 sidecar補完；因此不得靠事件數推算目前 coverage。

步驟 1 前或任何無法重播的驗證失敗 → 不移除、不摺；記 warn；下一輪重試。

---

## 5. Tier 1 · Episodes 與回查

### 5.1 Episodes 日誌（agent 本機，唯一真相）

```
~/.mur/agents/<name>/episodes/<YYYY-MM-DD>/<root-id>.jsonl
```

每輪 append 三種 event（與 `mur-common::conversation::Message` 同 schema `v:1`，`src` 新增 `mur-agent`）：`user`、`assistant`、`ledger`，另有 `fold` event。**不是**只在摺疊時寫——每輪都寫，摺疊只是視窗端的事；這樣 restart、crash、或使用者中途換 agent 都不會漏。

event 映射與冪等契約：

- `user`：該輪完整 user message；`assistant`：最終 assistant message；`ledger`：該輪 `TurnLedger` 的序列化結果。缺少某一種仍寫其餘 event，不以空行佔位。
- 多段 content 依原順序收在同一 event；文字段保留文字，tool call／tool result 只留型別、工具名、狀態與 `tool_ref`，原始 arguments／result 由下述 10 KB 規則外置，不能展開進可回想文字。
- `event_id = <root-id>:<turn-id>:<kind>:<index>`；三種每輪 event 的 `index=0`，同類多筆（目前只有未來 schema 可能產生）才按原始順序遞增。append 前以 `event_id` 查當日檔與 ingest checkpoint：相同 ID、相同 payload 視為成功；相同 ID、不同 payload 拒寫並記 error。crash 重試因此不會製造重複 episode。
- 每筆 event 都帶 `importance: 1..=10`：第 1 級 fold 產生的 episode 使用摘要器評分；尚未 fold 或只走第 0 級時固定為 5。`fold` event 另沿用 §4.5 的 `fold_id` 冪等鍵。

行大小規則沿用 `2026-04-19` 規格 §4.1：>10 KB 用 `tool_ref`。§1.2 看到的 99 KB 單則（fleet router 的成員清單 prompt）就是這條的用武之地。

scope 欄位（federation 原則 4，預先過濾）：

```json
"meta": {"agent":"mur","user":"<id>","project":"<repo root id>","fleet":"deep-research|null","channel":"<id|null>"}
```

scope identity 的來源與 null 語義固定如下：

- `agent`：啟動中的 agent name；`user`：本機 OS UID 經 MUR installation salt 做不可逆雜湊，不能接受模型或工具參數提供的值。
- `project`：先取 canonical Git repository root；不在 Git repository 時取 canonical cwd；再以同一 installation salt 雜湊。路徑無法 canonicalize 時整筆 episode 拒寫並 warn，不產生 null／臨時 ID。
- `fleet`：由可信 runtime execution context 填 fleet name；一般對話為 null。`fleet = null` 可命中 `same-project` 與 `any`，**永不**命中 `same-fleet`；查詢端自身 fleet 為 null 時，`same-fleet` 直接回空集合。
- `channel` 只供引用與除錯，不參與授權；任何 scope 欄都不得由 episode content、模型輸出或工具參數覆寫。

**scope 必須成為索引欄位，不能只躺在 `meta` JSON 裡，也不能用 `conv_id` 前綴硬塞。** 現況 `mur-core/src/conversations/index.rs:86-92` 的 LanceDB schema 只有 `id / ts / source / conv_id / role / layer / content`，`search()`（`index.rs:212-218`）的過濾參數只有 `source_filter: Option<Source>` 與 `layer: Option<i8>`——沒有任何欄位能表達 `same-project` / `same-fleet`，「預先過濾」在這條管線上無處可掛。若把 scope 編進 `conv_id`（例如 `mur:proj-abc:fleet-x:<root>`），過濾就變成字串前綴比對，`any` 與 `same-fleet` 都得在取回後再篩，違反原則 4，而且 `conv_id` 是 `Message.conv` 的直接映射（`mur-common/src/conversation.rs:48-49`），塞進去會污染所有既有 source 的 dedup 與 rollup。

因此 P3 的 schema 變更（`index.rs` 加四個 `Utf8, nullable` 欄）：

| 欄 | 來源 | 過濾用途 |
|---|---|---|
| `agent` | `meta.agent` | 一律等於呼叫方 agent（硬牆） |
| `user` | `meta.user` | 一律等於當前 user（硬牆） |
| `project` | `meta.project` | `same-project` |
| `fleet` | `meta.fleet` | `same-fleet` |

`search()` 新增 `scope: ScopeFilter { agent, user, project: Option, fleet: Option }`，轉成 LanceDB `only_if` WHERE 子句在向量／關鍵字查詢**之前**套用。非 `mur-agent` 來源這四欄為 null，既有 `mur chat ask` 路徑傳 `ScopeFilter::none()`，行為不變。

### 5.1.1 平行詞法索引：SQLite FTS5

現有 `mur-core/src/conversations/index.rs` 只有 LanceDB 向量搜尋；`ingest/dedup.rs` 的 trigram 是 shingling 去重，不是 lexical search。Tier 1 因此新增一條明確、獨立的詞法索引；FTS5 與 LanceDB 語意上互補，不互為索引，也不把 dedup trigram 冒充搜尋索引。

位置：`~/.mur/conversations/index/fts.sqlite`

```sql
CREATE VIRTUAL TABLE fts_messages USING fts5(
  content,
  content_cjk,                       -- CJK bigram 預切；非 CJK 為空
  agent UNINDEXED,
  user  UNINDEXED,
  project UNINDEXED,
  fleet UNINDEXED,
  msg_id UNINDEXED,                  -- 對應 LanceDB 的 id
  ts    UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);
```

- `content` 存原文，非 CJK 由 `unicode61` 切分。
- `content_cjk` 只在原文含 CJK 時存應用層 bigram 預切結果，供 `MATCH` 命中中文；完全不含 CJK 時存空字串，避免英文同時進兩個 BM25 rank list。下方共享切分函式對無 CJK 輸入回傳原文，但 FTS writer 會先以同一 CJK 判定函式分流，不把該回傳值寫入 `content_cjk`。
- scope 四欄沿用 §5.1 語意；`UNINDEXED` 表示不參與 BM25，但仍以欄位值供 SQL `WHERE` 硬過濾，不藏在 metadata JSON。
- `msg_id` 與 LanceDB `id` 一對一。

**CJK fallback 定案。** 不使用 SQLite trigram tokenizer：它對兩字查詢（例如「預算」）無效，且與 `unicode61` 形成兩套詞頻分佈。新增 `mur-core/src/conversations/index/cjk_bigram.rs`，由應用層在寫入與查詢時使用同一函式預切相鄰兩字：

```text
輸入："預算沿用今天的"
輸出："預算 算沿 沿用 用今 今天 天的"

輸入："EDEADLK 的意思是 deadlock"
輸出："EDEADLK 的意思是 deadlock"              // 無 CJK，原樣
```

CJK 判定沿用 §4.3.0(a) 的區段（`U+3000–U+9FFF`、`U+AC00–U+D7AF`、`U+F900–U+FAFF`、`U+FF00–U+FFEF`）；規格只允許這一份 CJK 判定函式，token 估計器與 bigram 切分器都呼叫它。查詢同時執行 `content MATCH`（unicode61）與 `content_cjk MATCH`（bigram）。兩條 MATCH 各自以 `bm25(fts_messages) ASC, msg_id ASC` 排名（SQLite FTS5 的 BM25 值越小越好），再以 `msg_id` union；同一 `msg_id` 取兩條列表中的最佳 rank，不比較或合併跨欄 raw BM25。單一 CJK 字查詢退化為 prefix `MATCH '字*'`，接受。

**生命週期。**

- 寫入：ingester 每批先寫 FTS5、後寫 LanceDB，但兩側冪等性獨立判斷。重放時，FTS5 已有 `msg_id` 只跳過該側，仍須檢查並補寫缺少的 LanceDB 列；反之亦然。兩側都存在時，分別以來源列的 canonical payload（`content`、scope、`ts`）計算 hash 並比對；同 ID 異 payload 是資料完整性錯誤，記 error、停止該批且不推進 checkpoint。只有兩側都存在且 payload hash 一致，該列才算完成。
- 刪除：retention 先刪 LanceDB，再執行 `DELETE FROM fts_messages WHERE msg_id = ?`。順序刻意與寫入相反：寧可暫留查得到但 join 不到的孤兒 FTS 列，不留仍有內容但來源已刪的孤兒向量。
- 重建：`fts.sqlite` 是衍生物。版本不合或損毀時刪檔，從 LanceDB 逐列讀取 `content` 與 scope 重建；期間 `recall_conversation` 降級為純向量並標 `lexical_unavailable`，不得全表掃描。

### 5.1.2 LanceDB v1 → v2 migration

`index.rs::ensure_table()` 對既有表直接返回，沒有 add-column migration；LanceDB OSS 0.26.2 也不支援 `rename_table`。因此不採「原表補欄」或 rename，改用版本化實體表與原子 active-table manifest：

- 實體表固定為舊 `conversations`（v1）與新 `conversations_v2`（v2）；v2 schema 增加 `agent`、`user`、`project`、`fleet` 四個 `Utf8, nullable` 欄。
- `~/.mur/conversations/index/active-table.json` 是唯一邏輯名稱解析來源，內容至少含 `{"schema_version":2,"table":"conversations_v2"}`；以同目錄 tmp → rename → directory fsync 原子更新。所有讀寫、FTS5 重建與 retention 都必須先經 resolver 取得 active table，不得硬編碼實體表名。
- 啟動時若 manifest 已指向 v2，只開啟 `conversations_v2`；v1 即使尚未清理也不得再讀寫。
- 若只有 v1，或 v1 與 v2 同在但 manifest 尚未指向 v2：保留 v1，刪除任何未啟用的 v2，建立全新的 v2，逐列複製並將四個 scope 欄補 null。複製完成後核對 row count 與逐列 canonical payload hash，寫入 v2 的 `_schema_version = 2` metadata，最後才原子提交 manifest。
- manifest 提交前崩潰：v1 仍是唯一 active table，重啟後丟棄 v2 並重做複製；提交後崩潰：v2 已是唯一 active table，重啟直接使用 v2。流程不呼叫 OSS 不支援的 rename，也不會先刪唯一完整副本。
- v1 只可在 manifest 已持久指向 v2、v2 驗證完成且沒有舊 reader 後清理；清理失敗只留下不再讀取的舊表，不影響正確性。
- 讀取端遇 manifest/schema metadata 非 2、manifest 指向不存在的表，或 metadata 與 manifest 不一致時拒絕服務並記 warn，不讀半遷移狀態。

`ScopeFilter` 四欄一律用 `=` 比對，不用 `IS NULL`。v1 搬入 v2 的資料其 `agent`／`user` 為 null，永遠不匹配任何 agent 或 user 字串，因此結構性地不可被 `recall_conversation` 命中；這是安全設計，不是偶然副作用。

### 5.2 進入既有管線：新增 `Source::MurAgent` ingester

`mur-core/src/conversations/ingest/mur_agent.rs` 輪詢每個 agent 家目錄的 `episodes/`，走既有 pre-filter（normalize → dedup → filter）進 `raw/<date>/mur-agent_<root>.jsonl`，接著寫入 §5.1.1 FTS5、LanceDB 索引、日摘要、週／月 rollup。索引時把 `meta.{agent,user,project,fleet}` 抄進兩條索引；`meta` 缺必填 scope 的行拒絕入索引並 warn。

retention 沿用既有 conversations 設定，但只清 `raw/`、LanceDB、FTS5 與 rollup，不碰 agent 的原始 `episodes/`；原始日誌只受 §5.1 的 `episode_retention_days` 控制。

#### 5.2.1 Checkpoint 與 read-your-writes

每個 agent 維護 per-file checkpoint：`~/.mur/agents/<name>/episodes/.index-checkpoint.json`。

```json
{
  "v": 1,
  "files": {
    "<episode 絕對路徑>": {
      "inode": 12345,
      "byte_offset": 8192,
      "last_event_id": "01J..."
    }
  },
  "updated_at": "2026-09-22T..."
}
```

- `inode` 偵測輪替或刪除重建；`byte_offset` 指最後成功索引的完整行結尾，不含正在寫入的尾行；`last_event_id` 只供該列冪等去重與診斷，不具排序性，禁止用 `<`／`>` 比較或當跨檔 watermark。
- freshness 以 per-file cursor map 判定：對每個依 `{episode_date, absolute_path}` 穩定排序的 episode 檔，比較 checkpoint 的 `{path, inode, byte_offset}` 與目前已 fsync 的完整行 EOF。新路徑、inode 改變，或任一檔 EOF 大於其 checkpoint offset，都表示有落差；跨檔順序只用 `{episode_date, absolute_path, byte_offset}`，不用 event/message ID。
- ingester 每批預設 100 行。每列依 §5.1.1 分別修復 FTS5 與 LanceDB，整批所有列在兩側都存在且 canonical payload hash 一致後，checkpoint 才以 tmp → rename → directory fsync 原子推進。崩潰最多重放一批，不能因 FTS5 已有 `msg_id` 就跳過 LanceDB 修復。
- `recall_conversation` 查詢前讀 checkpoint cursor map，再掃描 `episodes/` 各檔已 fsync 的完整行 EOF。若任一 per-file cursor 落後，同步執行 catch-up，依上述穩定檔案順序處理，以上限 200 行或 200 ms 為界，先到者停止。
- catch-up 完成才查詢；若因上限仍未追平，使用舊水位查詢並回覆 `stale: true`。正常情況同一段對話只有數行差距，第一輪即可讀到上一段對話已 fsync 的 episode。
- checkpoint 損毀或 inode 改變時從該檔 offset 0 重掃，以 `msg_id` 去重；不得猜 offset 或永久跳過資料。

代價：`conversations.enabled` 要能只開 `mur_agent` 一個來源（config 已是 per-source 布林，加一個鍵即可）。

### 5.3 `recall_conversation` 工具（Letta `conversation_search` 的對應 ⚠️）

```text
recall_conversation(query, since?, until?, scope?: same-project|same-fleet|any)
  → {items: [{when, root, turn, role, excerpt, score}], stale, lexical_unavailable, vector_unavailable}
```

預設 `same-project`。所有 scope 都由 runtime 轉成 §5.1 的 `ScopeFilter`，並在 FTS5 SQL `WHERE` 與 LanceDB `only_if` 中於檢索前套用；模型只能選 enum，不能傳 agent/user/project/fleet 字串。`any` 仍固定當前 agent + user，只放寬 project/fleet；永不跨 agent 或跨 OS user。工具輸出只回 excerpt，不回原始 tool arguments 或 secrets。

**RRF 與 relevance 的唯一定義：**

```text
rrf(msg)       = Σ_i 1 / (60 + rank_i(msg))       // i ∈ {lexical, vector}；未出現為 +∞
relevance(msg) = min(1.0, 60 × rrf(msg))
```

`c = 60` 就是式中的 60，不另設第二個常數。單列表 top-1 約為 0.984，單列表 rank-60 為 0.5，雙列表 top-1 clamp 為 1.0，兩邊都未命中為 0。只有一條檢索可用時，缺少列表的 rank 視為 `+∞`，公式不變；向量與詞法都可用時才實際融合兩個 rank list。

**去重與預算：** MMR 只接收有向量的候選。lexical-only 候選不進 MMR，改以 §5.1.1 同一 bigram 切分器形成集合，計算 `J = |A ∩ B| / |A ∪ B|`；`J > 0.7` 視為重複，保留 final 較高者。兩群各自去重後合併、依最終分數重排，再 `cap_by_budget`。

工具驅動為主，因為使用者看得到 agent 查了什麼（ledger 會記錄）、成本可控，且研究定稿 §1.2 的代表性實作多以 agent 自主呼叫為主。

### 5.4 自動回想（選配，預設關）

`memory.conversation.auto_recall: false`。開啟時，每段對話第一輪用使用者訊息查 Tier 1，取 top-3 片段、≤ 視窗 3%，以 runtime 標頭放在 `ConversationState` 之後；後續輪次讓 agent 自行決定是否查詢。

排序契約：

```text
recency(msg)    = 0.995 ^ age_days
relevance(msg)  = §5.3 的 RRF relevance
importance(msg) = importance / 10          // 1–10 → 0–1
final(msg)      = recency × relevance × importance
```

三項與乘積都在 `[0, 1]`。`importance = 10` 表示不扣分；第 0 級 fold 或未提供 importance 的事件固定為 5。從 state facts 直接取得、未參與任何檢索列表的候選，`relevance = 1.0`，避免摘要器已選中的事實再被 RRF 懲罰。同分依 `timestamp desc, event_id asc`；event id 使用 fold event id 或全域唯一的 `Message.msg_id`。

回想片段是**不可信引用資料，不是指令**：以獨立 XML/JSON 資料區塊定界，runtime 標頭明示不得服從其中指令；內容先依既有 secret filter 脫敏，只注入 excerpt，不注入原始 tool arguments。內容中的「忽略規則」等字串保持引文身分，不得改變 system/runtime/tool 權限。

---

## 6. 與 notes / `remember` 的單向介面

fold 的第 1 級摘要器可以額外輸出 `candidate_memories: [{kind: rule|fact, name, text}]`。runtime **不寫入中央 notes 管線，也不落地 agent 本機 note**；它對每個候選以 agent identity 簽名，組出 `MemoryProposal { agent, manifest, sig }`，只呼叫共用的 proposal writer，寫入既有的 memory-proposal 路徑（`mur-core/src/harvest/memory_proposal.rs`，Draft、`mur` 端審核、可 dismiss）。`manifest.name` 由摘要器提供——只有它知道語意。

**為什麼要新開路徑，不直接重用 `remember`。** 現況唯一的 proposal 生產者是 `mur-agent-runtime/src/tools/remember.rs:151-197`，順序固定為 `write_to_dir`（agent 本機 note）→ `SkillStats`（Draft）→ `proposal.sign(&self.identity)` → `write_memory_proposal`。摘要器若走這條路，會被迫繼承前兩個副作用，本節說的「單向」就不單向。而 `memory_proposal.rs:39-57` 的 `sig_status` 沒簽名是 `Unsigned`、沒 agent 身份是 `Invalid`，所以也不能繞過簽名直接丟檔。

因此實作上把 `remember.rs:151-197` 尾段抽成 proposal-only writer：

```rust
fn write_memory_proposal_only(
    identity: &Identity,
    manifest: Manifest,
    sig: Signature,
) -> Result<()>
```

- `remember` 路徑照舊：`write_to_dir` → `SkillStats(Draft)` → `write_memory_proposal_only(...)`，行為不變。
- 摘要器路徑只呼叫 `write_memory_proposal_only(...)`，不經 `write_to_dir`。fold 是 runtime 背景工作，`self.identity` 本來就在手上。
- 候選缺 `name` → 丟棄並記 warn，不產生 `Invalid` proposal 汙染收件匣。

理由：federation 原則 5「Capture is visible. Nothing is remembered silently.」摘要是背景工作，沒有人在看，所以它沒有資格直接記住任何關於使用者的事——包括在 agent 本機。

ADD/UPDATE/DELETE/NOOP（✅ Mem0）屬於 notes 管線的整併問題，本規格不碰。

---

## 7. 錯誤處理與降級階梯

| 失敗 | 行為 |
|---|---|
| fold 的 LLM 呼叫失敗／逾時／JSON 不合 schema | 用第 0 級抽取式；記 warn 一次 |
| 第 0 級也失敗（不應發生） | `drop_oldest_turn`，今天的行為 |
| episodes 寫入失敗 | 不摺、不刪；視窗可能超預算一輪；下一輪重試 |
| 背景 fold 完成時使用者已分岔（sidecar `based_on` ≠ 新 frame 的 `parent`） | 丟棄 sidecar，不記事件；不算失敗 |
| 背景 fold 還在跑，85%／400 先到 | 同步第 0 級 fold 進 frame；abort 背景 handle、刪 sidecar |
| `fold_model` 指到不存在／不合格的 ref | warn 一次，退回 smart／主鏈那一列 |
| frame 解不開／鏈中斷（被 sweep 砍掉中段） | `prior` 用已收集到的部分，標 `truncated`，warn 一次；等同今天的 miss |
| 舊格式對話檔（裸 `Vec<RichMessage>`） | 當成 `parent: null` 的單一 frame，照常讀 |
| FTS5 不可用（損毀或重建中） | `recall_conversation` 純向量；回覆標 `lexical_unavailable` |
| 無 embedder（`OMLX_API_KEY` 未設，本機已見此 warn） | 純 BM25；回覆標 `vector_unavailable` |
| checkpoint 損毀 | 從該檔 offset 0 重掃，以 `msg_id` 去重，不重複索引 |
| FTS5 與向量兩者皆不可用 | 工具回空並說明；不得退回全表掃描 |
| `conversations.enabled: false` | Tier 0 與 episodes 照常運作；只有 `mur chat` 那側不吃 |

---

## 8. 測試與驗收

### 8.1 單元（stub LLM，離線）

- `fold_keeps_latest_two_turns_verbatim`
- `fold_never_separates_ledger_from_its_turn`（沿用 turn-ledger 測試）
- `fold_level0_carries_failures_verbatim`：`Failed("EDEADLK …")` 摺疊後仍逐字在 `ledger_digest`
- `fold_marks_superseded_never_deletes`
- `fold_aborts_when_episode_write_fails`（三道保護）
- `pending_fold_applies_only_when_parent_matches_based_on`（分岔後 sidecar 被丟、frame 無 state）
- `single_flight_fold_per_conversation_key`（50% 連續兩輪只 spawn 一次）
- `sync_fold_wins_over_in_flight_background_fold`（85% 先到：frame 有第 0 級 state、sidecar 不存在、handle 已 abort）
- `fold_request_uses_fold_model_when_set`；`fold_request_hits_primary_chain_when_smart_disabled`（記錄 `Event::Routing.intent == "background/maintenance"` 且候選為主鏈）
- `message_cap_triggers_fold_before_drop_oldest_turn`（401 則、200k 視窗：先 fold，fold 後不超才不砍）
- `chain_walk_stops_at_state_frame`；`chain_walk_survives_missing_parent`
- `fork_from_same_parent_keeps_both_branches`（編輯重送不污染另一支）
- `legacy_bare_vec_file_still_loads`
- `sweep_is_pure_lru_and_keeps_sibling_branch_ancestors`（A 支 fold 後，B 支的共同祖先不被優先砍）
- `fold_event_records_frame_ids_not_turn_numbers`（分岔後兩支各自 fold，各批 digest 各自可重算）
- `fold_state_tracks_cumulative_coverage_but_event_tracks_batch`（連續兩次 fold：frame count/range 累積，兩個事件各自只指本批）
- `structured_output_native_schema_when_supported`；`structured_output_parse_or_schema_failure_falls_back_to_level0`
- `state_budget_truncates_all_fields_in_declared_order`（含超大的 decisions/open/artifacts）
- `ledger_digest_truncates_last_and_marks_every_cut`（保留最新失敗與 `truncated` 標記）
- `fold_commit_recovers_after_each_fsync_boundary`（四個 crash point 重啟後補完且事件只一筆）
- `fold_id_collision_with_different_payload_is_rejected`
- `episode_retry_dedupes_by_stable_event_id`（user／assistant／ledger 在每個 crash point 重試都只留一筆；同 ID 不同 payload 拒寫）
- `episode_retention_preserves_source_of_truth_by_default`（清 raw／index／rollup 後可由 episodes 重建；只有明設 `episode_retention_days` 才刪原始 event）
- `scope_identity_is_runtime_derived_and_null_fleet_never_matches_same_fleet`（含非 Git cwd、canonicalize 失敗拒寫）
- `malicious_recall_is_escaped_redacted_and_never_executable`（含偽 system 指令、tool arguments 與 credential）
- `episode_importance_is_present_on_every_event`（level 1 為 1–10；未 fold／level 0 固定 5）
- `state_block_is_first_message_and_runtime_attributed`
- `budget_split_holds_at_8k_and_200k`

計量（§4.3.0）：

- `estimator_counts_cjk_as_one_token`：`"預算沿用今天的"` 估 7，不是 21÷4=5
- `estimator_matches_bytes_div_4_on_pure_ascii`（不回歸英文既有行為）
- `estimator_walks_chars_not_bytes`（`len()` 用在多位元組字串上是 bug 本身）
- `ratio_ewma_clamps_to_half_and_triple`：連續灌入 10× 的離群樣本，ratio 停在 3.0
- `ratio_is_none_until_three_samples`；`cjk_heavy_conversation_gets_conservative_margin`（CJK > 30% 且樣本不足時乘 1.15）
- `ratio_persists_across_reload`（存在 frame 的 `token_ratio`，沿 `parent` 複製，第一次 fold 前就有值）
- `ratio_ignores_zero_input_tokens`（Ollama `unwrap_or(0)` / stub 的 0 不進 EWMA，`samples` 不加）
- `ratio_resets_on_model_change`（`LlmResponse.model` 變 → `{ratio:1.0, samples:0}`，不跨模型平均）
- `ollama_ratio_only_moves_upward`（Ollama 來源、`sample < ratio` 的樣本被丟棄；`sample > ratio` 進 EWMA）
- `summarizer_calls_do_not_feed_ratio`（fold 子請求不是完整 prompt，不進校準）
- `estimated_prompt_tokens_includes_system_and_tools`（`messages` 相同、工具清單多 2k token → 估計值同步上升；今天的 `estimated_tokens(history)` 不會）
- `forced_fold_fires_before_window_overflow_on_cjk_transcript`：**這是第 2 題的迴歸測試**——一份全中文 transcript 餵到 85%，舊估計器不觸發、新估計器觸發
- `route_estimated_tokens_unchanged`（`mur-core/src/route/` 不受本規格影響）

- `lexical_only_cjk_hit`：無 embedder 時「預算」命中含該詞的 episode；單字「預」透過 prefix 命中
- `fts5_bigram_and_unicode61_union`：同一查詢命中純英文與純中文 episode，union 後 `msg_id` 不重複
- `v1_to_v2_migration_resumes_after_crash`：複製中途殺掉，重啟後 v2 從頭重建、v1 保留，無半狀態被讀取
- `v1_null_scope_never_recalled`：v1 資料補 null 後，任何 agent `ScopeFilter` 都不命中
- `rrf_relevance_clamped_to_one`：雙列表 top-1 為 1.0；單列表 rank-60 為 0.5
- `final_score_is_product_of_three`：固定三分數斷言乘積；`importance=10` 不壓過 `recency=0.1`
- `mmr_skips_lexical_only`：詞法-only 候選不進 MMR，改走 Jaccard，重複者只留 final 較高者
- `read_your_writes_after_episode_fsync`：episode 寫入並 fsync 後立刻 recall，第一輪命中
- `checkpoint_atomic_advance`：整批成功前殺掉，checkpoint 不推進；重跑以 `msg_id` 去重
- `catchup_bounded_by_lines_and_time`：製造 1000 行落差，catch-up 在 200 行或 200 ms 停止並標 `stale`

- `oss_migration_never_renames_table`：v1 → v2 只切換原子 manifest，驗證未呼叫 `rename_table`，且提交前不刪 v1
- `watermark_uses_file_offsets_not_event_id_order`：打亂 event/message ID 字典序，catch-up 仍只依 `{episode_date, path, inode, byte_offset}` 判斷與排序
- `crash_between_fts_and_vector_repairs_missing_side`：FTS5 commit 後、LanceDB commit 前殺掉；重啟補齊向量側，兩側 payload hash 一致後才推進 checkpoint
- `fts5_bm25_lower_score_ranks_first`：兩條 MATCH 都以 BM25 升冪排名，同一 `msg_id` 取最佳 rank，不取 raw score max

### 8.2 迴歸（必須繼續通過）

- turn-ledger 規格 §6 全部：2026-09-18 事故的三個條件（空 ledger 可見、無附件可見、ledger 不被折進 assistant 文字）在摺疊後仍成立。

### 8.3 行為驗收（對應 LongMemEval 五能力 ✅ arXiv 2410.10813，作為清單而非分數）

| 能力 | MUR 驗收情境 |
|---|---|
| 資訊抽取 | 第 40 輪問「一開始那個錯誤碼是什麼」——答案在 `facts` |
| 多會話推理 | 新對話問「上次 EDEADLK 怎麼解的」——`recall_conversation` 命中並引用 |
| 時間推理 | 「昨天 vs 今天的決定」——episodes 帶 ts，摘要帶 turn 範圍 |
| 知識更新 | 第 12 輪改方向後，第 30 輪不回頭提舊方向——`superseded_by` |
| 拒答 | 沒談過的事要說沒談過——工具回空、摘要留空 |

黃金路徑：30 輪 stub 對話、8k 預算、每輪 500 tokens；結束時 state ≤ 1.2k tokens、尾窗 ≥ 2 輪、episodes 90 行、fold 事件 ≥ 3，且每個 fold 的 digest 可由 episodes 重算驗證。

---

## 9. 分期

| 期 | 內容 | 價值 |
|---|---|---|
| **P1** | §4.1 一輪一 frame + 父指標走鏈；`ConversationState` 變體；**§4.3.0(a) 字形感知估計器**；第 0 級抽取式 fold；episodes 每輪 append；三道保護 | 寫入由 O(N²) 變 O(N)；分岔不再互相覆蓋；截斷變成有摘要的截斷；離線可測；零 LLM 成本 |
| **P2** | 第 1 級 LLM 增量摘要（`TaskRunner` 直接 spawn、pending sidecar、單飛、`fold_model`、structured-output capability／驗證、全欄 15% 裁切、`fold_id` 冪等 crash recovery）；`superseded_by`；importance 分數；**§4.3.0(b) `input_tokens` 漂移校準** | 長對話不再失憶；閥門對得上真實計費；跨檔提交可重播不重複 |
| **P3** | §5.1.1 FTS5 + CJK bigram；§5.1.2 LanceDB v2 migration 與 scope 硬牆；checkpoint／read-your-writes；`Source::MurAgent` ingester；RRF + `recall_conversation` | agent 能立即、安全地回查自己；詞法與向量可獨立降級；`mur chat ask` 涵蓋 agent 對話 |
| **P4** | 自動回想（預設關）；memory-proposal 候選（先抽出 `write_memory_proposal_only`，`remember` 路徑重構後行為不變，§6）；輪內 `ToolResults` 剪枝（同類工具只留最近 M 筆，Anthropic context editing 🟡 同思路） | 錦上添花，各自可獨立取捨 |

P1 是一個 PR 的量（`task_runner.rs` + 新 `conversation_state.rs` + `llm/mod.rs` 一個變體 + 各 provider 的渲染 fallback）。

---

## 10. 開放問題

1. **Channel 與 episodes 的關係。** Channel（`~/.mur/channels`）已是 Hub 的真相來源、有 FTS5。長期是否讓 Tier 1 直接讀 channel、不另寫 episodes？今天不做，因為 runtime 在沙箱裡、單一寫者原則（federation 原則 3）讓 runtime 不該連 `mur-channel` 寫檔；而且 channel 沒有 ledger 與 fold 事件。等 channel 有「runtime 附註」事件種類再回頭合併。
2. **Hub 要不要顯示 ConversationState。** 對使用者是很好的「agent 現在以為的狀態」透明面板，但屬 UI 規格。
3. **中文 BM25 的後續量測。** §5.1.1 已定案應用層 CJK bigram，不再考慮 trigram tokenizer；上線後量測兩字、單字 prefix 與中英混合查詢的 precision／recall，再調整查詢擴展，不改變唯一 CJK 切分函式。
4. **數字都是 MUR 的選擇。** 50/85%、15%、尾窗 2 輪、c=60、γ=0.995——研究定稿 §5 明講業界沒有可信的通用數值；這些寫進 config，黃金路徑測試守住行為，數字留給實測調。
5. **估計器的下一步。** §4.3.0(a) 的 CJK≈1 token 是各 BPE 詞表的公約數，不是任一家的真值；(b) 的 ratio 會把殘差吸掉，但每個 provider 的 ratio 其實不同——若實測顯示同一對話換模型時 ratio 跳動過大，就把 `token_ratio` 從單值改成 per-model map。先不做，等 (b) 上線後看真實分佈。

---

## 11. 參考

- 研究定稿 `~/.mur/artifacts/deep-research/20260921-final-agent-memory-best-practices.md` §1.4、§2.1–2.3、§3.1–3.5、§4
- MemGPT arXiv 2310.08560 ✅ · Lost-in-the-middle arXiv 2307.03172 ✅ · Generative Agents arXiv 2304.03442 ✅ · Zep arXiv 2501.13956 ✅ · Mem0 arXiv 2504.19413 ✅ · LongMemEval arXiv 2410.10813 ✅
- 本庫：`task_runner.rs:98-130, 216-392, 730-748`；`turn_ledger.rs`；`mur-core/src/conversations/{retention.rs, ask/retrieve.rs, ingest/}`；`mur-channel/src/index.rs:161-165`
