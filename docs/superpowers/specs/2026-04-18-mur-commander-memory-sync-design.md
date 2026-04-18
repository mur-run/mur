# Design — mur ↔ mur-commander 學習閉環 (Memory Sync Protocol)

> Spec version: **draft-1**
> Date: 2026-04-18
> Author: alan@twdd.com.tw (brainstormed with Claude)
> Status: **pending user review**
> Scope: v2.x of mur + commander

---

## Executive Summary

這份設計讓 `mur-commander` (閉源、部署在 VPS、多租戶執行引擎) 能把執行結果與學到的偏好回流到 `mur` (開源、個人電腦、記憶 CLI),透過一個共用的 `mur-server` (閉源、SaaS) 作為中樞。

目標以**三個回流 Channel** 實現「學習閉環」:
- **C1 Evidence** — Commander 執行 workflow 成功/失敗 → 更新對應 pattern 的 Evidence
- **C2 Chat 萃取** — Commander gateway 從 Slack/TG/DC 訊息萃出新 pattern draft
- **C3 Procedural** — Commander 背景分析 AuditStore,歸納重複執行軌跡為 procedural pattern

所有寫入都透過 **non-real-time pull model**:commander/mur 各自有本機 outbox/inbox,定期和 mur-server 同步。個人 mur 對 server 的依賴完全是 opt-in (OSS 不 push/fetch 100% 可用)。

支援 **Personal / Team 付費層級**,且 Team scope 為純商業 SaaS,不允許自架。

---

## Context & Motivation

### 現況問題

1. **mur 生態的記憶層與執行層脫節** — Commander 已有完整 workflow runner / trigger / MCP / AuditStore,但執行結果不會回灌 mur pattern 系統。`mur evolve` 全靠被注入次數 (`injection_count`),不知道「被注入後 AI 有沒有採納、執行有沒有成功」。
2. **Commander 的對話知識完全未被學習** — Slack/TG/DC 上的使用者偏好、團隊慣例、技術事實丟失。
3. **Commander 有龐大 AuditStore 但無法變成可重用知識** — 幾千次執行的模式沒被萃取。
4. **Evidence 是全域計數器** — Alice 的 override signal 會和 Bob 的 success signal 互相抵消,無法做 per-user 或 per-team 個人化。
5. **多用戶情境基礎設施基本是空的** — mur `Origin.user` schema 存在但 100% 寫 `None`,commander `AuditEntry` 沒有 `user_id`。

### 競品教訓

- Mem0 生產環境 audit 顯示 **97.8% 記憶是 junk** (GitHub issue #4573);不過濾的記憶系統毀滅性失敗
- Letta 的 **sleeptime agent** 已成為背景記憶整理的業界範式
- claude-mem 的**漸進式披露 3 層注入**把 token 成本降 90%
- A-Mem (NeurIPS 2025) 證明 **Zettelkasten 動態連結+evolution** 是長期記憶 SOTA

### 戰略目標

此設計對應 `docs/mur-函數清單與競品分析.md` 戰略缺口 #6「mur↔commander 記憶統一」,為整個 mur 生態建立**人無法複製**的「學習 → 執行 → 再學習」閉環。

---

## Goals / Non-goals

### Goals

1. Commander 執行的真實結果能影響 mur pattern 的 Evidence 與 Maturity
2. Commander 從 chat 學到的偏好能變成 mur 新 pattern (user 審核後)
3. Commander 的重複執行軌跡能被歸納成 Procedural pattern (user 審核後)
4. 支援 Personal + Team scope 兩個付費層,Team 純商業
5. OSS mur 保持 local-first,不強迫使用 server (push/fetch 完全 opt-in)
6. 完整的 multi-user identity provenance (但先不做 identity resolution)
7. 所有跨網路操作 delay-tolerant,離線可用

### Non-goals

1. **即時雙向同步** — 用 pull model,延遲是 feature
2. **跨用戶身份合併 (identity resolution)** — v1 只記錄 `(source, native_id)` tuple,不做 canonical user 映射
3. **P2P sync** — 一律走 mur-server 中心化
4. **本機 mur ↔ commander 直接通訊** — 即使在同台機器,也強制走 server
5. **自架 Team scope** — Team 是商業 SaaS 專用功能
6. **圖資料庫 / 實體萃取** — 此設計不處理,留 Zep/Cognee 等級的進階能力給未來
7. **自動 YAML 衝突合併** — 一律 last-writer-wins + 存 conflicts/ 供人工 diff

---

## 1. Architecture Overview

### 1.1 三節點拓撲

```
┌─ 個人電腦 (OSS mur,單用戶) ────────────┐
│  ~/.mur/patterns/*.yaml                 │  personal scope
│  ~/.mur/teams/{team_id}/*.yaml          │  team scope (付費解鎖)
│  ~/.mur/outbox/                         │  待 push 的訊號佇列
│  ~/.mur/inbox/                          │  從 server 拉回的 signals
└─────────────────┬───────────────────────┘
                  │ HTTPS + OAuth (mur push / mur fetch)
                  ▼
┌─ mur-server (fly.io, Go, 閉源) ────────┐
│  Postgres: patterns, evidence, actors, │
│            signals_queue               │
│  API: /v1/patterns, /v1/signals,       │
│       /v1/evidence, /v1/actors         │
└─────────────────┬───────────────────────┘
                  │ HTTPS + Service Account + X-Acting-On-Behalf-Of
                  ▼
┌─ VPS (閉源 commander) ─────────────────┐
│  ~/.mur/commander/                      │
│  ├─ users/{user_id}/                    │
│  │  ├─ patterns/     (唯讀 cache)       │
│  │  └─ inbox/        (待 flush 訊號)    │
│  ├─ teams/{team_id}/                    │
│  │  ├─ patterns/                        │
│  │  └─ inbox/                           │
│  ├─ outbox/          (批次 flush 到 server)│
│  └─ audit/                              │
└─────────────────────────────────────────┘
```

### 1.2 核心設計原則

1. **Pull-majority, not realtime** — outbox/inbox 模式,30-60 秒級延遲,換取 delay tolerance
2. **Server is single source of truth** — 衝突一律以 server 版本為準
3. **Scope 正交於 Tier** — Pattern 同時有 `scope (Personal/Team/Community)` 和 `tier (Session/Project/Core)`
4. **物理佈局對稱** — 個人 `~/.mur/{patterns,teams/{id}}/` 與 commander `~/.mur/commander/{users/{id},teams/{id}}/patterns/` 結構幾乎相同
5. **OSS 邊界** — `mur-common` 只放純資料型別,sync logic 在 mur-core (OSS) 和 commander (閉源) 各自實作
6. **Evidence 自動、新 Pattern 必需人工 accept** — 關鍵不對稱,防止 Mem0 式 junk

### 1.3 三 Channel 流向總覽

| Channel | 產生端 | 寫 commander outbox | 進 mur-server | 進個人 inbox | 個人 mur 採納 |
|---|---|---|---|---|---|
| **C1 Evidence** | commander runner / AutoFix / breakpoint | `signal.yaml` | 聚合進 pattern 的 `evidence` | `mur fetch` | **自動** |
| **C2 Chat 萃取** | commander gateway | `draft_pattern.yaml` | 進 pattern queue (draft) | `mur fetch --drafts` | **需 `mur accept/reject`** |
| **C3 Procedural** | commander AuditStore 分析 | `proposed_pattern.yaml` | 進 pattern queue (draft) | `mur fetch --drafts` | **需 `mur accept/reject`** |

---

## 2. Data Model Changes

全部在 `mur-common` (OSS)。所有變更為 additive + auto-migration,零破壞既有 patterns。

### 2.1 新增 `Scope`

```rust
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Team { team_id: String },
    Community { pack_id: Option<String> },
}

impl Default for Scope { fn default() -> Self { Self::Personal } }
```

**Migration**:既有 pattern YAML 無 `scope:` 欄位 → 讀取時自動當 `Personal`。

### 2.2 `Actor` (provenance only)

```rust
pub struct Actor {
    pub source: ActorSource,     // ClaudeCode | Slack | Telegram | Discord | CommanderDaemon | MurCli
    pub native_id: String,       // "U123ABC" (Slack), session_id, etc.
    pub display_name: Option<String>,
    pub resolved_user_id: Option<String>,  // server 端填,client 留空
}
```

**設計選擇**:client 不做 identity resolution,只記錄 provenance。Server 端有 `actor_bindings` 表映射 `(source, native_id) → canonical user_id`。

### 2.3 擴充 `Origin`

```rust
pub struct Origin {
    pub source: String,
    pub trigger: OriginTrigger,
    pub actor: Option<Actor>,        // 取代舊的 user: Option<String>
    pub confidence: f64,
}
```

### 2.4 擴充 `Evidence`

```rust
pub struct Evidence {
    // 既有全域聚合 (不動)
    pub source_sessions: Vec<String>,
    pub injection_count: u64,
    pub success_signals: u64,
    pub override_signals: u64,
    pub failure_signals: u64,

    // 新增 per-actor side-car
    pub contributions: HashMap<String, Contribution>,  // key = "source:native_id"
}

pub struct Contribution {
    pub success_signals: u64,
    pub override_signals: u64,
    pub last_seen: DateTime<Utc>,
}

impl Evidence {
    pub fn effectiveness(&self) -> f64 { /* 既有邏輯 */ }
    pub fn effectiveness_by_actor(&self, actor: &Actor) -> f64;  // 新
}
```

### 2.5 新增 `Signal` (outbox/inbox 傳輸單位)

```rust
pub struct Signal {
    pub id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub actor: Actor,
    pub target: SignalTarget,
    pub kind: SignalKind,
    pub scope: Scope,
    pub confidence: f64,  // 0.0-1.0,低 confidence 在 server 端降比重
    pub schema_version: u32,
}

pub enum SignalTarget {
    Pattern { name: String, scope: Scope },
    NewDraftPattern { payload: Box<Pattern> },
}

pub enum SignalKind {
    ExecutionSuccess,
    ExecutionFailure { error: String },
    UserOverrideAtBreakpoint { reason: Option<String> },
    AutoFixApplied { step: String },
    NewPatternProposal { origin_context: String },
}
```

### 2.6 新增 `Pattern.scope` 欄位

既有 Pattern struct 加 `scope: Scope` (預設 `Personal`)。YAML 層:
```yaml
scope:
  kind: team
  team_id: ops
```

### 2.7 Migration 風險彙總

| 變更 | 風險 | 處理 |
|---|---|---|
| Scope enum | 零 | 預設 Personal |
| Actor struct | 零 | optional |
| Origin.actor | 低 | schema bump + autofix |
| Evidence.contributions | 零 | 預設空 HashMap |
| Pattern.scope | 低 | 自動遷移 |
| Signal | 零 | 全新型別 |

---

## 3. Topology & Protocol

### 3.1 mur-server API (新增端點)

| Method | Path | 目的 | Client |
|---|---|---|---|
| `POST` | `/v1/signals/batch` | 批次送 signals | mur CLI + commander |
| `GET`  | `/v1/signals/pending?since={cursor}` | 拉對我有影響的 signals | mur CLI |
| `POST` | `/v1/signals/ack` | 確認已消化 | mur CLI |
| `GET`  | `/v1/patterns?scope=...&since=...` | 拉 pattern 快照 | 兩端 |
| `POST` | `/v1/patterns/accept` | 接受 draft | mur CLI |
| `POST` | `/v1/patterns/reject` | 拒絕 draft | mur CLI |
| `POST` | `/v1/actors/resolve` | 映射 `(source, native_id) → canonical user_id` | commander |
| `GET`  | `/v1/teams/{id}/members` | 列團隊成員 | 兩端 |

### 3.2 Auth

- **個人 mur CLI** → 既有 device code OAuth。需補:token response 新增 `user_id` 欄位,存進 `~/.mur/auth.json`
- **Commander → server** → 服務帳號 + `X-Acting-On-Behalf-Of: <user_id>`;commander instance 註冊時產 keypair,user 在 mur-server settings 授權 instance 代表自己
- **Actor resolution** → 一律在 **server 端**執行。用戶在 mur-server 個人設定頁綁定 `slack:U123ABC` 等。兩個流程:
  - (a) Commander 送 signal 帶 actor provenance (`source + native_id`),server 在持久化前查 `actor_bindings` 表,填上 `resolved_user_id`;找不到擺 `unresolved_actors` queue 等認領
  - (b) Commander 可主動呼 `POST /v1/actors/resolve` 預查 user_id (例如要把 draft pattern 放到對的 user inbox),這不是做解析的邏輯,只是**查 server 的映射結果**

### 3.3 Outbox / Inbox 機制

- 檔案路徑 `outbox/YYYY-MM-DDTHH:MM:SS-{uuid}.yaml`,append-only
- Flush (commander): 每 60s 掃 outbox → 打包 ≤100 條 → `POST /v1/signals/batch` (gzip) → accepted 移 `.flushed/` (保留 7 天)、rejected 移 `.rejected/`、網路失敗保留原地 + 指數 backoff
- Fetch (個人 mur): 每 5m 或手動 `mur fetch` → `GET /v1/signals/pending?since={cursor}` → 寫 inbox → apply → ack

### 3.4 Sync 頻率

| 事件 | 頻率 |
|---|---|
| commander outbox flush | 60s (可配置) |
| commander patterns refresh | 10m 或 lazy on inject |
| 個人 mur fetch | 5m 或手動 |
| 個人 mur push (本機變更回寫) | 在 `mur feedback` / `mur evolve` 後立即 flush |

---

## 4. Channel 實作細節

### 4.1 Channel 1 — Evidence 回流 (P0)

**觸發點 (commander 側)**:

| 位置 | 檔案 | Signal |
|---|---|---|
| WorkflowRunner 跑完 | `engine/src/workflow/runner.rs` | `ExecutionSuccess/Failure` |
| AutoFix 修了 step | `engine/src/workflow/autofix.rs` | `AutoFixApplied { step }` (計為 override) |
| Breakpoint 用戶否決 | runner paused_at resume | `UserOverrideAtBreakpoint` |
| Chat 用戶誇讚 | unified_handler | `ExecutionSuccess` 放大 N (user_praise_boost) |

**Signal 來源總數**:4 種 commander 訊號源都算 Channel 1。

**Pattern 命中判斷** — 執行前 inject 時記錄 top-K 被注入的 pattern IDs 到 AuditStore,執行後對這 K 個 ID 各發一個 signal:

```rust
// 執行前
let inj = mur_client.context(query, scope).await?;
audit.record_injection(&inj.patterns);

// 執行後
for pattern_id in inj.patterns {
    outbox.write(Signal {
        kind: if success { ExecutionSuccess } else { ExecutionFailure {...} },
        target: SignalTarget::Pattern { name: pattern_id, scope },
        actor, ..
    });
}
```

**Guard rails**:
- Server 端同 `(actor, target)` 5 分鐘內只算 1 票 (dedupe)
- `UserOverride` 權重 = `3 × ExecutionSuccess` (否定訊號稀缺高價值)
- 每個 signal 可帶 `confidence: f64`;低 confidence 降比重

**自動套用**:`mur fetch` 抓到 evidence signal → 直接寫 `evidence.contributions[actor_key]` + 遞增全域聚合。**不需 user 確認**。

### 4.2 Channel 2 — Chat 萃取 (P1)

**觸發**:commander gateway 收到 chat 訊息,`unified_handler` 用 LLM 粗篩判斷是否「可學習」:

- 臨時指令 (「現在幫我跑 X」) → 忽略
- 個人偏好 → `scope=Personal`, `kind=Preference`
- 團隊慣例 → `scope=Team(from channel)`, `kind=Behavioral`
- 技術事實 → scope 看 channel, `kind=Fact`

**Scope 決策**:

| Chat 情境 | Scope |
|---|---|
| DM → bot | Personal (sender) |
| 公開頻道 `#general` | 詢問發言者 (Slack Block Kit 按鈕) |
| 預配置 team-bound 頻道 (`#ops → team:ops`) | Team (預設),給 override 按鈕 |

**商業閘門**:Personal 總是免費;Team scope 需訂閱,否則 server 回 `403 need_team_subscription`,bot 通知用戶。

**Draft 流程**:commander LLM normalize 成 `Pattern` draft → outbox → server → 個人 mur inbox/drafts/ → Alice `mur drafts list` / `accept` / `reject [--reason]`。Reject 反饋回 server,未來相似擷取降 confidence。

### 4.3 Channel 3 — Procedural 萃取 (P2)

**資料源**:commander AuditStore (非 chat)。

**時機**:daemon sleeptime (無 workflow + 無 trigger ≥ 30 分鐘) 觸發。

**演算法**:
1. 掃最近 N 天 audit,分組 `(workflow_id, success, actor_scope)`
2. 找執行 ≥ 5 次、成功率 ≥ 80% 的 workflow
3. 抽重複 step 序列 → procedural 候選;抽反覆 variable 值 → fact 候選;抽 AutoFix 常修步驟 → preference 候選
4. 去重 (cosine sim ≥ 0.85 跳過)
5. 產 draft `Pattern` tier=project, scope 從 audit actor_scope
6. 附 `evidence_trail: Vec<audit_id>` 強化可解釋性

**Draft 流程**:同 C2,需 `mur accept/reject`。

### 4.4 三 Channel 對比

| 軸 | C1 Evidence | C2 Chat | C3 Procedural |
|---|---|---|---|
| 觸發 | workflow end / breakpoint / autofix | 每則 user 訊息 | sleeptime 30m idle |
| LLM? | 無 | 有 (篩+normalize) | 有 (歸納) |
| 自動套用? | 是 | 否 (draft) | 否 (draft) |
| Target | 既有 pattern | 新 pattern | 新 pattern |
| 付費閘門 | 無 | Team scope 要付費 | Team scope 要付費 |

---

## 5. OSS / 閉源 Boundary

### 5.1 Repo 職責

| Repo | License | 職責 | 依賴 commander? |
|---|---|---|---|
| `mur-common` | OSS (Apache/MIT) | 純型別 | ❌ |
| `mur-core` | OSS | mur CLI + sync client | ❌ |
| `mur-server` | 閉源 | REST API + Postgres + aggregation | — |
| `mur-commander` | 閉源 | daemon/scheduler/trigger/gateway/MCP/三 Channel emit | ✅ 依賴 mur-common |
| `mur-dashboard` | OSS | Web UI (後端 API 閉源) | — |

### 5.2 Wire format 契約

- `mur-common` 所有 serde-ready struct 構成公開契約
- Pattern/Signal 帶 `schema_version: u32`
- Minor version 只能加欄位;break 欄位 bump major,server 同時支援 N 與 N-1

### 5.3 Commander 獨家能力

1. Gateway Slack/TG/DC 整合
2. Chat 萃取 LLM prompt 與 normalizer
3. Procedural 萃取演算法
4. Multi-tenant actor partition
5. Constitution / Shadow / AutoFix / DLQ / Multi-machine SSH
6. Scheduler / Trigger daemon

### 5.4 OSS 獨立可用

Alice 只裝 OSS mur,不碰 commander,仍能:
- 本機 CRUD pattern (既有)
- 註冊 mur-server 免費帳號 → `mur push` / `mur fetch` (新,但 opt-in)
- Personal scope 完整使用
- Team scope 需付費,但**閘門在 server,不在 client**

### 5.5 神聖邊界

個人 mur 和 commander **永遠不直接通訊**,即使在同台機器。所有互動走 mur-server。理由:隔離、授權 (acting-on-behalf-of)、商業閘門插點。

---

## 6. Team 付費機制

### 6.1 付費層級

| 層級 | mur CLI | mur-server | commander | 月費 |
|---|---|---|---|---|
| 純本機 | ✅ 完整 | ❌ | ❌ | $0 |
| Personal Free | ✅ + push/fetch | ✅ free account | ❌ | $0 |
| Personal Pro (選做) | ✅ + 高 quota | ✅ | ❌ | $X |
| Team | ✅ + team scope | ✅ + team scope | ⚪ 可加購 | $Y/user |
| Team + Commander | ✅ | ✅ | ✅ VPS multi-tenant | $Z/user |

### 6.2 閘門實作

**Server-side only**:

```
client: mur push → POST /v1/signals/batch { scope: team:ops, ... }
server: check subscription
  └─ 無 → 403 { code: "need_team_subscription", upgrade_url: "..." }
  └─ 有 → accept + aggregate
```

Personal 和 Team signals 在同一 outbox,server 分別處理。Personal 過,Team 被拒的單獨回錯,不影響 Personal。

### 6.3 Commander feature flag

Commander 啟動時 `GET /v1/instances/me/features`,取得 feature flags 決定是否啟用 teams/ 相關 handler。

**防繞過**:
- Flag TTL 24h
- 24h 無法聯網 → degrade Personal 模式 + 警告
- 180 天完全離線 → 停用 Team 功能

### 6.4 Team schema

```sql
CREATE TABLE teams (id UUID PK, name TEXT, owner_user_id UUID, created_at TIMESTAMP);
CREATE TABLE team_memberships (
  team_id UUID, user_id UUID, role TEXT,  -- owner|admin|member
  joined_at TIMESTAMP, invited_by UUID
);
CREATE TABLE team_connected_actors (
  team_id UUID, actor_source TEXT, actor_native_id TEXT, user_id UUID
);
```

### 6.5 權限

| 動作 | 誰 |
|---|---|
| 建 team | 任何付費用戶 |
| 邀成員 | owner/admin |
| Push team pattern | 任何成員 |
| Accept/Reject team draft | owner/admin (可改「成員能 accept 自己 draft」) |
| 刪 team pattern | owner/admin |
| 解散 team | owner + 2FA |

### 6.6 降級/踢人 Grace Period

- **成員被踢**:本機 cache 保 30 天供匯出為 personal
- **Team 降級**:既有資料 90 天唯讀,180 天後歸檔/刪除
- **跨 team borrowing**: `mur team patterns promote <name> --from team:A --to team:B`,新 pattern evidence 歸零

### 6.7 Team Evidence 聚合

```rust
pub struct TeamEvidence {
    pub combined: Evidence,
    pub by_member: HashMap<user_id, Evidence>,
    pub canonical_contributions: HashMap<user_id, Contribution>,
}

impl TeamEvidence {
    pub fn member_leaderboard(&self) -> Vec<(UserId, f64)>;  // dashboard,可 opt-in 關閉
    pub fn personalized_effectiveness(&self, for_user: &UserId) -> f64;
}
```

---

## 7. 衝突、錯誤、可觀察性

### 7.1 衝突情境

**A. 同 pattern 被多方改**:
- 每 pattern 有 `updated_at` + `etag` (Postgres row version)
- Client push 帶 `if_match: <etag>`,不 match 回 409
- Client 存本地版到 `~/.mur/conflicts/{name}-{ts}.yaml`,提示 `mur diff`
- **決不自動 merge YAML**

**B. Draft 與既有 pattern 同名**:
- Server 自動 suffix `{name}-{short-uuid}`
- Alice accept 時可 `--rename` 或 `--merge-into existing-name` (LLM 輔助)

**C. Contributions key dedupe**:
- Server 端 5 分鐘 rate limit 去重

**不處理**:
- 跨 scope 同名 pattern (兩個不同 key)
- 離線 > 14 天 outbox 堆積 (CLI 警告,不自動清)
- 多 commander instance 同服一 user (不支援)

### 7.2 錯誤模式

| 錯誤 | 處理 |
|---|---|
| Server 5xx | 指數 backoff (30s→1m→5m→15m→1h) |
| Auth 過期 | auto refresh;失敗 `mur login` |
| Outbox YAML parse fail | `outbox/.quarantine/` + log |
| `~/.mur/` 遭刪 | `mur doctor --repair` |
| LanceDB 壞 | fallback keyword-only + 背景 reindex |
| Commander 磁碟滿 | 停收新 signal + ops 告警 + 7 天未 flush 自動 gc |
| Actor 找不到映射 | `unresolved_actors` queue + dashboard 提示認領 |
| Pattern lock race | advisory lock,inject 拿 read lock |

### 7.3 可觀察性 (client + server)

**mur CLI**:
- `mur sync status` / `mur sync logs --tail 50` / `mur doctor --sync`
- `~/.mur/logs/sync.jsonl` append-only

**Commander**:
- AuditStore 擴充 `signal_emission`, `signal_flush`, `pattern_cache_refresh`
- SSE event stream 廣播

**mur-server**:
- `signals` 表原始保留 30 天
- `signal_processing_job` 統計
- Prometheus metrics: `mur_signals_received_total{scope,kind}`, `mur_signals_rejected_total{reason}`, `mur_patterns_created_total{source}`, `mur_evidence_updates_total{actor_source}`, `mur_fetch_duration_seconds`

### 7.4 資料完整性

**Server**:
- `signals` 表 unique index `(user_id, scope, target_hash, actor_key, 5min_window)`
- `evidence_contributions` transaction
- 每日 checksum diff > 1% 告警

**Client**:
- 本機 atomic `.tmp` → fsync → rename (既有)
- `mur sync --dry-run`
- `mur sync --repair` 從 server 拉 snapshot 覆蓋

### 7.5 PII 防線

- Client 發出前 `sanitize_pattern()` + `secret_scan`
- Server 收後再跑 secret scan,命中 reject + log
- `origin_context` 強制 `max_len: 500`
- Team scope 可 opt-in `team.privacy_redact` 替換人名為 `@member-{id}`

---

## 8. Testing & Phase Plan

### 8.1 測試金字塔

```
           ┌──────────────────────┐
           │  E2E (手動 + 半自動) │  3 條黃金路徑
           ├──────────────────────┤
           │ Cross-binary 整合    │  docker-compose, ~20 scenarios
           ├──────────────────────┤
           │  Contract 測試       │  OpenAPI + pact, ~50 cases
           ├──────────────────────┤
           │       單元測試       │  每模組 > 80% coverage
           └──────────────────────┘
```

### 8.2 Contract 測試

- `mur-common` types → OpenAPI 3.1 via `schemars`
- commander 和 server 都驗 schema
- CI 在 mur-common breaking change 時 fail build

### 8.3 整合測試 (docker-compose)

20 個必測 scenarios,覆蓋 push/fetch/conflict/rate-limit/draft/offline/grace-period/actor-unresolved/cross-team 等。

### 8.4 Golden Paths (每 release 手測)

1. Alice 個人旅程 (裝 OSS mur → 註冊 → push → 另台 fetch)
2. Alice 團隊採納 (升 Team → 邀 Bob → 共享 pattern → leaderboard)
3. 團隊加購 Commander (裝 VPS → Slack 萃取 → accept → 執行 → evidence 回流)

### 8.5 Phase Plan

| Phase | 週 | 交付 | 指標 |
|---|---|---|---|
| 0 | W0 | spec + ADR + OpenAPI draft | 評審通過 |
| 1.A | W1-2 | mur-common schema + migration | 既有測試全綠 |
| 1.B | W3 | mur-core outbox/inbox (loopback) | `mur push/fetch --dry-run` 可運作 |
| 1.C | W4 | mur-server `/v1/signals/*`, `/v1/patterns/*` | Contract 測試綠 |
| 1.D | W5 | Commander C1 emit + flush | Scenarios 1-6 過 |
| 1.E | W6 | 跨 binary 整合 + Golden Path 1 | Golden Path 1 過 |
| 2.A | W7-8 | Team scope + membership + 付費閘門 | Scenarios 7-13 過 |
| 2.B | W9 | C2 chat 萃取 | 100 條 Slack 訊息 FP < 30% |
| 2.C | W10 | Golden Path 2+3 | 兩條過 |
| 3 | W11-14 | C3 procedural + sleeptime | 真實 audit accept rate ≥ 40% |

**P0 ship: W6; P2 complete: W14**.

### 8.6 成功指標

**P0 (C1)**:
- `evidence.effectiveness()` vs user `mur feedback helpful/unhelpful` Spearman > 0.6
- `mur feedback unhelpful` 事件數 ↓ ≥ 30%
- Signal → evidence p95 < 10 分鐘

**P1 (C2)**:
- Chat draft accept rate ≥ 40%
- FP (reject + "not useful") < 30%
- 每週 draft 中位數 3-10 條

**P2 (C3)**:
- Procedural accept rate ≥ 50%
- 每位 ≥ 30 次 workflow 執行的 user,月均 1-3 條 procedural draft
- Accepted procedural pattern 後續使用 ≥ 3 次

### 8.7 風險

| 風險 | 機率 | 影響 | Mitigation |
|---|---|---|---|
| Chat 萃取變 Mem0 junk | 🔴 高 | 毀滅核心價值 | 嚴格 guard rails + 每週抽樣 + 可隨時關閉 |
| Actor resolution 失控 | 🟡 中 | 延誤 P1 | MVP 不做跨平台合併 |
| mur-server SPOF | 🟡 中 | 服務中斷 | fly.io 多區 + 讀寫分離 + 離線容忍 |
| Team 付費複雜度超預期 | 🟡 中 | 時程滑 | server 端集中,client 只顯示 |
| signals 表爆炸 | 🟢 低 | 維護痛 | 30 天保留 + 聚合歸檔 + 分區 |
| OSS 社群反彈「需註冊」 | 🟡 中 | 品牌傷害 | 強調 opt-in + 不註冊功能等價 |

---

## Open Questions / Future Work

### v1 之後的擴展

1. **Actor identity resolution (v2)** — Slack U-ID ↔ GitHub ↔ email 的 canonical 對映
2. **跨團隊 pattern borrowing 的 lineage 追蹤** — borrowed pattern 效果回饋原 team 做「出口貢獻」統計
3. **Sleeptime consolidate 的 commander 參與** — 此 spec 只用 sleeptime 做 C3 萃取;未來讓 commander 代跑 `mur evolve`/`mur gc`
4. **時序推理 (對應戰略缺口 #3 雙時間失效)** — 配合 commander evidence 流做被取代模式的 validity window
5. **AGENTS.md sync target** — Commander 可產出合成 AGENTS.md 給非 mur tool 用 (對應戰略缺口 #4)

### 需更深思考的點

- Commander instance 註冊失竊如何撤銷?
- 若 mur-server 倒閉,OSS 用戶能否 export 出 patterns 檔案離開 ecosystem?→ 必需能
- Team 跨組織 clone (fork): 是產品面還是技術面問題?
- 計費與發票 (Stripe/LemonSqueezy) 整合不在此 spec 範圍
- GDPR/刪帳號流程:user 刪帳 → 對 team 留下的 contributions 如何處理 (匿名化 or 刪除)?

---

## Decision Log

| # | 決策 | 結果 | 時間 |
|---|---|---|---|
| 1 | 整合目標 | (A) 學習閉環 — commander 結果餵 mur | 2026-04-18 |
| 2 | Channel 範圍 | P0 C1 + P1 C2 + P2 C3 全要分階段 | 2026-04-18 |
| 3 | 拓撲 | (C) Pull model + commander 自帶 per-user pool;路徑 `~/.mur/commander/` | 2026-04-18 |
| 4 | Identity | Source-tuple (provenance only, no resolution) | 2026-04-18 |
| 5 | Team 建模 | (4) Hybrid — Scope 正交欄位 + 目錄分家 | 2026-04-18 |
| 6 | mur-server license | 閉源 | 2026-04-18 |
| 7 | mur-dashboard license | OSS | 2026-04-18 |
| 8 | Team 自架? | (A) 純商業 SaaS,不允許 | 2026-04-18 |

---

## Glossary

- **Actor** — `(source, native_id, display_name, resolved_user_id)` 四元組,記錄訊號的來源主體而不做身份解析
- **Channel (C1/C2/C3)** — 三種回流訊號類別:Evidence / Chat-extracted / Procedural-extracted
- **Contribution** — Evidence 中 per-actor 的分解單位
- **Draft Pattern** — 新 pattern 候選,等待用戶 `accept/reject`
- **Inbox** — client 從 server 拉回訊號後的本機暫存
- **Maturity** — 既有概念: Draft → Emerging → Stable → Canonical
- **Outbox** — client 待送 server 的訊號佇列
- **Pull model** — 非即時 server-driven sync,client opt-in `push/fetch`
- **Scope** — 正交於 Tier 的「擁有者」維度: Personal / Team / Community
- **Signal** — 單筆事件記錄 (wire format),可能是 evidence 更新或 draft pattern
- **Tier** — 既有概念: Session (14d) / Project (90d) / Core (365d) 半衰期分層

---

## References

- 競品對比: `docs/mur-函數清單與競品分析.md`
- Mem0 junk audit (強調 Evidence 必要性): [GitHub issue #4573](https://github.com/mem0ai/mem0/issues/4573)
- A-Mem paper (Zettelkasten evolution): [arxiv 2502.12110](https://arxiv.org/abs/2502.12110)
- Letta sleeptime mechanism: [Letta docs](https://docs.letta.com/guides/agents/architectures/sleeptime)
- claude-mem progressive disclosure: [GitHub](https://github.com/thedotmack/claude-mem)
- Text2Mem IR (未來標準化參考): [arxiv 2509.11145](https://arxiv.org/abs/2509.11145)
