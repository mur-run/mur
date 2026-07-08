# AI × Security-Scanning Tools — Survey (mid-2026)

Produced by AURA **Mode 1** (one-shot public research): the `deep-research` workflow,
109 ephemeral subagents, adversarially verified (25 claims → 20 confirmed, 5 refuted).
No agent, fleet, or per-agent sandbox involved. Full machine output:
`aura-mode1-security-scanning-report.json`.

## Core verdict

AI's real, best-evidenced value in security scanning is **reachability + context
reasoning → fewer false positives, better prioritization** (strongest in SAST/SCA).
**Standalone LLM vulnerability *detection* is largely hype.** What works is **hybrid**
(a conservative traditional scanner feeding an LLM), not AI-autonomous discovery.

## By category

### 4. SAST — strongest AI-native category ✅
- **Endor Labs AI SAST** — multi-agent call-graph/dataflow **reachability**; prioritizes
  actually-reachable/exploitable findings. ⚠️ Its self-reported "2.6x more vulns / 60%
  fewer FPs" figures were **refuted (0-3, self-benchmark)** — only the architecture claim survived.
- **Semgrep Multimodal** — static analysis + LLM reasoning → IDORs, broken authorization,
  business-logic flaws beyond pattern matching.
- FP filtering: **Datadog Bits AI** (LLM reads code context to classify true/false positive);
  Semgrep reachability (high/critical FPs "up to 98%" — vendor "up to").
- Sources: endorlabs.com/learn/ai-sast-benchmark…, semgrep.dev, datadoghq.com/blog/using-llms-to-filter-out-false-positives

### 5. SCA ✅
- **Semgrep SCA reachability** (exploitable-dependency identification, high/critical FPs
  "up to 98%"); Endor reachability prioritization. Prioritization is well-evidenced;
  AI auto-remediation / transitive-CVE fix acceptance rates remain an open question.

### 1+2. Web App / DAST ⚠️ (research direction, not proven commercial)
- **xOffense** — autonomous multi-agent pentest framework on a fine-tuned open-source
  **Qwen3-32B** (CoT-tuned recon/scan/exploit agents). Single non-peer-reviewed preprint,
  79% self-reported — treat as direction, not a settled product. Source: arxiv.org/pdf/2509.13021
- Web-app AI gains land mostly in Semgrep Multimodal's business-logic/IDOR detection.

### 3. System / Infra / Cloud / Container ❓ under-covered
- No verified claims survived for Wiz / Snyk / Aqua-Trivy / Prisma AI prioritization.
  Flagged as an open question — do not treat this report as authoritative for this category.

## The pattern that works: hybrid, not autonomous ✅
Conservative SAST (open-source **Bearer**) → LLM, told to find only *new* vulns:
TP-rate **17.91% (raw LLM) → 68.89%**, precision 86%. Adding a RAG knowledge layer
**hurt** (F1 76.54% → 49.18%). Source: arxiv.org/html/2409.15735v3

## Hype check ⚠️ (all 3-0 confirmed)
- Fine-tuned LLM detection **52.1%** (~2 pts above chance); "calibration without
  comprehension"; leakage-corrected PrimeVul dropped a SOTA 7B model to **3.09% F1**;
  GPT-4 "akin to random guessing" (FNR ~90%).
- Renaming variables flips answers 26%/17%; patched code still flagged vulnerable.
- Cobalt 2026 survey (455 pros): **78%** of teams hit critical false negatives from
  fully automated scanning.
- Vendor-benchmark bias: RealVuln is authored by the winning tool's vendor (Kolega) and
  the ranking flips between F3 and F1.

## Refuted claims (excluded)
Endor's "2.6x more vulns" and "60% fewer FPs" benchmark figures; Semgrep's "80% fewer
FPs via AI triage"; a blanket "LLMs not ready" generalization; Cobalt's "trust collapsed
29%→9%" stat. All 0-3.

## One-line takeaway
> AI moves the needle today on prioritization, FP reduction, and reachability
> (especially SAST/SCA). Autonomous end-to-end vulnerability discovery is an open
> research question, not a settled capability. Discount vendor "LLM finds vulns"
> numbers; buy the reachability/context-triage substance.
