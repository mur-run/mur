# `mur browser replay --heal`：離線自我修復、驗證後才寫回、有預算

**狀態**：proposed，還沒有程式碼。
**取代**：`2026-09-10-browser-phase1-spec.md` §6 步驟 3–4 的 L3（agent）自癒方案。
**落地計畫**：`docs/superpowers/plans/2026-09-23-browser-skill-layer-plan.md` Task 5（本文同步改寫）。

## 問題

`Step.healed`（`mur-browser/src/recorder.rs:145`）沒有任何程式會設它，`--heal` 目前直接報錯：

```rust
// mur-core/src/cmd/browser/mod.rs:788
bail!("mur browser replay --heal is not implemented yet (self-healing lands in Task 5)");
```

原計畫 5.4 寫「首選 locator 失敗 → 依序試其餘候選 → 標 `Healed`」，但 `run_step`（`mur-browser/src/replay.rs:193`）本來就依序試全部 `locators`。照做只是把現在的 `Passed` 改名為 `Healed`，沒有產生任何新 locator，也沒有東西可寫回。真正的自癒要處理的是**全部候選都 miss** 的情況。

## 決策

| # | 題目 | 決定 |
|---|---|---|
| D1 | 誰來找新元素 | **Rust 離線比對，零 LLM**。L3（agent）列為非目標 |
| D2 | 怎樣算找到 | **role 相同 + 名稱相似 + 明顯領先第二名**，否則不 heal |
| D3 | 怎樣算 heal 被證實 | **下一個元素步驟必須直接命中**；沒有後續可驗證的不寫回，標 `unverified` |
| D4 | 預算分母 | **元素步驟數**，允許次數 `max(1, floor(n × ratio))`，只在 `mode: test` 生效 |

## 名詞

- **元素步驟**：`action.needs_locator()` 為真且 `action != AssertText` 的步驟，即 click / fill / select / press / hover / assert_visible / assert_value。
  `assert_text` 雖然也會解析 locator，但送出的參數只有 `{text}`（`replay.rs:261`），ref 根本不交給 Playwright，所以它**不 heal、不算分母、不能拿來證實 heal**。它若失敗，照舊使整個 run 失敗。
- **直接命中**：`locators[]` 中既有候選在 snapshot 裡解析成功，或 testid 後備（`71c638ed`）被 server 接受。不經過 heal。

## D1 — 離線找節點

只在以下條件**全部**成立時才進入 heal：`--heal` 有開、元素步驟、**定位失敗**（見下）。

### 定位失敗 vs 動作失敗

heal 只處理「找不到元素」，不處理「找到了但動作失敗」。判斷點放在**動作送出之前**：

- `run_step` 的解析階段（snapshot `resolve`）全部 miss，而且步驟**沒有 testid 候選** → 回 `StepError::LocateMiss`，這是唯一會觸發 heal 的錯誤。此時還沒有任何動作送給 Playwright，heal 後重送不會重複操作。
- testid 後備一旦送出，之後任何錯誤（找不到、不可操作、斷言不符、動作後等待失敗）都歸為 `StepError::Action`，**直接 fail，不 heal**。Playwright 的錯誤文字分不出這幾種，而點擊可能已經發生。
- `assert_args` 的錯誤、snapshot 本身失敗、transport 錯誤，都不是 `LocateMiss`。

代價：帶 testid 候選的步驟永遠不會 heal。真實 Playwright snapshot 不帶 testid（`replay.rs:206` 註解），這類候選只有靠 testid 後備才會用到；寧可讓這種步驟失敗、請使用者重錄，也不要冒著重複點擊的風險 heal。

1. **取 role**：從 `step.locators` 找第一個 `role:` locator 的 role。沒有 role locator → 不 heal，理由 `no role locator to anchor on`。
   `ref_at_record` 只供除錯，永遠不用（`recorder.rs:149` 的既有約定）。
2. **取線索**：舊 locator 的 name / text / label 值，加上 `step.intent`。
3. **篩候選**：snapshot 裡 `role` 相同（不分大小寫）的節點。
4. **計分**：對每個候選的 `name`、`text`、`label` 與線索做詞彙重疊度（見下），取最高者為該節點分數。
5. **採用條件**：最高分 ≥ `HEAL_MIN_SCORE`，**且**與第二名差距 ≥ `HEAL_MIN_MARGIN`（只有一個候選時，第二名視為 0）。否則不 heal，錯誤訊息列出前兩名的 role / name / 分數。
6. **產生新 locator**：對選中節點呼叫 `locator::candidates_for_ref`（`locator.rs:147`），它依 role → testid → label → text 產生、從不產生 `@ref`。結果與舊 `locators` 去重後**前插**。

### 詞彙切分

- 全部轉小寫，Unicode NFKC 正規化。
- ASCII 字母數字：以非字母數字切詞。
- CJK 字元：取**字元 bigram**（單字時取單字），因為中文沒有空白分詞。「送出」vs「送出訂單」→ `{送出}` ⊂ `{送出, 出訂, 訂單}`。
- 分數 = |線索 ∩ 候選| / |線索 ∪ 候選|（Jaccard）。

### 常數（`mur-browser/src/heal.rs`，不寫死在邏輯裡）

| 常數 | 初值 | 說明 |
|---|---|---|
| `HEAL_MIN_SCORE` | `0.3` | 起始值，要用測試 fixture 校準 |
| `HEAL_MIN_MARGIN` | `0.15` | 同上 |
| `DEFAULT_HEAL_RATIO` | `0.2` | 沿用計畫 |

兩個比對常數**暫定**：初值會在實作時拿 fixture（testid 改名、文字小修、同 role 多節點）調整，調整結果回填本表。

## D3 — 驗證與回滾

heal 產生的狀態：

```rust
pub enum HealStatus { Pending, Verified, Unverified, RolledBack }

pub struct HealEvent {
    pub step: u32,
    pub from: Vec<String>,   // heal 前的 locators
    pub to: Vec<String>,     // candidates_for_ref 產生、去重後要前插的整組 locators
    pub node: String,        // 選中節點的 "role name"，給人看
    pub score: f32,
    pub reason: String,      // 為什麼 heal（哪些候選 miss）
    pub status: HealStatus,
}
```

（計畫原本的 `from: String` / `to: String` 都改為 `Vec<String>`：全部 miss 時沒有單一的「舊 locator」；寫回要前插整組候選，而 CLI 手上沒有當時的 snapshot，無法事後重建，所以整組必須放在報告裡。）

規則：

1. heal 成功 → 該步驟 `StepStatus::Healed`，事件 `Pending`。
2. 下一個**元素步驟**：
   - 直接命中 → 事件 `Verified`。
   - 失敗，或它自己也要靠 heal → 前一個事件 `RolledBack`，**前一個 heal 的步驟**改標 `Failed`，訊息附選中節點；run 停止。兩個 heal 不能互相擔保。
3. 中間夾的 `assert_text` 失敗 → 同樣回滾 `Pending` 的事件並失敗。
4. run 結束時仍 `Pending`（後面沒有元素步驟）→ `Unverified`，本次仍算通過，但不寫回。

### 寫回 `actions.yaml`

- 只寫 `Verified` 的事件：把 `HealEvent.to` 整組前插、`healed: true`、`last_hit: 0`。
- **只在整個 run 沒有 `Failed` 且沒超預算時才寫**。其他步驟失敗代表頁面狀態不可信，已驗證的 heal 也先不存。（**假設**，與 spec §6 步驟 3「寫回並繼續」不同，選保守。）
- 寫檔用 temp file + rename（CLAUDE.md 開發備註）。寫回動作放在 `mur-core` 的 `replay` 命令，`mur-browser::replay` 只回傳報告，保持無副作用、好測。

## D4 — 預算

```rust
pub fn allowed_heals(total: u32, max_ratio: f32) -> u32   // total==0 → 0；否則 max(1, floor)
pub fn budget_exceeded(healed: u32, total: u32, max_ratio: f32) -> bool  // healed > allowed_heals
```

- `total` = 元素步驟數；`healed` = `Verified` + `Unverified` + `Pending`，**不含 `RolledBack`**（那一步已經 Failed）。
- 先算整數允許次數再比較；`floor` 前加 `f64` 轉換與 `1e-6` 容差，確保 `10 × 0.2` 得 2。
- 只在 `Mode::Test` 生效；automation 照樣 heal、驗證、寫回，但不受預算限制。
- `--max-heal-ratio` 必須在 `0.0..=1.0`，否則 clap 階段報錯。
- 超預算：`replay` **仍回 `Ok(report)`**，報告帶 `budget_exceeded: Some(BudgetExceeded { healed, allowed, max_ratio })`。CLI 照常先寫報告 yaml，再判斷 `failed > 0 || budget_exceeded.is_some()` 回非零；**不寫回任何 heal**。這樣失敗那次的報告不會遺失，也不會留著上次成功的舊報告。錯誤訊息：
  `heal rate too high: 3 of 10 element steps healed (allowed 2 at --max-heal-ratio 0.2); the recording is stale — re-record it`

## 輸出

- `ReplayReport.heals: Vec<HealEvent>`，寫進既有 report yaml（`paths::run_report`）。
- `verdict()` 改為：`failed > 0 || budget_exceeded.is_some()` → `red`；否則 `healed > 0` → `yellow`；否則 `green`。超預算時命令回非零，摘要不能是 yellow。
- `ReplayReport.written_back: u32`（`#[serde(default)]`），**由 CLI 在 `actions.yaml` rename 成功後填入**，`mur-browser::replay` 一律回 0。不能拿 `Verified` 數量代替：有 `Failed`、超預算或寫檔失敗時，就算有 `Verified`，`written_back` 仍是 0。
- CLI 順序：寫回（錯誤先接住，不用 `?`）→ 填 `written_back` → 寫報告 yaml → 印 `summary()` → 最後才依 `failed > 0 || budget_exceeded.is_some() || 寫回錯誤` 回非零。寫回失敗也不會丟報告。
- `summary()` 多一行：`healed 2 (1 verified, 1 unverified, written back 1)`，written back 取 `written_back` 欄位。
- tracing：每次 heal、驗證、回滾各一筆 `info`，欄位 `step`、`status`、`score`、`node`。

## 非目標

- **L3 agent 自癒**。`mur agent` 沒有 `run` 子指令（只有 `Send`，`mur-core/src/cli/agent.rs:112`），也沒有 `browser-worker` agent；要做得先補這兩樣。之後若做，在 `heal.rs` 抽 trait，本設計的驗證、預算、寫回全部沿用。
- 跨 role 比對（`link` ↔ `button`）。
- heal `assert_text`。

## 前置：拆檔（獨立 PR，純搬移）

CLAUDE.md 規則 4：單檔 ≤ 800 行，拆檔與行為變更分開。

| 檔案 | 現在 | 拆法 |
|---|---|---|
| `mur-browser/src/replay.rs` | 823 行 | 測試移到 `replay/tests.rs`；`StdioCaller` 移到 `replay/stdio.rs` |
| `mur-core/src/cmd/browser/mod.rs` | 879 行 | `replay` 與其輔助函式移到 `cmd/browser/replay.rs`，同 `doctor.rs` / `setup.rs` 的樣式 |

## 測試

`heal.rs` 單元測試：

- 預算：`(2,10,0.2)` false、`(3,10,0.2)` true、`(1,3,0.2)` false、`(2,3,0.2)` true、`total == 0` false。
- 比對（直接呼叫比對函式的單元測試）：testid 改名但 role+name 相同 → 採用；「送出」→「送出訂單」→ 採用；同 role 兩個分數相近的節點 → 拒絕；沒有 role locator → 拒絕；role 不同但名稱相同 → 拒絕。
- 新 locator 不含 `@ref`、已去重、前插。

`replay.rs`（用既有 fake `ToolCaller`）：

- 真正全部 miss 的整合案例：錄製時 `role:button name=送出`、無 testid，頁面改成「送出訂單」→ 進 heal → 下一步命中 → `Verified`。（testid 改名但 role/name 不變的情況會直接命中既有 role locator，不會進 heal，只放在單元測試。）
- 步驟有 testid 候選、testid 後備送出後 Playwright 回錯 → `Failed`，**不 heal**，`call_tool` 對該動作只被呼叫一次。
- snapshot 命中但動作回錯（元素不可操作）→ `Failed`，不 heal。
- heal 後下一個元素步驟命中 → `Verified`。
- heal 後下一個元素步驟也要 heal → 前者 `RolledBack` 且為 `Failed`。
- heal 後 `assert_text` 失敗 → 回滾。
- 最後一個元素步驟 heal → `Unverified`，run 通過。
- `mode: automation` 超過比率 → 不報錯；`mode: test` → 報錯。
- `failed == 0 && budget_exceeded.is_some()` → `verdict()` 為 `red`。
- `replay` 回傳的報告 `written_back` 一律為 0。

`mur-core`：寫回只含 `Verified`，且 `to` 的多個候選全部依序前插；有 `Failed` 時檔案不變；超預算時 `actions.yaml` 不變、**報告 yaml 已寫入且含 `heals` 與 `budget_exceeded`**、命令回非零。`written_back`：正常寫回時等於實際前插的步驟數；有 `Verified` 但超預算（或有 `Failed`）時為 0。

## 待決

1. 兩個比對常數的最終值（實作時用 fixture 校準）。
2. `replay` 失敗且原因是全部 miss 時，要不要提示「加 `--heal` 再試」——建議加，一行訊息。
