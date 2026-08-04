<!-- Languages: [English](README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md) -->

# MUR Deep Research — 详细设计

> 由 MUR fleet 端到端自有的 native 深度研究。
> 于 **v2.45.0**(2026-07-10)发布,PR #663–#672。

一支 sandboxed 的 MUR agent 小队拆解问题、通过单一审计 gateway 研究实时网络、对每个论断做对抗式验证,最终收敛成一份**带引用、密码学可归属的报告**——全部在 MUR 自有的编排上,而非 host subagents。

- **规格:** `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`
- **Crate:** `mur-research-gateway` · **命令:** `mur-core/src/cmd/deep_research`

---

## §1 · 为什么要 native,而非 host subagents

Claude Code 内置的 deep-research 研究得很好——但它以 *host subagents* 运行。工作是 Claude 的,MUR 只是贴标签。Native 编排才让成果是 *MUR 的*,而且可证明。

- **MUR 编排拥有它。** 由 router 的每轮动态 DAG(`cmd/fleet/plan.rs`)驱动的一支 MUR agent fleet,才是真正在做研究的主体——不是 host spawn 完就遗忘的 subagent。
- **密码学 provenance。** 每个论断都由产出它的 worker 写入一条 Ed25519 签名的 channel(Unified Channel v3d-2,*peer-writes-own*)。“这个 agent 找到这个”变得可验证,而不是一句标注。
- **平台集成。** 真实的 per-token 预算、kill-switch、Commander governance、调度、长期记忆,全部沿用既有的 fleet/agent 机制——免费。

**非目标:** 比内置更并行。并发两边都有上限(约 `min(16, cores−2)`)。Native 赢在所有权、provenance、governance,而非原始速度。

---

## §2 · 架构——三层,一个 choke point

Workers **不持有** egress,且是唯一的可注入面。只有确定性、无 LLM 的 gateway 能触网——而且是在强制的 kernel sandbox 内。

```
┌─────────────────────── fleet "deep-research" ───────────────────────┐
│  router (mur)  — 每轮动态 DAG (plan.rs)                              │
│  done_when: marker:RESEARCH_COMPLETE · budget-usd · deadline · kill  │
└──────────────────────────────┬──────────────────────────────────────┘
             │ channel/delegate（Ed25519 签名回复）
             ▼
┌──────────────── worker × k （entitlements: restricted） ────────────┐
│  model_ref → 真实 LLM · 挂载 1 个 MCP: research-gateway            │
│  工具: search · fetch      内置工具全 deny                          │
└──────────────────────────────┬──────────────────────────────────────┘
             │ MCP: search(query) / fetch(url)   — 只读动词
             ▼
┌────────── research-gateway（Rust,无 LLM,broad-audited egress）─────┐
│  SSRF guard · tier ladder 1→2→3 · content budget · 每次调用 audit    │
└──────────────────────────────────────────────────────────────────────┘
       强制于: B1 kernel sandbox + loopback egress proxy
```

被 prompt 注入的 worker 顶多只能请 gateway `fetch` 一个 URL——有记录、被 SSRF 拦查、且无法向任意主机 POST 任意数据。API 是 `fetch`,不是“开一个 socket”。

---

## §3 · 动态流程——decompose → research → verify → synthesize

Router 在每轮 `mur fleet run --loop` 产出一份新的 DAG;子问题数量在 decompose 时决定。loop 一直跑到收敛 marker 出现在它自己一行。

1. **Decompose** — router 把问题拆成子问题(可能 100+),写进 channel 作为工作队列。
2. **Research(×N 轮)** — router 把一批指派给 workers(受 `max_concurrency` 限制)。每个 worker `search()`、`fetch()` 最相关的来源,提取**每个都绑定 URL + 佐证引文的论断**,以签名回复写回。重复到队列清空。
3. **Verify(3 票对抗)** — 每个论断派给三个 workers,各用一个不同的反驳镜头:正确性 / 来源独立性 / 时效性。2 票确认才存活;否则丢弃(fail-safe = 丢弃)。
4. **Synthesize → 收敛** — router 把确认的论断折成一份引用报告,并在自己一行输出 `RESEARCH_COMPLETE`。结构化的 `done_when: marker:…` 确定性收敛——不需额外 LLM 调用,而且只是引用 marker 的散文无法假收敛。

**免费继承:** 带*真实* per-token 记账的 `--budget-usd` · `mur fleet stop` kill-switch · Commander kill/budget hooks(fail-closed)· 每个论断的签名 channel provenance · iteration-cap / deadline / 卡住检测 guards。

---

## §4 · research-gateway

一个随 MUR 发布、依赖轻量的小型 Rust MCP server。两个只读动词;固定、由代码驱动的 tier ladder(不由 LLM 决定 tier);每一 byte egress 都被治理。

```
search(query, limit?)  →  [{title, url, snippet}]
fetch(url, render?)    →  {url, status, title, text, tier}
```

### 升级阶梯(确定性代码,不是 skill)

| Tier | 引擎 | 说明 |
|------|------|------|
| **1 · http** | `reqwest` GET | 默认、最省。**Search 也走这里:** 通过同一条 proxy-honoring 路径 GET DuckDuckGo 的服务端渲染 HTML endpoint(需要浏览器风格 User-Agent,否则 DDG 回 HTTP 202),所以 search 在浏览器无法 spawn 的 sandbox 下也能运作。 |
| **2 · lightpanda** | `agent-browser --engine lightpanda` | JS 渲染页。`--args ""` 是强制的——Chrome stealth flags 会弄坏 Lightpanda。每次 fetch 用独立 `--session` id,并发 fetch 不共用 cookie jar。 |
| **3 · chrome** | `agent-browser --engine chrome` | 反爬虫 / 截图。stealth flags 以单一 `--args "<逗号分隔>"` 值传递(bare argv 会被当成子命令)。render 的 `fetch` 在 `Http` 失败*或*空渲染时升级 lightpanda → chrome;`chrome:true` 强制 tier 3。 |

- **SSRF guard(硬性、不可配置)** — 拒绝任何解析 IP 为 private / link-local / loopback / unique-local 的 URL,每个 tier 都筛查。浏览器 tier 的 guard 与 `deny_hosts` **在 gateway 代码 spawn 前**强制执行——proxy 看不到浏览器子进程自己的连接。
- **Content budget** — 单一 5 MB 页面会撑爆 worker 的 context。`fetch` 把返回文本截到 `max_fetch_chars`(默认 50 000;`0` 停用),在 codepoint 边界截断并加标记。5 MB body cap 限制传输/内存;`max_fetch_chars` 限制 context。search snippet 不截。
- **搜索可靠性** — N 个并发 worker 时 DDG 以 202 挑战页限流。`search` 在 202 时 retry,采指数 backoff + **query 派生 jitter**——不同子问题错开重试,不再同步再爆一次。
- **URL 级 audit** — 每次调用把 `{worker, url, tier, outcome}` 记录到 channel 与 telemetry。报告的每个引用都能对到一条 gateway audit 记录。

### 渲染引擎(实验性,opt-in)

通过 `MUR_RESEARCH_RENDER_ENGINE` 环境变量选择(或 `~/.mur/config.yaml` 中的 `research_gateway.render_engine:`);此处显式设置的值始终覆盖 auto-detect。自动检测(未设置 env/YAML 时):**当 `obscura` 与 `obscura-worker` 两个可执行文件都安装在 `~/.mur/aura/` 时用 `obscura`,否则用 `agent-browser`**(2026-07-10 正面对比:obscura 能渲染真实内容,包括纯 JS 页面,并能在 worker sandbox 下运行;agent-browser/Lightpanda 只返回 title-only 的空壳,且被 sandbox 拒绝)。

- **`agent-browser`** — 如上所述的 Lightpanda(tier 2)与 Chrome(tier 3)。obscura 未安装时的自动检测 fallback。
- **`obscura`** — 内嵌 V8、自包含。安装方式:把平台 tarball 解压到 `~/.mur/aura/`,保留 `obscura` 与 `obscura-worker` 两个可执行文件——自动检测便会自动选中它。单一渲染路径;没有 tier-2/3 升级。**优势:** egress 是 **proxy-governed** 的——通过 tier 1 的 loopback proxy 走(`obscura fetch <url> … --proxy http://<token>:@127.0.0.1:<port>`),消除浏览器 tier 的 egress 治理缺口。

---

## §5 · 安全模型——sandbox、同意、工具策略

Gateway 在强制 kernel sandbox 下运行,默认无 egress。访问权通过恰好一个明确同意步骤授予;worker 的工具策略被塑形,使 headless turn 既无法逃逸也不会卡住。

| 控制 | 机制 | 边界 |
|------|------|------|
| **Egress 授权** | 每 worker `mcp set-network research-gateway --broad-audited`——一次操作者同意,记录为 `EgressAuthorization`。 | Fleet 创建**绝不**隐式开 egress。“这是研究 fleet”不算同意。 |
| **Egress proxy** | 一个 loopback CONNECT proxy;每个 worker 的 gateway 子进程拿到一个依其 allow/deny 策略 scoped 的 `HTTPS_PROXY` token。 | 必须在 sandbox seal **之前**启动,并把 port 以 loopback-only 规则刻进 profile——否则子进程拨不到它。 |
| `mcp__research-gateway__*` | **allow** | 预先核准,让 headless fleet turn 跳过 HITL gate(无人可答 → 300 秒 timeout → 失败)。它本身不授予任何 egress。 |
| `bash` · `read_file` · `write_file` · `edit_file` | **deny** | 研究 turn 若伸手去用某个内置工具,会在同一个不可答的 gate 上死掉。deny → 不 advertise → 永不被调用。 |
| 其余一切 | **ask** | 上述两条规则以外的工具保留 fail-closed 默认。 |
| **Provenance** | 每个 worker 把自己的回复以 `Agent{self}` 事件写入,Ed25519 签名(v3d-2 peer-writes-own),fold 时逐 actor 验证。 | Router 不再代 worker 签名——归属是 worker 自己的密钥。 |
| **Export 安全** | `.fleet` import 把 broad-audited 降级为 `inherit` 并清除授权。 | 分享的 deep-research fleet 在本地重新授权前零 egress。 |

**Advisory-enforcement 的诚实。** Tier 1 honor proxy;tier 2/3 浏览器子进程可能不——以全局 gateway URL audit 缓解,且如实记录而非过度宣称。通过 opt-in 的 `render_engine: obscura`,这个缺口会被补上:obscura 把所有 egress 都通过 tier 1 使用的同一条 loopback proxy 走(以 `--proxy` 携带 gateway 的凭证),使 render tier 像 tier 1 一样 proxy-governed。滴水不漏的封装 = 未来 Phase-3 sbpl pin-to-proxy,届时只 pin 这一个 gateway。

---

## §6 · 操作——provision、grant、run

```bash
# 1 · 创建 k 个 restricted worker,各挂载 gateway,
#     并在同一个同意步骤授予 broad-audited egress
mur deep-research provision --count 4 --model claude_haiku --grant-egress --yes
#   tool policy: mcp__research-gateway__* → allow
#   tool policy: bash, read_file, write_file, edit_file → deny
#   Updated egress policy for 'research-gateway'.   # × 每个 worker

# 2 · 创建 fleet(router = mur),设定 done marker + 预算
mur fleet create deep-research \
    --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 \
    --goal "…你的研究问题…"
#   fleet.yaml loop: { max_iterations, budget_usd, done_when: marker:RESEARCH_COMPLETE }

# 3 · 跑 guarded loop——decompose → research → verify → synthesize
mur deep-research run deep-research --max-iterations 4 --deadline 30m
#   fleet 'deep-research' loop stopped after 1 iteration (~$6.14 spent): Converged
```

- **结果落在哪** — 每个 worker 的引用回复是 `~/.mur/channels/fleet-deep-research/events.jsonl` 里的签名事件。`~/.mur/index/channels/` 的 SQLite read-model 是可重建的投影。每个引用都能对到一条 gateway audit 记录。
- **Config knobs(无硬编值)** — `MUR_RESEARCH_MAX_FETCH_CHARS`、`…_SEARCH_LIMIT`、`…_SEARCH_ENDPOINT`、`…_TIMEOUT_SECS`、`…_LIGHTPANDA_PATH`、`…_DENY_HOSTS`——env 或 `~/.mur/config.yaml` 的 `research_gateway:`,绝不用字面值。

---

## §7 · 实现真相——干净的设计要真的实现付出了什么

规格的四箱架构是对的。但要让一支 fleet 真的收敛——sandboxed、headless、密码学可归属——浮现了九个各自独立的修复,每个都由 live operator 验证揪出,而非靠读代码。

| PR | 领域 | 修复 |
|----|------|------|
| **#663** | feat | **Native deep-research 核心**——gateway crate、fleet 接线、router/worker/verify skills。 |
| **#664** | HITL | **预核准 gateway 工具**——headless turn 没人回答 `tool/approval_needed` → 300 秒 timeout → 失败。provision 时盖 `mcp__research-gateway__* → allow`。 |
| **#665** | G1 · sandbox | **Pre-seal egress proxy**——proxy 在 sandbox seal *之后*才启动、port 从没刻进去 → 每个 scoped 授权 dead on arrival。改到 seal 前启动;刻一条 loopback-only port 规则。 |
| **#666** | G3 · channel | **授权 channel read-model 目录**——签名回复落进 `events.jsonl` 但 SQLite refresh 撞只读 DB → 假失败。授权 `index/channels`;post-append refresh 改非致命。 |
| **#667** | G2 · search | **无浏览器搜索 + 可用的 chrome**——search spawn 了 sandbox 禁的 `agent-browser`。改走 tier-1 HTTP;修 chrome `--args` 传法让 render fallback 真的启动。 |
| **#668** | egress | **`Proxy-Authorization` 大小写不敏感**——*总 egress 解锁*。proxy 大小写敏感比对 auth header,但 hyper 送小写 → token 掉了 → *每个* CONNECT 被拒。用 `nc` proxy trace 现场抓到。 |
| **#669** | content | **Fetch 内容预算**——单一 5 MB 页面撑爆 worker context(`anthropic 400: prompt too long`)。返回文本截到 `max_fetch_chars`。 |
| **#670** | convergence | **Deny worker 内置工具**——*收敛解锁*。研究 turn 伸手去用 `bash`(默认 `ask`)→ 不可答的 HITL gate → turn 失败 → 空回复 → step 失败。deny 内置工具;模型根本看不到它们。用 raw `channel/delegate` socket probe 钉死根因。 |
| **#671** | reliability | **DDG 202 retry + jitter**——并发 worker 触发 DuckDuckGo 限流。202 时用 backoff + query 派生 jitter 重试;报告从“access limited”变成每个 16–19 个引用。 |
| **#672** | G4 · cleanup | **加载时跳过非 skill 目录**——`skills/` 下的 fleet run-ledger(`fleet:<name>/`)每次开机喷约 14 个 name 验证警告。`load_all` 跳过无 manifest 的目录——只在注入路径,maturity sweep 不受影响。 |

**端到端验证:** 全新 provision → `run`:0 step failures、所有 worker 写了**签名**回复(Ed25519 provenance)、loop 停在 **Converged**、router 产出一份引用的 Ollama / LM Studio / LocalAI 比较——在 4-worker 并发下,完全在 kernel sandbox 内。

---

*本设计文档反映 v2.45.0 的发布实现。“滴水不漏”的 egress 封装(sbpl pin-to-proxy)与一级搜索 API 仍是刻意的 Phase-3 后续。*
