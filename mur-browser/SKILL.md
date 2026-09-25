---
name: browser
version: 1.0.0
publisher: human:mur-official
description: Browser skill hub — auth, testing, or automation. Picks a mode from the invocation argument and follows the matching reference doc; asks when the argument is missing or unrecognized. Never types credentials.
category: workflow
visibility: on_demand
provenance: human
tags:
  - browser
  - auth
  - test
  - automation
triggers:
  - type: keyword
    pattern: browser (login|auth|test|automat)|登入|瀏覽器驗證|端對端|網頁測試|自動化|重複操作
  - type: manual
priority: normal
---

# Browser — hub for auth, testing, automation

One skill, three modes. The invocation argument picks which reference doc to
follow:

| Argument | Reference | What it's for |
|---|---|---|
| `auth` | `references/auth.md` | Log a person into a site once, keep an encrypted profile. Prerequisite for the other two modes. |
| `testing` | `references/testing.md` | Record/replay a browser end-to-end test — fixed workflow, never edits assertions. |
| `automation` | `references/automation.md` | Run a repeatable browser task — dry-run before every first real run. |

## Procedure

1. Read the argument this skill was invoked with (the text after `Use the
   browser skill.`).
2. If it matches `auth`, `testing`, or `automation` (case-insensitive), load
   the matching file under `references/` and follow it exactly — do not
   summarize or skip steps in it.
3. If the argument is missing, or does not match any of the three, **ask**
   rather than guess (one question, with a recommendation):
   - "auth" if the request mentions logging in, signing in, or a saved
     session.
   - "testing" if the request mentions an assertion, a spec, or verifying a
     flow still works.
   - "automation" if the request mentions a repeatable task, a routine job,
     or "just do X on the site" with no notion of pass/fail.

   State the recommendation and wait for confirmation before proceeding —
   never silently pick one.

## Shared across all three modes

- The agent never enters credentials, into a prompt, a field, or via
  `{{secret:...}}` expansion. Login is always a human-in-the-loop handoff —
  see `references/auth.md` rule 1.
- `mur browser status` is the read-first command in every mode: it reports
  profile health and past runs before any mode does anything else.
- Every mode's reference file lists the exact commands and flags it uses.
  Do not invent flags not listed there.
