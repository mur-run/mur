<!-- Languages: [English](README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md) -->

# MUR Deep Research — 詳細設計

> 由 MUR fleet 端到端自有的 native 深度研究。
> 於 **v2.45.0**(2026-07-10)出貨,PR #663–#672。

一個 sandboxed 的 MUR agent 小隊拆解問題、透過單一稽核 gateway 研究即時網路、對每個聲明做對抗式驗證,最終收斂成一份**有引用、密碼學可歸屬的報告**——全部在 MUR 自有的編排上,而非 host subagents。

- **規格:** `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`
- **Crate:** `mur-research-gateway` · **指令:** `mur-core/src/cmd/deep_research`

---

## §1 · 為什麼要 native,而非 host subagents

Claude Code 內建的 deep-research 研究得很好——但它以 *host subagents* 執行。工作是 Claude 的,MUR 只是貼標籤。Native 編排才讓成果是 *MUR 的*,而且可證明。

- **MUR 編排擁有它。** 由 router 的每輪動態 DAG(`cmd/fleet/plan.rs`)驅動的一支 MUR agent fleet,才是真正在做研究的主體——不是 host spawn 完就遺忘的 subagent。
- **密碼學 provenance。** 每個聲明都由產出它的 worker 寫入一條 Ed25519 簽名的 channel(Unified Channel v3d-2,*peer-writes-own*)。「這個 agent 找到這個」變成可驗證,而不是一句標註。
- **平台整合。** 真實的 per-token 預算、kill-switch、Commander governance、排程、長期記憶,全部沿用既有的 fleet/agent 機制——免費。

**非目標:** 比內建更平行。併發兩邊都有上限(約 `min(16, cores−2)`)。Native 贏在所有權、provenance、governance,不是原始速度。

---

## §2 · 架構——三層,一個 choke point

Workers **不持有** egress,且是唯一的可注入面。只有確定性、無 LLM 的 gateway 能觸網——而且是在強制的 kernel sandbox 內。

```
┌─────────────────────── fleet "deep-research" ───────────────────────┐
│  router (mur)  — 每輪動態 DAG (plan.rs)                              │
│  done_when: marker:RESEARCH_COMPLETE · budget-usd · deadline · kill  │
└──────────────────────────────┬──────────────────────────────────────┘
             │ channel/delegate（Ed25519 簽名回覆）
             ▼
┌──────────────── worker × k （entitlements: restricted） ────────────┐
│  model_ref → 真實 LLM · 掛載 1 個 MCP: research-gateway            │
│  工具: search · fetch      內建工具全 deny                          │
└──────────────────────────────┬──────────────────────────────────────┘
             │ MCP: search(query) / fetch(url)   — 唯讀動詞
             ▼
┌────────── research-gateway（Rust,無 LLM,broad-audited egress）─────┐
│  SSRF guard · tier ladder 1→2→3 · content budget · 每次呼叫 audit    │
└──────────────────────────────────────────────────────────────────────┘
       強制於: B1 kernel sandbox + loopback egress proxy
```

被 prompt 注入的 worker 頂多只能請 gateway `fetch` 一個 URL——有記錄、被 SSRF 攔查、且無法對任意主機 POST 任意資料。API 是 `fetch`,不是「開一個 socket」。

---

## §3 · 動態流程——decompose → research → verify → synthesize

Router 在每輪 `mur fleet run --loop` 產出一份新的 DAG;子問題數量在 decompose 時決定。loop 一直跑到收斂 marker 出現在它自己一行。

1. **Decompose** — router 把問題拆成子問題(可能 100+),寫進 channel 作為工作佇列。
2. **Research(×N 輪)** — router 把一批指派給 workers(受 `max_concurrency` 限制)。每個 worker `search()`、`fetch()` 最相關的來源,萃取**每個都綁定 URL + 佐證引文的聲明**,以簽名回覆寫回。重複到佇列清空。
3. **Verify(3 票對抗)** — 每個聲明派給三個 workers,各用一個不同的反駁鏡頭:正確性 / 來源獨立性 / 時效性。2 票確認才存活;否則丟棄(fail-safe = 丟棄)。
4. **Synthesize → 收斂** — router 把確認的聲明折成一份引用報告,並在自己一行輸出 `RESEARCH_COMPLETE`。結構化的 `done_when: marker:…` 確定性收斂——不需額外 LLM 呼叫,而且只是引用 marker 的散文無法假收斂。

**免費繼承:** 帶*真實* per-token 記帳的 `--budget-usd` · `mur fleet stop` kill-switch · Commander kill/budget hooks(fail-closed)· 每個聲明的簽名 channel provenance · iteration-cap / deadline / 卡住偵測 guards。

---

## §4 · research-gateway

一個隨 MUR 出貨、依賴輕量的小型 Rust MCP server。兩個唯讀動詞;固定、由程式碼驅動的 tier ladder(不由 LLM 決定 tier);每一 byte egress 都被治理。

```
search(query, limit?)  →  [{title, url, snippet}]
fetch(url, render?)    →  {url, status, title, text, tier}
```

### 升級階梯(確定性程式碼,不是 skill)

| Tier | 引擎 | 說明 |
|------|------|------|
| **1 · http** | `reqwest` GET | 預設、最省。**Search 也走這裡:** 透過同一條 proxy-honoring 路徑 GET DuckDuckGo 的伺服器渲染 HTML endpoint(需要瀏覽器風格 User-Agent,否則 DDG 回 HTTP 202),所以 search 在瀏覽器無法 spawn 的 sandbox 下也能運作。 |
| **2 · lightpanda** | `agent-browser --engine lightpanda` | JS 渲染頁。`--args ""` 是強制的——Chrome stealth flags 會弄壞 Lightpanda。每次 fetch 用獨立 `--session` id,併發 fetch 不共用 cookie jar。 |
| **3 · chrome** | `agent-browser --engine chrome` | 反爬蟲 / 截圖。stealth flags 以單一 `--args "<逗號分隔>"` 值傳遞(bare argv 會被當成子命令)。render 的 `fetch` 在 `Http` 失敗*或*空渲染時升級 lightpanda → chrome;`chrome:true` 強制 tier 3。 |

- **SSRF guard(硬性、不可設定)** — 拒絕任何解析 IP 為 private / link-local / loopback / unique-local 的 URL,每個 tier 都篩查。瀏覽器 tier 的 guard 與 `deny_hosts` **在 gateway 程式碼 spawn 前**強制執行——proxy 看不到瀏覽器子行程自己的連線。
- **Content budget** — 單一 5 MB 頁面會撐爆 worker 的 context。`fetch` 把回傳文字截到 `max_fetch_chars`(預設 50 000;`0` 停用),在 codepoint 邊界截斷並加標記。5 MB body cap 限制傳輸/記憶體;`max_fetch_chars` 限制 context。search snippet 不截。
- **搜尋可靠度** — N 個併發 worker 時 DDG 以 202 挑戰頁限流。`search` 在 202 時 retry,採指數 backoff + **query 衍生 jitter**——不同子問題錯開重試,不再同步再爆一次。
- **URL 級 audit** — 每次呼叫把 `{worker, url, tier, outcome}` 記錄到 channel 與 telemetry。報告的每個引用都能對到一筆 gateway audit 記錄。

### 渲染引擎(實驗性,opt-in)

透過 `MUR_RESEARCH_RENDER_ENGINE` 環境變數選擇(或 `~/.mur/config.yaml` 中的 `research_gateway.render_engine:`);此處明確設定的值永遠覆蓋 auto-detect。自動偵測(未設定 env/YAML 時):**當 `obscura` 與 `obscura-worker` 兩個執行檔都安裝在 `~/.mur/aura/` 時用 `obscura`,否則用 `agent-browser`**(2026-07-10 正面比較:obscura 能渲染真實內容,包含純 JS 頁面,且能在 worker sandbox 下運作;agent-browser/Lightpanda 只回傳 title-only 的空殼,且被 sandbox 拒絕)。

- **`agent-browser`** — 如上述的 Lightpanda(tier 2)與 Chrome(tier 3)。obscura 未安裝時的自動偵測 fallback。
- **`obscura`** — 內嵌 V8、自成一體。安裝方式:把平台 tarball 解壓到 `~/.mur/aura/`,保留 `obscura` 與 `obscura-worker` 兩個執行檔——自動偵測就會自動選中它。單一渲染路徑;沒有 tier-2/3 升級。**優勢:** egress 是 **proxy-governed**——透過 tier 1 的 loopback proxy 走(`obscura fetch <url> … --proxy http://<token>:@127.0.0.1:<port>`),消除瀏覽器 tier 的 egress 治理缺口。

---

## §5 · 安全模型——sandbox、同意、工具政策

Gateway 在強制 kernel sandbox 下執行,預設無 egress。存取權透過恰好一個明確同意步驟授予;worker 的工具政策被塑形,使 headless turn 既無法逃逸也不會卡住。

| 控制 | 機制 | 邊界 |
|------|------|------|
| **Egress 授權** | 每 worker `mcp set-network research-gateway --broad-audited`——一次操作者同意,記錄為 `EgressAuthorization`。 | Fleet 建立**絕不**隱式開 egress。「這是研究 fleet」不算同意。 |
| **Egress proxy** | 一個 loopback CONNECT proxy;每個 worker 的 gateway 子行程拿到一個依其 allow/deny 政策 scoped 的 `HTTPS_PROXY` token。 | 必須在 sandbox seal **之前**啟動,並把 port 以 loopback-only 規則刻進 profile——否則子行程撥不到它。 |
| `mcp__research-gateway__*` | **allow** | 預先核准,讓 headless fleet turn 跳過 HITL gate(無人可答 → 300 秒 timeout → 失敗)。它本身不授予任何 egress。 |
| `bash` · `read_file` · `write_file` · `edit_file` | **deny** | 研究 turn 若伸手去用某個內建工具,會在同一個不可答的 gate 上死掉。deny → 不 advertise → 永不被呼叫。 |
| 其餘一切 | **ask** | 上述兩條規則以外的工具保留 fail-closed 預設。 |
| **Provenance** | 每個 worker 把自己的回覆以 `Agent{self}` 事件寫入,Ed25519 簽名(v3d-2 peer-writes-own),fold 時逐 actor 驗證。 | Router 不再代 worker 簽名——歸屬是 worker 自己的金鑰。 |
| **Export 安全** | `.fleet` import 把 broad-audited 降級為 `inherit` 並清除授權。 | 分享的 deep-research fleet 在本地重新授權前零 egress。 |

**Advisory-enforcement 的誠實。** Tier 1 honor proxy;tier 2/3 瀏覽器子行程可能不——以全域 gateway URL audit 緩解,且如實記錄而非過度宣稱。透過 opt-in 的 `render_engine: obscura`,這個缺口會被補上:obscura 把所有 egress 都透過 tier 1 用的同一條 loopback proxy 走(以 `--proxy` 帶上 gateway 的憑證),讓 render tier 像 tier 1 一樣 proxy-governed。滴水不漏的封裝 = 未來 Phase-3 sbpl pin-to-proxy,屆時只 pin 這一個 gateway。

---

## §6 · 操作——provision、grant、run

```bash
# 1 · 建立 k 個 restricted worker,各掛載 gateway,
#     並在同一個同意步驟授予 broad-audited egress
mur deep-research provision --count 4 --model claude_haiku --grant-egress --yes
#   tool policy: mcp__research-gateway__* → allow
#   tool policy: bash, read_file, write_file, edit_file → deny
#   Updated egress policy for 'research-gateway'.   # × 每個 worker

# 2 · 建立 fleet(router = mur),設定 done marker + 預算
mur fleet create deep-research \
    --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 \
    --goal "…你的研究問題…"
#   fleet.yaml loop: { max_iterations, budget_usd, done_when: marker:RESEARCH_COMPLETE }

# 3 · 跑 guarded loop——decompose → research → verify → synthesize
mur deep-research run deep-research --max-iterations 4 --deadline 30m
#   fleet 'deep-research' loop stopped after 1 iteration (~$6.14 spent): Converged
```

- **結果落在哪** — 每個 worker 的引用回覆是 `~/.mur/channels/fleet-deep-research/events.jsonl` 裡的簽名事件。`~/.mur/index/channels/` 的 SQLite read-model 是可重建的投影。每個引用都能對到一筆 gateway audit 記錄。
- **Config knobs(無硬編值)** — `MUR_RESEARCH_MAX_FETCH_CHARS`、`…_SEARCH_LIMIT`、`…_SEARCH_ENDPOINT`、`…_TIMEOUT_SECS`、`…_LIGHTPANDA_PATH`、`…_DENY_HOSTS`——env 或 `~/.mur/config.yaml` 的 `research_gateway:`,絕不用字面值。

---

## §7 · 實作真相——乾淨的設計要真的實現付出了什麼

規格的四箱架構是對的。但要讓一支 fleet 真的收斂——sandboxed、headless、密碼學可歸屬——浮現了九個各自獨立的修復,每個都由 live operator 驗證揪出,不是靠讀程式碼。

| PR | 領域 | 修復 |
|----|------|------|
| **#663** | feat | **Native deep-research 核心**——gateway crate、fleet 接線、router/worker/verify skills。 |
| **#664** | HITL | **預核准 gateway 工具**——headless turn 沒人回答 `tool/approval_needed` → 300 秒 timeout → 失敗。provision 時蓋 `mcp__research-gateway__* → allow`。 |
| **#665** | G1 · sandbox | **Pre-seal egress proxy**——proxy 在 sandbox seal *之後*才啟動、port 從沒刻進去 → 每個 scoped 授權 dead on arrival。改到 seal 前啟動;刻一條 loopback-only port 規則。 |
| **#666** | G3 · channel | **授權 channel read-model 目錄**——簽名回覆落進 `events.jsonl` 但 SQLite refresh 撞唯讀 DB → 假失敗。授權 `index/channels`;post-append refresh 改非致命。 |
| **#667** | G2 · search | **無瀏覽器搜尋 + 可用的 chrome**——search spawn 了 sandbox 禁的 `agent-browser`。改走 tier-1 HTTP;修 chrome `--args` 傳法讓 render fallback 真的啟動。 |
| **#668** | egress | **`Proxy-Authorization` 大小寫不敏感**——*總 egress 解鎖*。proxy 大小寫敏感比對 auth header,但 hyper 送小寫 → token 掉了 → *每個* CONNECT 被拒。用 `nc` proxy trace 現場抓到。 |
| **#669** | content | **Fetch 內容預算**——單一 5 MB 頁面撐爆 worker context(`anthropic 400: prompt too long`)。回傳文字截到 `max_fetch_chars`。 |
| **#670** | convergence | **Deny worker 內建工具**——*收斂解鎖*。研究 turn 伸手去用 `bash`(預設 `ask`)→ 不可答的 HITL gate → turn 失敗 → 空回覆 → step 失敗。deny 內建工具;模型根本看不到它們。用 raw `channel/delegate` socket probe 釘死根因。 |
| **#671** | reliability | **DDG 202 retry + jitter**——併發 worker 觸發 DuckDuckGo 限流。202 時用 backoff + query 衍生 jitter 重試;報告從「access limited」變成每個 16–19 個引用。 |
| **#672** | G4 · cleanup | **載入時跳過非 skill 目錄**——`skills/` 下的 fleet run-ledger(`fleet:<name>/`)每次開機噴約 14 個 name 驗證警告。`load_all` 跳過無 manifest 的目錄——只在注入路徑,maturity sweep 不受影響。 |

**端到端驗證:** 全新 provision → `run`:0 step failures、所有 worker 寫了**簽名**回覆(Ed25519 provenance)、loop 停在 **Converged**、router 產出一份引用的 Ollama / LM Studio / LocalAI 比較——在 4-worker 併發下,完全在 kernel sandbox 內。

---

*本設計文件反映 v2.45.0 的出貨實作。「滴水不漏」的 egress 封裝(sbpl pin-to-proxy)與一級搜尋 API 仍是刻意的 Phase-3 後續。*
