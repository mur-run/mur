# Official catalog: show what an agent can do, and what it can touch

**Date:** 2026-07-29
**Status:** design, not implemented
**Spans:** `mur-run/official-catalog` (build), `mur-run/mur-server` (API + dashboard)

## Problem

The Library's detail page for an official agent shows a name, a version, a tier
badge, and one line of description. That is enough to identify an item and not
nearly enough to decide whether to run it.

An agent is not a document. Installing one puts a process on the user's machine
with a model, a set of tools, and a set of permissions. The catalog's promise is
that these items are *signed and security-scanned* — but the page never says
what the scan was scanning, so the user is asked to trust a badge instead of a
description.

The asymmetry the user noticed is real but has two separate causes, and only one
of them is a gap:

- **Skills have "Install to Hub"; agents and fleets do not.** By design:
  `INSTALLABLE_LIBRARY_TYPES` covers skills / mcp / workflows because those
  install through the relay. An official agent's download carries an
  account-bound license that the import path requires, so only
  `mur official install` can complete it. Not a gap.
- **Agents and fleets have no substantive detail.** A gap, and the subject of
  this spec.

## What already exists

Every fact below was verified in the repositories, not assumed.

- `mur-run/official-catalog` holds each item's source: `agents/researcher/`
  contains `profile.yaml` and `prompt.md`. `catalog.yaml` is the hand-written
  manifest; CI generates `index.json` from it, adding `storage_key`, `sha256`
  and `size`.
- **`profile.yaml` already carries the entire capability picture** as plain
  YAML — no bundle to unpack:

  ```yaml
  model: { provider: ollama, name: "m", params: {} }
  mcp_servers: []
  skills: []
  capabilities: ["a2a.message.send", "a2a.tasks"]
  entitlements:
    network:    { outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"] } }
    filesystem: { read: [], write: [], deny: ["~/.ssh"] }
    processes:  { spawn: { mode: allowlist, allowed: [] } }
    limits:     { memory_mb: 512, file_descriptors: 1024, processes: 32 }
  ```

- The catalog's publish workflow already runs Rust (`dtolnay/rust-toolchain`)
  and `tools/official-sign`, so it can deserialize `mur_common::AgentProfile`
  rather than re-parsing YAML by hand.
- `GET /api/v1/core/catalog` returns the index, minus the server-internal
  download fields. There is no per-item endpoint.
- The dashboard detail page resolves an item from the list (there is nothing
  else to fetch) and renders description + install command.

## Design

### 1. Derive the summary; never ask anyone to write it

The build computes the capability summary from `profile.yaml` at publish time.
Prose written by hand would drift from the artifact it describes, and the one
thing this panel must never do is describe permissions the bundle does not have
— or omit ones it does. A derived summary cannot disagree with the thing it
ships beside.

Per item, the build emits:

| field | from | shown as |
|---|---|---|
| `model` | `model.provider` + `model.name`, or `model_ref` | "Runs on ollama/m" |
| `mcp_servers` | `mcp_servers[].name` | the external tools it can call |
| `skills` | `skills` + `installed_skills[].name` | what it knows |
| `capabilities` | `capabilities` | declared A2A surface |
| `network` | `entitlements.network.outbound.mode` + `allow_hosts` | "restricted — no hosts allowed" / "allowlist: api.x.com" / "unrestricted" |
| `filesystem` | `entitlements.filesystem.read/write/deny` | which paths it can read, write, and is refused |
| `processes` | `entitlements.processes.spawn` | whether it can run programs, and which |
| `limits` | `entitlements.limits` | memory / fd / process ceilings |
| `hitl` | `hitl` | when it stops to ask |

Fleets derive the equivalent from `fleet.yaml`: members, router, goal, and the
loop's budget and guards.

### 2. A per-item endpoint, not a fatter index

`GET /api/v1/core/catalog/{id}` returns one item plus its `capabilities` block.

The index stays the list's payload: a Library page renders cards from it and
should not carry every item's permission table to do so. The detail page fetches
one item when it is opened. This also gives the detail route its own reason to
exist — today it is a filter over the list.

The download route already occupies `/catalog/{id}/download`, and chi routes the
exact path before the wildcard (there is a comment in `server.go` about exactly
this ordering), so the new route sits beside it without ambiguity.

### 3. The dashboard renders two questions

The panel answers, in this order:

1. **What can it do** — model, skills, MCP servers, capabilities.
2. **What can it touch** — network, filesystem, processes, limits, HITL.

The second is the one that matters for trust, and it is the one the page cannot
show today. Where an entitlement is empty, say so plainly ("no filesystem
access") rather than omitting the row: an absent row reads as "unknown", and
"this agent cannot read your files" is the most reassuring thing the page can
say.

## Failure modes

| situation | behavior |
|---|---|
| the server has no `capabilities` for an item (older deployment, or an item published before this lands) | the detail page renders exactly what it renders today — description and install command — with no empty panel and no error. **This is not optional:** the same version skew already bit us today, when a dashboard filtered on a field the deployed API did not send and rendered an empty list that read as "nothing published". |
| `profile.yaml` is missing a field | the build emits the fields it has. A partial summary is honest; a build failure over a missing optional field is not. |
| a field's shape is unrecognised (a new entitlement kind) | the build emits it verbatim under a generic label rather than dropping it silently. A permission the panel cannot name is still a permission the user should see. |
| the item is a fleet | the same endpoint, a different derivation. The panel's two questions are the same. |

## Testing

- **Build:** a fixture profile with permissive entitlements (unrestricted
  network, filesystem write, spawn any) and one with the restrictive defaults —
  assert the derived summary distinguishes them. A summary that renders the same
  for both is the failure this test exists to catch.
- **Build:** a profile missing `mcp_servers` / `skills` entirely — assert the
  build succeeds and the summary omits those rows.
- **Server:** `GET /catalog/{id}` for a known id returns the capabilities block;
  for an unknown id, 404 — and the response still excludes `storage_key`,
  `sha256`, `size` (the existing exact-key assertion pattern).
- **Dashboard:** an item without a capabilities block renders the current page
  with no empty panel.

## Not in v1

- **A README per item.** The derived summary answers "what will this do to my
  machine", which is the question that blocks a decision. Prose can come later
  as an optional field, and it will be believed more once the derived facts sit
  beside it.
- **Rendering the agent's system prompt.** It is in the bundle
  (`prompt.md`), and it is the most revealing thing about an agent's behaviour —
  but it is also the item's substance, and publishing it in full on a page that
  precedes purchase is a separate product decision, not a technical one.
- **Diffing capabilities between versions.** Useful ("this update now wants
  network access"), and it needs version history the catalog does not keep yet.
