# MuR Skill/Notes/Workflow — 市場與未來價值分析

> **Status:** Analysis (策略研究,非 spec) | **Date:** 2026-05-29
> **Subjects:** `2026-05-28-mur-workflow-engine-design-v2.md`、`2026-05-28-mur-notes-design.md`
> **Lens:** 在「模型會持續變強」前提下,評估這兩個轉換設計的未來價值、競品、風險。
> **後續修訂:** 第 1/2/6 點已轉成 spec 修訂,見兩份 spec 末的 *Amendment 2026-05-29*。

## 結論先行

這兩個設計踩在 2026 年最對的一條縫上——對的不是「萃取」也不是「筆記」,而是
**lifecycle(生命週期治理)+ 確定性執行 + provenance(簽章/信任)** 這三層。
市場研究幾乎一面倒印證:**「把 skill 累積起來」大家都會,「管理 skill 的生老病死」
幾乎沒人做**,而這正是 mur 把 Pattern/Workflow/Note 收斂成一個 `Skill` 物件後白送的能力。

紅旗一條:**自動萃取的 skill,學界實測加值是 +0.0pp**(SkillsBench)。所以 v2 的價值不在
LLM judge,而在它後面那條 human-in-loop + lifecycle 的鏈。

## Q1:這套轉換設計有沒有未來市場價值?

**有,且是逆模型週期(模型越強越值錢)那種——但要看押哪一層。**

| 層 | 模型變強後 | 判斷 |
|---|---|---|
| 萃取 / LLM judge gate+extract | 被商品化;且「臨場重生成」會侵蝕「存起來再用」 | ⚠️ 護城河最淺,是成本項不是差異化 |
| 確定性 DAG executor + run-ledger | 不會過時。LLM 推論天生非確定性,審計/合規/可重現永遠需要 deterministic code | ✅ 耐久 |
| Lifecycle 治理 + 簽章/信任 | 越來越值錢。模型越強→自動產 skill 越多→library drift 越嚴重→治理需求越大 | ✅✅ 真正的護城河 |

模型變強的三個論點:

1. **確定性層不會消失,只會下沉。** 2026 共識是 *agent 推理、deterministic code 執行*,
   混合架構吃掉約 80% 企業場景(GitHub agentic workflows = agent 觸發 + GitHub Actions 執行)。
   數學約束:LLM 無法「相同輸入→相同輸出」,需可重現/可審計的任務永遠需要確定性執行。
2. **模型越強,lifecycle 越關鍵(最強論證)。** *Library Drift*(arXiv 2605.19576,約一週前)+
   SkillsBench:20+ 個 self-evolving skill 系統「versioning / conflict detection / deprecation
   幾乎全被忽略」;無品質閘門地累積會劣化檢索、注入過時指引,讓 agent 跌回甚至低於
   no-skill baseline。模型越強、自動產 skill 越快,drift 越快。mur 的 `next_state()`
   promote/demote/archive + decay + Broken fast-path 正是這篇說「大家都缺」的東西。逆週期。
3. **威脅:臨場重生成。** 模型強到能即時推出三步 workflow 時,「萃取並存」對簡單任務失去意義。
   → mur 該押**長程、多步、必須可重現/需審計**的任務,那裡重生成成本與風險都高。

> 一句話:**萃取是入口,不是價值;價值是入口之後那條「治理 + 確定性執行」的鏈。**

## Q2:競品比較 + 風險

### 戰場 A:Agent Memory / Skill(Workflow v2)

| 競品 | 比 mur 強 | mur 的位置 |
|---|---|---|
| **Mem0** | 41k stars、14M 下載、AWS Agent SDK 獨家記憶供應商;ADD/UPDATE/DELETE/NOOP 萃取成熟 | 分發完敗;但無 lifecycle 狀態機、無確定性執行、無簽章 |
| **Letta / Zep / Cognee** | 生產級、有融資、長程記憶/時序知識圖成熟 | 它們是 *agent runtime 記憶*,非 *人為策展 + 可執行 + 可分享* 的知識物件;niche 不同 |
| **Claude/OpenAI Agent Skills(SKILL.md)** | 數週內成跨廠標準,OpenAI 也採用;progressive disclosure、~100 token/skill | mur 收斂到 SKILL.md round-trip 是對的(搭順風車),但平台隨時可在標準上加 lifecycle 把你吃掉 |
| **AWM(CMU/MIT, ICML 2025)** | session→workflow 萃取的學術原型,已驗證方向 | 代表「萃取」非新事;mur 新意只在「萃取+確定性執行+lifecycle+簽章+分享」統一在一個物件 |

**戰場 A 最大風險:不是技術,是分發與被平台吸收。** 反制唯一路:當那個**跨模型、local-first、
可審計的「agent 知識的 Git」**——平台因鎖定誘因不會走這方向。

### 戰場 B:PKM / Notes

| 競品 | 比 mur 強 | mur 的位置 |
|---|---|---|
| **Smart Connections**(853k 下載)/ **Obsidian Copilot** | 巨大 UX 與分發領先 | 是「對既有筆記問答的搜尋框」,不能建立/交叉引用/標記矛盾 |
| **Mem ($12/mo)** | 自組織、chat-on-your-notes | 無正式 maturity lifecycle |
| **BrainDB** | 5,420+ 生產記憶、矛盾偵測、30 天半衰 | decay 只用於排序,**沒有 promote/demote/archive 狀態機**——最接近但缺的正是核心 |
| **Tana / Logseq / RemNote** | Supertag schema、spaced repetition、block model | 不同方法論,無生命週期 |

**好消息:** 搜遍 2026 PKM 市場,**找不到任何競品有形式化的「筆記成熟度生命週期」**。真空。
市場:$1.65B→$6.15B(30.3% CAGR)。痛點(digital graveyard、維護成本壓垮系統)對得上。

**風險:** (1) 模型原生記憶 + 超長 context 吃掉 casual PKM 需求;(2) Obsidian plain-text/local
護城河極深,`export --obsidian` 把 mur 定位成附加層而非主場。

## Q3:升級建議(已轉成 spec 修訂者標註)

1. **把賭注從「萃取」搬到「治理」。** SkillsBench:LLM 自動產 skill +0.0pp,人為策展 +16.2pp。
   解釋了 Pattern(`injection_count==0`)為何死——全行業現象。**v2 LLM judge 別過度自動化**;
   保留 `[Accept & edit]`、manual `mur-in/out`;P5b DBSCAN 降為「建議」非「萃取」。→ **已轉 spec 修訂**
2. **Library Drift 防治做成顯性賣點 + contradiction detection 前移。** `next_state` + Broken
   fast-path 已有;補 conflict detection(BrainDB 已驗證)。「主動告訴你哪兩條知識矛盾、哪條該退場」
   是 Smart Connections/Copilot 做不到、論文說全行業缺的。→ **已轉 spec 修訂**
3. **強化 provenance/簽章對抗 AI 洪水。** 模型越強→AI 生成 skill 越泛濫→「誰簽的、跑過幾次成功」越稀缺。
   平台不會做(它們要你信任黑盒)。
4. **守住 category-agnostic 基座。** Retrievable trait + 統一 `events.jsonl` 讓未來新 projection
   白嫖 lifecycle。別讓 category-private 邏輯滲回 scorer/evolve。
5. **找尖銳 ICP。** 楔子:「跑多 agent/多模型、且需可審計知識的個人開發者與小團隊」——
   local-first + 跨模型中立 + lifecycle 是平台因鎖定誘因不會做的交集。
6. **修 Notes「無失敗」退化。** retrieval 永遠成功 → `success_rate==1.0` → 升級退化成「次數+年齡」,
   demotion 只靠 decay。在模型原生記憶競爭下是弱點。引入負向信號(dismiss / 未採用 / 被 supersede)。
   → **已轉 spec 修訂**

## 一句話總評

選對了未來十年不會被模型吃掉的那一層(治理 + 確定性執行 + 信任),架構收斂也是漂亮的長期複利。
技術判斷高分。真正風險在**分發**——贏的唯一路徑是做成「跨模型、local-first、可審計的 agent 知識 Git」。
並且:**少押全自動萃取(+0.0pp),多押人機協作的策展與生命週期治理(+16.2pp)。**

## Sources

- Library Drift (arXiv 2605.19576) — https://arxiv.org/html/2605.19576
- EvoSkills / SkillsBench (arXiv 2604.01687) — https://arxiv.org/html/2604.01687v1
- Agent Workflow Memory (arXiv 2409.07429, ICML 2025) — https://arxiv.org/abs/2409.07429
- AI Agent Memory Systems 2026 (Mem0/Zep/Letta/Cognee) — https://explore.n1n.ai/blog/ai-agent-memory-comparison-2026-mem0-zep-letta-cognee-2026-04-23
- Claude Skills vs MCP (SKILL.md as cross-vendor standard) — https://claude.com/blog/skills-explained
- Deterministic vs Agentic Workflows 2026 — https://thinking.inc/en/blue-ocean/comparisons/deterministic-vs-agentic-workflows/
- AI for PKM 2026 (market size, digital graveyard) — https://remlabs.ai/blog/ai-knowledge-management-2026
- Obsidian alternatives 2026 (Smart Connections/Copilot/Mem) — https://www.remio.ai/post/top-10-obsidian-alternatives-for-smarter-knowledge-management-in-2026
