# deep-research setup：安裝 render browser（agent-browser + Lightpanda）與 doctor

- 日期：2026-09-23
- 狀態：設計已同意，實作進行中（`feat/deep-research-browser-install` worktree，尚未 commit）
- 範圍：只有 `mur deep-research` 的 render fetch。
  - browser automation（`mur browser record/replay`、browser-rs 接線）是另一條路線，不在這份 spec 裡。
  - obscura 維持選配（opt-in），不在這份 spec 裡安裝，也不改預設。

## 0. 脈絡：為什麼是 agent-browser，不是 obscura

照 `docs/superpowers/` 裡的計畫檔時間順序：

| 日期 | 檔案 | 對 render engine 的決定 |
|---|---|---|
| 2026-07-09 | `specs/2026-07-09-mur-native-deep-research-design.md` | 研究用 render tier = `agent-browser`：tier 2 `--engine lightpanda`，tier 3 `--engine chrome`（anti-bot / screenshot） |
| 2026-07-10 | `plans/2026-07-10-spike-obscura-render-tier.md` | spike：obscura 能否取代 Lightpanda tier（Q1、Q2 通過） |
| 2026-07-10 | `plans/2026-07-10-obscura-render-tier-implementation.md` | obscura 加成 config 可選的 engine；「Default stays `AgentBrowser`; obscura is opt-in via config/env until Q3-full validates it, then a gated task flips the default.」 |
| 2026-07-13 | `plans/2026-07-13-deep-research-ux-simplification.md` | 加入 `setup` wizard |
| 2026-09-09 | `specs/2026-09-10-deep-research-slash-design.md` | `--render-engine agent-browser\|obscura`，兩者並存 |
| 2026-09-10 | `specs/2026-09-10-browser-research-merged.md` | agent-browser / Lightpanda = 已裝、已 allowlist 的「輕量層」；Playwright = 完整層 |
| 2026-09-10 | `specs/2026-09-10-browser-phase1-spec.md` | browser-rs / Obscura 接線延後 |

Q3-full 沒有翻轉預設。現在的程式碼仍然是 agent-browser 預設：

`mur-research-gateway/src/browser.rs:20-22`

```rust
pub enum RenderEngine {
    /// `agent-browser` (Lightpanda tier-2 / Chrome tier-3) — current default.
    #[default]
```

所以研究用的 render 路線是 **agent-browser，預設掛 Lightpanda 當 engine；Lightpanda 被擋時才退到 Chrome**。
這份 spec 的前一版（改用 obscura + 寫 `render_engine: obscura`）會悄悄翻掉這個預設，已撤回。

## 1. 問題

`mur deep-research setup` 的 Q5 只問「要不要允許 worker 執行 render browser」，不會安裝它。

`mur-core/src/cmd/deep_research/provision.rs:291-295`：

```rust
if bins.is_empty() {
    // Nothing installed is not a provisioning failure: the worker simply
    // has no rendered fetch, and `browser.rs` says so when a page needs it.
    eprintln!("  note: no render browser found, so {worker} gets no rendered fetch.");
    return Ok(());
```

使用者回答 yes、看到 `Setup complete`，但 worker 其實沒有 render 能力，要等遇到只有 JS 的頁面才發現。

## 2. 決定

| # | 決定 | 理由 |
|---|---|---|
| D1 | 擴充現有 `setup` wizard，不新增 install 命令 | 同意和安裝放在同一個地方 |
| D2 | 安裝 **agent-browser**：`npm i -g agent-browser@latest`，再 `agent-browser install`（拉 Chrome for Testing 當 fallback） | 跟 §0 的研究路線一致；`agent-browser install` 不跑的話 npm 套件自己不能 render |
| D2b | 安裝 **Lightpanda**：走 deps installer，用 `registry_manifest.yaml` 的 `lightpanda` curated recipe（0.3.4，4 個平台都有 sha256），放到 `~/.mur/aura/lightpanda` | 「Lightpanda 優先、Chrome 備用」要成立，就得有 Lightpanda；gateway 只要這個檔案存在就優先用它（`provision.rs:324-326`）；有 sha256、不寫全域 |
| D3 | 先把要做的事**完整印出來**（npm 兩條命令 + Lightpanda 的 URL、sha256 前 12 碼、目的地），字面 `yes` 才執行；其他輸入 = 跳過。**只問一次**，涵蓋兩者 | 跟 egress、browser grant 同一套同意規則；npm -g 會寫全域，所以一定要先問 |
| D4 | **不寫** `research_gateway.render_engine` | 預設已經是 `AgentBrowser`；寫 config 反而會蓋掉之後的預設變更 |
| D5 | 安裝被拒或失敗**不讓 setup 失敗** | 沒有 render 時 plain fetch 仍然能用 |
| D6 | 新增唯讀的 `mur deep-research doctor` | 只回報、smoke test、印安裝命令；永遠不安裝 |

## 3. setup 流程（Q5 = `yes` 之後）

在 `setup.rs` 的 `cmd_setup` 裡、`grant_render_browser` 迴圈**之前**呼叫 `browser::ensure_render_browser`：

1. **檢查**：`provision::render_binaries`（`~/.mur/aura/lightpanda` 與 PATH 上的 `agent-browser`），兩個**分開**判斷缺不缺。
2. **缺了就提議安裝**：只列出缺的那幾項（D2 / D2b），問一次 `Type 'yes' to install these now (anything else = skip)`。
   - 非 `yes` → `skipped — plain fetch still works; run \`mur deep-research doctor\` later.`
   - 兩條路線**互相獨立**：Lightpanda 下載失敗、sha256 不符、或平台不在 recipe 裡 → 印原因、跳過 Lightpanda，agent-browser 照裝（之後直接走 Chrome）；npm 失敗 → 照樣裝 Lightpanda。
   - agent-browser 的兩條 npm 命令之間：第一步失敗就不跑第二步。
   - 裝完仍找不到 `agent-browser` → 提示檢查 npm prefix。
   - 只裝得到 Lightpanda、沒有 agent-browser → 提示：預設 engine 是 `AgentBrowser`，光有 Lightpanda 不會被用到，除非手動選 `--render-engine lightpanda`。
3. **smoke test**：對找到的每個 binary 跑版本檢查（`agent-browser --version`；`lightpanda` 吃 subcommand 不吃 flag，用 `smoke_argv` 區分）。
4. **grant**：照現有流程 `grant_render_browser`，涵蓋實際存在的 binary。

Q5 不是 `yes` → 跟現在完全一樣。

## 4. `mur deep-research doctor`（唯讀）

加在 `DeepResearchAction`（`mur-core/src/cli/actions.rs`）。

- 找不到任何 render browser → 印安裝命令與「或在 setup 回答 yes」，exit 非 0。
- 有 agent-browser 但沒有 `~/.mur/aura/lightpanda` → **warning**（exit 0）：render 會直接走 Chrome，印 Lightpanda 的安裝方式。
- 找到但 smoke test 全失敗 → exit 非 0。
- 不下載、不寫檔、不改 config。

## 5. 不做的事

- 不安裝 obscura、不寫 `render_engine`（obscura 仍可用 `--render-engine obscura` / config 手動選）。
- 不處理 browser automation（Playwright Chromium、browser-rs）。
- 不回滾：安裝成功但 smoke test 失敗時保留現狀。

## 6. 測試（對應 worktree 裡 `browser.rs` 的單元測試）

| 測試 | 驗證什麼 |
|---|---|
| 缺瀏覽器、使用者拒絕 | 印出兩條命令，runner 一次都沒被呼叫 |
| 字面 `yes` | 依序執行兩條命令 |
| 第一步失敗 | 停止、不是 error |
| 已有 lightpanda | 不安裝，只 smoke test，而且用 subcommand 不用 flag |
| doctor 在缺瀏覽器時 | 回 error、印安裝命令、不執行任何安裝 |
| `smoke_argv` | agent-browser 用 `--version` |
| 兩個都缺、`yes` | Lightpanda 走注入的 downloader、驗 sha256 後放到 `aura/lightpanda`；npm runner 依序兩條 |
| Lightpanda sha256 不符 | 不留下檔案、agent-browser 照裝、setup 不失敗 |
| npm 第一步失敗 | Lightpanda 照裝 |
| 平台沒有 recipe | 印「此平台沒有 Lightpanda」，只裝 agent-browser |
| doctor：有 agent-browser、沒 Lightpanda | warning、exit 0 |

npm runner 和 Lightpanda downloader 都可注入，測試不碰 npm 也不連網路（`installer::verify_and_place` 本身可以直接吃 bytes）。

## 7. 待決定

1. ~~Lightpanda 要不要一起裝~~ → 已決定：要，見 D2b。**worktree 裡的 `browser.rs` 目前還沒有這一段，實作要補。**
2. **smoke test 的深度**：目前只跑版本檢查，沒有真的 render 一個 JS 頁面。
3. **sandbox**：在 agent sandbox 裡直接執行 `agent-browser --version` 會得到 `Operation not permitted`，smoke test 要由 setup 本身（非 sandbox）或已授權的 worker 跑。
