---
name: mur
description: "Continuous learning — auto-injects relevant patterns from your learning history"
---
# MUR — Continuous Learning for AI Assistants

MUR automatically injects relevant patterns at session start via hooks.
The patterns you see in "Relevant patterns/knowledge from your learning history"
come from MUR's pattern store (`~/.mur/patterns/`).

## How Injection Works

At each session start, MUR:
1. Scores all patterns against the current project context
2. Ranks by relevance (keyword match, tags, recency, confidence, tier)
3. Applies MMR diversity filter (removes near-duplicates)
4. Injects only the **top 5 patterns** within a **2000 token budget**

Even with hundreds of patterns stored, only the most relevant few are injected.
These limits are configurable in `~/.mur/config.yaml` under `retrieval:`.

## Available Commands

Run these in the terminal when appropriate:

- `mur search <query>` — Find patterns by keyword
- `mur context` — Show what would be injected for the current project
- `mur feedback helpful <name>` — Mark a pattern as helpful (boosts confidence)
- `mur feedback unhelpful <name>` — Mark a pattern as unhelpful (lowers confidence)
- `mur new` — Create a new pattern interactively
- `mur stats` — Show pattern statistics
- `mur sync` — Sync patterns to other AI tool configs
- `mur evolve` — Run decay + maturity evaluation
- `mur reindex` — Rebuild semantic search index

## When to Give Feedback

- If an injected pattern helped you solve the task → `mur feedback helpful <name>`
- If a pattern was irrelevant or wrong → `mur feedback unhelpful <name>`
- If you discover a reusable insight → suggest creating a new pattern with `mur new`

## Pattern Tiers

Patterns have three tiers based on their scope:
- **Session** — short-lived, auto-discovered from recent sessions
- **Project** — project-specific knowledge (e.g. "this repo uses Swift Testing")
- **Core** — universal best practices that apply everywhere

## Pattern Lifecycle

Patterns evolve through maturity stages based on usage:
- **Draft** → **Emerging** → **Stable** → **Canonical**
- Confidence decays if patterns aren't used (half-life based)
- Patterns auto-archive below 0.1 confidence
- Your feedback directly influences confidence and importance scores
