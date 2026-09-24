# 第一期 Spec：`mur browser` + `/browser-auth` `/browser-test` `/browser-automation`（方案 B）

日期：2026-09-10 · 狀態：**部分落地（引擎層已 merge，skill 層延後）** — 詳見下方 Changelog
上游：`2026-09-10-browser-research-merged.md`（原 `MERGED.md`）§4b / §4b-1 / §6.2 方案 B；引擎第一期就用 Playwright MCP。
取代：`SPEC-phase1-A-superseded.md`（方案 A，留在 `~/.mur/artifacts/mur/browser-research-20260910/` 作對照，未搬入版控；§1 上游能力表與 §2 安裝步驟仍然有效，本文不重抄）。

## Changelog

| 日期 | 變更 |
|---|---|
| 2026-09-10 | 初版，狀態「待核准，未寫任何程式碼」。原檔位置 `~/.mur/artifacts/mur/browser-research-20260910/SPEC-phase1.md`（未受版控）。 |
| 2026-09-23 | 移入版控（`docs/superpowers/specs/`），原「待核准，未寫任何程式碼」抬頭**已過期**：引擎層在 commit `2955eca2`（`feat(browser): add Playwright auth and recording (#1244)`，2026-09-11）落地；skill 層延後至 `docs/superpowers/plans/2026-09-23-browser-skill-layer-plan.md` Task 1/6/7。`mur-browser/src/lib.rs` 的 `Design source:` 改指本檔。 |
| 2026-09-23 | 實跑 replay 後補：§3.2 新增 `assert_visible` 需 `role:` locator 的拒收規則；新增 §3.5（`@playwright/mcp@0.0.82` 實測約束）與 §3.6（record / replay 的 tracing）；§6 步驟 2 註明 testid 後備。§3.5 補 `target` 鍵與陣列值兩項約束，§3.6 補 record 丟棄 step 的 warn。對應 commit `f5ba890c` `321fa03c` `a272aa5c` `71c638ed` `5efa0ffb` `313a1c1f` `9b5fbba0`。 |
| 2026-09-24 | §6 步驟 3–4 的 L3（agent）自癒改由 `2026-09-24-browser-replay-heal-design.md` 取代：Rust 離線比對、下一個元素步驟驗證後才寫回、預算以元素步驟為分母。L3 列為非目標。 |

### §1.1 四項交付物實際狀態（2026-09-23 查證）

| # | §1.1 交付物 | 狀態 | 依據 |
|---|---|---|---|
| 1 | `mur-browser/` crate（`proxy.rs` `recorder.rs` `locator.rs` `auth.rs` `broker.rs` `state.rs` `paths.rs`） | **已落地** | `2955eca2`。注意：§1.1 列的 `replay.rs` **未**落地；`paths.rs` 是 spec 未列但實際新增的模組。 |
| 2 | `mur-core/src/cli/mod.rs` 新增 `Browser { action: BrowserAction }` | **已落地** | `2955eca2`（`cli/mod.rs` + `cli/actions.rs`）。`Replay` / `Broker` / `List` / `Show` / `Export` 變體存在但 doc comment 仍標「implemented in a later slice」。 |
| 3 | `mur-core/src/cmd/browser/` 指令實作 + 重用 `bridge_keychain.rs` 的 `Keychain` | **已落地** | `2955eca2`（實作集中於 `cmd/browser/mod.rs` 單檔，非 spec 所列的 `record.rs`/`replay.rs`/… 分檔；keychain 經 `mur_browser::broker::KeychainStore` 接入）。 |
| 4 | `~/.mur/skills/browser-{auth,test,automation}/SKILL.md` 三個 skill | **延後** | 未在 `2955eca2`；由 `2026-09-23-browser-skill-layer-plan.md` Task 1（auth）/ Task 6（test）/ Task 7（automation）承接。 |

## 0. 與方案 A 的差別，一句話

方案 A 靠 skill prompt 求 agent「每步記得寫 `intent` + `locators[]`」；方案 B 把這件事變成 **Rust 錄製器的 schema 驗證**——沒過驗證的步驟根本寫不進 `actions.yaml`。多出來的東西：

| 元件 | A | B（本文） |
|---|---|---|
| 錄製／落盤 | agent 自己寫 yaml | `mur browser record`：站在 agent 與 Playwright MCP 之間的 **MCP proxy**，攔每個 tool call 自動落盤 |
| 重放／自癒 | `npx playwright test` + `locators.ts` + agent 人工補救 | `mur browser replay`：Rust 執行 L1/L2，L3 才叫 agent；`healed` 由錄製器回寫 |
| 登入交付 | storageState 手動 | `mur browser auth`：handoff 狀態機 + `state.json` 加密存放；密碼由 **secret broker** 接 Keychain |
| 平行 | N 個 `--isolated` process | 第一期同 A（N process）；daemon + named context 留第二期，但 CLI 介面先定好 |
| 工程量 | 1–2 天 | **1–2 週**：一個新 crate `mur-browser`，`mur-core` 加一個 `Browser` subcommand |

三個決定沿用 A：兩個入口、`/browser-auth` 獨立、第一期引擎 = `@playwright/mcp`。

> **更正（2026-09-23）**：原句為「兩個入口一個引擎」，與 `2026-09-10-browser-research-merged.md` §4b 表格矛盾——`/browser-test` 用 Playwright、`/browser-automation` 用 browser-rs，是**兩個引擎**；共用的只有 `/browser-auth` 產出的 session（storageState / 真 profile），不是引擎。第一期只落地 Playwright 這一條路。locator 格式是否跨引擎相容**仍待確認**。

## 1. 架構

```
MURMUR /browser-test …
   │ skill 派工（只做參數解析與回報，不碰瀏覽器）
   ▼
browser-worker agent ──MCP stdio──▶ mur browser record --run <name> ──MCP stdio──▶ npx @playwright/mcp
                                      │  (proxy：轉發一切 tool call)
                                      ├─ 攔 browser_click/fill/select/press/navigate/verify_*
                                      │    → 補 browser_generate_locator → 驗 schema → 寫 actions.yaml
                                      ├─ 攔 browser_snapshot 回應 → 快取 @ref → 元素描述（給 intent 用）
                                      └─ secret broker：transform_input / redact_output（§5）
```

agent 眼中只有一個 MCP server 叫 `browser`，工具名與 Playwright MCP 完全相同，多三個 MUR 專屬工具（§3.3）。錄製器對 agent 透明——這是 B 能保證品質的關鍵：**agent 不寫檔，proxy 寫**。

### 1.1 程式碼落點（使用者專案 `/Volumes/Firecuda4tb/Projects/mur`）

| 路徑 | 內容 |
|---|---|
| `mur-browser/`（新 crate，加進 `Cargo.toml:4-16` members） | `proxy.rs` MCP stdio 中繼、`recorder.rs` schema + yaml 落盤、`locator.rs` L1/L2 解析與 pick、`replay.rs`、`auth.rs` handoff 狀態機、`broker.rs` secret broker server、`state.rs` state.json 加密 |
| `mur-core/src/cli/mod.rs` | 新增 `Browser { action: BrowserAction }`，樣式同 `:458 DeepResearch` |
| `mur-core/src/cmd/browser/` | `record.rs` `replay.rs` `auth.rs` `broker.rs` `status.rs`，樣式同 `cmd/deep_research/` |
| `mur-core/src/bridge_keychain.rs` | **重用** `Keychain` trait（`:19-25`，service 固定 `mur-agent`），broker 讀 `browser/<site>/<KEY>` |
| `~/.mur/skills/browser-{auth,test,automation}/SKILL.md` | 三個 skill，`mur skill install` 註冊 |

依賴：`rmcp`（已在 workspace，MCP server/client 都用它）、`keyring`（已有）、`age` 或 `chacha20poly1305`（state.json 加密，擇一，看 workspace 現有）、`tokio::process`。**不需要** Playwright 的 Rust binding——一切透過 MCP。

## 2. CLI 介面（第一期全部要有，含第二期才實作的旗標先報 `not yet`）

```
mur browser auth   <site> --url <login-url> [--reauth]
mur browser record --run <name> [--profile <site>] [--mode test|automation] [--trace]
                   （由 agent 的 MCP 設定啟動，不是人手打）
mur browser replay <name> [--heal] [--headless] [--parallel N] [--json]
mur browser broker --socket <path> --site <site>      （由 record 自動 spawn）
mur browser list | show <name> | export <name> --to spec-ts|workflow
mur browser status                                    （daemon/context 狀態；第一期只列 profiles 與 runs）
```

`export --to workflow` 產 `~/.mur/workflows/browser-<name>.yaml`，單一 step 是 `mur browser replay <name> --json`；這樣 `mur workflow run` / `remind` 排程 / `parallel_jobs` 都不用知道瀏覽器存在。

## 3. 錄製器 `mur browser record`

### 3.1 攔截規則

| Playwright MCP 工具 | 錄製動作 |
|---|---|
| `browser_navigate` | 落盤 `goto`，不需 locator |
| `browser_click` / `browser_type` / `browser_select_option` / `browser_press_key` / `browser_hover` / `browser_drag` | **轉發前**：對 `ref` 呼叫 `browser_generate_locator`；**轉發後**：組 step，驗 schema，落盤 |
| `browser_verify_element_visible` / `_text_visible` / `_value` / `_list_visible` | 落盤 `assert_*`（automation 模式下仍允許，但 replay 只記 log 不 fail）。`_element_visible` 必須錄得 `role:` locator，見 §3.2 拒收規則 |
| `browser_snapshot` | 不落盤；解析回應，快取 `@ref → {role, name, text}`，供 locator 候選鏈與 `intent` 預設值 |
| `browser_storage_state` / `browser_set_storage_state` | **拒絕**（`error: use mur browser auth`）——防 agent 把 session 寫到別處 |
| `browser_start_recording` / `_stop_recording` | **拒絕**——Playwright 內建錄製會把 fill 的值（含密碼）寫進程式碼 |
| 其他 | 直接轉發 |

### 3.2 Step schema（`recorder.rs` 用 `schemars` 產，`serde` 驗）

```yaml
- step: 2
  intent: "在搜尋框輸入 AirPods Pro"      # 必填，≥4 字，由 mur_intent 工具或 snapshot 描述自動填
  action: fill                             # goto|click|fill|select|press|hover|assert_visible|assert_text|assert_value
  value: "AirPods Pro"                     # 若經 broker 替換，這裡存佔位符 {{secret:pchome/PASSWORD}}，永不存明文
  locators:                                # 必填，≥1，順序 = 優先序
  - role:searchbox[name="搜尋"]            # 來自 browser_generate_locator
  - testid:search-input                    # 來自 snapshot 快取
  - text:搜尋
  healed: false
  last_hit: 0
  ref_at_record: "@e21"                    # 只做除錯，replay 永不使用
```

**拒收規則（寫進 `recorder.rs` 的驗證，附單元測試）**：
- `locators` 為空 → 拒收，proxy 回 agent `error: step has no stable locator; call browser_snapshot then retry`
- `assert_visible` 沒有任何 `role:` locator → 拒收（`Reject::NoRoleLocator`），提示 agent 以 snapshot 的 role 與 accessibleName 呼叫 `browser_verify_element_visible`。理由見 §3.5
- 任一 locator 含 `nth-child`、`nth-of-type`、`>` 鏈超過 3 層、或 class 名符合 `/^(css-|sc-|_|[a-z]{1,2}\d{3,})/` → 剔除；剔除後為空 → 拒收
- `intent` 缺或 < 4 字 → 用 snapshot 快取的 `{role} "{name}"` 自動填，並標 `intent_auto: true`（report 會列，但不擋）
- `value` 命中 broker 佔位符以外的高熵字串（長度 ≥ 12 且含大小寫數字）且 action 為 `fill` 在 `type=password` 欄位 → **拒收**，提示走 `{{secret:…}}`

### 3.3 三個 MUR 專屬 MCP 工具（proxy 自己實作，不轉發）

| 工具 | 用途 |
|---|---|
| `mur_intent {text}` | 為**下一步**預先宣告意圖；比 auto 描述準，skill 會教 agent 在每次操作前呼叫 |
| `mur_secret {name}` | 回傳佔位符字串 `{{secret:<site>/<name>}}`，agent 拿去當 `browser_type` 的 `text`；真值只在 broker 替換 |
| `mur_handoff {reason}` | 進入 handoff 狀態（§4），回傳後 proxy 拒絕所有 `browser_*` 直到 `mur_takeover` |

### 3.4 落盤位置

```
~/.mur/browser/
  profiles/<site>/state.json.age     # 加密 storageState（§4）
  profiles/<site>/meta.yaml          # url、last_auth、cookie 到期最早時間
  runs/<name>/actions.yaml           # 真相來源
  runs/<name>/trace.zip              # --trace 或 mode=test
  runs/<name>/report.md              # replay 後產
  runs/<name>/hits.jsonl             # replay 每步命中第幾個 locator
  broker.sock                        # 0600，record 啟動時建立、結束刪除
```

### 3.5 與 `@playwright/mcp@0.0.82` 的實測約束（2026-09-23 補）

版本釘在 `mur-browser/src/lib.rs:33` 的 `PLAYWRIGHT_MCP_PKG`。以下各點都是實跑 replay 後才發現的，對應 commit 已標註：

| 約束 | 處理 | commit |
|---|---|---|
| `browser_verify_*` 預設不存在，需 `--caps=testing` | replay 啟動 MCP server 時一律帶上 | `f5ba890c` |
| 三個 verify 工具的參數 schema 各不相同：`_element_visible` 吃 `{ role, accessibleName }`、`_text_visible` 吃 `{ text }`、`_value` 吃 `{ type, element, target, value }` | replay 依工具分別組參數；`assert_value` 的鍵名定為 `value` | `321fa03c` |
| `_element_visible` 不接受 `ref`，只能用 role + name 定位 | 錄製時強制要有 `role:` locator，否則拒收 | `a272aa5c` |
| snapshot 永遠不含 `data-testid` | snapshot 沒命中任何 locator 時，改送 `[data-testid="…"]` 讓 server 自行解析；snapshot 命中仍優先 | `71c638ed` |
| click / type / select 的元素 ref 放在 `target` 鍵，不是 `ref` | recorder 的 `locator_for` / `ref_at_record` 在沒有 `ref` 時改讀 `target` | `9b5fbba0` |
| `browser_select_option` 的值是陣列（`values: ["M"]`） | `value_for` 接受陣列，取第一個元素 | `9b5fbba0` |

### 3.6 可觀測性

`record` 與 `replay` 都輸出 tracing（`RUST_LOG=mur_browser=debug,mur=info`，stderr）：

| 指令 | 層級 | 事件 |
|---|---|---|
| record | info | `browser record started` / `browser record finished`（含 `ok`） |
| record | debug | 每個錄下的 step：`recorded step` |
| record | warn | 下游工具回傳錯誤、step 未落盤：`action failed downstream; step not recorded`（含 `tool`）。之前是無聲丟棄（`9b5fbba0`） |
| replay | info | `browser replay started`（run、mode、profile、steps、dry_run）/ `browser replay finished`（total、passed、failed、healed） |
| replay | debug | 每個 step：`replayed step`（status、實際使用的 locator、message） |

導向檔案時輸出含 ANSI 色碼，需要 grep 時加 `NO_COLOR=1`。

## 4. `mur browser auth <site>`（設計 3：handoff / takeover）

狀態機（`auth.rs`，附測試）：

```
Idle ──start──▶ AgentDriving（有頭、持久 profile、navigate 到 --url）
AgentDriving ──mur_handoff──▶ HumanDriving（proxy 拒絕所有 browser_*；MURMUR 顯示「瀏覽器交給你，登入完回 continue」）
HumanDriving ──使用者 continue──▶ Verifying（agent 只可 browser_snapshot；找登出鈕/使用者名）
Verifying ──ok──▶ Saving（proxy 自己呼叫 browser_storage_state → 讀回 → age 加密 → 刪明文）
Verifying ──fail──▶ HumanDriving
Saving ──▶ Done（寫 meta.yaml；回報「session 已存，最早 cookie 到期 <date>」）
```

- 加密金鑰：`Keychain::get("browser/state-key")`，首次 `auth` 時產生並 `put`。其他 agent 就算 `denyRead` 漏了也拿不到明文。
- `replay` / `record --profile` 啟動時解密到 `$TMPDIR/mur-browser-<pid>/state.json`（0600），以 `--storage-state` 傳給 Playwright MCP，process 結束即刪。
- **auth 期間 `browser_type` 一律拒絕**，就算 agent 想「幫忙」打帳密也不行——密碼只有兩條路：人親手打（handoff）或 broker 佔位符（§5）。
- `--reauth`：cookie 過期時重跑；`replay` 偵測到登入頁（`meta.yaml` 存的 `login_url` pattern 命中）自動提示。

## 5. Secret broker（`broker.rs`）——協定照抄 browser-rs，理由：已驗證、未來可直接接 browser-rs

來源：`deep/BROKER-PROTOCOL.md`（讀自 browser-rs `crates/ab-mcp/src/secret_broker.rs`）。

| 項目 | 值 |
|---|---|
| Transport | Unix socket `~/.mur/browser/broker.sock`，一連線一請求，JSON + `\n` |
| Auth | 每請求帶 `token`（32 hex，record 啟動時產生，經 env 傳給 proxy 後 `remove_var`） |
| Timeout | 3 s；逾時或 broker 死 → **fail-closed**，tool call 直接回錯不轉發 |
| `transform_input` | 掃 `value` 內所有字串，`{{secret:<site>/<KEY>}}` → `Keychain::get("browser/<site>/<KEY>")`；回 `lease`（32 hex）+ `boundary`（本次替換的真值 hash 清單） |
| `redact_output` | 對回應內所有字串做真值 → `[redacted:<KEY>]` 替換（含 snapshot、error message、截圖檔名旁的文字）；成功與錯誤回應都過 |

密碼放入 Keychain：**重用現有** `mur agent secret set browser-worker browser/pchome/PASSWORD`（`cli/agent.rs:383 Secret`），service 同為 `mur-agent`，不用新指令。

第一期 broker 只做 Keychain；1Password `op read` 留第二期，介面不變。

## 6. 重放 `mur browser replay <name>`（§4b-1 三層防線，Rust 執行）

1. 解密 state → 啟 Playwright MCP（`--isolated --headless --storage-state=…`）→ proxy 自己當 MCP client。
2. 逐 step：`goto` 直接送；其他先 `browser_snapshot`，用 `locator.rs` 把 `locators[]` 依序對 snapshot 解析成 `@ref`（**L1/L2 在 Rust 內完成，零 LLM**），第一個命中者送對應 tool；`hits.jsonl` 記 `{step, hit}`。
   - 例外（2026-09-23，`71c638ed`）：snapshot 不含 `data-testid`，所以全部 miss 時若有 `testid:` 候選，先把第一個以 `[data-testid="…"]` 直接交給 server 解析，server 也找不到才算 miss。見 §3.5。
3. （**已被 `2026-09-24-browser-replay-heal-design.md` 取代**，下文保留作歷史）全部 miss → 若無 `--heal`：fail，report 列出 step + intent。若 `--heal`：把 `intent` + 當前 snapshot 交給 browser-worker agent（透過 `mur agent run browser-worker --prompt …`，走既有 A2A），agent 只回一個 `@ref`；proxy 對它 `browser_generate_locator` → **prepend** 到 `locators[]`、`healed: true`、寫回 `actions.yaml`；繼續。
4. 自癒後**下一步的 assert（或下一個有 locator 的 step）必須命中**，否則回滾該 healed locator 並 fail——防癒錯。
5. 結果：綠 / 黃（有 `healed`，僅 `mode: test`）/ 紅。`--json` 輸出給 workflow 與 MURMUR。
6. `--parallel N`（第一期）：N 個 `replay` process、各自解密一份 state 到各自 tmp；`mur browser` daemon 與 named context 留第二期，旗標先保留。

## 7. 三個 skill（薄殼，各 ≤ 60 行）

| Skill | 做的事 | 不做的事 |
|---|---|---|
| `/browser-auth <site> [--url]` | 起 `mur browser auth`；把 handoff 訊息轉給使用者；收到 `continue` 轉 takeover | 不呼叫任何 `browser_*` |
| `/browser-test <url\|spec> [--profile] [--name] [--replay]` | 錄製：教 agent「每步先 `mur_intent` 再操作，密碼用 `mur_secret`」，結束 `export --to workflow`；重放：`mur browser replay <name> --heal --json` 後把黃/紅翻成人話 | 不寫 yaml、不碰 state |
| `/browser-automation <task> [--profile] [--parallel N] [--schedule]` | 同上 `--mode automation`；`--schedule` → `remind` 提案 cron 跑 `mur workflow run browser-<name> --yes` | 不斷言 |

Trigger 格式沿 `mur-run/SKILL.md:13-15`。agent 的 MCP 設定：

```bash
mur agent mcp add browser-worker browser \
  --command mur --arg browser --arg record --arg --run --arg '${MUR_RUN}' \
  --arg --profile --arg '${MUR_PROFILE}'
```

`mur agent mcp add` 只同步 `mur` 進 allowlist；Playwright 的 `npx` 與 Chromium 由 **`mur browser record` 自己 spawn**，所以 allowlist 要加 `npx` 與 `$HOME/Library/Caches/ms-playwright`（同 A §2 第 1 點）。

## 8. 工程切片（估 8–10 個工作天，可平行的標 ∥）

| # | 切片 | 產出 | 驗收 |
|---|---|---|---|
| 1 | `mur-browser` crate 骨架 + `proxy.rs` 純轉發 | agent 透過 `mur browser record` 能跑通 `browser_navigate` | `mur agent mcp inspect browser-worker --probe` exit 0；navigate 真開頁 |
| 2 ∥ | `recorder.rs` schema + 拒收規則 + 單元測試 | `actions.yaml` | 表 §3.2 四條拒收規則各一個測試；`@e21` 那種步驟寫不進去（對照 `agent-browser去pchome…yaml:59`） |
| 3 ∥ | `locator.rs`：前綴解析 + 對 snapshot 解析成 `@ref` | L1/L2 | 給定 snapshot fixture，`role:`/`testid:`/`text:`/`label:`/`css:` 各命中一次；壞 locator 被剔除 |
| 4 | `auth.rs` 狀態機 + `state.rs` 加密 | `state.json.age` | `/browser-auth pchome` 後 profile 存在、0600、transcript 無密碼；`browser_type` 在 auth 期間被拒 |
| 5 ∥ | `broker.rs` | broker.sock | 用 `deep/BROKER-PROTOCOL.md` 的兩則訊息當 fixture；broker 死 → tool call fail-closed；redact 蓋掉 snapshot 內真值 |
| 6 | `replay.rs` L1/L2 + `--heal` L3 + 回滾 | report.md / hits.jsonl | 改掉 testid 後 replay 黃、`healed: true`；癒錯（下一 assert 不過）→ 回滾 + 紅 |
| 7 | `export --to workflow|spec-ts` + 三個 skill | workflow yaml、SKILL.md | `mur workflow run browser-<name> --yes` 綠；`--parallel 3` 三 process 不互撞 |
| 8 | `mur-core` 接線、`mur browser status/list/show`、docs | CLI | `mur browser --help`；`CONTRIBUTING.md` 加 crate 說明 |

先做 1 → 2/3/5 平行 → 4 → 6 → 7 → 8。切片 2、3、5 檔案互斥、契約（schema、locator 前綴、broker JSON）在切片 1 結束時凍結，符合 `parallel_jobs` 的 parallel-code gate，可派 backend/rustsmith 分頭做。

## 9. 明確不做（第二期）

- `mur browser` 常駐 daemon、一個 browser N 個 named context（設計 1 的完整版；第一期 N process）
- 1Password / 其他 secret 來源（broker 介面已定，加 backend 即可）
- browser-rs / Obscura 接線（broker 協定相容，屆時只換下游）
- MURMUR 端「等你接手」的 UI 元件（第一期用純文字訊息）
- 錄製 `.spec.ts` 以外的目標語言

## 10. 已知風險

| 風險 | 緩解 |
|---|---|
| Playwright MCP 工具名／參數改版，proxy 攔截表失效 | `mur agent mcp pin`（`cmd/agent_mcp_pin.rs` 已有）鎖版本；攔截表以 tool name 為 key，新工具預設直通 |
| `browser_generate_locator` 回傳的 locator 品質不穩 | 它只是候選鏈第一項；快取的 snapshot 描述保證至少還有 `role:`+`name` 一組 |
| 有頭 auth 在無 GUI session（launchd）下起不來 | auth 只從 MURMUR 互動觸發；`replay` 一律 `--headless` |
| age 金鑰進 Keychain 後使用者換機 | `mur browser auth --reauth` 重做即可；state 本來就會過期 |
