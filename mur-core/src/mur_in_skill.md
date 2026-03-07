---
name: mur-in
description: "Start MUR session recording to capture your workflow for pattern extraction"
---
# mur-in — Start Recording
Run at the beginning of a coding session:
```bash
mur session start --source <tool-name>
mur context
```
Captures user prompts, AI responses, tool calls, errors. Used later by mur-out to extract patterns.
When to use: before debugging, exploring new codebases, or any non-trivial task.
