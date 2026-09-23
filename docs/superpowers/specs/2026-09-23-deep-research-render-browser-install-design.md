# deep-research setup：安裝 render browser（obscura）與 doctor

- 日期：2026-09-23
- 狀態：設計已同意，尚未實作
- 範圍：只有 `mur deep-research`。`mur browser record/replay`（npx + Playwright Chromium）是另一個子專案，不在這份 spec 裡。

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
另外，就算 obscura 已經在 `~/.mur/aura/`，gateway 也要 `research_gateway.render_engine: obscura` 才會用它；目前只有 `mur deep-research provision --render-engine obscura` 會印 NOTE 請使用者自己去改（`provision.rs:453`）。

## 2. 決定

| # | 決定 | 理由 |
|---|---|---|
| D1 | 擴充現有 `setup` wizard，不新增 install 命令（做法 A） | 同意和安裝放在同一個地方，不用多學一個命令 |
| D2 | 安裝 **obscura**，走現有 deps installer（`mur-core/src/cmd/deps/installer.rs` 的 `install()`） | registry 已有 curated recipe（`mur-common/src/deps/registry_manifest.yaml`，v0.1.9，4 個平台，含 sha256），重用 sha256 驗證與 `safe_join`，只寫進 `~/.mur/aura/`，不寫全域 |
| D3 | Q5 回答 `yes` 就代表同意安裝，不再多問一題 | wizard 已經有明確的同意時刻（字面 `yes`） |
| D4 | Q5 同意後直接寫 `research_gateway.render_engine: obscura`，不再問 | 同 D3 |
| D5 | config 裡 `render_engine` **已有值就不覆蓋**，只印出目前的值 | 保留使用者手動設定 |
| D6 | 新增唯讀的 `mur deep-research doctor` | 不改任何東西，只回報狀態 |

## 3. setup 流程（Q5 = `yes` 之後）

在 `setup.rs` 的 `cmd_setup` 裡、`grant_render_browser` 迴圈**之前**插入：

1. **檢查**：`~/.mur/aura/obscura` 與 `~/.mur/aura/obscura-worker` 是否都存在（路徑同 `provision.rs:85-86` 的 `OBSCURA_RELATIVE` / `OBSCURA_WORKER_RELATIVE`）。
2. **缺了就安裝**：
   - 以 `mur_common::deps::registry::recipe("obscura", <platform>)` 取 recipe。
   - 先印出要做的事（URL、sha256 前 12 碼、會寫入的兩個路徑），再呼叫 `deps::installer::install(&recipe, mur_home)`。
   - 目前平台沒有 recipe → 印 `note: no obscura build for <platform>; rendered fetch stays off`，**跳過 4–5**，setup 繼續（跟現在缺瀏覽器時一樣不算失敗）。
   - 下載或 sha256 驗證失敗 → 印錯誤與可以手動重試的命令，setup 繼續，不寫 config。
3. **grant**：照現有流程呼叫 `grant_render_browser`，它會把 obscura 的絕對路徑加進 exec 權限。
4. **寫 config**（D4、D5）：
   - `render_engine` 不存在 → 寫入 `obscura`，印 `render engine: obscura (written to ~/.mur/config.yaml)`。
   - 已有值（不管是不是 obscura）→ 不動，印 `render engine: <值> (kept; already set in config)`。
   - 寫法重用 `secret.rs` 的 `upsert_gateway_key` + `write_config_atomically`（`secret.rs:94`、`secret.rs:165`），保留 `research_gateway:` 裡的其他設定；已有測試 `existing_gateway_settings_are_preserved` 覆蓋這個行為。
5. **smoke test**：用 `~/.mur/aura/obscura` 抓一個本地 fixture 頁面（`data:` URL 或暫時起一個 127.0.0.1 server），頁面內容只由 JS 產生；輸出含那段文字才算通過。
   - 通過 → `render smoke test: ok`
   - 失敗 → `render smoke test: FAILED — <原因>`，**不回滾**安裝或 config，並建議跑 `mur deep-research doctor`。

Q5 的說明文字要改：目前寫的是 `EXECUTES \`agent-browser\``（`setup.rs:144-147`），要改成提到 obscura，並說明回答 `yes` 會下載約 N MB 到 `~/.mur/aura/`。

Q5 不是 `yes` → 跟現在完全一樣，不安裝、不寫 config。

## 4. `mur deep-research doctor`（唯讀）

加在 `DeepResearchAction`（`mur-core/src/cli/actions.rs:878`）。只讀、不寫、不下載。

| 檢查 | ok | 不 ok 時的提示 |
|---|---|---|
| obscura 兩個 binary 存在且可執行 | 印路徑 | `run: mur deep-research setup` |
| `render_engine` 設定值 | 印值 | 未設 → 提示 setup；設成 `obscura` 但 binary 不在 → 標成錯誤 |
| 每個 worker 有沒有 render exec 權限 | 列出已授權的 worker | 列出缺的 worker |
| 每個 worker 有沒有 egress | 同上 | 同上 |
| smoke test（加 `--smoke` 才跑） | `ok` | 失敗原因 |

有任何錯誤時 exit code 1，方便寫進 script。

## 5. 不做的事

- 不支援 `agent-browser` 的安裝（要 npm -g，會寫全域）。已裝的 `agent-browser` 仍照現有邏輯被 grant。
- 不覆蓋使用者已設定的 `render_engine`。
- 不回滾：安裝成功但 smoke test 失敗時，保留檔案與 config。
- 不處理 `mur browser record/replay` 的 Playwright Chromium。

## 6. 測試

| 測試 | 驗證什麼 |
|---|---|
| Q5 非 `yes` | 不呼叫 installer、config 不變 |
| Q5 `yes`、binary 已存在 | 不下載，直接 grant + 寫 config |
| Q5 `yes`、config 已有 `render_engine: lightpanda` | 值保留，輸出含 `kept` |
| Q5 `yes`、config 沒有 `render_engine` | 寫入 `obscura`，其他 `research_gateway` 鍵不變 |
| 平台無 recipe | 印 note、不寫 config、setup 成功 |
| sha256 不符 | 沒有檔案寫入（沿用 `verify_and_place` 的 fail-closed）、不寫 config |
| doctor 在 binary 缺少時 | exit 1、不寫任何檔案 |

installer 的下載要能注入（trait 或傳入 bytes），測試不打網路。

## 7. 本機現況（寫 spec 時查到的）

- `~/.mur/aura/` 已有 `obscura`、`obscura-worker`、`lightpanda`。
- `~/.mur/config.yaml` 沒有 `render_engine`。
- 這台機器在 sandbox 裡直接執行 `~/.mur/aura/obscura --version` 會得到 `Operation not permitted`，smoke test 要在已授權的 worker sandbox 裡跑，或由 setup 本身（非 sandbox）執行。
