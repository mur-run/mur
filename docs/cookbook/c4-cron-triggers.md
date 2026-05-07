# C4 — Cron Triggers for murmur Agents

Cron triggers let an agent send itself a scheduled user-turn message on a repeating
schedule — no external scheduler or push needed. The firing logic lives inside the
supervisor (`mur-agent-runtime`), so it runs for the lifetime of the agent process.

## How it works

`profile.yaml` has a `lifecycle.schedule` list:

```yaml
lifecycle:
  execution: daemon
  schedule:
    - cron: "0 9 * * 1-5"      # weekday 09:00 local time
      message: "Morning brief — what's on the agenda today?"
    - cron: "0 18 * * 1-5"     # weekday 18:00 local time
      message: "End-of-day summary: list the three most important things done."
      sends_to: summarizer      # send to a different agent instead of self
```

On startup the supervisor parses each entry and spawns a persistent tokio loop per
entry. Each loop sleeps until the next cron firing, injects the message as a `user`
turn via `TaskRunner::run_sync`, then sleeps until the following firing.

**Format:** `cron` is a 5-field POSIX expression `min hour dom month dow`. The
scheduler prepends `0 ` internally (seconds = 0). Standard shortcuts like
`@daily` are NOT supported — use explicit 5-field expressions.

**`sends_to`:** Specifying a different agent name is v2; in v1 the message is
always injected locally with a warning logged. Leave it unset unless you intend
to upgrade to a multi-agent topology.

## CLI

### Add a schedule entry

```bash
mur agent schedule add myagent \
  --cron "30 8 * * 1-5" \
  --message "Good morning! Summarise today's calendar."
```

### List schedule entries

```bash
mur agent schedule list myagent
# IDX  CRON                 MESSAGE                        SENDS_TO
# 0    30 8 * * 1-5         Good morning! Summar...        (self)
# 1    0 18 * * 1-5         End-of-day summary             (self)
```

### Preview next fire times

```bash
mur agent schedule next myagent --count 3
# [0] 30 8 * * 1-5
#   2026-05-11 08:30:00 CST
#   2026-05-12 08:30:00 CST
#   2026-05-13 08:30:00 CST
```

### Remove an entry

```bash
mur agent schedule remove myagent 0   # removes index 0
```

## Restart required

Schedule entries are read once at supervisor startup. After adding, removing, or
modifying entries via the CLI, restart the agent for changes to take effect:

```bash
mur agent stop myagent
mur_agent_myagent   # or however you normally start the agent
```

## Cron reference

| Expression      | Meaning                |
|-----------------|------------------------|
| `* * * * *`     | every minute           |
| `0 * * * *`     | top of every hour      |
| `0 9 * * 1-5`   | weekday 09:00          |
| `0 9,18 * * *`  | 09:00 and 18:00 daily  |
| `0 0 1 * *`     | first of every month   |
| `*/15 * * * *`  | every 15 minutes       |

Times are in the **system local timezone** of the machine running the agent
(`chrono::Local`). Set `TZ` in the process environment to override.

## Known limitations (v1)

- `sends_to` dispatches locally with a warning (cross-agent dispatch is C4 v2).
- No persistence of missed firings: if the agent is offline when a cron would
  have fired, that firing is skipped — there is no catch-up mechanism.
- No per-entry enable/disable toggle; remove and re-add to disable.
