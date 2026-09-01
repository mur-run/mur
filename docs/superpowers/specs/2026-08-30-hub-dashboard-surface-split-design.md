# Hub and Dashboard: which surface owns what

Status: implemented (2026-08-31). Two repos: `mur` (Hub, local API, CLI) and
`mur-web` (Dashboard). Corrections where the design turned out to be wrong
about the code are marked in place rather than silently edited out.

## Problem

MUR ships two graphical surfaces over the same `~/.mur`, and nothing decides
what belongs in which. The result is visible three ways at once:

- **A page with no backend.** The Dashboard's Schedules page calls
  `GET /api/v1/schedules`. That route does not exist on the local server, so
  the page cannot work outside cloud mode.
- **A backend with no page.** `/api/v1/agents` is served locally, deliberately
  ("read-only Phase 4", with its own 404 fallback and a considered response
  envelope), and nothing consumes it.
- **A UI that outlived its capability.** Patterns has three pages and a nav
  entry; the pattern pipeline was removed in 2026-06-11.

None of these is a bug in the usual sense. They are what happens when two
surfaces grow without a rule.

## The measurement

`mur daemon serve` on :3847, probing each endpoint the Dashboard calls.

**A status code alone will not answer this.** The server serves the SPA for any
unmatched path, so a missing route returns `200 text/html`. Content type is the
signal; a first pass reading `%{http_code}` concluded every endpoint existed.

A `GET` probe also under-reports `POST`-only routes — `search`,
`workflows/search` and `context` read as absent until their route definitions
were checked.

| endpoint | local `mur serve` |
|---|---|
| `health` `stats` `patterns` `workflows` `pipelines` `sessions` `skills` `tags` | JSON |
| `search` `workflows/search` `context` | JSON (POST) |
| `agents` (+ `agents/{name}/evals`) | JSON — **no consumer** |
| **`schedules`** | **absent** |
| **`commander`** | **absent** |
| **`fleets`** | **absent** |

## The architectural fact this rests on

```
Hub        →  67 Tauri invoke() calls,  0 HTTP
Dashboard  →   0 Tauri,                 all HTTP → localhost:3847
```

Two independent data paths into the same `~/.mur`. So placement is not a layout
question: **every shared capability costs two implementations**, and two
surfaces rendering the same state can disagree — the failure this codebase
meets most often.

That cost is the reason a rule is worth having, and the reason the rule should
be restrictive rather than generous.

## The rule

> **Anything an agent needs in order to run lives where agents live.**

| | surface | contents |
|---|---|---|
| the agent's world | **Hub** | agents, fleets, skills, MCP, models, permissions, chat, HITL, companion, panel |
| artifacts you author and browse | **Dashboard** | workflows, pipelines, sessions, import, search |
| spans both | split | **schedules** — the record is an artifact, the next fire is live |

### Why agents stay out of the Dashboard

Three reasons, in order of weight:

1. **The Dashboard's realtime channel cannot carry agent state.** `/api/v1/ws`
   exists and `mur-web/src/lib/realtime.ts` connects to it, but every publisher
   is an artifact CRUD notification raised by the Dashboard's own write routes
   — `notify(&state, "pattern:created", &name)` — with a `{type, id, ts}`
   payload meaning "refetch". There is no agent-state stream. A Dashboard
   agents page would be a polling snapshot going stale while it is read, which
   is the one property an agent view must not have.
2. **`/api/v1/agents` having no consumer is not an argument for building one.**
   It was built ahead of a consumer that never arrived; mobile goes over the
   relay socket, the Hub over Tauri.
3. **Fleets would follow, then chat, then HITL.** The rule stops that at the
   first step instead of relying on judgement at each one.

The one genuine benefit — viewing agents from a browser or phone — is already
served by a purpose-built path (mobile SDK + relay), and a read-only remote list
that cannot act on what it shows is thin: seeing an agent stuck still sends you
back to the Hub or the CLI to do anything about it.

### Why skills are Hub, though `/api/v1/skills` exists

Skills are a dependency of a running agent, not a document. Same reasoning as
agents, and the same conclusion about the endpoint: it stays for API and
external use, and **is not a reason to grow a Dashboard page**.

### The CLI is the union

`mur agent schedule`, `mur fleet`, `mur workflow` already do everything both
GUIs do and more. So **neither GUI needs to be complete.** A gap is acceptable
where the CLI covers it — provided the GUI says so rather than leaving a dead
control. That is what makes a read-only view an honest design instead of a
truncated one.

## Changes

### D1 — retire Patterns and Graph

The pattern pipeline (emergence/fingerprint mining, decay sweeps, injection) was
removed on 2026-06-11. `context_api::ingest` / `submit_feedback` still write
transitional Patterns pending the Notes migration, so the store is not gone —
but nothing mines, decays or injects them, and `/api/v1/patterns` reports
`pattern_count: 0`.

Remove from the Dashboard: `Patterns.svelte`, `NewPattern.svelte`,
`PatternEditor.svelte`, `Graph.svelte`, and both nav entries.

**Graph is the sharper case.** Its only data source is `getPatterns()`. It
renders an empty graph and looks like a working feature doing so.

`/api/v1/patterns` stays: the transitional write path still needs it, and
removing a route is not what this change is about.

Delete `SessionReview.svelte.backup` while there — an editor artefact committed
by accident.

### D2 — say which pages need the cloud

`Commander`, `CommanderWorkflows`, `CommanderWorkflowDetail`,
`CommanderExecution` and `AuditLog` depend on `/commander`, which the local
server does not serve. They are not deprecated — they are cloud features.

Today they fail the way Schedules does: indistinguishably from "nothing here".
Each needs to state, before it tries to load, that it requires the cloud
backend, and the `Local` badge in the header should be enough to predict that.

### D3 — schedules, on both surfaces

**D3a — `GET /api/v1/schedules` on the local server**, served from
`mur_core::schedule_status::schedule_status`, the aggregator that already folds
agent, workflow and fleet schedules into one view. This is the line that makes
the Dashboard's Schedules page work without an account, and it adds a fifth
consumer to an existing derivation rather than a new one.

**D3b — fix the Panel.** The Schedule tab has five defects, all visible in one
screenshot of an agent panel showing five rows, none of which belong to that
agent:

| # | defect | cause |
|---|---|---|
| 1 | every row is a fleet, none is the agent's | fleet and workflow items are included unconditionally |
| 2 | "Show all agents" changes nothing | it filters only the agent half, which is empty here |
| 3 | `interval:30m` shows `Next: —` | `next_n_fires(cron_expr)` parses cron only, and fails silently |
| 4 | four of five rows are `manual`, i.e. not scheduled | no distinction between *has a timetable* and *can be triggered* |
| 5 | `cron:*/15 * * * *` is shown raw | there is no cron describer on the Rust side |

Defect 3 is the one to fix first. A blank `Next` reads as "will not run again"
for a fleet that runs every thirty minutes. **Where the derivation cannot
answer, it must say it cannot** — not return a value that looks like an answer.
Same rule as the write-denial classifier: no cause it cannot support.

Defect 1 is a scope error, not a rendering one: an agent-scoped panel must not
show globally-scoped rows without labelling them as such, and the filter must
visibly apply to what is on screen.

**Landed:** 3 and 5 in #1095, 1 in #1096, 2 and 4 after. Defect 3's stated cause
was only half of it. `next_n_fires` does parse cron only, but the reason an
interval fleet was left with no answer was a second claim, written into
`schedule_status` itself: that nothing records the `.last_run` an interval is
measured from, "so the gap is permanent until something does". That claim was
false — `mur-daemon`'s fleet tick writes the stamp on every auto-run, and the
Hub already read the same file to show "Last auto-run". Interval fire times are
computed from it now. Two answers are still declined, because they cannot be
supported: a fleet that has never run has nothing to measure from, and a
boundary that passed without the fleet running means nothing is running it.

**D3c — Hub shows schedules read-only**, on agent and fleet detail: what fires
next, whether it is enabled, what happened last.

The agent view is read-only and says where editing lives (`mur agent schedule`).
**Fleet detail is not, and should not be:** the Hub already carries a
loop-settings editor there — trigger, budget, deadline, iteration cap — and
under the rule above that is exactly where fleets belong. This document's
original "editing stays in the CLI and the Dashboard" was written without
checking, and would have meant deleting a working surface to satisfy a
sentence. What D3c adds to fleet detail is the half the editor never answered:
when the fleet actually fires next, for every trigger kind rather than only
while a cron expression is being typed.

A link out to `localhost:3847` is acceptable **only if the Hub guarantees the
target is reachable** — starting `mur serve` on demand, or not offering the link.
A link into a dead port is worse than no link. This is a precondition, not a
detail: the server is not running by default, and had to be started by hand to
take the measurements in this document.

### D5 — reaching the Dashboard from the Hub

D3c leaves a link to `localhost:3847` conditional on the target being reachable.
That condition is general — it applies to every Hub → Dashboard link, not just
the schedule one — so it is specified here rather than inside D3.

**The check must identify MUR, not the port.** `GET /api/v1/health` returns
`{"source":"local","status":"ok","version":"2.71.9"}`. A TCP connect is not
enough, and neither is a 200: this server answers 200 with the SPA for any
unmatched path, and an unrelated process can hold 3847. The check is: health
responds, parses as JSON, and reports `status: "ok"`.

Three states, three behaviours:

| state | what the Hub does |
|---|---|
| health answers `ok` | open the browser |
| port closed | start the server, wait for health, then open — with a visible "starting…", not a frozen click |
| port held, not MUR | **do not open.** Say what is on the port |

The third row is the one that is easy to omit and expensive to get wrong:
without it the Hub opens a browser onto a stranger's server, which is worse than
the blank page this exists to prevent.

**Whoever starts it, stops it.** If the Hub started the server, the Hub shuts it
down when it quits; a listening socket the user never asked for should not
outlive the app that opened it. If it was already running, leave it alone —
someone else owns it.

**On demand, not at launch.** Opening a port when the Hub starts is a surprise,
and the kind of thing this project keeps deliberately opt-in. The trigger is the
user asking for the Dashboard.

The Hub already supervises child processes (`mur-gui-core/src/sidecar.rs`), so
this needs a new client of existing machinery rather than new machinery.

### D4 — one describer, one next-fire calculator

`mur-web/src/lib/schedule-parser.ts` is 288 lines carrying natural-language →
cron, `describeCron`, and `getNextRun`. The Rust side has `next_n_fires`; the
describer beside it (`describe_cron`, `describe_trigger`) landed in #1095 while
this was being written. That is two implementations of "when does this fire
next", and a third would appear the moment the Hub grew a schedule view.

**`next_fires` and a human description become part of the schedule record's API
shape.** Whoever serves the record fills them; the browser formats and does not
compute. `formatNextRun` stays; `getNextRun` and `describeCron` are retired from
the local path.

This keeps local-first intact — the local server does the computing, no account
involved. The cloud server computing the same fields for cloud-mode viewing is a
second implementation, but of a *different data set*, which is a tolerable
shape. Two answers to one question is the failure; two sources each answering
for their own data is not.

The Rust describer belongs beside `next_n_fires` — which is where #1095 put
it.

## Recorded, not acted on

**`/api/v1/agents` is an orphan.** It is deliberate, careful code — the module
doc explains why it skips the `{data, meta}` envelope and warns future
contributors not to "fix" that — and nothing calls it. Under the rule above it
should stay unconsumed by the Dashboard. Left in place: it is a reasonable API
for external tooling, and deleting a considered read-only surface to tidy an
inventory is not an improvement.

Anyone reaching for it as evidence that "the line is already laid" should read
this paragraph first.

## Rejected

**Agents and fleets in the Dashboard.** See the rule. The complexity concern is
real and the benefit is already served elsewhere.

**A skills page in the Dashboard**, on the strength of `/api/v1/skills`
existing. An endpoint is not a reason for a UI.

**Removing `/api/v1/patterns`.** The transitional write path still uses it. D1
retires the *interface*, which is what outlived its capability.

**Hub links out to the Dashboard for schedule editing, unconditionally.** Only
acceptable with the reachability guarantee in D3c.

## Verification

- The endpoint table above is reproducible: `mur daemon serve`, then probe
  **content type**, not status. Any check that reads `%{http_code}` alone will
  report every path as present.
- `POST`-only routes need their definitions read; a `GET` probe under-reports
  them.
- D3b defect 3 needs a test that an unparseable or unsupported trigger produces
  an explicit "cannot compute", never an empty string — the failure mode is a
  blank that reads as a value.
- D4 wants one shared set of trigger → expected-fires vectors, run against the
  Rust implementation, so a future second implementation has something to agree
  with.

## Context

- `mur-core/src/schedule_status.rs` — the aggregator; its doc comment already
  states the unconditional-globals behaviour behind defect 1
- `mur-agent-runtime/src/scheduler.rs` — `next_n_fires`, cron only
- `mur-core/src/server/mod.rs` — the local router, `notify`, and the SPA
  fallback that makes status codes unreliable here
- `mur-core/src/server_agents/mod.rs` — the orphan
- `mur-web/src/lib/api.ts` — `apiPath`, and the local/cloud/demo `DataSource`
- `mur-web/src/lib/schedule-parser.ts` — the second implementation
- `mur-hub-gui/ui/src/components/` — Hub's surfaces
