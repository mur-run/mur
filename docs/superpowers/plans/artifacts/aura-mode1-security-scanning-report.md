# AI × Security-Scanning Tools — Full Survey (mid-2026)

**Produced by AURA Mode 1** (one-shot public research): the `deep-research` workflow —
109 ephemeral subagents, 27 sources fetched, 115 claims extracted, 3-vote adversarial
verification. **25 claims verified → 20 confirmed, 5 refuted.** No persistent agent,
fleet, or per-agent sandbox involved. Machine-readable source of record:
`aura-mode1-security-scanning-report.json`.

Confidence legend: vote `3-0` = all three verifiers confirmed; `2-1` = one dissent;
figures marked "up to" / "self-reported" are vendor marketing unless independently replicated.

---

## Executive verdict

AI/ML genuinely reshapes security scanning through **reachability + context reasoning
→ fewer false positives and better prioritization of exploitable findings**. That is
where the strongest, best-evidenced value sits (SAST and SCA). A second real frontier is
**LLM-augmented detection** of business-logic flaws, IDORs, and broken authorization that
pattern matching misses.

The hardest capability — **LLMs reliably detecting vulnerabilities from raw code — is
largely hype**: independent, leakage-corrected benchmarks put fine-tuned LLMs at near-random.
The pattern that consistently works is **hybrid** (a conservative traditional scanner feeding
an LLM to triage/extend), not AI-autonomous discovery.

> **Net:** AI moves the needle today on prioritization, false-positive reduction, and
> reachability. Autonomous end-to-end vulnerability discovery is an open research question,
> not a settled capability.

---

## By category

### 4. SAST — strongest AI-native category ✅ (best evidence)
- **Endor Labs AI SAST** — a multi-agent system applying a **call-graph / dataflow
  reachability** engine to first-party code, "prioritizing vulnerabilities that are
  actually reachable and exploitable." Commercial. `3-0` on the architecture.
  ⚠️ Its headline figures ("2.6x more real vulns", "60% fewer false positives") were
  **adversarially refuted `0-3`** as self-benchmark cherry-picking — only the architecture claim survives.
- **Semgrep Multimodal** — "blends static analysis with AI reasoning to find OWASP risks,
  business logic flaws, and IDORs" beyond pattern matching. Commercial. Self-reported
  efficacy (8x TPs, 61% precision) is unreplicated.
- **Datadog Bits AI** (FP filtering) — an LLM classifies findings true/false by evaluating
  "how data moves through functions, how input is validated, and whether conditions for
  exploitation are actually met." `3-0`.
- Sources: endorlabs.com/learn/ai-sast-benchmark…, semgrep.dev, datadoghq.com/blog/using-llms-to-filter-out-false-positives

### 5. SCA ✅ (prioritization well-evidenced)
- **Semgrep SCA reachability** — identifies exploitable dependencies, "reducing false
  positives in high and critical severity findings by up to 98%" (vendor "up to"). `2-1`.
- **Endor Labs** reachability prioritization on transitive CVEs. Commercial.
- Open: AI **auto-remediation** / transitive-CVE fix generation — real merge/fix acceptance
  rates are unverified (open question #4).

### 1 + 2. Web App / DAST ⚠️ (research direction, not proven commercial)
- **xOffense** — an autonomous **multi-agent penetration-testing** framework on a fine-tuned
  open-source **Qwen3-32B**, with specialized recon / vuln-scan / exploitation agents under an
  orchestration layer; CoT-tuned reasoning generates precise tool commands. `3-0` on
  architecture, `2-1` on the AI-contribution detail. Open-source model.
  Skepticism: single **non-peer-reviewed preprint**, 79.17% self-reported; the specific tool
  list (Nmap/Nikto/Metasploit/SQLMap) is an unsourced elaboration. Treat as *direction*, not product.
- Web-app AI gains land mostly in **Semgrep Multimodal**'s business-logic / IDOR detection.
- Source: arxiv.org/pdf/2509.13021

### 3. System / Infrastructure / Cloud / Container ❓ under-covered
- **No verified claims survived** for AI prioritization in host/network/cloud/container-image
  scanning (Wiz, Snyk container, Aqua/Trivy, Prisma). Explicitly flagged as an open question —
  **do not treat this report as authoritative for this category** (open question #1).

---

## The pattern that works: hybrid, not autonomous ✅
Passing a conservative open-source SAST scanner (**Bearer**) output into an LLM, instructed to
find only *new* vulnerabilities, raised true-positive rate from **17.91% (raw LLM) to 68.89%**
with 86.11% precision (raw-LSAST). Adding a **RAG** knowledge layer (HackerOne reports) was
**counterproductive** — F1 76.54% → 49.18%. `3-0` both ways.
> "passing the Bearer findings to the LLM significantly increases the capabilities of LLMs in
> finding vulnerabilities." / "using a RAG infrastructure for knowledge retrieval can be
> counterproductive." — arxiv.org/html/2409.15735v3

---

## Hype check ⚠️ (all high-confidence)

**Standalone LLM detection is near-random and non-robust.** `3-0`
- Fine-tuned LLM best detection **52.1%** (~2 pts above chance); exact CWE Top-1 < 1.3%;
  fine-tuning is "calibration without comprehension… the underlying security reasoning remains absent."
- SecLLMHolmes (IEEE S&P 2024): models "flag code where vulnerabilities have been patched as
  still vulnerable"; renaming variables flipped answers 26% / 17% of the time.
- ICSE 2025: on leakage-corrected **PrimeVul**, a SOTA 7B model dropped **68.26% → 3.09% F1**;
  GPT-3.5/GPT-4 "akin to random guessing" (FNR ~90%).
- Sources: arxiv.org/html/2606.20502, arxiv.org/pdf/2312.12575, github.com/huhusmang/Awesome-LLMs-for-Vulnerability-Detection

**Vendor benchmarks are biased and metric-dependent.** `2-1`/`3-0`
- On **RealVuln** (26 real Python repos), under recall-weighted **F3**, specialized Kolega.Dev
  (73.0) and Claude Sonnet 4.6 (51.7) beat rule-based Semgrep (17.7) ~3x — but the benchmark is
  **authored by the winning vendor (Kolega)**, and under **F1** Sonnet (60.9) beats Kolega (52.4),
  so "3x dominance" is metric-dependent. — arxiv.org/pdf/2604.13764

**Both traditional and fully-automated scanning have severe FP/FN problems.** `3-0`
- Ghost Security: **99.5% FP** for command-injection in Python/Flask (91% overall).
- Fluid Attacks: best fully-automated tool found only **22.7%** of known vulns (avg F2 1.9% across 36 scanners).
- Cobalt 2026 survey (455 pros): **78%** of teams hit critical false negatives from automated scanning.
- (All vendors — cite as vendor benchmarks.) — arxiv.org/pdf/2604.13764, businesswire.com (Cobalt 2026)

**Adjacent risk — AI-written code mostly isn't correct-and-secure.** `3-0`
- Best agent (SWE-agent + DeepSeek-V3.1): only **15.2%** correct-and-secure (9.2% avg); ~70% of
  functionally-correct outputs still had security issues; agents introduced 14 CWE types.
  — arxiv.org/html/2509.22097v1

**Cross-category consensus:** whether LLMs can reliably *detect* vulnerabilities is an
**OPEN research question**, not a settled capability. `3-0`

---

## Refuted claims (excluded from findings, all `0-3`)
1. Endor AI SAST "found 192 real vulns — 2.6x Claude Opus 4.7, 3.5x Codex GPT-5.5, 2.4x Semgrep OSS."
2. Endor "~60% fewer false positives than Semgrep OSS/OpenGrep."
3. Semgrep "AI-assisted triage → 80% fewer false positives across SAST and SCA."
4. A blanket "LLMs are not ready" generalization.
5. Cobalt "trust in full automation collapsed 29% → 9%."

These are why the verification pass matters: the most quotable vendor numbers did **not** survive.

---

## Caveats
- **Coverage is uneven:** SAST/SCA deepest; DAST rests on one preprint; System/Infra/cloud/container
  **not corroborated** — under-covered.
- Many efficacy figures are **vendor "up to" marketing** (Semgrep 98%, Cobalt/Ghost/Fluid surveys),
  not independent replication. The RealVuln benchmark most favorable to LLM scanners is authored by
  the winning vendor and is metric-dependent.
- **Time-sensitive:** negative LLM-detection findings are scoped to specific models/domains (notably
  Linux-kernel systems C) and 2023–2026 model generations; frontier-model capability may shift.

## Open questions
1. Who leads AI-driven **System/Infra/cloud/container** scanning (Wiz, Snyk, Aqua/Trivy, Prisma), and what does the AI concretely add?
2. Do **2026+ frontier models** close the near-random detection gap, or does "calibration without comprehension" persist beyond systems C?
3. Is there any **vendor-independent, leakage-controlled** head-to-head benchmark of commercial AI SAST/SCA?
4. For SCA, how do AI **auto-remediation / transitive-CVE fixes** perform in real merge-acceptance terms (vs the well-evidenced prioritization claims)?

---

## Cross-category takeaway
> AI is real where it does **reachability, context triage, and false-positive reduction** — buy that
> substance (strongest in SAST/SCA). Discount "LLM autonomously finds vulnerabilities" marketing: the
> independent evidence puts it near random, and the winning vendor benchmarks don't survive scrutiny.
> The durable architecture is **traditional-scanner → LLM-triage hybrid**, not an AI oracle.
