# C6 — Idle / Heartbeat Triggers

Fire a configured message when an agent has been idle for a user-defined window.

## When to use

- A "still there?" check-in after a long silence on a chat-bridge agent.
- A periodic garbage-collection or health-probe sweep on a worker agent.
- A self-pinging keepalive that exercises the LLM path even when no one is talking to the agent.

C6 reuses the supervisor's existing `TaskRunner`, so each fire is an ordinary task — entitlements, sandboxing (B1), telemetry, and B0SafetyHook all apply unchanged.

## How it works

When `profile.lifecycle.idle_triggers` is non-empty, the supervisor spawns one `IdleScheduler` task that wakes every 30 s and inspects `TaskRunner::last_activity_at` (Unix seconds, bumped on every `start_async`). For each configured trigger:

1. If `(now - last_activity) < after_secs` → skip.
2. If `(now - last_fire) < cooldown_secs` → skip (refire suppression).
3. If `respect_quiet_hours` is true and now is past today's quiet-window start → skip.
4. Otherwise: inject `trigger.message` via the runner and record the fire time.

Per-trigger cooldowns are independent — two triggers can fire at different cadences without interfering.

## Profile schema

```yaml
lifecycle:
  restart: on_failure
  idle_triggers:
    - after_secs: 3600          # required: idle threshold
      message: "still there?"   # required: injected message body
      sends_to: peer_agent      # optional: A2A peer (default = self)
      cooldown_secs: 1800       # optional: refire cooldown (default 600)
      respect_quiet_hours: true # optional: suppress in quiet hours (default true)
```

`after_secs` and `cooldown_secs` are independent: a 1-hour idle threshold with a 30-min cooldown means the trigger fires at most once every 30 min, but only after the agent has actually been idle for 1 hour first.

## CLI

```bash
# Add a trigger
mur agent schedule idle-add my-agent \
  --after-secs 3600 \
  --message "still there?" \
  --cooldown-secs 1800 \
  --respect-quiet-hours

# List all idle triggers
mur agent schedule idle-list my-agent

# Remove by index
mur agent schedule idle-remove my-agent 0
```

`mur agent schedule idle-list` output:

```
IDX  AFTER      COOLDOWN   QH    MESSAGE                        SENDS_TO
0    3600       1800       yes   still there?                   (self)
1    86400      1800       no    daily heartbeat                ops_agent
```

## Restart semantics

Like C4 cron triggers, idle triggers are read at supervisor boot. Editing them via `mur agent schedule idle-{add,remove}` mutates `profile.yaml` but the running supervisor caches the trigger list — changes apply on the next `mur agent stop && mur agent start`.

## Quiet-hours interaction

`respect_quiet_hours: true` suppresses fires from the start of the quiet-hours window onward (configured under `companion.proactive.quiet_hours`). For agents without companion enabled, the field is ignored. The window resets at midnight local time.

## v1 limitations (deferred to v2)

- 30-second poll resolution is not configurable in production. (Tests can use `IdleScheduler::with_tick_interval` for fast smoke.)
- No "did fire" telemetry counter — fires are visible only as ordinary task records in `~/.mur/agents/<name>/telemetry/<date>.jsonl`.
- No CLI `next` command (unlike C4 cron) — idle triggers don't have a deterministic next-fire time.
- `sends_to` cross-agent dispatch logs a warning and injects locally; full A2A routing deferred.

## See also

- `docs/cookbook/c4-cron-triggers.md` — wall-clock-driven scheduling.
- `docs/cookbook/c5-webhook.md` — external HTTP-driven triggering.
- `mur-agent-runtime/src/idle_scheduler.rs` — implementation.
- Roadmap §5.6 (C6 row).
