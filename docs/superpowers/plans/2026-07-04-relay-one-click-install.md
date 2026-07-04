# Relay One-Click Install Implementation Plan (4/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A logged-in Dashboard user clicks "Install to Hub" on a Skill/MCP/Workflow/Plugin card and the item arrives at their Mac's Hub behind a consent modal.

**Architecture:** Dashboard POST → mur-server relay `install_request` command to the user's connected Mac daemon (existing `Hub.SendCommand` machinery) → daemon writes an install-request event → Hub GUI live-tail opens the consent modal → on consent, Hub fetches the item with the configured API key and routes to the existing type-specific installer. Spec §B. Cross-repo: `mur-server` (Go + dashboard) and `mur` (daemon + hub-gui). **Depends on Plan 1 (origin stamping for skill installs).**

**Tech Stack:** Go (mur-server `internal/relay`, `internal/api/handlers`), Next.js dashboard, Rust (`mur-daemon/src/relay_client.rs`, `mur-hub-gui`).

## Global Constraints

- Fail-closed: the Hub NEVER auto-installs; every request passes fetch → preview → security-scan → consent modal.
- No queue in P1: no connected Mac ⇒ API returns 409 `hub_offline`; Dashboard shows it.
- mur-server repo conventions per `/Volumes/Firecuda4tb/Projects/mur-server/CLAUDE.md` (read it before Task 1).
- Rust side: same build/test constraints as Plan 1.

---

### Task 1 (mur-server, Go): `POST /api/v1/install-request`

**Files:**
- Create: `internal/api/handlers/install_request.go`
- Modify: `internal/api/server.go` (route registration, session-authenticated group)
- Test: `internal/api/handlers/install_request_test.go`

**Interfaces:**
- Consumes: existing session auth middleware (same as other logged-in dashboard APIs), `Hub.SendCommand(userID, action, params, timeout)` (`internal/relay/hub.go:254`).
- Produces: request `{ "type": "skill|mcp|workflow|plugin", "id": "mur-official/brainstorming" }`; responses `200 {"status":"delivered"}`, `409 {"error":"hub_offline"}`, `400` on bad type/id.

- [ ] **Step 1: Failing tests** — table-driven: (a) valid request with a fake hub that records the command → 200 and the hub saw `action="install_request"`, params containing type+id+requesting user; (b) hub returns no-agent error → 409; (c) `type: "exe"` → 400; (d) unauthenticated → 401 (middleware test, follow the repo's existing handler-test pattern).
- [ ] **Step 2:** `go test ./internal/api/handlers/ -run TestInstallRequest` → fail.
- [ ] **Step 3:** Implement: validate `type` against the four-value whitelist, validate `id` as `<publisher>/<name>` slug (lowercase, `[a-z0-9-]`, one slash), then `h.hub.SendCommand(user.ID, "install_request", params, 10*time.Second)`; map the "no agent connected" error to 409.
- [ ] **Step 4:** Tests pass; `go vet ./...`.
- [ ] **Step 5:** Commit in mur-server: `feat(api): install-request relay endpoint`.

### Task 2 (mur, Rust): daemon handles the `install_request` command

**Files:**
- Modify: `mur-daemon/src/relay_client.rs` (frame dispatch in the read loop of `run_once`)
- Create: `mur-core/src/install_request.rs` (event write + dedup) — declared in both `lib.rs` and `main.rs` module trees (known gotcha)
- Test: `#[cfg(test)]` in `install_request.rs`

**Interfaces:**
- Consumes: whatever envelope `Hub.SendCommand` wraps commands in — read `relay_handler.go`'s agent-side command marshaling FIRST and mirror its field names exactly; the daemon must reply with the ack shape `SendCommand` waits for (that's how "delivered" becomes true).
- Produces: `pub fn record_install_request(mur_home: &Path, req: &InstallRequest) -> Result<PathBuf>` appending a JSON line `{"kind":"install_request","type":…,"id":…,"requested_at":<unix>,"request_id":<uuid from server>}` to `<mur_home>/hub/install-requests.jsonl` (same dir family the Hub already tails for mobile events), deduped by `request_id`.

- [ ] **Step 1: Failing tests** — (a) `record_install_request` appends one line, fields round-trip; (b) same `request_id` twice appends once; (c) rejects `type` outside the four-value whitelist (defense in depth — the server validated, the daemon re-validates).
- [ ] **Step 2:** fail to compile.
- [ ] **Step 3:** Implement + wire the relay_client dispatch branch: parse command envelope → `record_install_request` → send ack. Unknown/invalid payload → log warn, ack with error, never crash the relay loop.
- [ ] **Step 4:** `cargo nextest run -p mur-core install_request` + `-p mur-daemon` PASS.
- [ ] **Step 5:** Commit: `feat(daemon): receive install_request over relay`.

### Task 3 (mur, Rust+TS): Hub consent modal + install routing

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs` (or a new `install_inbox.rs` if mcp_skills nears 800 lines): tauri commands `install_inbox_list`, `install_inbox_consent(request_id, approve)`
- Modify: Hub UI (mur-hub-gui/ui): watcher on `install-requests.jsonl` (same live-tail mechanism as mobile events), consent modal
- Test: Rust-side routing tests

**Interfaces:**
- Consumes: Task 2's jsonl; existing installers — skill: registry install path (`cmd/agent/skill.rs`, stamps origin); mcp: feather `mcp add-remote` engine; plugin: Hub plugin import; workflow: **new** `install_workflow(mur_home, yaml) -> Result<PathBuf>` (validate `Workflow` parse + reuse quill's security scan, write `~/.mur/workflows/<name>.yaml`, refuse overwrite without explicit flag).
- Produces: consent modal showing type, id, publisher, description, scan result, "official" badge when publisher == `mur-official`; Approve → download from mur-server with the configured API key (`mobile_relay.api_key` config) → route by type; Deny → mark the jsonl entry consumed. Processed request_ids recorded in `install-requests.done` so restarts don't re-prompt.

- [ ] **Step 1: Failing tests (Rust)** — (a) `install_workflow` writes a valid workflow yaml and refuses an invalid one; (b) refuses overwrite of an existing workflow; (c) route-by-type dispatch table rejects unknown type.
- [ ] **Step 2:** fail.
- [ ] **Step 3:** Implement Rust commands + routing; then UI modal (follow the existing HITL/consent modal component patterns in the Hub UI; reuse `modal.css` conventions).
- [ ] **Step 4:** `cargo nextest run` (hub manifest path) PASS; `npm run build` in ui.
- [ ] **Step 5:** Commit: `feat(hub): install-request consent modal + type routing`.

### Task 4 (mur-server, dashboard): "Install to Hub" buttons

**Files:**
- Modify: dashboard library pages (Skills / MCP Servers / Workflows cards — e.g. the MCP page shown in the design session screenshot) in `/Volumes/Firecuda4tb/Projects/mur-server/dashboard/`
- Test: component test per the dashboard's existing test setup (check for one before inventing)

- [ ] **Step 1:** Button on each card → `POST /api/v1/install-request`; states: loading → "Sent to your Hub ✓" / "Hub offline — is MUR running on your Mac?" (409) / error toast. Keep a "copy CLI command" secondary action (`mur agent skill install <id>` etc.).
- [ ] **Step 2:** `npm run build` green; commit in mur-server: `feat(dashboard): one-click Install to Hub`.

### Task 5: Live E2E

- [ ] With daemon connected to the relay (API key configured): click Install on a skill card → consent modal appears on the Hub → approve → `mur agent skill list mur` shows it origin-stamped → `mur skill upgrade --check` says UpToDate.
- [ ] Negative: stop the daemon → click → Dashboard shows hub-offline. Deny path: modal Deny → nothing installed, request marked done, no re-prompt after Hub restart.
