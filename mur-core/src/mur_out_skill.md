---
name: mur-out
description: "Stop recording and extract learned patterns from the captured mur session"
---
# mur-out — Stop Recording & Extract
Run at the end of a session:
```bash
mur session stop --analyze
mur sync
```
Runs: noise filter → significance scoring → pattern extraction → dedup → sync to AI tools.
When to use: after debugging, discovering workarounds, or completing features.
