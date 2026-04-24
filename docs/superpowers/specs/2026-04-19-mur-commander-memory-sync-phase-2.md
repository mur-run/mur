# Phase 2 — Channel 2 Chat 萃取 + Team Scope 計費閘門

**Status**: Draft scoping (2026-04-19)
**Depends on**: Phase 1 (`2026-04-18-mur-commander-memory-sync-design.md`) ✓ shipped
**Audience**: implementers — assumes Phase 1 vocabulary (Signal / Scope / Actor / Outbox / Inbox)

## Goals

1. 把 commander 收到的 chat 訊息轉為 Pattern draft,走同一條 outbox→server→mur 管道。
2. Team scope 正式啟用,帶計費閘門 — Personal 免費,Team 需訂閱。
3. Draft 不自動進 user 的 `patterns/`;需 `mur drafts accept` 明確批准。

## Non-goals

- Channel 3 (Procedural extraction from audit store) — Phase 3。
- User-facing dashboards for draft review — CLI only in Phase 2。
- Multi-tenant UI for team billing admin — 用 server 現有的 subscription flow。

---

## 1. Chat Extraction Pipeline

### 1.1 觸發點

`crates/gateway/src/unified_handler/` 在 `handle_message` 結尾,訊息處理完後異步呼叫 `extract_chat_signal()`。

**Fire-and-forget** — 失敗不影響 chat 回覆。寫入本地佇列 `~/.mur/commander/chat-extract-queue/`,由一個背景 worker 批次處理。

### 1.2 粗篩 LLM

**Prompt**:輸入 `(user_message, bot_response, channel_context)`,要求輸出 JSON:

```json
{
  "learnable": true,
  "kind": "preference | behavioral | fact | none",
  "confidence": 0.0,
  "extracted": {
    "name": "…",
    "description": "…",
    "principle": "…",
    "technical": "…"
  }
}
```

**Rejection filter**:
- `learnable=false` → discard silently。
- `confidence < 0.6` → discard。
- `kind == "none"` → discard。
- 類 Mem0 "97.8% junk" 警示:保守判定,寧缺勿濫。

### 1.3 Scope 決策

在 gateway 側決定,不依賴 LLM:

| Chat 情境 | Scope | 備註 |
|---|---|---|
| DM → bot | `Personal(sender)` | 免費 |
| 公開頻道 未綁 team | `Personal(sender)` | 預設個人,降低誤傷 |
| team-bound 頻道 (config 映射) | `Team(team_id)` | 需訂閱;未訂閱 → 降級 Personal + bot 回訊 |
| community 設定 on 的頻道 | `Community(pack_id)` | Phase 2 不實作提交流程,只打 log |

**預設安全**:任何模糊情境一律 Personal。Team scope 只在明確的 `channels.slack.T0/C0.team_id="..."` 配置下觸發。

### 1.4 Signal 寫出

> **Amendment (2026-04-23)**: Phase 1 already plumbed the draft carrier — no new `SignalKind::PatternDraft` variant, no schema bump. Use the existing `SignalTarget::NewDraftPattern { payload: Box<Pattern> }` to carry the full `Pattern` struct and `SignalKind::NewPatternProposal { origin_context }` for provenance. See `signal_with_new_draft_pattern_roundtrip` test in `mur-common/src/signal.rs` for the wire format.

Normalize 成 Phase 1 的 `Signal`:

```rust
Signal {
  target: SignalTarget::NewDraftPattern {
    payload: Box::new(Pattern {
      base: KnowledgeBase {
        name: extracted.name,
        description: extracted.description,
        content: Content::DualLayer { principle, technical },
        tier: Tier::Session,
        ..Default::default()
      },
      ..Default::default()
    }),
  },
  kind: SignalKind::NewPatternProposal {
    origin_context: format!("slack:{channel_id}#{ts} from user {user_id}"),
  },
  actor: Actor { source: Slack, native_id: user_id, resolved_user_id: None, ... },
  scope: <decided above>,
  confidence: extracted.confidence,
  schema_version: SIGNAL_SCHEMA_VERSION,  // stays at 1
  ..
}
```

Why reuse the existing plumbing: `NewDraftPattern` carries a full `Pattern` struct so every downstream consumer (server drafts store, client inbox renderer, `mur drafts accept`) gets the same types they already know — no parallel `DualLayer`/`name`/`description` fields to keep in sync. `SignalKind::NewPatternProposal.origin_context` is free-form string — callers encode whatever provenance is useful (slack message URL, audit id, etc.).

**No `mur-common` code changes for this phase.** The first actual Phase 2 PR is server-side (§2).

---

## 2. Server-side Draft 路由

### 2.1 Handler

新增 `POST /api/v1/core/drafts/batch` — 或複用 `/signals/batch`,server 依 `kind` 分派:`PatternDraft` → drafts store,其他 → 原 signals pipeline。

**推薦**:複用 `/signals/batch`,單一入口,單一 ack protocol。server 端內部分派即可。

### 2.2 Store

新資料表 `pattern_drafts`:

| col | type | notes |
|---|---|---|
| id | uuid pk | |
| actor_user_id | text fk users | |
| scope | jsonb | `{"kind":"team","team_id":"..."}` |
| source_actor | jsonb | Phase 1 `Actor` |
| extracted | jsonb | `{name, description, principle, technical, kind}` |
| confidence | float4 | |
| status | text | `pending|accepted|rejected|expired` |
| reject_reason | text | nullable |
| created_at | tstz | |
| reviewed_at | tstz | nullable |

Migration `032_add_pattern_drafts.up.sql`。

### 2.3 Billing Gate

Team-scope draft 進 server 時:

```go
if draft.Scope.Kind == "team" {
    has_sub, err := billing.HasTeamSubscription(tx, draft.ActorUserID, draft.Scope.TeamID)
    if !has_sub {
        return 403 {
            "error": "need_team_subscription",
            "team_id": draft.Scope.TeamID,
            "upgrade_url": "https://mur.run/billing/teams",
        }
    }
}
```

Gateway 收到 403 → bot 回訊 Block Kit:「此訊息要學為 team 記憶,需訂閱 team plan。點此升級 → [按鈕]」。用戶升級後,下一則訊息照常進 team drafts。

**Personal scope 不經 billing**。

---

## 3. Client 端 Fetch & Accept Flow

### 3.1 `mur fetch`

Phase 1 已有 — 補在 server 側:`GET /api/v1/core/signals/pending` 回 `{signals: [...], drafts: [...]}`。mur-core `Inbox` 多處理一種 `Draft` 類別,寫入 `~/.mur/inbox/drafts/{id}.yaml`。

### 3.2 新 CLI 指令

```
mur drafts list              # 列出待審 drafts (最近 30 天)
mur drafts show <id>         # 秀完整內容
mur drafts accept <id>       # 批准 → 寫入 ~/.mur/patterns/{name}.yaml,走 YAML→LanceDB reindex
mur drafts reject <id> [--reason "..."]   # 拒絕 → POST back to server, future similar ↓ confidence
```

`accept` 跟 Phase 1 的 signal apply 邏輯走同一段:驗 scope、decay 初始化、lifecycle = Draft→Emerging(immediately, 因為已被人類 gate 過)。

### 3.3 Reject feedback loop

Client 傳 `POST /api/v1/core/drafts/{id}/reject` with reason;server:
1. 更新 `pattern_drafts.status = rejected, reject_reason = ...`
2. 計算該 `(actor_user_id, extracted.name_semantic_hash)` 未來 draft confidence 降權
3. 若同一名/近似內容連續被拒 N 次,加到個人 negative-filter 清單,下次 LLM 粗篩加在 prompt 裡

---

## 4. 變更清單

### mur (OSS) client
- ~~`mur-common/src/signal.rs` — `SignalKind::PatternDraft`,bump `SIGNAL_SCHEMA_VERSION = 2`。~~ **Amendment: unnecessary — Phase 1 already ships `SignalTarget::NewDraftPattern` + `SignalKind::NewPatternProposal`.** Reuse those; no schema bump.
- `mur-core/src/sync/inbox.rs` — 分流 `SignalTarget::NewDraftPattern` 信號到 `~/.mur/inbox/drafts/`。
- `mur-core/src/cmd/drafts.rs` — 新指令 `list | show | accept | reject`。
- `mur-core/src/main.rs` — 註冊 `drafts` 子指令。

### mur-server (closed)
- `migrations/032_add_pattern_drafts.{up,down}.sql`。
- `internal/models/draft.go`。
- `internal/store/postgres/drafts.go` — insert + fetch pending + mark reviewed。
- `internal/services/billing.go` — `HasTeamSubscription(userID, teamID)`(可能已存在,確認)。
- `internal/api/handlers/signals.go` — `/batch` 接收時,當 `target.kind == new_draft_pattern` → drafts store。
- `internal/api/handlers/drafts.go` — `/reject` endpoint。

### mur-commander (closed)
- `crates/engine/src/mur_sync/signal_emit.rs` — 新 `emit_draft_signal(outbox, pattern, origin_context, actor, scope)` — 建 `Signal { target: NewDraftPattern { payload }, kind: NewPatternProposal { origin_context } }`。
- `crates/gateway/src/unified_handler/chat_extract.rs` — 新檔,粗篩 LLM + scope decider + 寫 signal。
- `crates/gateway/src/unified_handler/*.rs` — 在 post-message hook 呼叫 `chat_extract::try_extract(...)`。

---

## 5. 測試計畫

- Gateway 單元:粗篩 LLM mock、scope decider truth table、fire-and-forget 失敗不影響主路徑。
- Server 整合 (real Postgres):
  - personal draft → insert OK
  - team draft with subscription → insert OK
  - team draft without subscription → 403 + no row
  - reject updates status + logs reason
- Client e2e (`scripts/golden-path-2.sh`):
  1. seed draft on server side
  2. `mur fetch` → 看到 drafts
  3. `mur drafts list` 列出
  4. `mur drafts accept <id>` → `~/.mur/patterns/<name>.yaml` 產生
  5. 二次 `mur fetch` → draft 消失
- Rollback 驗證:`migrate-down` 032 不破壞 031。

---

## 6. 風險 & 未決

- **粗篩 LLM 成本**:每則 chat 多花 1 次 LLM。mitigation: 本地快取 "已判 learnable=false" 的 message hash;cheap model (haiku-4.5) 為主。
- **Slack Block Kit override UX**:公開頻道詢問發言者時需要好的互動設計。Phase 2 先只發問,不阻塞,預設 Personal。
- **Reject feedback 抑制邊界**:防止惡意用戶一直 reject 把別人的 draft 壓低。目前 feedback 只影響同一 `actor_user_id` 的未來 drafts,不跨用戶。
- ~~**SignalKind v1 vs v2**:舊 client 送 v1 signal 過來,server 正常處理;新 client v2 有 Draft variant。保持向後相容。~~ **Amendment: moot — wire format unchanged at schema v1. Drafts carried via `SignalTarget::NewDraftPattern` which has been in the format since Phase 1.**

---

## 7. 推薦實作順序

> **Amendment (2026-04-23)**: Step 1 dropped — Phase 1's signal wire format already carries drafts. Re-numbered.

1. **Server drafts store + billing gate + handler** — 先只測 Personal scope。Smallest self-contained unit; unblocks client + gateway testing.
2. Client `mur drafts` CLI + inbox 分流 — 能手工 seed 驗 end-to-end。
3. Gateway chat_extract LLM pipeline + scope decider — 最複雜,靠前面 gate 住品質。
4. Team scope full flow + 403 UX。
5. golden-path-2.sh 驗整條線。

**預估**:2-3 PRs per repo (mur-server 2, mur 2, mur-commander 2), ~7-8 PRs total, ~1.5-2 weeks focused time (one less PR than original estimate since the schema bump step is unnecessary).
