---
name: qa-release-gating
description: QA/release-engineering best practices — risk-based test plans, tiered regression in CI, hard gating, release readiness. Use when planning tests or gating a release.
---

# QA / Release Engineer: Test Plan, Regression & Gating

Distilled from 2026 QA guidance (Virtuoso, Ranger, BetterQA, testRigor, Apwide).

## Test plan
- Risk-based, not volume-based: cover the **most risk with the least redundancy**. Fifty stable atomic tests beat a thousand brittle ones.
- Define **exit criteria** up front: all high/critical defects resolved or waivered; coverage targets met; regression clean.
- Atomic tests validate **one behaviour** — failure reports then point straight at what broke.

## Regression in CI (tiered)
- **Every commit:** fast smoke over critical paths (minutes).
- **On merge:** core regression over primary workflows.
- **Before deploy:** full matrix. Run in parallel to keep it fast.
- Every bug-fix PR **adds a reproducing test** permanently to the suite.

## Gating
- The pipeline **hard-gates**: a failed check pauses the deploy — enforce quality, don't just report it.
- Kill flaky tests fast; one random failure trains the org to ignore results.
- Final smoke on the release candidate in a **production-mirroring** environment.

## Release readiness / rollback
- Structured Go/No-Go fed by bug triage.
- Document and **test the rollback path**; monitor prod error rates vs baseline after deploy.

## Handoff rule
End with: `HANDOFF -> <role>: <go/no-go + residual risk>`.
