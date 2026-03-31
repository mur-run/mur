---
name: mur-out
description: "Stop recording and extract learned patterns from the captured mur session"
---
# mur-out — Stop Recording & Extract

## Steps

1. Run `mur out` and show the output to the user.
2. The output includes a session summary and a numbered list of available actions.
3. Present the actions as a menu and ask the user which one they want (default: sync).
4. Run `mur out --action <choice>` with the user's selection.

## Example

```
$ mur out
Session stopped: 52f92a6f
Extracted 3 fingerprints from session.

Session summary:
  Events: 42
  Turns:  8 user, 8 assistant
  Duration: 12m 34s

Available actions:
  1. analyze  — Extract patterns with LLM analysis
  2. export   — Save session as markdown
  3. sync     — Sync patterns to AI tools
  4. skip     — Done, no further action

Run: mur out --action <analyze|export|sync|skip>
```
