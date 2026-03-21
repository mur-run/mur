---
name: mur-out
description: "Stop recording and extract learned patterns from the captured mur session"
---
# mur-out — Stop Recording & Extract

Run these commands **in sequence**:

```bash
# Step 1: Stop recording
mur session stop

# Step 2: Ask user what to do next
```

After stopping, ask the user:
> Session stopped. What would you like to do?
> 1. 🔍 **Analyze** — extract patterns with LLM (recommended)
> 2. 📦 **Export** — save session as markdown
> 3. ⏭ **Skip** — do nothing

Based on their choice:

**If Analyze (1):**
```bash
# Find the latest session recording
mur session list --last 1
# Extract patterns (replace SESSION_FILE with actual path)
mur learn extract --file ~/.mur/session/recordings/<session-id>.jsonl --llm
mur sync
```

**If Export (2):**
```bash
mur session export <session-id> --format markdown
```

**If Skip (3):** Done, no further action.

When to use: after debugging, discovering workarounds, or completing features.
