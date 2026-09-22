# MUR 瀏覽器自動化／E2E 引擎研究 — 合併報告（2026-09-10）

> 2026-09-23 移入版控。原檔：`~/.mur/artifacts/mur/browser-research-20260910/MERGED.md`；本文引用的 `slice1–3`、`deep/` 研究過程檔**仍留在該 artifacts 目錄**，未搬入 repo。內容修正見 §4「一個 command 還是兩個？」與 §4b 表格下方註記。

來源：dr_worker_1 / dr_worker_2 / dr_worker_3 三份切片（同目錄 slice1–3）。
模型：claude-haiku-4-5，每個 worker 5–10 輪 research-gateway search/fetch。
可信度標記：✔ = 多方來源一致或本機已驗證；△ = 單一 worker 陳述、未交叉驗證；✘ = 疑似幻覺，需人工確認。

## 1. 本機現況（✔ 本機驗證）

| 項目 | 狀態 |
|---|---|
| `agent-browser` (Vercel) | 已裝 `~/.npm-global/bin/agent-browser`，dr_worker profile.yaml:82 已允許 spawn |
| `lightpanda` | 已裝 `~/.mur/aura/lightpanda` (71MB)，profile.yaml:81 已允許 spawn |
| `node` / `npx` | `/usr/local/bin` |
| `playwright` | 未安裝 |
| `~/.mur/browser/` | 已有 `profiles/default`、`sandbox-settings.json`（denyRead ~/.ssh ~/.aws ~/.env ~/.netrc 等） |

## 2. 九個候選 × 七維總表

| 工具 | A 用途 | B 可重放腳本 | C 登入狀態 | D headless/沙盒 | E 平行 | F MCP/CLI | G 授權/維護 |
|---|---|---|---|---|---|---|---|
| **Playwright + @playwright/mcp** | E2E+自動化 ✔ | codegen + trace + storageState ✔ | storageState / user-data-dir / --http-credentials ✔ | headless ✔；需 node+npx+瀏覽器二進位 | context + worker + --port/--isolated ✔ | 官方 MCP（Microsoft）✔ | Apache-2.0，極活躍 ✔ |
| **Chrome DevTools MCP** | 自動化+除錯 | 無 codegen；perf trace 非行為記錄 △ | 走 Puppeteer userDataDir △ | 需已裝 Chrome | 單 Chrome 多 context △ | 官方 MCP（Google）✔ | Apache-2.0，活躍 ✔ |
| **Puppeteer** | 自動化為主 | 無官方 codegen（靠 DevTools Recorder）✔ | userDataDir + 手動 cookie | 沙盒常需 --no-sandbox △ | context | 第三方 MCP 已停更 △ | Apache-2.0 |
| **Selenium / BiDi** | E2E | 無 ✔ | 手動 cookie 序列化 | 需 driver 二進位 | Grid | 無 MCP ✔ | Apache-2.0 |
| **Vercel agent-browser** | agent 快照→決策迴圈 | `--json` action log △ | CLI 無會話持久化 △（需外部管理） | Rust 單 binary + Chrome for Testing ✔（已裝） | 多進程 | CLI-only，刻意不做 MCP ✔ | Apache-2.0，Vercel Labs |
| **Lightpanda** | 輕量 fetch / 爬取 | PandaScript `lightpanda run` △ | storageState / user-data-dir △ | Zig 單 binary、無 Chromium ✔（已裝） | 多 `serve` 實例 | MCP + CLI（fetch/serve/agent/run）△ | MIT，活躍 |
| **browser-use** | 自動化（Python） | 無原生 codegen ✔ | Playwright storageState（有 bug #3070 △） | 依 Playwright | Playwright context | Python SDK，無 MCP △ | MIT |
| **Stagehand** | agent 探索（observe/act/extract） | Browserbase session replay △ | Browserbase session；本地無 | 本地 Chromium 或雲 | 雲端平行 | SDK only，無 MCP △ | MIT，Browserbase |
| **Cypress / WebdriverIO** | E2E（開發者導向） | Cypress Cloud 錄製 △；「Cypress AI Skills / `cypress tap`」✘ 待查 | 各自 storageState 類機制 △ | headless ✔ | 各自 runner | 皆無官方 MCP △ | MIT |

### Rust 原生（不依賴 node）— worker_3，交叉驗證不足

| crate | 協議 | 成熟度 | 備註 |
|---|---|---|---|
| chromiumoxide | CDP（async） | 高 △ | 型別安全、tokio |
| fantoccini | W3C WebDriver | 高 △ | 跨瀏覽器，需 driver |
| headless_chrome | CDP（sync） | 中 △ | 較舊 |
| thirtyfour | W3C WebDriver | 中 △ | Selenium 風格 |
| browser-rs-mcp (maestrojeong) | v0.4.0, 23★ | ✔ | **更正**：先前誤標為幻覺；repo 存在且活躍。Rust MCP 伺服器驅動真 Chrome，68 工具、owner 隔離、secret broker。詳見 `deep/DEEP-obscura-vs-browser-rs.md` |
| Obscura (h4ckf0r0day) | 26.6k★ | ✔ | 自製 Rust 引擎（非 Chromium），30 MB、stealth、`--storage-dir`；Playwright 互動有 open bug (#807)，中文截圖有問題 (#606)。詳見同上 |

## 3. 三個 worker 的一致結論

1. **可重放腳本是硬需求，只有 Playwright 真正做到一級公民**：`playwright codegen` 錄製、trace viewer 回放、storageState 匯出。其他工具最多只有 JSON action log（agent-browser）或 DSL（Lightpanda PandaScript，未驗證）。
2. **登入狀態的業界標準做法一致是 Playwright `storageState`**：一次登入 → 存 cookies+localStorage JSON → 之後注入；agent 只拿 session，不拿密碼。
3. **agent-browser 與 Lightpanda 已在本機、已在 allowlist**，是零成本的「輕量層」；Playwright 是需要新裝的「完整層」。
4. Puppeteer / Selenium / Cypress / WDIO / browser-use / Stagehand 都因「無官方 MCP」或「無 codegen」或「綁雲服務」出局。

## 4. 建議架構（給 `/browser-test` 與 `/browser-automation` 設計用）

```
                ┌──────────────────────────────┐
   /browser-test│  Playwright + @playwright/mcp │  E2E、錄製、trace、斷言
 /browser-automation ──────┬───────────────────┘
                           │ 輕量任務（讀頁、抓資料）
                ┌──────────┴──────────┐
                │ agent-browser / Lightpanda │  已裝、已 allowlist、無 Chromium
                └──────────────────────┘
   secrets：/browser-auth setup → 一次互動登入 → storageState.json
            存 ~/.mur/browser/profiles/<site>/state.json（沙盒 denyRead 之外、agent 可讀）
            密碼來源：macOS Keychain 或 1Password CLI（op read），只在 setup 階段接觸
   平行：Playwright worker × browser context；每個 context 各自載入 storageState
   重放：錄製產出 .spec.ts（或 JSON action log）→ 存成 MUR workflow / skill
```

### 一個 command 還是兩個？
兩個入口、一份共用 session：`/browser-test` 預設開 assertion + trace + 產 `.spec.ts`；`/browser-automation` 預設不斷言、產 action log、可排程。共用 `/browser-auth` 管 session。

> **更正（2026-09-23）**：原句為「兩個入口、一個引擎」，與 §4b 表格矛盾——`/browser-test` 用 Playwright、`/browser-automation` 用 browser-rs，是**兩個引擎**；共用的只有 `/browser-auth` 產出的 session（storageState / 真 profile），不是引擎。locator 格式是否跨引擎相容**仍待確認**（見 §4b 表格下方註記）。

### 4b. 深入研究後的修訂（2026-09-10，來源 `deep/`）

深入讀過 browser-rs（`deep/BROKER-PROTOCOL.md`）、Obscura（`deep/DEEP-obscura-vs-browser-rs.md`）、ego (lite)（`deep/DEEP-egolite.md`）後，分層調整為：

```
/browser-test        → Playwright + 真 Chromium（唯一能跑斷言 + trace 的）
/browser-automation  → browser-rs（多 agent 共用登入 + secret broker）
輕量讀頁 / scrape    → Obscura 或 agent-browser / Lightpanda（已裝、無 Chromium）
/browser-auth        → 共用；密碼走 secret broker，session 走 storageState / 真 profile
```

> **Locator 跨引擎相容性（2026-09-23 補註）**：上表 `/browser-test`（Playwright）與 `/browser-automation`（browser-rs）是**兩個不同引擎**。MUR 錄製器的 locator 文法（`role:` / `testid:` / `text:` / `label:` / `css:`，見 `mur-browser/src/locator.rs`）目前**只在 Playwright MCP 上驗證過**；browser-rs 是否接受同一套 locator **尚未確認**。在確認相容之前，**禁止讓兩邊共用錄製產物**（`runs/<name>/actions.yaml`、`.spec.ts`）——為 `/browser-test` 錄的 run 不得直接餵給 `/browser-automation`，反之亦然。確認方式與結論由 `docs/superpowers/plans/2026-09-23-browser-skill-layer-plan.md` 追蹤。

四個候選都**沒有**錄製→重放；這層由 MUR 自己做（記 MCP tool-call log → 產腳本）。

從 ego (lite) 借三個設計（本體閉源不採用，設計可搬）：

| # | 設計 | 對應需求 | 出處 | MUR 落地方式 |
|---|---|---|---|---|
| 1 | **Space 式 context 隔離**：一個瀏覽器 process、每任務一個原生 BrowserContext；cookie/storage 隔離但預設繼承登入狀態。官方數字：6 併發 0.9 GB / 6 process，對比獨立 Chrome 副本 15 GB / 84 process | 平行化 | `DEEP-egolite.md:19`；browser-rs `?owner=`、Playwright `browser.newContext()` 同思路 | fleet 派工時每個 agent 拿一個 **named context**（語意同 `useOrCreateTaskSpace(name)`：回合結束不掉線，靠名字接回），不是各開一個 Chromium |
| 2 | **`loc=` 穩定 selector**：agent 操作用 a11y snapshot 的 `@N` ref 省 token；落盤用 `loc=css:/role:/href:` 語意 selector | 可重放 | `DEEP-egolite.md:20`；一對一對應 Playwright `getByRole` | MUR 錄製器把每一步從 `@N` 轉成 `loc=` 形式再寫進 `.spec.ts` / action log，否則 `@N` 每次編號都變、腳本無法重放 |
| 3 | **handoff / takeover 交接協定**：`handOffTaskSpace` → 人處理 captcha/2FA/付款 → 人明確說 continue → `takeOverTaskSpace`；同一時間只有一方握控制權，GUI 可隨時搶回 | 登入交付（broker 管不到的那一半） | `DEEP-egolite.md:21` | `/browser-auth` 首次登入、以及 `/browser-automation` 跑到 2FA/captcha 時的暫停點：MURMUR 顯示「等你接手」，在你回覆 continue 前 agent 不動 |

三者合起來各補一個硬需求：平行化（1）、可重放（2）、登入交付（3）；密碼不外流那一半則靠 browser-rs 的 secret broker（佔位符替換 + 輸出遮罩 + fail-closed，見 `deep/BROKER-PROTOCOL.md`）。

#### 4b-1. 頁面小改後腳本要不要重寫？（可重放需求的第二半）

**問題**：Playwright 腳本若錄成 `page.click('#btn-3')` 或 `div.x > span:nth-child(2)`，頁面稍改（換 class、多包一層 div、文案微調）就斷。傳統做法是人手重寫；MUR 是 agent 驅動，可以讓腳本**自癒**。

**設計原則：腳本存的是「意圖」，不是 DOM 路徑。** 三層防線，由便宜到貴：

| 層 | 做法 | 成本 | 擋得住什麼 |
|---|---|---|---|
| L1 語意 selector | 錄製時就只落盤 `loc=role:button[name="登入"]`、`loc=text:`、`loc=label:`、`data-testid`；**禁止** nth-child / 自動 class（對應上表設計 2） | 零（錄製期決定） | 改樣式、換 class、調 DOM 層級、搬位置 |
| L2 候選鏈 fallback | 每步存**多個** locator（主 + 2–3 個備援：role → testid → text → 錄製時的 a11y 路徑）；重放時依序試，命中即繼續，並記錄「用了第幾個」 | 幾乎零（每步多幾 ms） | 文案微調、role 屬性被改、單一 selector 失效 |
| L3 agent 自癒 | 全部候選都失效 → 抓當下 a11y snapshot，把「這一步的意圖（自然語言 + 原 locator + 前後步驟）」交給 agent 重新定位 → 命中後**寫回**新 locator 並標 `healed: true` | 一次 LLM 呼叫 | 元素改名、重排流程、新增中間步驟 |

**落盤格式**（MUR 自己的 action log，`.spec.ts` 由它生成）：

```yaml
- step: 3
  intent: "按下登入按鈕"                 # 自然語言，給 L3 用
  locators:                              # L1/L2 候選鏈，依序試
    - role:button[name="登入"]
    - testid:login-submit
    - text:登入
  action: click
  healed: false
  last_hit: 0                            # 上次命中第幾個候選
```

**重放流程**：
1. 依 `locators` 順序試，命中 → 執行，`last_hit` 更新。
2. 全部失效 → L3：agent 拿 `intent` + 當前 snapshot 找新元素，成功則 append 到 `locators` 頭部、`healed: true`。
3. 自癒後**不靜默**：`/browser-test` 模式下自癒視為 **soft fail**（測試通過但報告標黃，因為 UI 真的變了，可能是 bug 也可能是預期改版）；`/browser-automation` 模式下自癒視為正常，只記 log。
4. `intent` 找不到對應元素（例如按鈕真的被拿掉）→ 才是真失敗，交回 handoff（設計 3）讓人決定。

**與 4b 三個設計的關係**：L1 就是設計 2 的 `loc=`；L3 依賴 `intent` 欄位，所以錄製器在轉 `@N` → `loc=` 時**同時**要把 agent 當下的操作意圖寫進去——這一句自然語言是自癒的種子，錄製時不寫、事後補不回來。

**風險**：L3 可能「癒錯」（找到長得像的另一個按鈕）。緩解：自癒後的下一步必須通過原腳本的 assertion（或頁面 URL / 標題比對）才算數，否則回滾並報真失敗。

## 5. 待人工確認（研究品質備註）

- worker_2、worker_3 這一輪有 40–50% 的工具呼叫因為打錯工具名（`mcp__research_gateway__*` 底線 vs 正確的 `mcp__research-gateway__*` 連字號）而失敗，所以 △ 項目是靠剩下一半的查詢寫出來的。
- 版本號（Playwright 1.63、Puppeteer 25.10、DevTools MCP 1.9）與 star 數未交叉驗證，別直接寫進文件。
- Lightpanda「PandaScript / `lightpanda run` / agent mode 自動存 storageState」需要對照 https://lightpanda.io/docs 確認。

## 6. 設計提案（brainstorm 產出，2026-09-10，待核准）

研究到此為止，以下是把研究結論落到 MUR 現有機制上的設計。**HARD GATE：未核准前不寫任何程式碼。**

### 6.1 本機證據：需求不是假設的

- **slash command 在 MUR 就是 skill 的 `command` trigger**（`~/.mur/skills/mur-run/SKILL.md:13-15`）：
  ```yaml
  triggers:
  - type: command
    pattern: /mur-run
  ```
  所以 `/browser-test`、`/browser-automation` 不需要改 GUI 或 runtime，各裝一個 skill 即可。
- **「可重放」的容器已存在**：`~/.mur/workflows/*.yaml`（schema 2，`steps[].command` + `tool` + `on_failure`），`mur workflow run <name> --yes` 可重跑。
- **4b-1 的脆弱性已在你的機器上發生**：`~/.mur/workflows/agent-browser去pchome-24h找airpods-pro的價格.yaml:59`
  ```
  command: agent-browser fill @e21 "AirPods Pro" 2>&1 && agent-browser press Enter 2>&1
  ```
  `@e21` 是那一次 snapshot 的臨時編號，PChome 改版（甚至只是多一個 banner）就失效——這正是設計 2（`@N`→`loc=`）要解的問題。

### 6.2 三個方案

| | A. 純 skill + workflow（零 Rust） | B. skill + `mur browser` 子指令（Rust 錄製器） | C. 全部塞進 browser-rs fork |
|---|---|---|---|
| 做法 | 兩個 skill 用 prompt 教 agent：操作用 agent-browser/Playwright MCP，結束時把步驟寫成 workflow yaml，selector 由 agent 自己轉成 `loc=`/`getByRole` | 同 A，但錄製/轉換/自癒由 `mur browser record|replay|auth` 做，skill 只負責派工 | 在 browser-rs 上加錄製與 Keychain broker，MUR 只當 client |
| 可重放品質 | 靠 agent 自律，`@N` 漏轉風險高 | 錄製器強制：沒有 `locators[]` 的步驟不落盤 | 好，但綁單一引擎 |
| 登入交付 | storageState 手動 + `op read` | `mur browser auth <site>`：handoff 給人登入 → 存 `~/.mur/browser/profiles/<site>/`；broker 接 Keychain | browser-rs broker 原生 |
| 平行化 | 靠 fleet 派工，每 agent 各開 browser | `mur browser` 管一個 daemon、N 個 named context | browser-rs `?owner=` |
| 工程量 | 1–2 天 | 1–2 週 | 依賴外部單人 repo（23★） |
| 風險 | 腳本品質不穩、無自癒 | 多一個 Rust crate 要養 | 上游變動、Playwright 斷言仍要另接 |

**2026-09-10 決定：直接做 B，第一期就裝 Playwright MCP**，spec 見 `SPEC-phase1.md`（方案 A 版留 `SPEC-phase1-A-superseded.md`）。原建議如下，留作紀錄：

~~**建議走 B，但分兩期**：第一期先做 A（兩個 skill + 一個 `/browser-auth` skill，用 Playwright MCP + storageState 撐起來，驗證流程順不順）；第二期把錄製器與 broker 抽成 `mur browser`。A 的產物（workflow yaml）格式先按 4b-1 的 `locators[]` 定，之後 B 的錄製器直接吃同一格式，不用遷移。~~

### 6.3 第一期範圍（A）

1. **`/browser-auth <site>`**：開有頭瀏覽器 → handoff 給人登入（設計 3）→ 人說 continue → 存 storageState 到 `~/.mur/browser/profiles/<site>/state.json`；密碼永不進 agent context。
2. **`/browser-test <url|spec> [--profile <site>]`**：載入 storageState → 依指示操作 → 每步強制寫 `intent` + `locators[]`（設計 2）→ 產 `.spec.ts` + trace → 註冊為 workflow（`mur workflow new`）。
3. **`/browser-automation <task> [--profile <site>] [--parallel N]`**：同上但不斷言、產 action log；`--parallel` 走 fleet 派工，每 agent 一個 named context（設計 1）。
4. **重放**：`mur workflow run <name>` → 走 4b-1 的三層防線；L3 自癒後標 `healed: true` 並報黃。

### 6.4 待你決定

- 一個入口還是兩個？（研究結論：兩個入口、一份共用 session（引擎分別為 Playwright / browser-rs，見 §4 更正）；差別只在預設值——斷言/trace vs 不斷言/可排程）
- `/browser-auth` 要獨立 command，還是併成 `/browser-test --login` 的子流程？
- 第一期引擎：Playwright MCP（要先 `npm i`，未安裝）還是先用已裝的 agent-browser？
