# Authoring Skills

A **skill** is a reusable unit of knowledge or behaviour you teach a MUR agent:
a piece of context, a workflow, a slash command, or a note. Skills are loaded
from disk at agent start and surfaced to the model through *progressive
disclosure* — a short summary every turn, the full body only when it is
relevant.

This guide covers the canonical skill format, where skills must live on disk,
how progressive disclosure decides what reaches the model, and the commands you
use to author and attach a skill.

---

## What a skill is

The canonical, authoritative source of a skill is a **`skill.yaml`** file (a
`SkillManifest`) inside a **per-skill subdirectory**. One subdirectory, one
`skill.yaml`, one skill.

A minimal valid context skill:

```yaml
name: deploy-runbook
version: 1.0.0
publisher: human:alice
description: How we ship to production safely.
category: context
tags: [deploy, ops]
triggers:
  - type: session_start
  - type: keyword
    pattern: "(?i)deploy|release|ship to prod"
content:
  abstract: >
    Production deploys go through fly.io. Always run the smoke test after
    deploy and watch the error rate for 5 minutes before walking away.
  context: |
    ## Deploy checklist

    1. `cargo nextest run --workspace` is green on the branch.
    2. `fly deploy --app acme-prod`
    3. Run `./scripts/smoke.sh prod` and confirm 200s.
    4. Watch the dashboard error rate for 5 minutes.
    5. If error rate climbs, `fly releases rollback`.
```

### Field reference

The manifest fields, as enforced by the validator
(`mur-common/src/skill/manifest.rs`, `validate.rs`):

| Field | Required | Notes |
|-------|----------|-------|
| `name` | yes | Lowercase `[a-z0-9-]`, 1–64 chars, no leading/trailing `-`. |
| `version` | yes | Strictly `MAJOR.MINOR.PATCH`, all numeric (e.g. `1.0.0`). Pre-release / build suffixes are **not** accepted. |
| `publisher` | yes | `human:<name>` or `agent:<id>`. |
| `description` | yes | One-line summary (used by search). |
| `category` | yes | One of `context`, `workflow`, `command`, `meta`, `note`, `media`. Must match the populated content mode (see below). |
| `content.abstract` | yes | Layer-2 summary, injected every turn. Must be non-empty. |
| `content.context` / `procedure` / `command` / `note` | exactly one | The full body (Layer 3). Which one is allowed depends on `category`. |
| `tags` | no | Free-form strings, used by search. |
| `triggers` | no | When the body is injected (see Progressive disclosure). |
| `priority` | no | `low` / `normal` (default) / `high` / `critical`. |
| `requires` / `mcp_requirements` | no | Declared dependencies / MCP tool capabilities. |

**Category ↔ content mode** — the validator requires the body to match the
category:

| Category | Body field |
|----------|------------|
| `context` | `content.context` |
| `workflow` | `content.procedure` |
| `command` | `content.command` |
| `note` | `content.note` |
| `meta` | `content.context` |
| `media` | `content.context` |

Exactly one body field may be populated. Setting more than one, or none, fails
validation.

---

## Directory layout (the loadable form)

Skills load **only** from a per-skill subdirectory that contains a `skill.yaml`
(a legacy `skill.md` inside the subdir is also accepted). A flat `.yaml` or
`.md` file sitting directly in the `skills/` directory is **not** loaded.

`<MUR_HOME>` defaults to `~/.mur`.

**Global skills** (available to every agent):

```
<MUR_HOME>/skills/<name>/skill.yaml
```

**Agent-attached skills** (scoped to one agent; win over a global skill of the
same name):

```
<MUR_HOME>/agents/<agent>/skills/<name>/skill.yaml
```

The agent runtime loads skills by listing the **subdirectories** under each
`skills/` directory and reading `skill.yaml` (then `skill.md`) from inside each
one. This is why a loose `skills/deploy-runbook.yaml` is invisible to the
runtime — it has to be `skills/deploy-runbook/skill.yaml`.

---

## Progressive disclosure (the load-bearing concept)

A skill is exposed to the model in three layers. Authoring a good skill is
mostly about respecting the cost of each layer.

**Layer 1 — discovery.** The `name` and `description` feed skill search. This
is metadata only; it costs nothing per turn.

**Layer 2 — the abstract.** `content.abstract` is injected into the system
prompt every turn, but it is **budget-capped**. The injector
(`mur-agent-runtime/src/skills/injector.rs`) only considers skills that declare
a `session_start` trigger, sorts them by trust then priority, and then trims to
the configured budget:

- `max_skills_in_prompt` — default **5**
- `max_total_tokens` — default **2000**

(Both live under `skills:` in `~/.mur/config.yaml`; see
`mur-common/src/config.rs`.)

Because the abstract recurs on every turn, keep it to **1–3 sentences**. Skills
that overflow the budget are silently dropped from the prompt.

**Layer 3 — the body.** `content.context` (or `procedure` / `command`) is the
full body. It is injected **only when a trigger fires** for the current turn:

- `session_start` — matches at the start of a session.
- `keyword` — `pattern` is a **regex** matched against the user prompt.
- `command` — `pattern` matches when the prompt *starts with* the string
  (e.g. `/deploy`).
- `manual` — never auto-fires.

### Practical guidance

- **Always include a `session_start` trigger** if you want the abstract to show
  at all — Layer 2 only injects skills that have one.
- **Keep the abstract short.** It is paid for on every turn, capped by the
  token budget above.
- **Make keyword patterns specific.** A broad pattern like `(?i)test` fires
  constantly and injects the body needlessly; prefer domain-specific terms or
  anchored regexes.
- A `keyword` or `command` trigger **must** include a `pattern`; validation
  rejects it otherwise.

---

## Authoring workflow

> `mur skill new` / `mur skill edit` and the subdirectory-installing `mur agent
> skill add` require a mur build that includes the skill-authoring tooling.

1. **Scaffold a new skill.**

   ```bash
   mur skill new deploy-runbook              # creates deploy-runbook/skill.yaml from a template
   mur skill new deploy-runbook --agent <agent>   # or scaffold straight into an agent
   ```

   The template carries the required fields and inline comments explaining the
   `abstract` (always-on) vs `context` (trigger-loaded) split. `content.context: |`
   is a literal block scalar — no escaping is needed, just keep the indentation
   consistent.

2. **Edit and validate.**

   ```bash
   mur skill edit deploy-runbook                  # opens $EDITOR, validates on save
   mur skill validate deploy-runbook/skill.yaml   # or validate explicitly
   ```

   `validate` runs schema checks plus a content security scan, and warns if a
   markdown round-trip would alter content. Add `--warnings-only` to print
   findings without failing.

3. **Attach it to an agent** (optional — global skills under `~/.mur/skills/`
   are already visible to every agent):

   ```bash
   mur agent skill add <agent> deploy-runbook/skill.yaml
   ```

   `agent skill add` validates the input (`.yaml`/`.yml`, or `.md` which it
   converts to canonical) and installs it into the loadable per-skill
   subdirectory `<MUR_HOME>/agents/<agent>/skills/deploy-runbook/skill.yaml`. A
   file that is not a valid skill manifest is rejected rather than silently
   stored.

4. **Restart the agent** so it reloads skills from disk, then confirm:

   ```bash
   mur skill list                 # global skills
   mur agent skill list <agent>   # skills attached to an agent
   mur skill show deploy-runbook  # print the canonical YAML
   ```

### Quick command reference

| Command | What it does |
|---------|--------------|
| `mur skill validate <path>` | Schema + security validation of a `skill.yaml` / `skill.md`. |
| `mur skill list` | List installed global skills and their trust level. |
| `mur skill show <name>` | Print a skill's canonical YAML. |
| `mur skill search <query>` | Search installed skills by name / description / tags. |
| `mur skill fmt <path> [--to yaml\|md] [--write]` | Convert between canonical YAML and portable markdown. |
| `mur skill schema` | Emit the JSON Schema of the skill manifest. |
| `mur agent skill add <agent> <path>` | Validate and attach a skill to an agent. |
| `mur agent skill list <agent>` | List skills attached to an agent. |

Run `mur skill --help` or `mur agent skill --help` for the full list.

---

## `skill.yaml` vs. Anthropic-style `SKILL.md`

MUR's canonical, signable source of truth is **`skill.yaml`**. It carries the
lifecycle and governance fields an external `SKILL.md` cannot express:
`version`, `publisher`, `provenance`, `triggers`, content hash / signature, and
trust level. Validation, trust, and drift detection all operate on the YAML.

For interoperability with external tooling (e.g. Anthropic / agentskills.io
style `SKILL.md`), MUR can export a portable markdown form:

```bash
mur skill fmt deploy-runbook/skill.yaml --to md --write   # -> deploy-runbook/skill.md
mur skill fmt deploy-runbook/skill.md  --to yaml --write  # import back to YAML
```

The markdown form is frontmatter (the manifest without `content`) plus a
`# <name>` body holding the abstract and the context / steps / command. Treat
it as an **export/import bridge** — the canonical YAML remains authoritative,
and round-tripping through markdown can drop structure that markdown cannot
represent.

---

## Troubleshooting: "I added a skill but it doesn't load"

Check, in order:

1. **Layout.** The skill must be a per-skill *subdirectory* containing
   `skill.yaml`: `skills/<name>/skill.yaml` (global) or
   `agents/<agent>/skills/<name>/skill.yaml` (agent). A loose
   `skills/<name>.yaml` is ignored.
2. **Validity.** Run `mur skill validate <path>`. The runtime skips any skill
   whose manifest fails to parse or hash.
3. **Abstract injection.** If the skill loads but its summary never appears,
   make sure it has a `session_start` trigger — Layer 2 only injects skills
   that declare one — and that it isn't being trimmed by
   `max_skills_in_prompt` / `max_total_tokens`.
4. **Restart.** Skills are read at agent start; restart the agent after adding
   or editing one.
5. **Confirm.** `mur skill list` / `mur agent skill list <agent>` should show
   the skill. `mur skill list` flags subdirectories with no readable manifest.
