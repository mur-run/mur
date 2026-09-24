# `/browser-*` Skill 層實作計畫

> Date: 2026-09-23
> Spec source: `docs/superpowers/specs/2026-09-10-browser-phase1-spec.md`
> （Task 9 已完成：原檔 `~/.mur/artifacts/mur/browser-research-20260910/SPEC-phase1.md`
> 未受版控，已移入 repo，`mur-browser/src/lib.rs:17` 同步改指新路徑。）
> 執行技能：`mur-executing-plans`（單人循序）；Task 6/7 可用 `mur-delegate-dev` 併行

---

## Goal

把 SPEC-phase1 §1.1 唯一未交付的那一列補完：`~/.mur/skills/browser-{auth,test,automation}/SKILL.md`，並補上三個 skill 依賴、但引擎層還沒有的 `replay` / `export` / domain allowlist / 保留政策。

## Architecture

引擎層（`mur-browser` crate）與 CLI 層（`mur browser <sub>`）已經存在且可用；缺的是 agent 看得懂的入口。Skill 以子行程方式 spawn `mur browser`，子行程獨佔自己的 stdio，因此完全不與 murmur TUI 爭搶終端。`browser-auth` 產出加密 profile，`browser-test` 與 `browser-automation` 各自以該 profile 執行 record/replay。

## Tech stack

Rust 2024（`mur-browser`, `mur-core`）、`@playwright/mcp`（npx spawn）、age 加密 + OS Keychain、serde_yaml、Markdown + YAML frontmatter（SKILL.md）。

---

## 先修正：上一輪 review 的三個錯誤結論

執行者請以本節為準，不要照先前的口頭 review 施工。

| 先前說法 | 實際狀況（已讀碼驗證） |
|---|---|
| 「broker 是 `not_yet` 樁」 | **錯。** `mur-browser/src/broker.rs` 有 553 行完整實作，`mur-core/src/cmd/browser/mod.rs:136` 的 handler 會起 Unix socket 並 serve |
| 「storageState 是明文 session token」 | **錯。** `state.rs:1-4`：「The age identity is kept in the OS Keychain, never beside the encrypted state. The on-disk `state.json.age` file is always mode 0600.」 |
| 「`list` / `show` 是樁」 | **錯。** 兩者都有實作（`mod.rs:161`、`mod.rs:181`），`show` 還會先 parse 驗證再輸出 |

**真正還是 `not_yet` 的只有兩個**，`mur-core/src/dispatch.rs`：

```
609:            BrowserAction::Replay { .. } => cmd::browser::not_yet("replay")?,
619:            BrowserAction::Export { .. } => cmd::browser::not_yet("export")?,
```

秘密處理也已經做完：`recorder.rs:62` 明講 `Secrets are stored as {{secret:<site>/<KEY>}} — never plain.`，而 `proxy.rs:133-145` 的 BrokerHook 在 broker 不可用時 fail-closed。

**因此本計畫剩下的真實缺口是：** replay、export、domain allowlist、保留政策、以及三份 SKILL.md。

---

## Global Constraints

逐條複製自 spec 與既有程式碼約定，每個 Task 都隱含包含：

- 憑證永不進 agent context：值一律以 `{{secret:<site>/<KEY>}}` 佔位，真值只在 broker 的轉換後請求與記憶體租約表中存在。
- 任何落地的 state 檔為 age 加密 + mode `0600`；metadata 只在 state 加密成功後才發布（`auth.rs:36-37`）。
- 所有 `<run>` / `<site>` 名稱先過 `mur_browser::paths::validate_name`，不得直接拼路徑。
- 新增 CLI 行為時，同步更新 `BrowserAction` 的 doc comment，移除「implemented in a later slice」字樣。
- 每個 Task 以 TDD 進行：先寫失敗測試 → 看它失敗 → 最小實作 → 看它通過 → commit。
- 不得在 murmur TUI 行程內呼叫 `run_stdio`；skill 一律 spawn 獨立子行程。

---

## 檔案結構（施工前鎖定）

### 新增

| 路徑 | 唯一職責 |
|---|---|
| `mur-browser/src/replay.rs` | 把 `Run` 的 steps 轉成 Playwright MCP tool call 序列並執行，回傳 `ReplayReport` |
| `mur-browser/src/heal.rs` | locator 失敗時挑候選、記錄 heal 事件、計算 heal 率門檻 |
| `mur-browser/src/export.rs` | `Run` → `.spec.ts` 文字 |
| `mur-browser/src/guard.rs` | domain allowlist 比對與越界拒絕 |
| `mur-browser/tests/fixtures/run-roundtrip.yaml` | 涵蓋全部 9 個 `Action` 變體的 `Run`，鎖住 action ↔ tool 映射 |
| `docs/superpowers/specs/2026-09-10-browser-phase1-spec.md` | 從 artifacts 搬入版控的 phase 1 spec |
| `docs/superpowers/specs/2026-09-10-browser-research-merged.md` | 同上，研究合併結論 |
| `~/.mur/skills/browser-auth/SKILL.md` | 登入與 profile 生命週期入口 |
| `~/.mur/skills/browser-test/SKILL.md` | 有斷言的錄製 / 重播 / 產 spec |
| `~/.mur/skills/browser-automation/SKILL.md` | 無斷言的重複任務執行 |

### 修改

| 路徑 | 改動 |
|---|---|
| `mur-browser/src/lib.rs` | 掛上 4 個新 module；`Design source:` doc comment 改指版控內路徑（Task 9.3） |
| `mur-browser/src/recorder.rs` | `Action` 增加 `tool_name()`（Task 0），與既有 `action_for` 互為反向 |
| `mur-browser/src/auth.rs` | `ProfileMeta` 增加 `allow_domains: Vec<String>` |
| `mur-core/src/cli/actions.rs` | `BrowserAction::{Replay, Export}` 補參數；`Replay` 加 `--max-heal-ratio`；`Auth` 補 `--allow-domain`；新增 `Prune` |
| `mur-core/src/dispatch.rs` | 兩個 `not_yet` 換成真 handler；接上 `Prune` |
| `mur-core/src/cmd/browser/mod.rs` | `replay` / `export` / `prune` handler |

---

## Task 0 — action ↔ tool 映射鎖定（所有 replay 工作的前置）

`recorder.rs:241-256` 的 `action_for` 已有 `tool_name → Action` 的**正向**映射，
`value_for`（`recorder.rs:257-268`）也已釘死參數鍵名：`Goto→url`、`Fill→text`、
`Select→values`、`Press→key`、`AssertText/AssertValue→text`。

replay 需要的是**反向**：`Action → tool_name + arguments`。這個函式目前不存在，
而 Task 4.5「逐步送 tool call」正是靠它。沒有它，施工者只能自己猜 tool 名稱，
且猜錯不會被任何測試擋下 —— 因為正反兩份對照表沒有任何機制綁在一起。

不需要連真實 MCP server：`recorder.rs` 已經是這些名稱的權威來源，
fixture 就是 round-trip 測試本身。

### Interfaces

**Consumes:** `recorder::{Action, Step, Run, Mode}`
**Produces:**
- `pub fn mur_browser::recorder::Action::tool_name(self) -> &'static str`
- `pub fn mur_browser::replay::call_for(step: &Step) -> Result<(String, serde_json::Value)>`
- `tests/fixtures/run-roundtrip.yaml` — 一個涵蓋全部 9 個 `Action` 變體的 `Run`

### Steps

- [x] 0.1 先寫測試：對 9 個 `Action` 變體逐一呼叫 `tool_name()`，
      再餵回 `Recorder::action_for` 的同名查表，必須還原成同一個 `Action`
      （`browser_type`/`browser_fill` 這類多對一，正向取第一個即可）
- [x] 0.2 補測試：`call_for` 對 `Goto` 產出的 `arguments` 必須含 `url` 鍵，
      對 `Fill` 含 `text`，對 `Press` 含 `key` —— 鍵名直接對照 `value_for`
- [x] 0.3 補測試：`Action::needs_locator()` 為真的步驟，`call_for` 必須帶
      `ref` 或 `element`；`Goto` 則不得帶
- [x] 0.4 `cargo test -p mur-browser roundtrip` → 失敗
- [x] 0.5 實作 `tool_name` 與 `call_for`，讓測試轉綠
- [x] 0.6 寫 `tests/fixtures/run-roundtrip.yaml`，9 個變體各一步，
      `mode: test`、`recorded_at` 用固定時戳（避免測試不穩定）
- [x] 0.7 `cargo test -p mur-browser && cargo clippy --all-targets -- -D warnings`，commit

**這個 Task 擋住 4、5、6、7。** 完成前不要派工那四個。

---

## Task 1 — `browser-auth` skill（可立刻交付）

引擎層已完整，這個 Task 不碰 Rust。

### Interfaces

**Consumes:** 既有 CLI `mur browser auth <site> --url <URL> [--reauth] [--browser <engine>]`、`mur browser status`
**Produces:** `~/.mur/skills/browser-auth/SKILL.md`；後續兩個 skill 以「先呼叫 browser-auth」為前置

### Steps

- [x] 1.1 讀 `mur-core/src/cli/actions.rs:797-812` 確認 `Auth` 的四個參數與 `BrowserEngine` 值域，抄進 skill 的指令表
- [x] 1.2 建立 `/tmp/browser-auth/SKILL.md`，frontmatter 比照 `~/.mur/skills/mur-brainstorm/SKILL.md`：`name: browser-auth`、`category: workflow`、`visibility: on_demand`、`provenance: human`、triggers 含 keyword `browser (login|auth)|登入|瀏覽器驗證` 與 `manual`
- [x] 1.3 內文明訂**三條不可違反的規則**：(a) agent 絕不代打帳號密碼，一律 handoff 給人；(b) 非互動情境下若 profile 不存在就直接報錯，不得自行開登入流程；(c) 呼叫時必須帶 `--browser`，避免 `select_browser` 進入互動詢問
- [x] 1.4 內文加「何時該 reauth」判準：`mur browser status` 顯示 `(incomplete)` / `(metadata missing)`，或 `earliest_cookie_expires` 已過
- [x] 1.5 `mur skill install /tmp/browser-auth`，預期輸出含 skill 名稱
- [x] 1.6 驗證：`mur skill info browser-auth` 讀得到、`mur skill validate ~/.mur/skills/browser-auth/skill.yaml` 回 `ok`（安裝產物是 canonical `skill.yaml`；`SKILL.md` 只是 `mur skill fmt` 的另一種表示，並非必要檔。原驗收條件寫錯，已更正）
- [x] 1.7 commit plan 進度（skill 裝在 `~/.mur`，repo 端只有此 plan 的勾選變更；
      若無其他檔案變動則跳過 commit，不要製造空 commit）

---

## Task 2 — profile domain allowlist

沒有這層，一個帶已登入 session 的 automation 可以在使用者帳號裡導航到任何頁面。

### Interfaces

**Consumes:** `ProfileMeta`（`mur-browser/src/auth.rs:28-33`）
**Produces:**
- `ProfileMeta.allow_domains: Vec<String>`（`#[serde(default)]`，空 = 不限制，向後相容既有 meta 檔）
- `pub fn mur_browser::guard::is_allowed(url: &str, allow: &[String]) -> bool`
- `pub fn mur_browser::guard::check(url: &str, allow: &[String]) -> anyhow::Result<()>`

### Steps

- [x] 2.1 新建 `mur-browser/src/guard.rs`，先寫測試：空 allowlist 全放行；`["example.com"]` 放行 `https://example.com/a` 與 `https://app.example.com/b`，拒絕 `https://evil.com` 與 `https://notexample.com`；拒絕非 http/https scheme
- [x] 2.2 `cargo test -p mur-browser guard` → 預期編譯失敗（module 未掛）
- [x] 2.3 `lib.rs` 加 `pub mod guard;`，實作 `is_allowed`：解析 host，比對「完全相等或以 `.` + allow 項結尾」，避免 `notexample.com` 誤中
- [x] 2.4 `cargo test -p mur-browser guard` → 全綠
- [x] 2.5 `auth.rs` 的 `ProfileMeta` 加 `#[serde(default)] pub allow_domains: Vec<String>`，`save_profile` 增加同名參數並寫入
- [x] 2.6 補測試：舊的不含該欄位的 meta YAML 仍可反序列化成功（向後相容）
- [x] 2.7 `actions.rs` 的 `Auth` 加 `#[arg(long = "allow-domain")] allow_domain: Vec<String>`；dispatch 與 `cmd::browser::auth` 一路傳下去；未指定時預設填入 `--url` 的 host
- [x] 2.8 `cargo test -p mur-browser && cargo test -p mur-core browser` → 全綠，commit

---

## Task 3 — `mur browser export`

`browser-test` 的產出物就是這個；沒有它，錄完的東西進不了測試套件。

### Interfaces

**Consumes:** `recorder::{Run, Step, Action, Mode}`、`recorder::from_yaml`、`paths::run_actions`
**Produces:**
- `pub fn mur_browser::export::to_spec_ts(run: &Run) -> anyhow::Result<String>`
- CLI `mur browser export <name> [--out <path>]`；無 `--out` 時印到 stdout

### Steps

- [x] 3.1 讀 `mur-browser/src/recorder.rs:52-98` 抄下 `Step` 全部欄位與 `Action` 的完整 variant 列表，逐一決定對應的 Playwright 呼叫
- [x] 3.2 新建 `mur-browser/src/export.rs`，先寫測試：一個含 `goto` + `fill` + assert 的 `Run`，輸出須含 `import { test, expect } from '@playwright/test';`、`test('<run.name>', async ({ page }) => {`、以及 `await page.goto(`
- [x] 3.3 補測試：`value` 為 `{{secret:acme/PASSWORD}}` 的 step，輸出**不得**含該佔位符原文，須轉成 `process.env.ACME_PASSWORD` 形式；且輸出絕不含明文密碼
- [x] 3.4 補測試：`mode: automation` 的 Run 呼叫 `to_spec_ts` 回 `Err`（automation 沒有斷言，不該產 spec）
- [x] 3.5 `cargo test -p mur-browser export` → 失敗
- [x] 3.6 實作 `to_spec_ts`：`locators` 取第一順位候選；每個 step 前輸出 `// <intent>` 註解；TS 字串一律跳脫單引號與反斜線
- [x] 3.7 `cargo test -p mur-browser export` → 全綠
- [x] 3.8 `cmd/browser/mod.rs` 加 `pub fn export(name: &str, out: Option<&Path>) -> Result<()>`：`validate_name` → 讀 `run_actions` → `from_yaml` → `to_spec_ts` → 寫檔或印出
- [x] 3.9 `actions.rs` 的 `Export` 加 `#[arg(long)] out: Option<PathBuf>`，doc comment 去掉 later slice 字樣；`dispatch.rs:619` 換成真 handler
- [x] 3.10 `cargo test && cargo clippy --all-targets -- -D warnings` → 全綠，commit

---

## Task 4 — `mur browser replay` 核心

### Interfaces

**Consumes:** `recorder::Run`、`proxy::run_stdio` 與 `playwright_command`、`broker::{Broker, SocketClient}`、`guard::check`（Task 2）
**Produces:**
- `pub struct mur_browser::replay::ReplayReport { pub run: String, pub total: u32, pub passed: u32, pub failed: u32, pub healed: u32, pub steps: Vec<StepOutcome> }`
- `pub struct mur_browser::replay::StepOutcome { pub step: u32, pub status: StepStatus, pub locator_used: Option<String>, pub message: Option<String> }`
- `pub enum mur_browser::replay::StepStatus { Passed, Healed, Failed, Skipped }`
- CLI `mur browser replay <name> [--profile <site>] [--heal] [--dry-run]`

### Steps

- [x] 4.1 新建 `mur-browser/src/replay.rs`，先寫測試：`--dry-run` 對一個 3 步的 Run 回傳 `total: 3`、`passed: 0`、全部 `Skipped`，且**不 spawn 任何子行程**
- [x] 4.2 補測試：Run 內某步的 `goto` URL 不在 profile 的 `allow_domains` 內時，回 `Err` 且錯誤訊息含該 host
- [x] 4.3 `cargo test -p mur-browser replay` → 失敗
- [x] 4.4 實作 dry-run 路徑：解析 Run、對每個 `goto` 呼叫 `guard::check`、產出全 `Skipped` 的報告
- [x] 4.5 實作實跑路徑：比照 `cmd/browser/mod.rs:57-82` 的 record 流程起 broker + `run_stdio`，逐步送 tool call，逐步收結果填 `StepOutcome`
- [x] 4.6 `cargo test -p mur-browser replay` → 全綠
- [x] 4.7 `cmd/browser/mod.rs` 加 `pub async fn replay(...)`；報告以 YAML 寫入 `paths::run_report(&home, name)` 並印出摘要行
- [x] 4.8 `actions.rs` 的 `Replay` 補三個參數，`dispatch.rs:609` 換真 handler
- [x] 4.9 `cargo test && cargo clippy --all-targets -- -D warnings`，commit

---

## Task 5 — 自癒與 heal 預算

設計：`docs/superpowers/specs/2026-09-24-browser-replay-heal-design.md`（2026-09-24 改寫本節：原 5.4「依序試其餘候選」是 `run_step` 既有行為，不算自癒）。

### Interfaces

**Consumes:** `replay::{StepOutcome, StepStatus}`、`locator::{SnapshotNode, Locator, candidates_for_ref}`
**Produces:**
- `pub enum mur_browser::heal::HealStatus { Pending, Verified, Unverified, RolledBack }`
- `pub struct mur_browser::heal::HealEvent { pub step: u32, pub from: Vec<String>, pub to: Vec<String>, pub node: String, pub score: f32, pub reason: String, pub status: HealStatus }`
- `pub fn mur_browser::heal::find_replacement(step: &Step, nodes: &[SnapshotNode]) -> Result<(String, f32), String>`（回選中的 ref 與分數；拒絕時回原因）
- `pub fn mur_browser::heal::allowed_heals(total: u32, max_ratio: f32) -> u32`
- `pub fn mur_browser::heal::budget_exceeded(healed: u32, total: u32, max_ratio: f32) -> bool`
- `pub const DEFAULT_HEAL_RATIO: f32 = 0.2; HEAL_MIN_SCORE; HEAL_MIN_MARGIN`
- `ReplayReport.heals: Vec<HealEvent>`
- CLI `mur browser replay ... [--heal] [--max-heal-ratio <0.0..=1.0>]`

### Steps

- [ ] 5.0 **前置 PR（純搬移）**：`replay.rs`（823 行）拆出 `replay/tests.rs`、`replay/stdio.rs`；`cmd/browser/mod.rs`（879 行）的 `replay` 移到 `cmd/browser/replay.rs`。行為不變，測試全綠後 commit
- [ ] 5.1 新建 `heal.rs`，先寫預算測試：`(2,10,0.2)` false、`(3,10,0.2)` true、`(1,3,0.2)` false、`(2,3,0.2)` true、`total == 0` false
- [ ] 5.2 比對單元測試（直接呼叫比對函式）：testid 改名 → 採用；「送出」→「送出訂單」→ 採用；同 role 兩節點分數相近 → 拒絕；無 role locator → 拒絕；role 不同 → 拒絕；新 locator 不含 `@ref`、去重、前插
- [ ] 5.3 `cargo test -p mur-browser heal` → 失敗
- [ ] 5.4 實作 `heal.rs`（詞彙切分含 CJK bigram、Jaccard、門檻＋領先差距），用 fixture 校準兩個常數並回填 spec
- [ ] 5.5a `run_step` 錯誤分類：解析階段全部 miss 且無 testid 候選 → `StepError::LocateMiss`；testid 後備送出後的任何錯誤、動作錯誤 → `StepError::Action`。補測試：testid 後備回錯與動作回錯都 `Failed` 且不 heal，動作只呼叫一次
- [ ] 5.5 `replay_with` 接 heal：只在元素步驟（非 `assert_text`）回 `LocateMiss` 時觸發；下一個元素步驟直接命中 → `Verified`，失敗或也要 heal、或中間 `assert_text` 失敗 → 回滾並把 heal 步驟標 `Failed`；結束仍 `Pending` → `Unverified`。補 spec「測試」節的五個 replay 案例
- [ ] 5.6 預算：分母為元素步驟數，只在 `Mode::Test` 檢查；超過 → 仍回 `Ok(report)`，`report.budget_exceeded = Some(BudgetExceeded { healed, allowed, max_ratio })`；CLI 先寫報告 yaml 再回非零，訊息含實際次數、允許次數、`--max-heal-ratio`。`verdict()` 在超預算時回 `red`。補測試：automation 不受限；test 超預算時報告已寫入、`actions.yaml` 不變；`failed == 0 && budget_exceeded.is_some()` → `red`
- [ ] 5.7 `mur-core`：`actions.rs` 的 `Replay` 加 `--max-heal-ratio`（clap 限 0–1）、拿掉 `--heal` 的 bail；只在無 `Failed` 且未超預算時，把 `Verified` 的 heal 以 temp + rename 寫回 `actions.yaml`，rename 成功後才填 `report.written_back`（寫回錯誤先接住，報告照寫再回非零）。補寫回測試，含「有 `Verified` 但超預算 → `written_back == 0`」
- [ ] 5.8 `cargo test -p mur-browser -p mur-core` + clippy `--all-targets -D warnings` → 全綠，commit

---

## Task 6 — `browser-test` skill

### Interfaces

**Consumes:** Task 1 的 browser-auth、Task 3 的 export、Task 4/5 的 replay `--heal`
**Produces:** `~/.mur/skills/browser-test/SKILL.md`

### Steps

- [ ] 6.1 建立 `/tmp/browser-test/SKILL.md`，frontmatter `name: browser-test`，triggers keyword `browser test|e2e|端對端|網頁測試`
- [ ] 6.2 內文固定工作流：`mur browser status` 檢查 profile → 缺則轉 browser-auth → `mur browser record --run <name> --mode test --trace --profile <site>` → `mur browser replay <name> --heal` → `mur browser export <name> --out <path>`
- [ ] 6.3 內文明訂：**mode 一律 `test`**；trace 預設開；自癒被拒（Task 5.5 回滾或 5.6 超預算）時不得重試，直接回報需重錄
- [ ] 6.4 加「絕不」清單：不得改 `actions.yaml` 的 assert、不得為了讓測試變綠而刪步驟
- [ ] 6.5 `mur skill install /tmp/browser-test`，`ls ~/.mur/skills/browser-test/SKILL.md` 驗證

---

## Task 7 — `browser-automation` skill

### Interfaces

**Consumes:** Task 1、Task 2 的 allowlist、Task 4 的 replay `--dry-run`
**Produces:** `~/.mur/skills/browser-automation/SKILL.md`

### Steps

- [ ] 7.1 建立 `/tmp/browser-automation/SKILL.md`，`name: browser-automation`，triggers keyword `browser automat|自動化|重複操作`
- [ ] 7.2 內文工作流：profile 檢查 → `mur browser record --run <name> --mode automation --profile <site>` → **必先 `mur browser replay <name> --dry-run`** 給人看 action plan → 確認後才實跑
- [ ] 7.3 內文明訂平行規則：storageState 唯讀載入、每個 worker 獨立 browser context、執行期 cookie 變動一律不回寫
- [ ] 7.4 內文明訂：automation **沒有斷言**，失敗即重試（上限 2 次）後回報，不得自行判斷「應該算成功」
- [ ] 7.5 加「絕不」清單：不得執行不可逆動作（付款、刪除、送出）除非使用者在本次對話明示；越界 domain 由 Task 2 的 guard 擋下，skill 不得試圖繞過
- [ ] 7.6 `mur skill install /tmp/browser-automation` 並驗證

---

## Task 8 — `mur browser prune`

Playwright trace 單次動輒數十 MB，目前只有產出沒有清理。

### Interfaces

**Consumes:** `paths::run_dir`、`recorder::from_yaml`（讀 `recorded_at`）
**Produces:** CLI `mur browser prune [--keep <N>] [--older-than <days>] [--dry-run]`

### Steps

- [x] 8.1 `cmd/browser/mod.rs` 加 `pub fn prune(keep: usize, older_than: Option<u32>, dry_run: bool) -> Result<()>`
- [x] 8.2 先寫測試（用 tempdir）：10 個 run、`keep = 3` → 刪 7 留最新 3；`dry_run` 時一個都不刪但印出將刪清單
- [x] 8.3 實作：依 `recorded_at` 排序；`--keep` 預設 10；無法解析的 run 目錄一律保留不刪（保守）
- [x] 8.4 `actions.rs` 加 `Prune` variant、`dispatch.rs` 接線
- [x] 8.5 `cargo test && cargo clippy --all-targets -- -D warnings`，commit

---

## Task 9 — 把 spec 搬進版控，再修正內容

**查證結果（2026-09-23）：** `~/.mur` 確實是 git repo，但
`git ls-files artifacts` 回傳 **0 個檔案**，而 `.gitignore` 裡沒有任何
`artifacts` 字樣 —— 那些檔案只是從沒被 `git add` 過。

這推翻了原本的前提。`SPEC-phase1.md` **不是** source of truth，它是一份
沒人追蹤的 run artifact，改了也不會進任何人的 diff、任何人的 review。
而 `mur-browser/src/lib.rs:17` 的 doc comment 卻指著它：

```rust
//! Design source: `~/.mur/artifacts/mur/browser-research-20260910/SPEC-phase1.md`.
```

一份 crate 的設計依據指向一個未受版控、且隨時可能被 artifacts 清理掉的路徑。
先搬家，再修內容 —— 順序反了的話等於在沙上寫字。

### Steps

- [x] 9.1 `mkdir -p docs/superpowers/specs` 已存在；
      `git mv` 不適用（跨 repo），改用 `cp` 將 `SPEC-phase1.md`、`MERGED.md`
      複製為 `docs/superpowers/specs/2026-09-10-browser-phase1-spec.md`
      與 `2026-09-10-browser-research-merged.md`
- [x] 9.2 `SPEC-phase1-A-superseded.md` 與 `deep/`、`slice*.md` **不搬**
      —— 那些是研究過程，留在 artifacts 合理
- [x] 9.3 更新 `mur-browser/src/lib.rs:17` 的 `Design source:` 指向新的 repo 內路徑
- [x] 9.4 在新 spec 抬頭下方加 changelog 區塊：原抬頭「待核准，未寫任何程式碼」
      改為實際狀態，逐列標註 §1.1 四項交付物與對應 commit
      （前三項 `2955eca2`，第四項 skill 層 → 本 plan Task 1/6/7）
- [x] 9.5 修正新 merged 文件的「兩個入口、一個引擎」（原 `MERGED.md:67`）為
      「兩個入口、一份共用 session」—— §4 表格裡 test 用 Playwright、
      automation 用 browser-rs，本來就是兩個引擎，原句會讓人誤以為 locator 跨引擎相容
- [x] 9.6 在 §4 表格下方補一行，明文說明 locator 格式是否跨引擎通用；
      若不通用，禁止兩邊共用錄製產物
- [x] 9.7 在舊 artifacts 的 `SPEC-phase1.md` 抬頭插一行
      「**已移入版控：`docs/superpowers/specs/2026-09-10-browser-phase1-spec.md`，
      此檔不再維護**」，避免下次又有人讀到舊的
- [x] 9.8 `cargo test -p mur-browser`（doc comment 改動需確認 doctest 未壞），commit

---

## 自我審查

**Spec 覆蓋：** §1.1 四項交付物 → 前三項已由 `2955eca2` 完成（Task 9 負責如實記錄），第四項 skill 層 → Task 1/6/7。§4 的三指令分工 → Task 1/6/7 各一。

**先前 review 的八點意見落點：** broker（已存在，撤回）、引擎措辭矛盾 → 9.2、自癒無煞車 → Task 5（5.5 驗證回滾＋5.6 預算）、parallel 與 storageState → 7.3、留存清理 → Task 8、list/show（已存在，撤回）、爆炸半徑 → Task 2 + 7.5、文件說謊 → Task 9。

**跨 Task 型別一致性：** `ReplayReport` 在 Task 4 定義、Task 5 追加 `heals` 欄位，兩處名稱一致；`guard::check` 在 Task 2 定義、Task 4.2 消費；`to_spec_ts` 在 Task 3 定義、Task 6.2 經 CLI 消費；`Action::tool_name` / `call_for` 在 Task 0 定義、Task 4.5 消費。

**跨 Task 語意一致性（不只名稱）：** `Run.mode`（`recorder.rs:84`，值域 `Test` / `Automation`）是貫穿全案的分岔點，三處行為必須一致 ——
- Task 3 的 `to_spec_ts` 只對 `Mode::Test` 有意義；遇到 `Mode::Automation` 應回 `Err` 而非產出無斷言的空殼 spec
- Task 5.6 的 heal 預算只在 `Mode::Test` 生效（automation 無斷言，自癒率高不代表失效）
- Task 7.3 的 automation 平行路徑不得接受 `Mode::Test` 的 Run（斷言在平行環境下會互相干擾）

施工者請在各自 Task 補一個 mode 不符的測試，不要只靠型別檢查。

**相依順序：** **Task 0 最先，擋住 4/5/6/7**；Task 1 獨立可先行；0 → 2 → 4 → 5；3 獨立；6 需 3+5；7 需 2+4；8 獨立；9 獨立。**可平行派工：Task 0、1、3、8、9**（Task 0 只動 `recorder.rs` 的 impl 區塊與新測試檔，Task 3 只動 `export.rs`，互不重疊）。

**已知風險：** Task 4 的實跑路徑需要 `@playwright/mcp` 的 tool call schema。Task 0 以 `recorder.rs:241-268` 的既有正向映射為權威來源建立 round-trip 測試，涵蓋名稱與參數鍵名；但**回應**格式（tool call 回來長什麼樣、失敗如何表達）仍只在 `proxy.rs` 測試 fixture 中間接可見。Task 4.5 若發現回應不足以判斷單步成敗，停下來回報，不要猜著寫。

**報告的所有權（2026-09-23 新增）：** `reports/taskN.md` **是驗收紀錄，不是 agent 交付物**。rustsmith 在 Task 0 與 Task 3 皆交付程式碼但未產出報告，模式穩定；決議不改派工閘門，改由驗收者親跑 `cargo test` / `cargo clippy` 後補寫報告。理由：讓 agent 撰寫它自己未執行的閘門輸出，會製造假的 verified 訊號。Task 的完工判準因此綁交付物與閘門實跑結果，**不綁報告檔是否存在**。
