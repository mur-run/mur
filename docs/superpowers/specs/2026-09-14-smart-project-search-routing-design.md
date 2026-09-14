# 智慧專案搜尋路由：agent-first 設計

**狀態**：設計完成，待審閱
**日期**：2026-09-14
**腦力激盪決策**：以 agent-first 的智慧路由技能為主體，另提供不理解自然語言的薄狀態 helper；不建立全包式 bash 搜尋 wrapper。

## 問題

`mur project search` 擅長概念與意圖查詢，但索引可能落後工作樹；`rg` 精確、完整且反映當前檔案，卻不提供語意召回。以 `git status` 是否乾淨作為唯一開關，會因為無關的 README 修改而丟棄可用的語意搜尋，也會錯把非 Git 目錄當成搜尋失敗。

現有 builtin skill 的 scope 說明也過時：程式目前預設搜尋 current directory project，而不是所有已索引專案。

## 目標

- 讓 MUR 與其他 agent 依查詢意圖及索引可用性，可靠選擇 semantic、`rg` 或混合搜尋。
- 精確符號、字串與完整列舉永遠以 `rg` 為準。
- 意圖搜尋在索引可用時保留 `mur project search` 的召回；工作樹有變更時，以 `rg` 補搜受影響檔案。
- 對 status 提供正式 JSON 輸出，禁止 helper 解析面向人類的 CLI 文案。
- 非 Git 專案若索引可用，仍可做 semantic 搜尋。

## 非目標

- 不做能由人直接輸入任意自然語言並可靠分類的全包 bash CLI。
- 不在 shell 中合併、重排或比較 semantic score 與 grep 結果。
- 不聲稱 semantic 零結果代表程式碼不存在。
- 不在本期建立或重建索引；路由器只觀測並選擇 fallback。

## 考慮方案

### A：技能決策＋薄狀態 helper（採用）

技能理解使用者意圖與對話上下文；helper 只蒐集 deterministic 事實：Git root、受影響路徑和 index 狀態。技能直接呼叫 MCP `mur_project_search`，或以 `rg` 搜尋；混合模式的結果由 agent 解讀與標記來源。

優點：自然語言分類留在模型層，shell 小且可 fixture-test，MCP 不需要繞經 CLI，能處理多語言查詢與上下文。

### B：從查詢到結果的 bash wrapper（不採用）

wrapper 必須以 regex 猜測查詢意圖、處理 shell quoting、解析／合併異質結果與排序。它可作為日後人類 CLI 的基礎，但 `--auto` 無法形成可靠正確性契約；若日後需要，應提供明確 `--intent`、`--exact`、`--all-callers` 模式，而非把猜測當保證。

## 元件與責任

| 元件 | 責任 |
|---|---|
| `mur-project-search` skill | 分類查詢、取得 freshness 狀態、選工具、整合結果、說明 fallback 原因 |
| `mur project status --json` | 輸出版本化的 `ProjectStatusInfo` JSON，不改變既有文字輸出 |
| 狀態 helper（若需要） | 以 `git status --porcelain=v1 -z` 取得 root、clean 與 changed paths；不接受 query、不執行搜尋 |
| `mur_project_search` MCP tool | 搜尋目前專案的既有索引內容 |
| `rg` | 精確、完整、或索引不可用時的 working-tree 搜尋；混合模式只限 changed paths |

## 路由契約

| 查詢類型 | index usable | Git 狀態 | 行為 |
|---|---:|---|---|
| exact symbol／string | 任意 | 任意 | 全專案 `rg` |
| exhaustive（所有 caller、rename、dead-code） | 任意 | 任意 | 全專案 `rg` |
| intent | 是 | clean | `mur_project_search` |
| intent | 是 | dirty | `mur_project_search` 加 `rg` 搜尋 modified、staged、untracked、renamed-to 路徑 |
| intent | 否 | 任意 | 全專案 `rg` fallback |
| intent | 是 | 非 Git | `mur_project_search` |

`usable` 的定義是：`indexed == true`、`indexing_in_progress == false`、`stale_dims == null`。semantic 呼叫本身失敗時，也改用全專案 `rg`，並報告原因。

deleted 路徑不交給 `rg`；技能必須把 semantic 命中於 deleted 路徑者視為過時而不採信。rename 同時視為 old path 過時、new path 需要補搜。

## JSON 契約

擴充既有指令：

```text
mur project status --path <root> --json
```

輸出維持 `ProjectStatusInfo` 欄位，並加入穩定 schema 版本：

```json
{
  "schema_version": 1,
  "name": "mur",
  "path": "/Volumes/Firecuda4tb/Projects/mur",
  "indexed": true,
  "chunks": 123,
  "last_indexed": null,
  "indexing_in_progress": false,
  "progress": null,
  "stale_dims": null
}
```

Git dirty-path 資訊不塞入 project status：它是 VCS 可選能力，且屬於薄 helper 的責任。這讓 status 在非 Git 目錄仍是正確且通用的 index API。

## 技能規則

1. 先判斷查詢是否 exact 或 exhaustive；若是，直接用 `rg`，不需要 status。
2. 對 intent 查詢，以 project root 明確呼叫 status JSON；不可從 `mur project status` 的顯示文字擷取欄位。
3. `usable` 時使用目前專案 scope 的 semantic search，不使用 `--all`，除非使用者明確要求跨專案。
4. dirty 時只把 changed paths 作為 `rg` 的檔案範圍；不得因一個無關變更放棄 semantic。
5. 回答時標記哪些發現來自 indexed snapshot、哪些來自 working tree；需要時開啟實際檔案驗證。
6. semantic 結果為零不是不存在證據；改以 `rg`／檔案檢閱確認。

## 錯誤處理

- 無 index、indexing 中或 stale dimensions：不等待、不自動 index，立即 fallback `rg`。
- `mur project status --json` 失敗：說明 status 不可取得，安全地使用全專案 `rg`。
- Git 指令失敗：將專案視為 non-Git；若 index usable 仍做 semantic，否則 `rg`。
- `rg` 在指定 dirty path 找不到檔案（刪除、競態或 rename）：忽略該 path 並在結果中說明，不能讓整個搜尋失敗。

## 實作與測試

1. 在 `ProjectAction::Status` 加 `json: bool`，並讓 `cmd_project_status` 在 JSON 模式序列化 `schema_version: 1` 的輸出；保留文字輸出與其既有行為。
2. 更新 dispatch 與 CLI help。
3. 改寫 `mur_project_search.yaml`：修正 scope 說明、加入上述路由表及 dirty hybrid 行為，保持 skill budget。
4. 將 builtin skill 的註冊與 parsing tests 擴及修改後內容。
5. 為 CLI JSON 測試至少覆蓋：未索引、可用索引、indexing in progress、stale dims 的序列化與 `schema_version`。
6. 為 skill 加 fresh-context routing 微測試：intent+clean 選 semantic；exact 選 `rg`；intent+dirty 選 hybrid；non-Git+usable 不降級；indexing/stale 選 `rg`。

## 驗收條件

- `mur project status --json` 是可機器讀取且有版本的穩定輸出。
- 有無關 Git 修改的 intent 查詢仍使用 semantic，並補搜髒路徑。
- exact／exhaustive 查詢不會錯送 semantic。
- status／Git 不可用不造成失敗或假陽性，只可靠降級為 `rg`。
- `mur-project-search` 的 builtin 安裝、解析與 disclosure budget 測試通過。
