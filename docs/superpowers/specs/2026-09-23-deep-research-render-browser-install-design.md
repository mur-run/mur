# deep-research setup：安裝 render browser（原生 Lightpanda 優先）與 doctor

- 日期：2026-09-23
- 狀態：路線已定（原生 Lightpanda 當主力，照 gateway 現有行為），實作進行中（`feat/deep-research-browser-install` worktree，尚未 commit）
- 範圍：只有 `mur deep-research` 的 render fetch。
  - browser automation（`mur browser record/replay`、browser-rs 接線）是另一條路線，不在這份 spec 裡。
  - obscura 維持選配（opt-in），不在這份 spec 裡安裝，也不改預設。
  - 「原生 Lightpanda 失敗時退回 Chrome」要改 gateway，另開 issue / PR，不在這份 spec 裡（§5）。

## 0. 脈絡：gateway 實際會用哪個 engine

照 `docs/superpowers/` 的計畫檔與 `mur-research-gateway` 的 commit，時間順序：

| 日期 | 來源 | 對 render engine 的決定 |
|---|---|---|
| 2026-07-09 | `specs/2026-07-09-mur-native-deep-research-design.md` | 研究用 render tier = `agent-browser`：tier 2 `--engine lightpanda`，tier 3 `--engine chrome`（anti-bot / screenshot） |
| 2026-07-10 | `plans/2026-07-10-spike-obscura-render-tier.md` | spike：obscura 能否取代 Lightpanda tier（Q1、Q2 通過） |
| 2026-07-10 | `plans/2026-07-10-obscura-render-tier-implementation.md`、`3cf9bfe5` | obscura 加成可選 engine；`RenderEngine` 的 Rust `Default` 維持 `AgentBrowser` |
| 2026-07-10 | `382de179` | config 沒設 `render_engine` 時改走**自動偵測**（obscura 有裝就用，否則 agent-browser） |
| 2026-07-11 | `19cc0a78` | 新增 **原生 Lightpanda** engine：直接跑 `lightpanda fetch`，不經過 agent-browser |
| 2026-07-11 | `1a44c192` | 自動偵測改成 **原生 Lightpanda → obscura → agent-browser** |
| 2026-07-13 | `plans/2026-07-13-deep-research-ux-simplification.md` | 加入 `setup` wizard |
| 2026-09-09 | `specs/2026-09-10-deep-research-slash-design.md` | `--render-engine agent-browser\|obscura`，兩者並存 |
| 2026-09-10 | `specs/2026-09-10-browser-research-merged.md` | agent-browser / Lightpanda = 已裝、已 allowlist 的「輕量層」；Playwright = 完整層 |
| 2026-09-10 | `specs/2026-09-10-browser-phase1-spec.md` | browser-rs / Obscura 接線延後 |

這份 spec 的前一版把 `browser.rs:20-22` 的 `#[default]` 當成「gateway 預設是 agent-browser」。那是錯的：`#[default]` 只是 Rust 的 `Default`，gateway 讀 config 時沒設 `render_engine` 就走自動偵測：

`mur-research-gateway/src/config.rs:329`

```rust
        .unwrap_or_else(|| auto_detect_render_engine(mur_home));
```

`mur-research-gateway/src/config.rs:373-378`

```rust
fn auto_detect_render_engine(mur_home: &Path) -> crate::browser::RenderEngine {
    // 1. Prefer NATIVE lightpanda — fastest, usually already installed at
    //    aura/lightpanda, and egress-governed (head-to-head 2026-07-11).
    if mur_home.join(DEFAULT_LIGHTPANDA_RELATIVE_PATH).exists() {
        return crate::browser::RenderEngine::Lightpanda;
    }
```

只有 `AgentBrowser` 會從 Lightpanda 升級到 Chrome：

`mur-research-gateway/src/server.rs:36-38`

```rust
pub(crate) fn render_can_escalate(cfg: &BrowserCfg) -> bool {
    matches!(cfg.render_engine, browser::RenderEngine::AgentBrowser)
}
```

所以沒有明寫 `render_engine` 時：

| `~/.mur/aura/lightpanda` | 實際跑哪個 | 失敗時退回 Chrome |
|---|---|---|
| 有 | **原生 Lightpanda**（`lightpanda fetch`），不經過 agent-browser | 不會 |
| 沒有（obscura 也沒有） | agent-browser 直接 `--engine chrome` | 本來就是 Chrome |

選原生 Lightpanda 有實測根據，見 `docs/design/deep-research/README.md:95`：

> The `agent-browser` wrapper returns title-only stubs under the sandbox and its lightpanda tier is sandbox-denied — prefer `lightpanda` or `obscura`.

以及 `README.md:97`：2026-07-11 的 head-to-head 裡，原生 Lightpanda 在 8/8 個目標都 render 出內容（包括只有 JS 的頁面），比 obscura 快、抽出的內容也比較多，能在 sandbox 裡跑，egress 也受 proxy 管。

**結論：這份 spec 照現有程式碼走。setup 的主力是原生 Lightpanda；agent-browser + Chrome 只在裝不了 Lightpanda 時才用。**

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
| D2 | 主力：安裝 **原生 Lightpanda**。走 deps installer，用 `registry_manifest.yaml` 的 `lightpanda` curated recipe（0.3.4，4 個平台都有 sha256），放到 `~/.mur/aura/lightpanda` | 檔案存在，gateway 自動偵測就選它（§0）；有 sha256、不寫全域；跟 `mur fleet install-deps deep-research`（`README.md:99`）用同一份 recipe |
| D2b | 備用：**只有在這個平台沒有 Lightpanda recipe 時**，才改提議 agent-browser：`npm i -g agent-browser@latest`，再 `agent-browser install`（拉 Chrome for Testing） | Lightpanda 在的時候 gateway 不會用到 agent-browser（§0），裝了只是多寫全域、多下載一個 Chrome |
| D3 | 先把要做的事**完整印出來**（Lightpanda：URL、sha256 前 12 碼、目的地；或 npm 兩條命令），字面 `yes` 才執行；其他輸入 = 跳過。**只問一次** | 跟 egress、browser grant 同一套同意規則 |
| D4 | **不寫** `research_gateway.render_engine` | 不寫才會走自動偵測：Lightpanda 在就用它，不在就退到 agent-browser。寫死 `lightpanda` 的話，之後刪掉 Lightpanda 會直接壞掉，不會退回去；也會蓋掉使用者之後的選擇 |
| D5 | 安裝被拒或失敗**不讓 setup 失敗** | 沒有 render 時 plain fetch 仍然能用 |
| D6 | 新增唯讀的 `mur deep-research doctor`；`--render` 多跑一次真的 render | 只回報、smoke test、印安裝方式；永遠不安裝 |

## 3. setup 流程（Q5 = `yes` 之後）

在 `setup.rs` 的 `cmd_setup` 裡、`grant_render_browser` 迴圈**之前**呼叫 `browser::ensure_render_browser`：

1. **檢查**：`~/.mur/aura/lightpanda` 在不在。
2. **Lightpanda 已經在** → 不安裝，直接 smoke test。
3. **Lightpanda 不在** → 查 `recipe("lightpanda", current_platform())`：
   - **有 recipe** → 印 URL、sha256 前 12 碼、目的地，問 `Type 'yes' to install this now (anything else = skip)`。
     - 非 `yes` → `skipped — plain fetch still works; run \`mur deep-research doctor\` later.`
     - 下載失敗 / sha256 不符 → 印原因（sha256 不符時不留下檔案）；如果 PATH 上已經有 agent-browser，說明 render 會走 Chrome；否則印 agent-browser 的兩條命令讓使用者自己決定。**不自動改跑 npm**，因為那兩條命令沒有被同意過。
   - **沒有 recipe（平台不支援）** → 如果 PATH 上已經有 agent-browser 就不裝；沒有就印 D2b 的兩條命令，同一套 `yes` 規則。第一步失敗就不跑第二步；裝完仍找不到 `agent-browser` → 提示檢查 npm prefix。
4. **smoke test（L1 + L2）**：由 setup 自己跑（使用者的 terminal，不在 worker sandbox 裡），只測**自動偵測會選的那個 engine**，見 §3.1。結果只回報，**不讓 setup 失敗**（D5）。
5. **grant**：照現有流程 `grant_render_browser`，涵蓋實際存在的 binary。

Q5 不是 `yes` → 跟現在完全一樣。

### 3.1 smoke test 分級

| 等級 | 做什麼 | 網路 | 誰跑 |
|---|---|---|---|
| L1 | 版本檢查：`lightpanda version`（吃 subcommand 不吃 flag）；`agent-browser --version` | 無 | setup、`doctor`（預設） |
| L2 | 用自動偵測會選的 engine，真的 render 一個要跑 JS 才有內容的本機頁面 | 只有 loopback（`127.0.0.1`） | setup、`doctor --render` |
| L3 | render 外部真實網站 | 對外 egress | **不做**（§5） |

**要測哪個 engine**：照 gateway 的自動偵測順序（`config.rs:373`）。

| 狀況 | L1 | L2 |
|---|---|---|
| 有 `aura/lightpanda` | lightpanda | 原生 Lightpanda |
| 沒有 Lightpanda，有 obscura 兩個 binary | 不測，只回報「obscura（opt-in，不在 smoke test 範圍）」 | 不測 |
| 只有 agent-browser | agent-browser | agent-browser + Chrome |
| 有 Lightpanda，也有 agent-browser | 兩個都測 | 只測原生 Lightpanda，另外註明 agent-browser 現在不會被用到 |

有明寫 `MUR_RESEARCH_RENDER_ENGINE` 或 `research_gateway.render_engine` 時，印一行提醒：「明寫的值會蓋掉自動偵測，這次 smoke test 測的是自動偵測的結果」。

**L2 的頁面**：mur 在 `127.0.0.1:0` 起一個只回一頁的 HTTP server（std `TcpListener`、一個 thread、回完就關），內容：

```html
<div id="o"></div><script>document.getElementById('o').textContent='MUR-RENDER-'+(6*7)</script>
```

通過條件：輸出含 `MUR-RENDER-42`。這個字串不在原始碼裡，只有 JS 真的跑過才會出現，所以「抓到 HTML 但沒執行 JS」會判失敗。
不用 `file://` / `data:`：不確定 Lightpanda 支不支援；loopback HTTP 讓兩個 engine 走同一條路。

**L2 的 argv** 對齊 gateway：

| engine | 對齊 | argv |
|---|---|---|
| 原生 Lightpanda | `build_lightpanda_argv`（`mur-research-gateway/src/browser.rs:122`） | `~/.mur/aura/lightpanda fetch <url> --dump markdown --http-timeout 30000` |
| agent-browser + Chrome | `build_fetch_argv`（`browser.rs:63`）的 chrome 分支 | `agent-browser --engine chrome --session mur-smoke-<pid> open <url> snapshot` |

- 原生 Lightpanda **不帶** `--http-proxy`：smoke test 在使用者 terminal 跑，沒有 gateway 的 egress proxy；目標是 loopback。
- 原生 Lightpanda **不帶** `--block-private-networks`：gateway 也不帶（`browser.rs:119-121`），而且它會擋掉 loopback 頁面。
- Chrome 分支不帶 stealth args（loopback 頁面用不到）。
- agent-browser 的 session 跑完 best-effort `close`，失敗忽略（指令名實作時確認；文件寫法見 `plans/2026-07-08-aura-research-agent.md:86`）。
- `mur-core/Cargo.toml` 沒有依賴 `mur-research-gateway`，所以 argv 在 mur-core 的 `browser.rs` 另寫一份，用測試釘住上面的規則（見 §7-2）。
- 每次 L2 限時 30 秒（Chrome 第一次冷啟動比較慢），超時算失敗。
- L1 失敗的 binary 不跑 L2。

**結果怎麼印**：

```
Render browser smoke test:
  engine (auto-detect): lightpanda
  ✓ lightpanda 0.3.4                 L1
  ✓ render via lightpanda  (0.8s)    L2
  · agent-browser 0.x.y found — not used while lightpanda is installed
```

| 結果 | setup 的反應 |
|---|---|
| L1、L2 都過 | ✓，繼續 |
| L2 失敗 | ⚠ 印 stderr 最後 3 行；有 Lightpanda 時要**明講**「gateway 仍然會選 Lightpanda，失敗不會退回 Chrome」，建議之後跑 `mur deep-research doctor --render` |
| 沒有任何 engine | 提醒 plain fetch 仍然能用；setup 照樣完成 |
| spawn 得到 `PermissionDenied` | 印「被 sandbox 擋住，請在一般 terminal 跑」，**不**當成瀏覽器壞掉 |

## 4. `mur deep-research doctor`（唯讀）

加在 `DeepResearchAction`（`mur-core/src/cli/actions.rs`），形狀是 `Doctor { #[arg(long)] render: bool }`。

| 命令 | 跑什麼 | 大約多久 |
|---|---|---|
| `mur deep-research doctor` | 檢查 + L1 | 1 秒內 |
| `mur deep-research doctor --render` | 檢查 + L1 + L2 | 幾秒；Chrome 冷啟動最久 30 秒 |

- 第一行印自動偵測會選的 engine（§3.1 的表）。
- 什麼都沒有 → 印 Lightpanda 的安裝方式（`mur fleet install-deps deep-research`，或在 setup 回答 yes）；平台沒有 recipe 時改印 agent-browser 的兩條命令。exit 非 0。
- 只有 agent-browser、沒有 Lightpanda → **warning**（exit 0）：render 會走 Chrome，印 Lightpanda 的安裝方式。
- 被選中的 engine L1 失敗 → exit 非 0。有 Lightpanda 時，即使 agent-browser 正常也一樣：gateway 只看檔案在不在，壞掉的 Lightpanda 照樣會被選中。
- `--render`：被選中的 engine L2 通過 → exit 0；沒通過 → exit 非 0。
- 不下載、不寫檔、不改 config。L2 只連 loopback，不對外連線。

## 5. 不做的事

- 不安裝 obscura、不寫 `render_engine`（obscura 仍可用 `--render-engine obscura` / config 手動選）。
- 不在 Lightpanda 可以裝的平台上安裝 agent-browser（D2b）。
- 不改 gateway：原生 Lightpanda 失敗時退回 Chrome 是另一個 issue / PR。做了之後，setup 才有理由在 Lightpanda 之外也裝 agent-browser。
- 不處理 browser automation（Playwright Chromium、browser-rs）。
- 不回滾：安裝成功但 smoke test 失敗時保留現狀。
- 不做 L3（render 外部網站）：結果會受網路、對方網站、anti-bot 影響，失敗時分不出是誰的問題；而且 setup 當下 worker 的 egress 還沒授權。

## 6. 測試（對應 worktree 裡 `browser.rs` 的單元測試）

| 測試 | 驗證什麼 |
|---|---|
| 缺 Lightpanda、有 recipe、使用者拒絕 | 印出 URL / sha256 前 12 碼 / 目的地；downloader 一次都沒被呼叫 |
| 缺 Lightpanda、字面 `yes` | downloader 拿到 recipe URL；sha256 對的 bytes 被放到 `aura/lightpanda`，可執行 |
| Lightpanda sha256 不符 | 不留下檔案、不跑 npm、setup 不失敗 |
| Lightpanda 下載失敗、PATH 上沒有 agent-browser | 印 agent-browser 的兩條命令，npm runner 沒被呼叫 |
| 平台沒有 recipe、字面 `yes` | npm runner 依序跑兩條命令；downloader 沒被呼叫 |
| 平台沒有 recipe、npm 第一步失敗 | 停止、不是 error |
| 已有 Lightpanda | 不問、不安裝，只 smoke test；L1 用 subcommand 不用 flag |
| 有 Lightpanda 也有 agent-browser | L2 只跑原生 Lightpanda；印「agent-browser not used」 |
| L2 argv：原生 Lightpanda | 第一個參數是 `fetch`；有 `--dump markdown`；沒有 `--http-proxy`、沒有 `--block-private-networks` |
| L2 argv：Chrome | `--engine chrome`；沒有 `--executable-path`；有 `open` 和 `snapshot` |
| L2 判定 | 輸出含 `MUR-RENDER-42` → 過；只含原始 HTML（`6*7`）→ 不過 |
| L2 失敗且有 Lightpanda | setup 印 ⚠ 並說明不會退回 Chrome、不失敗；`doctor --render` exit 非 0 |
| L1 失敗的 binary | 不跑 L2 |
| spawn 回 `PermissionDenied` | 印 sandbox 提示，不歸類成瀏覽器壞掉 |
| doctor：什麼都沒有 | 回 error、印安裝方式、不執行任何安裝 |
| doctor：只有 agent-browser | warning、exit 0 |
| doctor：Lightpanda L1 失敗、agent-browser 正常 | exit 非 0 |
| `doctor`（沒有 `--render`） | runner 沒收到任何 render 指令 |
| loopback server | 測試直接打 `127.0.0.1` 拿到固定頁面（不需要瀏覽器，CI 可跑） |

npm runner、Lightpanda downloader、render runner 都可注入，測試不碰 npm、不開瀏覽器，也不連外網（`installer::verify_and_place` 本身可以直接吃 bytes）。
平台由參數傳入，測試不依賴跑 CI 的機器是哪個平台。
真的開瀏覽器跑 L2 只在手動驗收做，不放進 CI。

## 7. 待決定

1. **Lightpanda 的 telemetry**：`README.md:97` 寫 Lightpanda 會連 `telemetry.lightpanda.io`，建議用 `--deny-host telemetry.lightpanda.io` 擋掉；但 setup 呼叫 `grant_egress` 時 deny list 是空的（`setup.rs:297`、`setup.rs:304`）。setup 裝了 Lightpanda 以後，它就是預設 engine，要不要順便把這個 host 加進 deny list？這份 spec 先不做，另外決定。
2. **L2 argv 與 gateway 會分家**：`mur-core` 沒有依賴 `mur-research-gateway`，L2 的 argv 是複製的。之後如果 gateway 改了 `build_lightpanda_argv` 或 `build_fetch_argv`，這邊要跟著改；可以考慮把 argv builder 搬到 `mur-common`，這份 spec 不做。自動偵測順序（§3.1）也是同樣的情況。
