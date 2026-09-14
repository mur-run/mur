# Durable Monitor：監看無法當下得知結果的工作

**狀態**：設計完成，尚未實作  
**日期**：2026-09-11  
**腦力激盪決策**：持久化 monitor 引擎為核心；事件可提前喚醒、輪詢可靠保底；agent 自動註冊且允許手動新增；規則優先、agent 補位；低風險自動處置、高風險走 HITL。

## 問題

CI、MUR run、Codex／Claude Code 子程序與其他非同步工作，不會在啟動它們的對話回合內必然產生結果。現在若 agent 只說「等完成再看」，便沒有可靠機制保證之後真的會回來確認：

1. 工作成功後，應通知、啟動下游工作或執行其他成功動作。
2. 工作失敗後，應蒐集證據、套用已知補救、有限次重跑，或請人決定。
3. 工作仍在執行時，應按退避策略繼續查看。
4. 查詢來源故障時，不可把「不知道」誤判成被監控工作失敗。
5. MUR 或 agent 重啟後，等待事項不可消失。

現有 cron 不能作為可靠核心：

> “No persistence of missed firings: if the agent is offline when a cron would have fired, that firing is skipped — there is no catch-up mechanism.”  
> `docs/cookbook/c4-cron-triggers.md:74-75`

## 目標

- 對所有具有可查詢憑證的非同步工作建立 durable monitor。
- 統一 GitHub Actions、MUR run、Codex／Claude Code 子程序的觀測結果。
- 讓明確規則先處理常見結果，未涵蓋情況才交由 agent 判斷。
- 自動執行低風險處置；改程式、部署、付費、刪除資料、修改權限等高風險操作先請示。
- 使用租約、冪等鍵與 append-only history，避免重啟或重試造成重複副作用。
- stalled、soft deadline、hard deadline 各自有清楚而不同的語意。
- 正常輪詢保持安靜，只在值得人注意的事件通知。

## 非目標

- MVP 不做任意第三方服務的專用 adapter；先提供 `custom` 擴充契約。
- 不把 cron 改造成 durable workflow engine。
- 不保證修復任何未知失敗；agent 可診斷與提案，但仍受權限、風險與嘗試上限約束。
- 不以「超時」直接代表被監控工作失敗。
- 不把 secrets 或 access token 寫進 monitor spec、事件紀錄或通知。

## 選定方案

採用 **持久化 Monitor 引擎＋來源 Adapter**。

- SQLite 保存 monitor、觀測週期、租約、動作與事件紀錄。
- daemon 從 store 領取到期 monitor；程序重啟後補跑已過期項目。
- `SourceAdapter` 以輪詢作可靠保底；若來源能提供 webhook／事件，只用來提前喚醒同一筆 monitor。
- `PolicyEngine` 先套結構化規則；規則無法決定時才呼叫 `AgentResolver`。
- `ActionExecutor` 只直接執行通過風險政策的動作，其他建立 HITL approval。

### 未採用：Webhook-only

速度快、查詢少，但每個來源都要整合，漏事件會讓工作永久沉睡，不能單獨承擔可靠性。

### 未採用：定期叫 agent 掃描所有等待事項

第一版容易做，但狀態格式不穩、推理成本高，且難以提供租約、冪等、重啟恢復與精確 deadline。

## 核心元件

| 元件 | 責任 |
|---|---|
| `MonitorStore` | 持久化 spec、runtime state、觀測、租約、嘗試次數及事件紀錄 |
| `MonitorScheduler` | 領取到期 monitor、限制並行數、過期租約回收、重啟補跑 |
| `SourceAdapter` | 查詢來源並回傳原始證據與標準化所需欄位 |
| `OutcomeNormalizer` | 將來源結果統一為 `pending/succeeded/failed/cancelled/unknown` |
| `PolicyEngine` | 依 monitor 規則與全域政策選擇下一步 |
| `AgentResolver` | 規則未涵蓋時，根據 spec、history 與 logs 提出處置 |
| `ActionExecutor` | 冪等執行動作；高風險動作送 HITL |
| `Notifier` | 只傳送有意義的狀態事件，並去除敏感資訊 |
| `WakeupReceiver` | 接 webhook／內部事件，只提前 `next_check_at`，不繞過正常判定 |

## `MonitorSpec` 契約

自動與手動建立都正規化成同一份契約：

```yaml
schema_version: 1
name: wait-for-ci
source:
  type: github_actions       # github_actions | mur_run | codex | claude_code | custom
  reference: owner/repo/run-id
  credential_ref: github-default  # 只存引用，不存 secret

outcomes:
  success: completed && conclusion == success
  failure: completed && conclusion in [failure, cancelled, timed_out]

actions:
  on_success:
    - type: notify
    - type: start_downstream
      workflow: publish-preview
  on_failure:
    - type: collect_logs
    - type: apply_known_remedy
    - type: rerun
  on_unknown:
    - type: reschedule_monitor

policy:
  mode: hybrid
  max_remediation_attempts: 3
  stalled_after: 20m
  soft_deadline: 3h
  hard_deadline: 8h
  retain_monitoring_after_hard_deadline: true

notifications:
  events:
    - stalled
    - soft_deadline
    - approval_required
    - remediation_failed
    - terminal

idempotency_key: ci:owner/repo:run-id
created_by:
  actor: agent:commander
  reason: "CI was started and returned a trackable run id"
  originating_run_id: optional-run-id
```

### 建立時驗證

1. `schema_version` 可支援。
2. `source.type` 有已啟用的 adapter，且 `reference` 格式合法。
3. 來源至少可進行一次唯讀查詢；權限問題須立即明示，不能建立一筆注定查不到的 monitor。
4. `stalled_after < soft_deadline < hard_deadline`。
5. 動作名稱及參數符合 schema。
6. `idempotency_key` 在 active monitor 中唯一；重複建立回傳既有 monitor，而非再生一筆。
7. `credential_ref` 存在，但不展開後寫入 store 或 logs。
8. agent 自動建立必須附 `reason` 與可用的原始 run／conversation 關聯。

## 自動註冊邊界

當 agent 啟動 GitHub Actions、MUR run、Codex／Claude Code 或其他非同步工作，並取得可追蹤 ID 時，啟動與 monitor 建立必須盡可能採原子語意：

1. 若來源支援先建立記錄再啟動，先以 `registering` 保存，再啟動工作並補入 reference。
2. 若來源只能啟動後取得 ID，啟動成功後立即建立 monitor；建立失敗時，原回合必須明確回報「工作已啟動但未受監看」及可追蹤 ID，並寫入可恢復的 registration outbox。
3. 若只有模糊承諾而沒有 queryable reference，不得聲稱已監看；應要求補充來源或提供手動查詢命令。
4. 使用者也可透過 CLI 手動新增相同 spec。

## 狀態模型

Monitor runtime state 與 observed outcome 分開：

```text
Monitor state:
registering → active → checking → sleeping ─┐
                │         │                │
                │         ├→ action_pending│
                │         ├→ awaiting_approval
                │         ├→ completed
                │         └→ exhausted
                └──────────────────────────┘

Observed outcome:
pending | succeeded | failed | cancelled | unknown
```

### Monitor state 語意

| 狀態 | 語意 |
|---|---|
| `registering` | 非同步工作與 monitor 正在完成關聯 |
| `active` | 可被 scheduler 領取 |
| `checking` | worker 持有租約並正在查詢 |
| `sleeping` | 等待 `next_check_at` |
| `action_pending` | 結果已知，等待執行 policy 選出的動作 |
| `awaiting_approval` | 高風險動作已建立 HITL，監看本身仍可低頻更新 |
| `completed` | 終態動作已成功結算，不再正常輪詢 |
| `exhausted` | 已超過自動補救上限或無法可靠繼續，等待人工處理 |

### Outcome 語意

- `pending`：來源明確表示工作尚未結束。
- `succeeded`：來源明確表示成功終態。
- `failed`：來源明確表示失敗終態。
- `cancelled`：來源明確表示取消；預設走 failure policy，但保留不同原因。
- `unknown`：查詢失敗、回應無法解析、憑證失效或 adapter 故障；這是 monitor 問題，不是被監控工作失敗。

## 排程、租約與退避

### 一般 pending 退避

預設序列：

```text
30s → 1m → 2m → 5m → 15m → 30m
```

到達 30 分鐘後維持上限，並加入 bounded jitter，避免大量 monitor 同時打來源 API。adapter 可根據來源提供 `recommended_poll_after`，但不可突破全域最短間隔及 rate-limit policy。

### unknown 退避

`unknown` 使用獨立、較短且有上限的 query-error backoff；持續失敗會發出 monitor-health 通知，但不觸發被監控工作的失敗補救。認證失效等永久錯誤可直接進入 `awaiting_approval` 或 `exhausted`。

### 租約

- scheduler 以資料庫 transaction 原子領取到期工作並設定 `lease_owner/lease_expires_at`。
- worker 在長查詢期間更新 lease heartbeat。
- daemon 崩潰後，過期租約可被其他 worker 回收。
- 所有 observation 與 action 都帶 cycle id；舊租約晚到的結果不得覆寫較新的 cycle。

## stalled 與期限

Codex／Claude Code 及其他 agentic 子程序採以下預設：

| 門檻 | 預設 | 行為 |
|---|---:|---|
| 無進度 stalled | 20 分鐘 | 診斷、通知，可執行明確且低風險的喚醒／蒐證動作 |
| soft deadline | 3 小時 | 升級診斷與通知；可依規則有限次低風險補救 |
| hard deadline | 8 小時 | 停止新的自動補救，通知人工；若設定允許，轉低頻唯讀監看 |

「進度」包括新的 stdout/stderr、來源 heartbeat、狀態／step 變更、子程序活動，或 adapter 定義的 progress token。只有實質變化才更新 `last_progress_at`；重複回傳同一狀態不能讓 stalled 永遠延後。

期限自 monitor 對應工作真正啟動的時間計算，而不是 daemon 重啟或每次補救的時間。補救產生新工作時建立新的 observation cycle，保留 parent/child 關係，不覆寫舊結果。

hard deadline 不是工作失敗判定。若 `retain_monitoring_after_hard_deadline: true`，停止自動補救後以低頻（預設每 2 小時）唯讀查詢，直到來源終態、使用者取消或 retention 到期。

## 混合處置策略

處置順序固定如下：

1. **結構化規則**：先匹配精確的 source、outcome、error code、branch、attempt 等條件。
2. **已知補救**：例如蒐集 logs、重新連線、重跑 flaky job；每項動作都要有風險分類與冪等鍵。
3. **AgentResolver**：只有規則未涵蓋或補救失敗時才介入，輸入為去敏後 spec、最近 observations、logs 摘要及已嘗試動作。
4. **風險閘門**：低風險自動執行；高風險送 HITL。agent 不得靠改寫動作名稱繞過分類。
5. **重新觀測**：補救若啟動新 run，保存新 reference 並建立 child cycle。
6. **上限**：最多 3 次自動補救；達上限進入 `exhausted`，通知並停止自動補救。需要時可繼續低頻唯讀監看。

### 風險政策

可自動執行的典型低風險動作：

- 唯讀查詢、蒐集及摘要 logs。
- 依已核准規則重查或重新連線。
- 對明確標記 flaky 且未達上限的同一 CI job 執行 rerun。
- 發送去敏通知。

預設需要批准：

- 修改程式、設定或權限。
- 合併 PR、部署、rollback 或發布。
- 產生費用或擴大資源。
- 刪除資料、取消其他人的工作。
- 任何 privileged 或政策無法分類的動作。

`on_success: start_downstream` 只有在下游 workflow 已預先核准、參數符合固定 schema 且不提升風險時才可自動執行；例如「成功就部署 production」不因寫在 success action 就自動成為低風險。

## 冪等與事件紀錄

每個副作用使用穩定 action key：

```text
<monitor-id>:<cycle-id>:<observed-terminal-version>:<action-type>:<action-index>
```

ActionExecutor 先以 unique constraint claim key，再執行動作並保存結果。對支援原生 idempotency key 的外部 API，將同一 key 傳給來源。若外部動作成功但本地程序在記錄前崩潰，恢復時先查外部狀態；無法證明安全重試則轉人工，不盲目重做。

history 為 append-only，至少記錄：

- monitor 建立、規格版本及建立原因；
- 每次查詢時間、adapter 結果、證據摘要及 progress token；
- 狀態轉換與 policy decision；
- action claim、approval、執行結果及錯誤；
- 通知送達結果；
- 人工 retry/cancel/edit。

Secrets、完整環境變數與未去敏 logs 不得寫入 history。

## 通知策略

正常 polling 不通知。預設只通知：

- 首次進入 `stalled`；恢復進度後可發一則 recovery。
- 首次跨過 soft deadline。
- 需要 approval。
- 自動補救失敗或達嘗試上限。
- 成功、失敗、取消等終態完成結算。
- monitor 自身持續 `unknown`、credential 失效或 adapter 壞掉。

每種 event 以 monitor＋cycle＋event type 去重；狀態未改變時不重複吵人。通知須包含 monitor 名稱、來源 reference、已知結果、執行過的動作、下一次檢查時間，以及需要使用者做的單一步驟。

## MVP Adapter

### GitHub Actions

- Reference：repository＋run id，必要時含 job id。
- 查詢：run/job status、conclusion、updated timestamp、attempt。
- Progress：status、job/step 變更或 `updated_at` 推進。
- 補救：下載 logs；明確規則允許時 rerun failed jobs/run。
- Webhook：`workflow_run` 事件只作 wakeup，輪詢負責最終確認。

### MUR run

- Reference：MUR `run_id`。
- 查詢：使用統一 run status，區分 semantic state 與 liveness。
- 若呼叫工具等待逾時，沿用既有原則：timeout 只代表呼叫端停止等待，不代表工作失敗；monitor 接手後以 run id 查詢，不能重新 dispatch 同一工作。
- Progress：heartbeat、step、iteration 或狀態轉換。

### Codex／Claude Code 子程序

- Reference：由 launcher 管理的 process/session id，不以 PID 單獨作 durable identity。
- 查詢：process existence、exit code、heartbeat、輸出 offset、子程序活動及產物狀態。
- 預設門檻：20 分鐘 stalled、3 小時 soft、8 小時 hard。
- daemon 重啟後若無法重新附著，但能證明程序仍在，標為 `unknown`／detached 並只做唯讀查詢；不能因失去 stdout pipe 就宣告失敗。
- 若 process 已不存在且沒有可靠 exit record，結果是 `unknown`，不是 `failed`。

### Custom

MVP 只定義 plugin contract：validate reference、observe、redact evidence、可選 wakeup parser、可選 progress token。custom adapter 預設只讀，不可自行新增 ActionExecutor 權限。

## CLI

```text
mur monitor add --file <spec.yaml>
mur monitor list [--state active|sleeping|awaiting-approval|completed|exhausted]
mur monitor show <id> [--history]
mur monitor cancel <id>
mur monitor retry <id> [--reset-remediation-budget]
```

- `add` 驗證 spec 並回傳 monitor id、來源及首次查詢時間。
- `list` 預設顯示未結束 monitor，包含 outcome、last progress、next check 與 deadline。
- `show` 顯示去敏 spec、目前租約、最近 evidence、處置與通知。
- `cancel` 停止 monitor，預設不取消被監控工作；若要取消來源工作是另一個明確、高風險 action。
- `retry` 重新啟用 `exhausted` 或 monitor-error 狀態；重設補救額度需額外明示，且保留舊 history。

Agent 啟動非同步工作時應呼叫同一個 service API，而不是 shelling out 到 CLI。

## SQLite 資料模型

MVP 至少包含：

| Table | 關鍵內容 |
|---|---|
| `monitors` | id、spec JSON、state、outcome、reference、deadlines、next check、progress、版本 |
| `monitor_cycles` | parent cycle、source attempt/reference、started/finished、terminal outcome |
| `monitor_leases` | monitor id、owner、expiry、fencing token |
| `monitor_observations` | cycle、observed_at、outcome、progress token、去敏 evidence、adapter error |
| `monitor_actions` | action key、risk、approval id、state、attempt、result |
| `monitor_events` | append-only type、payload、created_at |
| `monitor_notifications` | event key、channel、delivery state |
| `monitor_registration_outbox` | 已啟動但尚未完成 monitor 關聯的恢復資料 |

`monitors.version` 採 optimistic concurrency；lease 使用單調遞增 fencing token，阻擋過期 worker 寫回。

## daemon 恢復

啟動時依序：

1. 執行 schema migration。
2. 回收已過期 leases，留下 recovery event。
3. 處理 registration outbox，補建缺少的 monitor。
4. 將已到 `next_check_at` 的 monitor 排入 bounded work queue。
5. 重查狀態不明的 claimed actions；不能安全對帳者送人工。
6. 恢復 webhook receiver，再開始正常排程。

重啟補跑要有啟動節流與 jitter，避免所有逾期工作同時轟向 GitHub 或本機 subprocess manager。

## 錯誤處理

| 情況 | 處理 |
|---|---|
| API rate limit | 尊重 reset/retry-after，保存 `unknown` monitor health，不判工作失敗 |
| credential 失效 | 暫停有權限需求的查詢／動作，通知更新 credential reference |
| adapter parse error | 保存去敏原始摘要與 adapter 版本，短退避重試，達門檻通知 |
| daemon crash during query | lease 到期後重查；observation 以 cycle/fencing token 防舊寫入 |
| daemon crash during action | 先對帳外部狀態；只在可證明冪等時重試 |
| source says run not found | 先區分 eventual consistency、權限與真正刪除；未能證明時為 `unknown` |
| malformed agent proposal | PolicyEngine 拒絕，保存理由；不得直接交給 shell 執行 |
| notification failure | 不回滾已完成 action；獨立退避重送並在 CLI 顯示 delivery state |

## 安全與隱私

- credential 只以 reference 解析；SQLite、history 與通知不保存 secret。
- adapter 輸出在交給 agent、store 或 notifier 前統一 redaction。
- 自然語言 action proposal 必須轉成結構化、schema-validated action，再經風險政策。
- command/custom adapter 禁止任意 shell 字串；採 allowlisted executable＋argv schema，並沿用 MUR sandbox entitlements。
- webhook 必須驗簽、防 replay，且只能 wakeup 既有 monitor；不能直接宣告成功或觸發 action。
- monitor 所屬 agent 停止或被刪除時，monitor 不消失；轉給 system owner 或標為 orphaned 等待處理。

## 測試策略

### Unit tests

- `MonitorSpec` schema、deadline 順序與 credential redaction。
- 所有來源狀態到標準 outcome 的 mapping。
- pending／unknown backoff、jitter 邊界與 rate-limit override。
- progress token 只有實質變更才重置 stalled timer。
- risk classification 不能被 action 名稱或 agent prose 繞過。
- action key 穩定且 terminal action 不重複。

### Store／scheduler tests

- 兩個 worker 同時領取時只有一個取得 lease。
- 過期 worker 的 fencing token 無法寫回。
- daemon 中斷並重啟後，missed check 會補跑。
- 大量逾期 monitor 啟動時有節流，不產生 thundering herd。
- active `idempotency_key` 重複註冊回傳同一 monitor。

### Adapter contract tests

每個 adapter 使用 fixture 驗證 pending、success、failure、cancelled、not-found、rate-limit、malformed response、credential failure 與 redaction。GitHub webhook 只能令 monitor 提前到期，不能直接完成它。

### Action／recovery tests

- success/failure 終態各只觸發一次 action。
- 外部 action 成功、本地記錄前 crash：可對帳者不重做；不可對帳者進人工。
- 補救啟動 child run，舊 cycle history 不被覆寫。
- 第 3 次補救後進 `exhausted`，不出現第 4 次自動修復。
- 高風險 action 必須停在 `awaiting_approval`。

### 時間語意 tests

使用 fake clock 驗證：

- 20 分鐘無進度只通知一次 stalled。
- 新 output offset／heartbeat／step 會解除 stalled。
- 重複相同 heartbeat 不重置計時。
- 3 小時 soft deadline 觸發診斷但不宣告失敗。
- 8 小時 hard deadline 停止新補救；若保留監看則轉 2 小時低頻輪詢。

### End-to-end tests

1. GitHub fixture run：pending → failed → 規則 rerun → succeeded → downstream once。
2. MUR run：tool wait timeout → monitor 接手 → run 完成；不得重新 dispatch。
3. Codex／Claude Code fixture：20 分鐘無 output 但 process 活著 → stalled；之後恢復 output → recovery。
4. daemon 在查詢及 action 邊界各 crash 一次，重啟後不漏查、不重複副作用。
5. agent 啟動工作後 monitor insert 失敗，registration outbox 成功補建。

## 可觀測性

提供不含 secrets 的 metrics／diagnostics：

- active monitors、due lag、checks by outcome、adapter error rate；
- lease expiry/recovery 數量；
- stalled/soft/hard deadline 次數；
- actions by risk/result、approval wait、exhausted monitor 數；
- notification delivery failures；
- oldest overdue monitor。

`mur monitor show` 是使用者第一診斷面；daemon logs 必須帶 monitor id、cycle id、adapter 及 action key，不輸出 credential 或完整未去敏 logs。

## 實作順序

1. **契約與 store**：`MonitorSpec`、SQLite schema、history、idempotency、migrations。
2. **scheduler**：租約、fencing、退避、fake-clock tests、daemon recovery。
3. **唯讀 adapters**：MUR run、GitHub Actions、Codex／Claude Code process registry。
4. **CLI**：`add/list/show/cancel/retry`，先讓唯讀監看可操作。
5. **Policy／actions**：結構化規則、風險分類、HITL、action idempotency。
6. **AgentResolver**：規則無法決定時才接入，輸出強制 schema validation。
7. **自動註冊與 outbox**：接到各非同步啟動入口。
8. **wakeup events**：GitHub webhook 與內部 run events，輪詢仍保底。
9. **通知與 operational metrics**。

## MVP 驗收條件

- daemon 離線跨過一次預定檢查，重啟後仍會補查並留下 recovery history。
- 三種第一方 adapter 都能區分 `failed` 與 `unknown`。
- 同一成功／失敗終態即使被觀測多次，也只執行一次副作用。
- Codex／Claude Code 預設門檻確定為 **20 分鐘 stalled／3 小時 soft／8 小時 hard**。
- 低風險已知補救可自動執行，最多 3 次；高風險動作必須可證明停在 HITL。
- hard deadline 後不再自動補救，但預設保留低頻唯讀監看。
- 正常 polling 不通知；stalled、soft deadline、approval、補救失敗與終態會通知且去重。
- agent 只要啟動有 trackable id 的非同步工作，就能自動產生 monitor；沒有 reference 時不會假裝已監看。
