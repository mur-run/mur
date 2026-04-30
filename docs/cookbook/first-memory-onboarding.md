# First-Memory Onboarding (D2)

The 5-step wizard runs on first launch of a `mur agent export --format gui` bundle (and via `mur agent companion init <name>` from the CLI).

## Steps

1. **Name your agent** — display name only; the slug from `mur agent create` stays the same.
2. **Voice** — opt-in, default *Skip — enable later*. Voice setup in Settings → Voice triggers a one-time ~190 MB whisper download.
3. **Relationship** — Friend / Coach / Accountability buddy / Mentor.
4. **First memory** — one fact the agent should remember. Surfaced on day-3+ `morning_greeting` templates.
5. **Behavior** — three-layer toggle:
   - *Warm voice only* (default, recommended)
   - *Warm + behavior collection*
   - *All including proactive check-ins*

## Where it's stored

- `~/.mur/agents/<name>/profile.yaml` — `companion.onboarding.{completed_at,agent_display_name,first_memory}`, plus `companion.{enabled,rhythm.enabled,proactive.enabled}` for the three-layer toggle.
- `~/.mur/agents/<name>/companion/relationship.json` — duplicates `first_memory.text` (for runtime + character-card export).

## Re-running

```bash
mur agent companion init <name> --re-init
```

## Character card

`mur agent export --format card` (D4) emits `extensions.mur.first_memory.{text, established_at}` round-trippable per CCv3 passthrough.

## Acceptance gates

```
scripts/e2e/v1-d2-onboarding.sh
```

- Wizard ≤ 120 s
- `mur agent companion preview <name> --situation morning_greeting --no-llm` output references the first_memory string verbatim
- 72-hour MockClock test produces a proactive message containing the first_memory
