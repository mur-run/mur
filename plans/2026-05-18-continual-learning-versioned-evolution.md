# MuR / MuR Commander — 持續學習與版本化演進規格 v2

**日期：** 2026-05-18（v2 修訂：併入 Q1-Q4 + A-D 深度問題）
**狀態：** Draft（規格，待 review）
**取代：** v1（同檔，commit 留存）
**範疇：** mur-core / mur-common / mur-agent-runtime / mur-daemon / mur-hub-gui，整合 MuR Commander

**v2 主要變更（vs v1）：**
1. 新增 §0 Decisions Log（Q1-Q4 + A-D 共 8 項裁決）
2. **E1 改為雙 git repo 設計**（知識層 `~/.mur/.git` + 執行層 `~/.mur/agents/.git`）
3. **新增 E6：Agent Pattern Federation**（解決 A: agent 與記憶系統解耦）
4. E5 contract 明確化（mur 側 inbox 已 live，缺 commander 側 writer）
5. 路線圖延至 120 天

---

## 0. Decisions Log（v2 新增）

| # | 議題 | 決策 | 出處 |
|---|---|---|---|
| D1 | Reflector model 預設 | **獨立 role 概念 + 3 階自動降階**：`local/qwen2.5-14b` → `anthropic/haiku-4-5` → 首次 prompt。Reflector / Curator / Embedding 各為一個 role，在 `~/.mur/models.yaml` 配置。 | Q1 |
| D2 | Sleep cycle 預設值 | **opt-in，三段式 rollout**：v2.17 default off + 三時機提示；v2.18-19 收 telemetry；v2.20 若 rollback 率 <1% 才考慮 default-on。 | Q2 |
| D3 | `mur internals git` | **YES 暴露，三色名單**：白（log/status/diff/show）直通；灰（checkout/stash）confirm；黑（reset --hard/rebase/push）需 `--i-know-what-im-doing`。daemon 啟動偵測外部 HEAD 變動自動 rebuild index。 | Q3 |
| D4 | C1/C2/C3 v1 transport | **`127.0.0.1` bind + bearer token**，重用已 frozen 的 `mur-common::signal::Signal` v1 schema（`signal.id` 做 idempotency、`actor.source` 即 `ActorSource` enum、`schema_version` 為 wire 版本）；簽章走既有 `SignedEnvelope` wrapper，v1 可選 / v2 強制。詳見 wire-protocol spec。 | Q4 |
| D5 | Agent ↔ 記憶系統的關係 | **Hybrid Federation 模型**：daemon 為 canonical store；每個 agent 啟動時拉取 `applies` 過濾後的 pattern snapshot 進 `~/.mur/agents/<name>/patterns_cache/`；agent 寫 evidence 到本地 outbox；export bundle 連 snapshot 一起打包，可離線運作，rejoin 時同步。詳見 **E6**。 | A |
| D6 | mur ↔ commander 管控分工 | **三層職責分離**：mur CLI 管 lifecycle（create/edit/inspect）、commander 管 task orchestration（workflows/audit）、hub-gui 管 monitoring + chat UI。新增 **AgentManifest** 概念作為兩者共用的宣告式 spec（K8s 風格）。詳見 E5 §8.3。 | B |
| D7 | `~/.mur/agents/` 是否進 git | **拆兩個 git repo**：`~/.mur/.git`（知識層：patterns/workflows/config）+ `~/.mur/agents/.git`（執行層：agent profiles/skills/perms）。**不**用 per-agent `.git`（100 agents = 100 repo 不可行）。telemetry/outbox-ledger/crashlogs/running.lock 全 gitignore。export 時用 `git subtree split` 產出 per-agent 乾淨歷史。詳見 **E1 §4.2**。 | C |
| D8 | LongTermStore 回流 gap 現況 | **確認 gap 存在**。mur 側 `mur-core/src/sync/inbox.rs:14-100` `Inbox::apply_all()` 已實裝且 live；commander 側 outbox writer / local_bridge **0%**。E5 的工作量集中在 commander 側 + 兩端 contract 明確化（mur 側只需把 Curator 接到既有 `apply_all`）。 | D |

---

## 1. 為什麼此時做？業界 2025-2026 風向

### 1.1 三個共識（v1 已寫，此處摘要）

1. **學習在 token-space，不在 weight-space**（[a16z](https://a16z.com/why-we-need-continual-learning/)、[Letta CL](https://www.letta.com/blog/continual-learning)）
2. **記憶 = 可變、可審計資產**，SSGM 點名 Memory Poisoning / Semantic Drift / Conflict 三大失敗模式（[SSGM](https://arxiv.org/html/2603.11768v1)）
3. **角色分工**：ACE Generator/Reflector/Curator（[ACE](https://arxiv.org/abs/2510.04618)） + Letta Primary/Sleep-time（[Sleep-time](https://www.letta.com/blog/sleep-time-compute)）

### 1.2 v2 新增的第四共識：Agent ≠ Memory Sink

最新一波 production 系統都不再把 agent 當「無記憶執行器」：
- **Letta Context Repositories**（2025 Q4）每個 agent 都有自己的 git-backed memfs
- **OpenAI Agents SDK + Memory** 把 memory tool 當第一公民傳給 agent
- **Anthropic Skills**（MuR 已部分採用）把 markdown 技能視為 agent 級資產

**MuR 現在的反例：** 完整 grep `mur-agent-runtime/src/` 找 `patterns` → 零次知識層讀取。每個 agent 是「啞 supervisor + LLM + skills」，學習成果由中央 daemon 包辦，但 export 出去就斷線。

→ **這驅動 E6 的存在**。

### 1.3 業界 vs MuR 對應更新

| 業界做法 | MuR 現況 | 缺什麼 | 對應 Epic |
|---|---|---|---|
| Letta git-backed memory | YAML 為真相但無 git history | 版本化 store | **E1** |
| ACE Reflector/Curator | `capture/feedback.rs` 純 keyword | 角色分工 + delta | **E2** |
| Sleep-time compute | 無閒置 reasoning | sleep cycle | **E3** |
| MemoryBench/Evo-Memory | 只有 B0 M11 安全 eval | retrieval eval | **E4** |
| Letta git memory per agent / OpenAI memory tool | **agent 與記憶完全隔離** | Agent federation | **E6 (新)** |
| Commander 雙向同步（draft 2026-04-18） | mur 側 inbox 已 ready, commander 側 0% | commander writer + contract | **E5** |

---

## 2. MuR Codebase 現況盤點（v2 補充 agent 層）

### 2.1 既有「學習迴圈」（v1 已寫，不重複）

略——見 v1 §2.1，內容不變。

### 2.2 致命缺口（v1 表格 + v2 新增三項）

| 缺口 | 證據 | 影響 |
|---|---|---|
| 無 pattern 版本欄位 | `KnowledgeBase.schema = 2` 常數 | A/B 不可能、無法回滾 |
| 無 audit log | 只有 `updated_at` 戳記 | 無法追溯變更 |
| edit 是 destructive | `store/yaml.rs` 原子寫入但覆蓋 | bad edit 永久損失 |
| 無 retrieval 品質 eval | `inject/stats.rs` 只計數 | 改演算法無法量化 |
| Workflow 版本化半成品 | `workflow.rs:41` 有欄位但 store 不支援多版本 | 無法平行跑兩版 |
| `team.rs` dead code | 整檔 `#![allow(dead_code)]` | 跨 agent 學習無法觸發 |
| Commander feedback 未出貨 | `2026-04-18` spec draft + `2026-05-18` plan 0% | 千筆 audit 無法回流 |
| **v2 新增：Agent ↔ memory 完全隔離** | `mur-agent-runtime/src/` grep `patterns` 零讀寫；export 不含 LanceDB | Exported agent 無學習能力；offline VPS agent 是 stateless |
| **v2 新增：agent 目錄與 telemetry 混居** | telemetry/jsonl + outbox-ledger 與 profile.yaml 同層 | 整目錄無法乾淨 git 化 |
| **v2 新增：無 multi-agent orchestrator** | 每個 agent 獨立 `running.lock`、無 supervisor 樹 | A2A 只能 1:1，無法做工作流編排 |

### 2.3（v2 新增）Agent 層盤點

從 explore 報告精煉：

- **mur-agent-runtime** 每個實例獨立，無 inter-agent 通訊以外的協調
- **`~/.mur/agents/<name>/` 結構**：
  - **User-edited（要 version）**：`profile.yaml`, `sys_prompt.md`, `skills/`, perm config, secret metadata
  - **Runtime cache（不要 version）**：`running.lock`, `telemetry/<date>.jsonl`, `companion/outbox-ledger`, `crashlogs/`, `.extract_digest`, `companion/inbox/`
- **`mur agent` 40+ 子命令**：lifecycle / comm / service / stats / export / mcp / perm / skill / prompt / secret / companion / rekey / schedule
- **export 機制**：`mur agent export --format=pkg|bin`，前者 tar.gz（profile + sys_prompt + skills + mcp prereq），後者把整 agent 目錄嵌入二進制（`MUR_EXPORT_AGENT_DIR`）
- **A2A v0.3 supervisor** 在 `mur-agent-runtime/src/supervisor.rs:40-150` — 純 per-agent，無集中視角
- **HostGuard/Landlock/SBPL** 由 `profile.entitlements` 驅動 per agent

---

## 3. 規格總覽：6 個 Epic（v2 加 E6）

```
        ┌─→ E2  Reflector + Curator  ─┐
        │                              │
E1 ─────┼─→ E3  Sleep-time            ─┼─→ E5  Commander Feedback Loop
(基礎)   │                              │
        ├─→ E4  Eval Harness          ─┤
        │                              │
        └─→ E6  Agent Federation      ─┘
                  (v2 新增)
```

**依賴規則**：
- E1 是所有其他 Epic 的前提（沒有版本化就沒有安全的自動寫入）
- E2 / E3 / E4 / E6 可平行（建議順序 E1 → E4 → E2 → E6 → E3 → E5）
- E5 整合所有上游，是最後 milestone

---

## 4. E1 — Versioned Store（v2 重大改寫：雙 git repo）

### 4.1 目標

讓 mur 的所有可變狀態都 git-backed、可 rollback、可審計。**v2 變更：拆兩個 git repo** 對應知識層與執行層職責不同。

### 4.2 雙 git repo 設計

#### 4.2.1 佈局

```
~/.mur/
├── .git/                              # ① 知識層 repo
├── .gitignore                         # 忽略 agents/, inbox/, session/, cache/, lance.db
├── patterns/*.yaml                    # tracked
├── workflows/*.yaml                   # tracked
├── config.yaml                        # tracked
├── models.yaml                        # tracked
├── policy.yaml                        # tracked
├── archive/                           # tracked
│   └── patterns/<name>/v<n>-<sha>.yaml
├── .mur-versions.yaml                 # tracked (索引)
│
├── inbox/                             # ignored (高頻；E5 用)
├── outbox/                            # ignored (高頻；E5 用)
├── session/                           # ignored (active.json + recordings/)
├── cache/                             # ignored (lance.db 衍生品)
├── secrets/                           # ignored (絕對不入 git)
│
└── agents/                            # ① 內 gitignore，但自己有 ②
    ├── .git/                          # ② 執行層 repo
    ├── .gitignore                     # 細粒度，每 agent 子目錄都有規則
    ├── README.md                      # tracked: 機器列表 + agent 清單
    ├── agent-a/
    │   ├── profile.yaml               # tracked
    │   ├── sys_prompt.md              # tracked
    │   ├── skills/*.md                # tracked
    │   ├── identity.pub               # tracked (Ed25519 public)
    │   ├── identity.prev              # tracked
    │   ├── patterns_cache/            # tracked (E6: snapshot pointer)
    │   │   └── .snapshot-ref          # 指向知識層 commit SHA
    │   ├── telemetry/                 # ignored (高頻 jsonl)
    │   ├── companion/outbox-ledger    # ignored (高頻狀態)
    │   ├── companion/inbox/           # ignored
    │   ├── crashlogs/                 # ignored
    │   ├── running.lock               # ignored
    │   └── .extract_digest            # ignored
    └── agent-b/
        └── ...
```

#### 4.2.2 為什麼分兩個 repo

| 維度 | 知識層 `~/.mur/.git` | 執行層 `~/.mur/agents/.git` |
|---|---|---|
| 寫入頻率 | 中（Curator commit、user 編輯） | 低（user 改 profile/skills） |
| 寫者 | daemon（Curator / sleep cycle） + user | user + daemon（E6 snapshot ref 更新） |
| 內容性質 | 「mur 學到什麼」 | 「user 配置了什麼」 |
| 多人協作 | 未來可能跨機器同步 | 通常單機/單 user |
| Export 對象 | pattern set / workflow set | 個別 agent（用 `git subtree split`） |
| 適合的 retention | 永久 | 長期但可 prune |

**反例（為何不用單一 git）**：telemetry 每秒 append → 整 repo 寫入熱點轉移到非知識內容，git pack 效率劣化、`git log patterns/` 變慢。

**反例（為何不用 per-agent .git）**：
- 100 agents = 100 個 `.git/` 目錄，磁碟膨脹（每個基本 4MB起跳）
- 跨 agent 操作（`mur agent diff <a> <b>` / `mur agent batch rollback`）需手動跑 100 次
- 無集中視角看 agent 配置歷史

#### 4.2.3 Export 時的 history 抽取

```bash
mur agent export --format=git --include-history my-agent
# 內部執行：
#   git -C ~/.mur/agents subtree split --prefix=my-agent -b export-my-agent
#   git -C ~/.mur/agents bundle create my-agent.gitbundle export-my-agent
#   tar czf my-agent.murpkg profile.yaml ... my-agent.gitbundle
# 接收端：
#   tar xzf my-agent.murpkg
#   git clone my-agent.gitbundle ~/.mur/agents/my-agent
#   mur agent register my-agent
```

→ 跨機器遷移 agent **自動帶版本歷史**，不需要中央 server。

#### 4.2.4 資料模型變更（mur-common）

`KnowledgeBase` 新增（v1 已寫，重申）：
```rust
pub version: u32,                       // 1, 2, 3...
pub revision: Option<String>,           // 12-char git short SHA
pub previous_revision: Option<u32>,
```
schema 升 `3`。

`AgentProfile` 新增（v2 新增）：
```rust
pub snapshot_ref: Option<SnapshotRef>,  // E6 用
```
```rust
pub struct SnapshotRef {
    pub knowledge_commit: String,       // 知識層 SHA
    pub taken_at: DateTime<Utc>,
    pub pattern_filter: PatternFilter,  // applies / tier / maturity 過濾條件
}
```

#### 4.2.5 寫入路徑

- `VersionedYamlStore`（知識層 wrapper）→ commit 到 `~/.mur/.git`
- `VersionedAgentStore`（執行層 wrapper，v2 新增）→ commit 到 `~/.mur/agents/.git`
- 兩者共用底層 `git2` crate + 一個 commit message convention：

```
<kind>(<scope>): <one-line>

kind  = pattern | workflow | profile | skill | maturity | feedback | snapshot | rollback
scope = pattern/workflow/agent name
```

#### 4.2.6 CLI 新增（合併 v1）

```
# 知識層
mur pattern history <name>
mur pattern diff <name> [v1] [v2]
mur pattern rollback <name> --to v<n>
mur pattern branch <name> <branch>
mur pattern merge <branch>

# 執行層（v2 新增）
mur agent history <name>
mur agent diff <name> [v1] [v2]
mur agent rollback <name> --to v<n>

# 通用 escape hatch（D3 決策）
mur internals git --layer=knowledge|agents <git-subcommand>
mur internals rebuild-index --layer=knowledge|agents
```

#### 4.2.7 daemon 啟動安全檢查

對兩個 repo 各自：
1. `git fsck` 偵測 broken refs
2. HEAD SHA vs `.mur-versions.yaml` 比對；不一致 → rebuild index + hub notification
3. `git gc --auto`（每 7 天）
4. 若 `.git` 損毀 → daemon 進 read-only，禁止 Curator 寫入

> **`.mur-versions.yaml` 是 load-bearing 索引，不是診斷檔（spike FIN-3）。** 內含每個 pattern / workflow 的版本對 commit SHA 映射，供 `mur pattern history` O(1) 查詢。詳見 §4.2.8 規則 #3 與 [ADR-0001](../docs/architecture/adr/0001-e1-versioned-store-spike-findings.md)。

#### 4.2.8 強制實作規則（2026-05-18 spike 結論，**production code MUST 遵守**）

詳細推導見 [ADR-0001](../docs/architecture/adr/0001-e1-versioned-store-spike-findings.md)。

1. **FIN-1 — save hot path 禁用 `git index.add_all`**。`VersionedYamlStore::save_pattern` / `VersionedAgentStore::save_profile` 只能 `index.add_path(p)` 顯式路徑（caller 傳入的 pattern 檔 + archive 檔）。違反 → repo 規模上升後 O(N²) 拖垮 daemon。
   - 例外：`repair_*` 一次性 disaster recovery 可用 add_all。

2. **FIN-2 — save hot path 禁用 git history walk**。version 派發必須 O(1)。建議實作：
   ```rust
   fn current_version(name: &str) -> u32 {
       archive_dir_count(name) + (if current_file_exists(name) { 1 } else { 0 })
   }
   ```
   或把 `version: u32` 寫進 pattern YAML metadata（schema=3 已預留欄位）。**不可**呼叫 `git log` 或 revwalk 在 save 路徑。

3. **FIN-3 — `.mur-versions.yaml` 升級為 load-bearing 每-pattern 索引**。原 v1 spec 是純 HEAD-SHA 比對的診斷檔；spike 量到 3000-commit repo 上 `git log` 走訪 1.94 秒（目標 100ms 的 20×）。**index 必填內容**：
   ```yaml
   schema_version: 3
   knowledge_head: <12-char sha>
   agents_head:    <12-char sha>
   patterns:
     <name>:
       current_version: <u32>
       versions:
         - { v: 1, sha: <12-char>, ts: <iso8601>, reason: <commit-msg-1st-line> }
         - { v: 2, sha: <12-char>, ts: <iso8601>, reason: ... }
   workflows: ...
   ```
   每次 save → 同步 append 一筆（O(1)）。`mur pattern history <name>` 直接讀索引；live revwalk 只在索引缺失 / 損毀時的 fallback。重建命令：`mur internals rebuild-index --layer=knowledge|agents`（離線 O(total-commits)）。

4. **FIN-4 — `.gitignore` 用 bare 模式，不要 `*/` anchor**。libgit2 對 `*/foo/` 在 `statuses()` 的 reporting 有不一致行為；用 bare `foo/` 才穩。正式 production `~/.mur/agents/.gitignore`：
   ```gitignore
   telemetry/
   outbox-ledger
   inbox/
   crashlogs/
   running.lock
   .extract_digest
   .apply-staging/
   .apply-in-progress
   ```
   單元測試驗 gitignore 正確性 **必須** 用 `Repository::is_path_ignored(path)`，不能用 `statuses()` 迭代判斷。

### 4.3 驗收條件

- [ ] `mur reindex --bootstrap` 後兩個 `.git` 都存在，且有初始 commit
- [ ] 編輯任一 pattern 3 次 → `mur pattern history` 顯示 3 revision，archive 對應
- [ ] 編輯任一 agent profile 3 次 → `mur agent history` 顯示 3 revision
- [ ] `mur agent export --format=git --include-history` 產出可在另一台機器 `git clone` 並 register 的 bundle
- [ ] daemon 從外部跑 `cd ~/.mur && git reset --hard HEAD~1` 後重啟，自動 rebuild index 不崩
- [ ] telemetry 每秒寫入 24 小時，`~/.mur/agents/.git/` 大小不超過 5MB 增長
- [ ] **`history()` 在 1000-pattern × 3-revision 的 repo 上回傳時間 < 100ms（走 `.mur-versions.yaml` 索引，不走 live git log）**（FIN-3 驗收）
- [ ] **`save_pattern` 在 10k-pattern repo 上的 wall-clock 與在 100-pattern repo 上的差距 < 2×**（FIN-1 + FIN-2 驗收，確認 O(1) per save）

---

## 5. E2 — Reflector + Curator（v2 微調：併入 D1）

v1 設計大致不變。**v2 補強：Reflector model 走 role 概念。**

### 5.1 Role-based model 配置（D1 決策落地）

```yaml
# ~/.mur/models.yaml (新增 roles)
providers:
  local:
    base_url: http://localhost:11434
  anthropic:
    api_key_ref: secret://anthropic_key
  openai:
    api_key_ref: secret://openai_key

models:
  local/qwen2.5-14b:
    provider: local
    context: 32768
  anthropic/haiku-4-5:
    provider: anthropic
    context: 200000
    cost_per_mtok_in: 1.00
    cost_per_mtok_out: 5.00

roles:
  reflector:
    primary: local/qwen2.5-14b
    fallback: anthropic/haiku-4-5
    cost_budget_per_day_usd: 0.50
    privacy_local_only_when_sensitive: true   # 看 policy.yaml sensitive_paths
  curator:
    primary: anthropic/haiku-4-5
    fallback: local/qwen2.5-14b
  embedding:
    primary: local/bge-m3
```

首次啟動偵測：
```rust
// mur-core/src/cmd/role_setup.rs (新)
fn auto_detect_reflector() -> RoleConfig {
    if ollama_has("qwen2.5:14b") || ollama_has("gemma3:12b") { /* local primary */ }
    else if has_secret("anthropic_key") { /* haiku */ }
    else { push_hub_notification("setup_reflector_role") }
}
```

CLI：
```
mur model role set reflector <model_name>
mur model role list
mur model role usage [--since 7d]
```

### 5.2 其餘設計（Reflector / Curator / 排程）

維持 v1 §5.2-5.3，不重複。

---

## 6. E3 — Sleep-time Companion（v2 微調：併入 D2 + 增 agent-side）

### 6.1 v2 變更：兩種 sleep cycle

**v1 只考慮 daemon-side sleep。v2 新增 agent-side sleep cycle**，因為 E6 後每個 agent 有自己的 patterns_cache 和 evidence outbox：

| 層 | 何時跑 | 做什麼 | 寫到哪 |
|---|---|---|---|
| **daemon-side** | 使用者閒置 15 分鐘 | drain commander/c1c2c3 inbox → reflect → curate → consolidate → decay | 知識層 git |
| **agent-side** | agent 處於 idle session 5 分鐘 | flush local evidence outbox → 同步 snapshot（看是否有新版） | agent outbox + snapshot_ref |

兩種 cycle 都受 D2 三段式 rollout 規範（opt-in）。

### 6.2 安全閾值 + telemetry 指標（D2 決策落地）

新增 telemetry：
- `sleep_cycle.enable_rate`
- `sleep_cycle.rollback_rate_per_1k_commits`
- `sleep_cycle.cost_p95_per_user_per_week`
- `sleep_cycle.disable_after_enable_rate`
- `sleep_cycle.agent_side.snapshot_drift_pct`（agent 拒收新 snapshot 的比例 — 過高代表 daemon 學歪）

升級 default-on 的 gate：上述前三項全部達標連續 6 週。

### 6.3 三時機 opt-in 提示（D2）

略——同上次回答內容。實作在 `mur-core/src/cmd/sleep_onboarding.rs`。

---

## 7. E4 — Eval Harness（v2 微調：新增 federation eval）

v1 設計不變，v2 新增第 5 個 suite：

```rust
pub struct FederationEval;   // E6 用：
// 給定一連串「daemon 學到新 pattern → agent snapshot 更新 → agent 用該 pattern」
// 量測：snapshot lag (median seconds)、agent acceptance rate、cross-agent
// pattern reuse rate
```

CLI 新增 `mur eval run federation`。

---

## 8. E5 — Commander Feedback Loop（v2 重寫 contract，2026-05-18 修訂）

> **修訂註記**：在寫 §8.2 過程中掃了 codebase 才發現 schema 基礎建設遠比 v1 spec 假設的完整。**原本要新建的 `FeedbackEnvelope` 是重複輪子**——直接重用既有 `mur-common::signal::Signal`（v1 schema，11 個測試）。完整 HTTP wire contract 已抽出到獨立 spec：
> 👉 **`docs/superpowers/specs/2026-05-18-commander-feedback-wire-protocol-design.md`**（本節為摘要 + 連結）

### 8.1 v2 變更（D8 決策落地）

**mur 側基礎設施掃描結果**（比 v1 想像更 ready）：

| 元件 | 狀態 | 位置 |
|---|---|---|
| `Signal` envelope (v1 schema) | ✅ live + frozen 2026-05-18 | `mur-common/src/signal.rs` |
| `SignalKind` 5 variant 涵蓋 C1/C2/C3 | ✅ live | 同上 |
| `SignalTarget::Pattern / NewDraftPattern` | ✅ live | 同上 |
| `Actor` + `ActorSource`（含 `CommanderDaemon`） | ✅ live | `mur-common/src/actor.rs` |
| `SignedEnvelope` Ed25519 wrapper | ✅ live（C7 Slack bridge 在用） | `mur-common/src/bridge/envelope.rs` |
| `Inbox::apply_all()` 受端 | ✅ live | `mur-core/src/sync/inbox.rs:14-100` |
| `Outbox` 發端 | ✅ live | `mur-core/src/sync/outbox.rs` |
| **`POST /v1/signals/batch` HTTP handler** | ❌ 未實裝 | `mur-core/src/server_agents/routes/` |
| **Bearer token 認證** | ❌ 未實裝 | — |
| **Dedup cache** | ❌ 未實裝 | — |
| **Commander 側 LocalBridge / outbox writer** | ❌ 未實裝（閉源倉） | — |

**剩下工作集中在三件事**：
1. **HTTP endpoint 與 dedup**（mur 側）— wire-protocol spec §6
2. **Commander 端 outbox writer + flusher**（commander 倉，閉源，平行開工）— wire-protocol spec §5 已備 reference impl
3. **Curator 接到 inbox**（mur 側，順接 E2/E3 sleep cycle 的 `drain_inbox()`）

### 8.2 HTTP contract 摘要（細節見 wire-protocol spec）

**裁決：不新建 `FeedbackEnvelope` 型別**。重用既有 `Signal`，只新增一個 HTTP 批次包裝：

```rust
// mur-common/src/signal.rs (在既有檔內新增；不開新 module)
pub struct SignalBatch {
    pub batch_id: Uuid,                  // 給 commander 端做 at-most-once 重試
    pub schema_version: u32,             // = SIGNAL_SCHEMA_VERSION = 1
    pub signals: Vec<Signal>,            // 既有 frozen v1 型別
}
```

**Endpoint**（單一批次端點，不分 channel — channel 由 `SignalKind` + `SignalTarget` 隱含）：

```
POST /v1/signals/batch    (127.0.0.1 only, Bearer token, 可選 SignedEnvelope)
GET  /v1/signals/pending  (既有 pull 路徑，不變)
```

**Channel 對應**（無需新 variant）：

| Channel | SignalKind | SignalTarget |
|---|---|---|
| C1 evidence | `ExecutionSuccess` / `ExecutionFailure` / `UserOverrideAtBreakpoint` / `AutoFixApplied` | `Pattern { name, scope }` |
| C2 chat extraction | `NewPatternProposal { origin_context }` | `NewDraftPattern { payload: Box<Pattern> }` |
| C3 procedural | `NewPatternProposal { origin_context }` | `NewDraftPattern { payload: Box<Pattern> }` |

**Auth**：daemon 首啟產 `~/.mur/secrets/commander-token.yaml`（D4），bearer 在 v1 必填、`SignedEnvelope` 在 v1 可選 / v2 強制。

**Idempotency**：server 對 `signal.id` (UUID) 維護 7 天 dedup cache（SQLite at `~/.mur/cache/signal-dedup.sqlite`）；重送回 202 + `{ "deduplicated": true }`。

**File-drop fallback**：同機部署可繞過 HTTP，commander 直接寫 `~/.mur/inbox/<ts>-<uuid>.yaml`（與 `Inbox::receive()` 相同格式），sleep cycle 下次 drain 自動接收。

**詳細部份請看 wire-protocol spec**：
- §3 HTTP 完整 request/response + 6 個 status code 對應情境
- §4 Auth model（bearer + Ed25519 verification path）
- §5 Commander 端 reference implementation（`LocalBridge` 完整 Rust 偽碼 + flusher loop）
- §6 MuR 端 handler 偽碼
- §7 Versioning rules（哪些變更可加在 v1、哪些要 bump v2）
- §8 Test fixtures（canonical YAML + snapshot test + integration test）
- §11 雙倉 implementation checklist（mur 13 項 + commander 10 項）

### 8.3 AgentManifest（D6 決策落地，新概念）

宣告式 spec，mur CLI 與 commander 雙方共用：

```yaml
# example: ~/.mur/agents/agent-a/manifest.yaml
apiVersion: mur.run/v1
kind: AgentManifest
metadata:
  name: code-reviewer
  workspace: default
spec:
  profile_ref: profile.yaml
  sys_prompt_ref: sys_prompt.md
  skills:
    - skills/review-rust.md
    - skills/review-typescript.md
  patterns:
    filter:
      applies_in: [code, review]
      tier: [project, core]
      maturity: [stable, canonical]
    snapshot_policy: pull-on-start
  resources:
    token_budget_per_day_usd: 5.00
    max_concurrent_sessions: 3
  entitlements:
    network: [github.com, sentry.io]
    filesystem_read: [/Users/david/Projects]
    filesystem_write: []
  federation:
    evidence_outbox: outbox/
    sync_interval_minutes: 15
```

部署：
```bash
mur agent apply -f manifest.yaml         # 本地建立/更新
mur commander apply -f manifest.yaml     # 由 commander 推到遠端機器
```

→ 兩個工具相同 schema，類 K8s 風格，避免 mur/commander 之間 spec 漂移。

### 8.4 Team scope 啟用（v1 §8.2.4 強化）

`team.rs` 移除 `#![allow(dead_code)]`，配合 E1 commit 序解決 conflict：
- 同一 pattern 從不同 `actor_id` 來的 evidence → 不互相覆蓋，各記 per-actor `helpful_count`
- 若兩 actor signal 矛盾（A success / B override）→ Curator 產生 `InsightKind::Conflict` 並 log 到 hub

### 8.5 驗收（v1 + v2 增量）

- [ ] Commander mock 打一個 `SignalBatch` 含 C1/C2/C3 各 1 個 `Signal` → 回 202 / `accepted: 3` → 三筆落 `~/.mur/inbox/` → sleep cycle 跑完知識層各產一 commit
- [ ] 重送同 `batch_id` → 回 cached 202；重送同 `signal.id`（不同 batch）→ `deduplicated++`、無 double apply
- [ ] Bearer token 缺/錯 → 401；token 對但 `SignedEnvelope` 簽章不符（v2 模式）→ 422 `signature_mismatch`
- [ ] `mur internals replay-inbox --from <date>`（debug/migration）可重跑歷史 signal YAML
- [ ] `mur-common/tests/wire_format_snapshot.rs` 跑 insta snapshot 通過，意外改動 schema → CI 紅

---

## 9.（v2 新增）E6 — Agent Pattern Federation

### 9.1 目標

打通 A 問題：讓 agent 從「無記憶 thin client」變成「有 snapshot + 本地 outbox 的記憶節點」，同時不破壞 daemon 為 canonical store 的中心化模型。

### 9.2 設計

#### 9.2.1 三個關鍵概念

1. **Snapshot** — daemon 在 agent 啟動時，依 `manifest.spec.patterns.filter` 過濾出該 agent 用得到的 pattern subset，寫入 `~/.mur/agents/<name>/patterns_cache/*.yaml`，並在 `.snapshot-ref` 記下知識層 commit SHA
2. **Evidence Outbox** — agent runtime 執行中產生的 signal（pattern 是否被引用、被使用者讚/反駁）寫到 `~/.mur/agents/<name>/outbox/<ts>-<nonce>.yaml`
3. **Federation Sync** — 兩種同步：
   - **pull**（snapshot 更新）：agent idle 5 min 或 commander/hub 強制觸發 → daemon 重算 snapshot → diff → 寫入 patterns_cache
   - **push**（outbox 上行）：agent 每 N 分鐘掃 outbox → 透過 daemon local API 注入 inbox → Curator 處理

#### 9.2.2 Snapshot 過濾語言

```yaml
patterns:
  filter:
    applies_in: [code, review]          # KnowledgeBase.applies.contexts
    applies_to_projects: [project-x]    # 預設「不限」
    tier: [project, core]
    maturity: [stable, canonical]
    importance_min: 0.5
    max_count: 200
  snapshot_policy: pull-on-start | pull-periodic | manual
```

snapshot policy 三檔讓不同信任度的 agent 採不同節奏（例如 production agent 用 manual，dev agent 用 pull-on-start）。

#### 9.2.3 Offline 場景

`mur agent export --format=bin --include-snapshot` 把 patterns_cache 一起嵌入二進制 → 拿到無網路 VPS 跑：
- agent 用 snapshot 中的 pattern（read-only）
- evidence 仍寫 outbox（local file）
- 下次連回有 daemon 的環境 → `mur agent reconnect <name>` 主動 push outbox

#### 9.2.4 安全模型

- agent 沒有寫 `~/.mur/patterns/` 的權限（檔案系統層用 D3 sandbox 阻擋）
- agent 唯一寫入：自己的 outbox + telemetry
- 所有對 canonical 的影響都經過 daemon Curator → 等於把 E2 的安全閘門複用到 federation

#### 9.2.5 衝突解決

當 daemon 推新 snapshot 而 agent 本地 outbox 有未 flush evidence 時：
1. 先 push outbox（讓 daemon 知道最新 evidence）
2. daemon 把 evidence 套用到 canonical（可能改變該 pattern）
3. 重新計算 snapshot 並 pull
4. agent 收到「自己貢獻過 evidence 的 pattern」更新版

→ 形成完整 federation feedback loop，呼應 ACE「context as living playbook」。

### 9.3 mur-agent-runtime 改動

新增三個模組：
- `mur-agent-runtime/src/federation/snapshot.rs` — pull/diff/apply
- `mur-agent-runtime/src/federation/outbox.rs` — write/queue/flush
- `mur-agent-runtime/src/federation/client.rs` — talks to daemon local API

新增 supervisor 啟動步驟（在 sandbox apply 之後）：
```rust
// supervisor.rs
federation::pull_snapshot(profile.snapshot_ref, manifest.spec.patterns.filter)?;
federation::start_outbox_flusher(interval_minutes);
```

daemon 側新增 endpoint：
```
GET  /v1/agents/:name/snapshot       — 回傳 snapshot YAML tarball + ref SHA
POST /v1/agents/:name/evidence       — agent push outbox 用
POST /v1/agents/:name/reconnect      — offline agent 重連
```

### 9.4 驗收

- [ ] 建一個 agent + manifest 過濾 `tier: [core]` → snapshot 只含 core patterns
- [ ] daemon 升級某 pattern 到 core → 下次 snapshot pull 含該 pattern；agent log 顯示「snapshot updated to <sha>」
- [ ] agent 在 offline VPS 跑 1 小時產生 10 條 outbox → `mur agent reconnect` 後 daemon inbox 收到 10 envelope
- [ ] E4 `mur eval run federation` 顯示 snapshot lag p50 < 5 分鐘、agent acceptance rate > 95%

---

## 10. 120 天路線（v2 延長）

| 週 | Epic | 工作 |
|---|---|---|
| W1-W3 | E1 | schema=3、雙 git repo、archive、CLI history/diff/rollback、`mur internals git` |
| W4 | E1 | GUI History（hub）、integration test、文件 |
| W5-W6 | E4 | retrieval / maturity / reflector eval suites、CI |
| W7-W8 | E2 | role 配置（D1）、Reflector LLM + heuristic fallback、Curator、conflict resolution |
| W9-W11 | **E6** | manifest schema、snapshot pull/diff、outbox、daemon endpoints、agent runtime hook |
| W12 | E6 | `mur agent export --include-snapshot`、offline reconnect、federation eval |
| W13-W14 | E3 | daemon-side sleep cycle、agent-side sleep cycle、IdleScheduler hook、安全閾值 |
| W15 | E3 | opt-in onboarding（D2）、hub 顯示、telemetry 指標 |
| W16 | E5 | mur 側 HTTP endpoint（D4）、bearer token、AgentManifest CLI（`mur agent apply`） |
| W17 | E5 | 與 commander 對齊 contract、跑 mock e2e |
| W18 | (release) | dogfood、bug bash、文件、v2.20 release |

每 Epic 結束跑 E4 全套 suite。

---

## 11. 護城河說明（v2 補強）

| 對手 | 強項 | 對 MuR 威脅 | v2 完成後差異化 |
|---|---|---|---|
| **Letta** | git-backed memory、Skills、sleep-time | 直接撞 MuR 核心 | 雙 git 設計 + agent federation 比 Letta 單層 memory 更貼近 multi-agent ops；**MuR 本地優先 + 跨 runtime 可攜（E6）** 是 Letta 沒有的 |
| **Mem0** | 20+ backend、token-efficient algo | retrieval 品質 | E4 eval harness 量化追趕；dual-layer pattern + maturity lifecycle + federation 是結構優勢 |
| **ACE** | Reflector/Curator + delta | 演算法領先 | E2 直接吸收；**MuR 多 per-actor evidence、commander 執行回饋、agent snapshot federation** 三條 ACE 沒觸及的工程閉環 |
| **OpenAI / Anthropic 原生 memory** | 模型原生 | 大廠紅利 | **跨模型可攜（agent 換 model 不丟資產）、純本地、git 可審計**；E6 export 可離線跑這條他們完全沒有 |
| **K8s/Operator 風格 agent 編排** | 成熟生態 | 未來企業需求 | E5 §8.3 `AgentManifest` 直接對齊 K8s 風格，但執行體是輕量 supervisor 而非 container — 對開發者友善 |

---

## 12. 風險與緩解（v2 補強）

| 風險 | 緩解 |
|---|---|
| 雙 git repo → 磁碟雙倍 | 兩個都跑 `git gc --auto`；archive 用 git-LFS-like external storage（v2.5） |
| Reflector LLM 費用爆 | role-level cost budget hard cap（D1）；local primary 預設 |
| sleep cycle 把 pattern 改壞 | E1 rollback + E4 bisect 是治理；drift 指標自動 alert |
| commander outbox 沒人寫 | E5 §8 把 contract 寫死，閉源 commander 倉照規格實作；mur 側 mock server 在 dev tools |
| **v2 新增：snapshot 與 outbox 衝突** | §9.2.5 順序（push outbox → 重算 canonical → pull snapshot），不可顛倒 |
| **v2 新增：agent 太多 → snapshot 過載** | filter 強制 `max_count: 200`，超過拒收；daemon limit 並發 snapshot 計算 |
| **v2 新增：export bundle 含 snapshot 洩密** | 預設 `--include-snapshot` opt-in，文件警告 snapshot 可能含 sensitive pattern |

---

## 13. Sources

- [a16z — Why We Need Continual Learning](https://a16z.com/why-we-need-continual-learning/)
- [Jiayi Weng — Learning Beyond Gradients](https://trinkle23897.github.io/learning-beyond-gradients/)
- [Letta — Continual Learning in Token Space](https://www.letta.com/blog/continual-learning)
- [Letta — Context Repositories](https://www.letta.com/blog/context-repositories)
- [Letta — Sleep-time Compute](https://www.letta.com/blog/sleep-time-compute)
- [ACE — Agentic Context Engineering (arXiv 2510.04618)](https://arxiv.org/abs/2510.04618)
- [Sleep-time Compute (arXiv 2504.13171)](https://arxiv.org/abs/2504.13171)
- [Evo-Memory (arXiv 2511.20857)](https://arxiv.org/html/2511.20857v1)
- [MemoryBench (arXiv 2510.17281)](https://arxiv.org/html/2510.17281v4)
- [SSGM — Governing Evolving Memory](https://arxiv.org/html/2603.11768v1)
- [Mem0 — State of AI Agent Memory 2026](https://mem0.ai/blog/state-of-ai-agent-memory-2026)
- [NVIDIA — TTT for LLM Memory](https://developer.nvidia.com/blog/reimagining-llm-memory-using-context-as-training-data-unlocks-models-that-learn-at-test-time/)
- 內部 spec：`docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md`
- 內部 spec：`docs/superpowers/specs/2026-04-22-murmur-p0a-agent-runtime-design.md`
- 內部 plan：`docs/superpowers/plans/2026-05-18-mur-commander-channel1-closure.md`
