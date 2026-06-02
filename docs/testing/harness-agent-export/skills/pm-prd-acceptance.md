---
name: pm-prd-acceptance
description: Product-manager best practices for writing a tight PRD and testable acceptance criteria. Use when scoping a feature or handing requirements to engineering.
---

# PM: PRD + Acceptance Criteria

Distilled from 2026 PM guidance (Product School, Atlassian, Perforce, Aakash Gupta).

## Write the PRD around "why/what", never "how"
- Open with a **problem statement backed by data**, not a UI idea (a UI-first PRD biases engineering design).
- Define **measurable success** up front. If you cannot quantify success, you are not ready to write the PRD.
- Explicitly state **what you are NOT building** in v1 — your strongest defense against scope creep.
- Separate **functional** from **non-functional** (perf/security) requirements; don't bury an NFR inside a feature paragraph.
- Keep it 2–10 pages. Treat it as a **living single source of truth**; version it.
- Keep an **Open Questions** log so unknowns are never lost.

## Acceptance criteria
- They are **specific, testable conditions** that gate release. If one item fails, the feature is not ready.
- Use **Given / When / Then**.
- Be measurable: "page loads within 2s", not "should be fast".
- Cover **edge cases and exceptions**, not just the happy path.

## Pitfalls
- Over-specifying (dictating UI) stifles engineering; under-specifying ("user friendly") creates conflicting interpretations.

## Handoff rule
End every PRD handoff with a single line: `HANDOFF -> <role>: <the one decision/artifact they now own>`.
