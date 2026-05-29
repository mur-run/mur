# Fleet-Sync (Pro) — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the migration-independent foundation of Pro cross-device fleet-sync: a per-user (not team) sync of **agent profiles + model bindings** over a new `/api/v1/core/fleet/` endpoint, gated by the user's Pro plan, with version/conflict handling and per-device identity-key generation.

**Architecture:** The Go `mur-server` gains a user-scoped, versioned opaque-blob store (`fleet_entities`) plus push/pull handlers mirroring the existing team pattern-sync. The Rust client (`mur-core`) gains a fleet manifest + `build_fleet_changes`/`apply_fleet_pull`, reusing the pattern-sync conflict-retry loop. Secrets never leave the device (model bindings already store `SecretRef`, not plaintext); the Ed25519 private key is already a separate file, so the synced profile excludes it; a device missing a local key for a pulled agent generates its own (per-device key, shared owner).

**Tech Stack:** Go (chi router, postgres, sqlc-style store) on the server; Rust 2024 (`mur-core`, `mur-common`, `reqwest`, `serde`, `clap`) on the client.

**Scope boundary:** This plan covers **Phase 1 only**. Phase 2 (syncing the unified skill corpus `~/.mur/skills/<name>/` via `events.jsonl` set-union + re-reduce) is **blocked on the Pattern→Skill migration** (`2026-05-28-mur-notes-design.md` + Workflow Engine v2) and gets its own plan once that migration's reducer/event schema is final. See spec §10.

**Spec:** `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`

---

## File Structure

**Rust — `mur-common`:**
- Modify `mur-common/src/sync_types.rs` — add fleet DTOs (`FleetEntityType`, `FleetEntity`, `FleetChange`, `FleetPushRequest`, `FleetPushResponse`, `FleetPullResponse`).

**Rust — `mur-core`:**
- Create `mur-core/src/cmd/fleet_sync.rs` — fleet manifest, `build_fleet_changes`, `apply_fleet_pull`, push/pull flow, per-device key generation. (Keeps `sync_cmd.rs` under the 800-line rule; fleet logic is a sibling module.)
- Modify `mur-core/src/auth.rs` — add `fetch_effective_plan()` helper (GET `/api/v1/core/auth/me`).
- Modify `mur-core/src/cli/mod.rs` — add `SyncAction::Fleet { direction, force_local }` + extend `mur sync status`.
- Modify `mur-core/src/cmd/sync_cmd.rs` — dispatch the new `Fleet` action (thin call into `fleet_sync`).

**Go — `mur-server`:**
- Create `internal/store/postgres/migrations/<n>_fleet_entities.sql` — `fleet_entities` table.
- Modify `internal/models/models.go` — add `FleetEntity` model.
- Create `internal/store/postgres/fleet_store.go` — `SaveFleetEntities`, `GetFleetEntities`, `MaxFleetVersion`.
- Create `internal/services/fleet_service.go` — `Push`, `Pull` (user-scoped, base_version conflict).
- Create `internal/api/handlers/fleet.go` — `Push`, `Pull` handlers + Pro gate.
- Modify `internal/api/server.go` — register `/api/v1/core/fleet/*` under the protected group.

---

## Task 1: Fleet sync DTOs (mur-common)

**Files:**
- Modify: `mur-common/src/sync_types.rs` (append after the pattern DTOs, ~line 60)
- Test: `mur-common/src/sync_types.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Append to `mur-common/src/sync_types.rs`:

```rust
#[cfg(test)]
mod fleet_tests {
    use super::*;

    #[test]
    fn fleet_push_request_roundtrips() {
        let req = FleetPushRequest {
            base_version: 7,
            entity_type: FleetEntityType::AgentProfile,
            changes: vec![FleetChange {
                action: "upsert".into(),
                logical_id: "agent-abc".into(),
                content_hash: "deadbeef".into(),
                payload: Some("name: scout\n".into()),
            }],
            force_local: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FleetPushRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.base_version, 7);
        assert_eq!(back.entity_type, FleetEntityType::AgentProfile);
        assert_eq!(back.changes[0].logical_id, "agent-abc");
    }

    #[test]
    fn fleet_entity_type_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&FleetEntityType::ModelBinding).unwrap(),
            "\"model_binding\""
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common fleet_tests`
Expected: FAIL — `cannot find type FleetPushRequest`.

- [ ] **Step 3: Add the DTOs**

Append to `mur-common/src/sync_types.rs` (above the test module):

```rust
/// Kinds of fleet entity synced per-user across devices (Phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetEntityType {
    AgentProfile,
    ModelBinding,
}

impl FleetEntityType {
    /// URL path segment used in `/api/v1/core/fleet/<segment>`.
    pub fn path_segment(self) -> &'static str {
        match self {
            Self::AgentProfile => "agent_profile",
            Self::ModelBinding => "model_binding",
        }
    }
}

/// One create/update/delete of a fleet entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetChange {
    /// "upsert" | "delete"
    pub action: String,
    /// Stable logical id: agent UUIDv7 for profiles, model key for bindings.
    pub logical_id: String,
    /// SHA-256 of the canonical payload (empty for deletes).
    pub content_hash: String,
    /// Canonical YAML/JSON payload (None for deletes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

/// One entity returned by a fleet pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetEntity {
    pub logical_id: String,
    pub content_hash: String,
    pub version: i64,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPushRequest {
    pub base_version: i64,
    pub entity_type: FleetEntityType,
    pub changes: Vec<FleetChange>,
    #[serde(default)]
    pub force_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPushResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetPullResponse {
    pub entities: Vec<FleetEntity>,
    pub version: i64,
}
```

Confirm `use serde::{Serialize, Deserialize};` is already imported at the top of the file (it is — the pattern DTOs use it).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common fleet_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/sync_types.rs
git commit -m "feat(sync): fleet entity DTOs (profile + model binding)"
```

---

## Task 2: `fleet_entities` storage (Go server)

**Files:**
- Create: `internal/store/postgres/migrations/<next-number>_fleet_entities.sql`
- Modify: `internal/models/models.go` (add `FleetEntity` near the `Pattern` model, ~line 245)
- Create: `internal/store/postgres/fleet_store.go`
- Test: `internal/store/postgres/fleet_store_test.go`

> Mirror the existing `pattern_store.go` for connection/test harness conventions. Find the highest-numbered file in `internal/store/postgres/migrations/` and use the next integer.

- [ ] **Step 1: Write the migration**

Create `internal/store/postgres/migrations/<n>_fleet_entities.sql`:

```sql
-- Per-user (not team) fleet entity sync. Server treats payload as an opaque
-- versioned blob; all schema-aware merge happens on the Rust client.
CREATE TABLE IF NOT EXISTS fleet_entities (
    id            BIGSERIAL PRIMARY KEY,
    user_id       TEXT        NOT NULL,
    entity_type   TEXT        NOT NULL,   -- 'agent_profile' | 'model_binding'
    logical_id    TEXT        NOT NULL,   -- agent UUIDv7 / model key
    content_hash  TEXT        NOT NULL,
    payload       TEXT,                   -- canonical YAML/JSON; NULL for tombstones
    deleted       BOOLEAN     NOT NULL DEFAULT FALSE,
    version       BIGINT      NOT NULL,   -- monotonic per (user_id, entity_type)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, entity_type, logical_id)
);

CREATE INDEX IF NOT EXISTS idx_fleet_entities_user_type_version
    ON fleet_entities (user_id, entity_type, version);
```

- [ ] **Step 2: Add the model**

In `internal/models/models.go` after the `Pattern` struct:

```go
// FleetEntity is a per-user, opaque versioned blob synced across a user's
// devices. The server never parses Payload.
type FleetEntity struct {
    ID          int64     `db:"id" json:"id"`
    UserID      string    `db:"user_id" json:"user_id"`
    EntityType  string    `db:"entity_type" json:"entity_type"`
    LogicalID   string    `db:"logical_id" json:"logical_id"`
    ContentHash string    `db:"content_hash" json:"content_hash"`
    Payload     *string   `db:"payload" json:"payload,omitempty"`
    Deleted     bool      `db:"deleted" json:"deleted"`
    Version     int64     `db:"version" json:"version"`
    CreatedAt   time.Time `db:"created_at" json:"created_at"`
    UpdatedAt   time.Time `db:"updated_at" json:"updated_at"`
}
```

- [ ] **Step 3: Write the failing store test**

Create `internal/store/postgres/fleet_store_test.go` (follow `pattern_store_test.go`'s DB setup helper, e.g. `newTestStore(t)`):

```go
func TestFleetStore_SaveAndGetSince(t *testing.T) {
    st := newTestStore(t)
    ctx := context.Background()
    body := "name: scout\n"

    // First save → version 1
    v, err := st.SaveFleetEntities(ctx, "user-1", "agent_profile", 0, []models.FleetEntity{
        {LogicalID: "agent-a", ContentHash: "h1", Payload: &body},
    })
    require.NoError(t, err)
    require.Equal(t, int64(1), v)

    // Pull since 0 → returns the entity at version 1
    ents, maxV, err := st.GetFleetEntities(ctx, "user-1", "agent_profile", 0)
    require.NoError(t, err)
    require.Len(t, ents, 1)
    require.Equal(t, int64(1), maxV)
    require.Equal(t, "agent-a", ents[0].LogicalID)

    // Pull since 1 → nothing new
    ents, _, err = st.GetFleetEntities(ctx, "user-1", "agent_profile", 1)
    require.NoError(t, err)
    require.Empty(t, ents)
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `go test ./internal/store/postgres/ -run TestFleetStore_SaveAndGetSince`
Expected: FAIL — `st.SaveFleetEntities undefined`.

- [ ] **Step 5: Implement the store**

Create `internal/store/postgres/fleet_store.go`:

```go
package postgres

import (
    "context"

    "github.com/.../internal/models" // match the import path used in pattern_store.go
)

// MaxFleetVersion returns the current max version for a (user, type) pair (0 if none).
func (s *Store) MaxFleetVersion(ctx context.Context, userID, entityType string) (int64, error) {
    var v int64
    err := s.db.GetContext(ctx, &v,
        `SELECT COALESCE(MAX(version), 0) FROM fleet_entities
         WHERE user_id=$1 AND entity_type=$2`, userID, entityType)
    return v, err
}

// SaveFleetEntities upserts the given entities at maxVersion+1 and returns the new version.
// Caller is responsible for the base_version optimistic-concurrency check.
func (s *Store) SaveFleetEntities(ctx context.Context, userID, entityType string,
    _ int64, ents []models.FleetEntity) (int64, error) {
    tx, err := s.db.BeginTxx(ctx, nil)
    if err != nil {
        return 0, err
    }
    defer tx.Rollback()

    var cur int64
    if err := tx.GetContext(ctx, &cur,
        `SELECT COALESCE(MAX(version),0) FROM fleet_entities
         WHERE user_id=$1 AND entity_type=$2`, userID, entityType); err != nil {
        return 0, err
    }
    next := cur + 1

    for _, e := range ents {
        if _, err := tx.ExecContext(ctx,
            `INSERT INTO fleet_entities
               (user_id, entity_type, logical_id, content_hash, payload, deleted, version)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (user_id, entity_type, logical_id) DO UPDATE
               SET content_hash=$4, payload=$5, deleted=$6, version=$7, updated_at=now()`,
            userID, entityType, e.LogicalID, e.ContentHash, e.Payload, e.Deleted, next,
        ); err != nil {
            return 0, err
        }
    }
    if err := tx.Commit(); err != nil {
        return 0, err
    }
    return next, nil
}

// GetFleetEntities returns entities with version > since, plus the current max version.
func (s *Store) GetFleetEntities(ctx context.Context, userID, entityType string,
    since int64) ([]models.FleetEntity, int64, error) {
    var ents []models.FleetEntity
    if err := s.db.SelectContext(ctx, &ents,
        `SELECT * FROM fleet_entities
         WHERE user_id=$1 AND entity_type=$2 AND version > $3
         ORDER BY version ASC`, userID, entityType, since); err != nil {
        return nil, 0, err
    }
    maxV, err := s.MaxFleetVersion(ctx, userID, entityType)
    return ents, maxV, err
}
```

Adjust `s.db` / `Store` receiver names to match `pattern_store.go`.

- [ ] **Step 6: Run test to verify it passes**

Run: `go test ./internal/store/postgres/ -run TestFleetStore_SaveAndGetSince`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add internal/store/postgres/migrations/ internal/models/models.go internal/store/postgres/fleet_store.go internal/store/postgres/fleet_store_test.go
git commit -m "feat(fleet): per-user fleet_entities store with versioning"
```

---

## Task 3: Fleet service with base_version conflict (Go server)

**Files:**
- Create: `internal/services/fleet_service.go`
- Test: `internal/services/fleet_service_test.go`

- [ ] **Step 1: Write the failing test**

Create `internal/services/fleet_service_test.go`:

```go
func TestFleetService_PushConflictOnStaleBase(t *testing.T) {
    svc := newFleetServiceWithFakeStore() // store stub returning MaxFleetVersion=5
    resp, err := svc.Push(context.Background(), "user-1", "agent_profile",
        services.FleetPushRequest{BaseVersion: 3, Changes: nil})
    require.NoError(t, err)
    require.True(t, resp.Conflict)
    require.False(t, resp.OK)
}

func TestFleetService_PushAdvancesVersion(t *testing.T) {
    svc := newFleetServiceWithFakeStore() // MaxFleetVersion=5, SaveFleetEntities→6
    body := "name: scout\n"
    resp, err := svc.Push(context.Background(), "user-1", "agent_profile",
        services.FleetPushRequest{BaseVersion: 5, Changes: []models.FleetEntity{
            {LogicalID: "agent-a", ContentHash: "h", Payload: &body},
        }})
    require.NoError(t, err)
    require.True(t, resp.OK)
    require.Equal(t, int64(6), resp.Version)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test ./internal/services/ -run TestFleetService_Push`
Expected: FAIL — `services.FleetPushRequest undefined`.

- [ ] **Step 3: Implement the service**

Create `internal/services/fleet_service.go`:

```go
package services

import (
    "context"

    "github.com/.../internal/models"
)

type FleetStore interface {
    MaxFleetVersion(ctx context.Context, userID, entityType string) (int64, error)
    SaveFleetEntities(ctx context.Context, userID, entityType string, base int64, ents []models.FleetEntity) (int64, error)
    GetFleetEntities(ctx context.Context, userID, entityType string, since int64) ([]models.FleetEntity, int64, error)
}

type FleetService struct{ store FleetStore }

func NewFleetService(s FleetStore) *FleetService { return &FleetService{store: s} }

type FleetPushRequest struct {
    BaseVersion int64
    Changes     []models.FleetEntity
    ForceLocal  bool
}
type FleetPushResponse struct {
    OK       bool
    Version  int64
    Conflict bool
}

func (s *FleetService) Push(ctx context.Context, userID, entityType string,
    req FleetPushRequest) (*FleetPushResponse, error) {
    cur, err := s.store.MaxFleetVersion(ctx, userID, entityType)
    if err != nil {
        return nil, err
    }
    if !req.ForceLocal && req.BaseVersion != cur {
        return &FleetPushResponse{OK: false, Conflict: true}, nil
    }
    v, err := s.store.SaveFleetEntities(ctx, userID, entityType, req.BaseVersion, req.Changes)
    if err != nil {
        return nil, err
    }
    return &FleetPushResponse{OK: true, Version: v}, nil
}

func (s *FleetService) Pull(ctx context.Context, userID, entityType string,
    since int64) ([]models.FleetEntity, int64, error) {
    return s.store.GetFleetEntities(ctx, userID, entityType, since)
}
```

Add a `newFleetServiceWithFakeStore()` test helper implementing `FleetStore` in the test file.

- [ ] **Step 4: Run test to verify it passes**

Run: `go test ./internal/services/ -run TestFleetService_Push`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add internal/services/fleet_service.go internal/services/fleet_service_test.go
git commit -m "feat(fleet): push/pull service with base_version conflict"
```

---

## Task 4: Fleet handlers, routes, and Pro gate (Go server)

**Files:**
- Create: `internal/api/handlers/fleet.go`
- Modify: `internal/api/server.go` (register routes in the protected group, ~line 463 where pattern sync routes live)
- Test: `internal/api/handlers/fleet_test.go`

> The Pro gate uses the per-user plan. Reuse the existing user-plan accessor used by other handlers — find how `handlers/auth.go` builds the `/me` response (`EffectivePlan()` on the user model) and how a handler obtains the authenticated user from context (the `authMiddleware` sets it). Match that pattern.

- [ ] **Step 1: Write the failing handler test**

Create `internal/api/handlers/fleet_test.go`:

```go
func TestFleetHandler_PushRejectsNonPro(t *testing.T) {
    h := newFleetHandlerForTest(t, "free") // user plan = free
    rr := doAuthedPost(t, h.Push, "/api/v1/core/fleet/agent_profile",
        `{"base_version":0,"entity_type":"agent_profile","changes":[]}`)
    require.Equal(t, http.StatusPaymentRequired, rr.Code) // 402
}

func TestFleetHandler_PushAcceptsPro(t *testing.T) {
    h := newFleetHandlerForTest(t, "pro")
    rr := doAuthedPost(t, h.Push, "/api/v1/core/fleet/agent_profile",
        `{"base_version":0,"entity_type":"agent_profile","changes":[]}`)
    require.Equal(t, http.StatusOK, rr.Code)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test ./internal/api/handlers/ -run TestFleetHandler_Push`
Expected: FAIL — `newFleetHandlerForTest undefined` / handler missing.

- [ ] **Step 3: Implement the handler**

Create `internal/api/handlers/fleet.go`:

```go
package handlers

import (
    "encoding/json"
    "net/http"

    "github.com/go-chi/chi/v5"
    "github.com/.../internal/models"
    "github.com/.../internal/services"
)

type FleetHandler struct {
    svc *services.FleetService
}

func NewFleetHandler(svc *services.FleetService) *FleetHandler {
    return &FleetHandler{svc: svc}
}

// proAllowed reports whether the authenticated user may use fleet sync.
func proAllowed(u *models.User) bool {
    switch u.EffectivePlan() {
    case "pro", "team", "enterprise":
        return true
    default:
        return false
    }
}

type fleetPushBody struct {
    BaseVersion int64                `json:"base_version"`
    EntityType  string               `json:"entity_type"`
    Changes     []fleetChangeBody    `json:"changes"`
    ForceLocal  bool                 `json:"force_local"`
}
type fleetChangeBody struct {
    Action      string  `json:"action"`
    LogicalID   string  `json:"logical_id"`
    ContentHash string  `json:"content_hash"`
    Payload     *string `json:"payload,omitempty"`
}

// Push handles POST /api/v1/core/fleet/{entity_type}.
func (h *FleetHandler) Push(w http.ResponseWriter, r *http.Request) {
    user := userFromContext(r.Context()) // same accessor other protected handlers use
    if !proAllowed(user) {
        http.Error(w, "fleet sync requires a Pro plan", http.StatusPaymentRequired)
        return
    }
    entityType := chi.URLParam(r, "entity_type")
    var body fleetPushBody
    if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
        http.Error(w, "bad request", http.StatusBadRequest)
        return
    }
    ents := make([]models.FleetEntity, 0, len(body.Changes))
    for _, c := range body.Changes {
        ents = append(ents, models.FleetEntity{
            LogicalID:   c.LogicalID,
            ContentHash: c.ContentHash,
            Payload:     c.Payload,
            Deleted:     c.Action == "delete",
        })
    }
    resp, err := h.svc.Push(r.Context(), user.ID, entityType,
        services.FleetPushRequest{BaseVersion: body.BaseVersion, Changes: ents, ForceLocal: body.ForceLocal})
    if err != nil {
        http.Error(w, err.Error(), http.StatusInternalServerError)
        return
    }
    writeJSON(w, http.StatusOK, map[string]any{
        "ok": resp.OK, "version": resp.Version, "conflict": resp.Conflict,
    })
}

// Pull handles GET /api/v1/core/fleet/{entity_type}?since=N.
func (h *FleetHandler) Pull(w http.ResponseWriter, r *http.Request) {
    user := userFromContext(r.Context())
    if !proAllowed(user) {
        http.Error(w, "fleet sync requires a Pro plan", http.StatusPaymentRequired)
        return
    }
    entityType := chi.URLParam(r, "entity_type")
    since := parseInt64Query(r, "since", 0) // small helper; or strconv.ParseInt
    ents, maxV, err := h.svc.Pull(r.Context(), user.ID, entityType, since)
    if err != nil {
        http.Error(w, err.Error(), http.StatusInternalServerError)
        return
    }
    writeJSON(w, http.StatusOK, map[string]any{"entities": ents, "version": maxV})
}
```

Use the existing `writeJSON` / `userFromContext` helpers (grep `handlers/` for their real names; `sync.go` uses them).

- [ ] **Step 4: Register routes**

In `internal/api/server.go`, inside the same protected `r.Group` that hosts the pattern sync routes (~line 463), add:

```go
fleetHandler := handlers.NewFleetHandler(services.NewFleetService(store))
r.Post("/core/fleet/{entity_type}", fleetHandler.Push)
r.Get("/core/fleet/{entity_type}", fleetHandler.Pull)
```

(Confirm the group is already mounted under `/api/v1`, so the literal here is `/core/fleet/...` — match how the pattern routes are written in that block.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `go test ./internal/api/handlers/ -run TestFleetHandler_Push`
Expected: PASS (2 tests). Then `go build ./...` to confirm route wiring compiles.

- [ ] **Step 6: Commit**

```bash
git add internal/api/handlers/fleet.go internal/api/handlers/fleet_test.go internal/api/server.go
git commit -m "feat(fleet): push/pull handlers + Pro gate + routes"
```

---

## Task 5: Client Pro-entitlement check (mur-core)

**Files:**
- Modify: `mur-core/src/auth.rs` (add `fetch_effective_plan`)
- Test: `mur-core/tests/cli_fleet.rs` (new integration test file using `wiremock`, mirroring `tests/cli_drafts.rs`)

- [ ] **Step 1: Write the failing test**

Create `mur-core/tests/cli_fleet.rs` (mirror `tests/cli_drafts.rs` mock-server setup):

```rust
// Mocks GET /api/v1/core/auth/me and asserts fetch_effective_plan parses it.
#[tokio::test]
async fn fetch_effective_plan_reads_me() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/auth/me"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "effective_plan": "pro",
            "trial_active": false
        })))
        .mount(&server)
        .await;

    let plan = mur_core::auth::fetch_effective_plan(&server.uri(), "tok")
        .await
        .unwrap();
    assert_eq!(plan, "pro");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cli_fleet fetch_effective_plan_reads_me`
Expected: FAIL — `function fetch_effective_plan not found`.

- [ ] **Step 3: Implement the helper**

In `mur-core/src/auth.rs`:

```rust
#[derive(serde::Deserialize)]
struct MeResponse {
    effective_plan: String,
}

/// GET /api/v1/core/auth/me and return the user's effective plan
/// ("free" | "trial" | "pro" | "team" | "enterprise").
pub async fn fetch_effective_plan(base: &str, token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/v1/core/auth/me", base))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<MeResponse>()
        .await?;
    Ok(resp.effective_plan)
}

/// True when the plan permits Pro features (fleet sync).
pub fn plan_allows_fleet(plan: &str) -> bool {
    matches!(plan, "pro" | "team" | "enterprise")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --test cli_fleet fetch_effective_plan_reads_me`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/auth.rs mur-core/tests/cli_fleet.rs
git commit -m "feat(fleet): client reads effective_plan from /auth/me"
```

---

## Task 6: Fleet manifest + `build_fleet_changes` for agent profiles (mur-core)

**Files:**
- Create: `mur-core/src/cmd/fleet_sync.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub(crate) mod fleet_sync;`)
- Test: `mur-core/src/cmd/fleet_sync.rs` (inline `#[cfg(test)]`)

> Mirror `sync_cmd.rs::build_sync_changes` (line 728) and its `.sync_manifest.json` format. Profiles live at `~/.mur/agents/<slug>/profile.yaml`. The private key is in a separate `identity.key` file and is **never** read here, so the profile payload is key-free by construction.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet_sync.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn build_changes_detects_new_and_changed_profiles() {
        let mur = tempdir().unwrap();
        let agents = mur.path().join("agents");
        fs::create_dir_all(agents.join("scout")).unwrap();
        fs::write(agents.join("scout/profile.yaml"), "id: agent-scout\nname: scout\n").unwrap();
        // also a stray private key that must NOT be included
        fs::write(agents.join("scout/identity.key"), b"\x00\x01secret").unwrap();

        let manifest = mur.path().join(".fleet_manifest.json");
        let changes = build_fleet_profile_changes(mur.path(), &manifest).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "upsert");
        assert_eq!(changes[0].logical_id, "agent-scout");
        let payload = changes[0].payload.as_ref().unwrap();
        assert!(payload.contains("name: scout"));
        assert!(!payload.contains("secret")); // key file never included
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet_sync::tests::build_changes_detects`
Expected: FAIL — `build_fleet_profile_changes not found`.

- [ ] **Step 3: Implement manifest + change builder**

At the top of `mur-core/src/cmd/fleet_sync.rs`:

```rust
use anyhow::Result;
use mur_common::sync_types::FleetChange;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// `{ logical_id: { content_hash, version } }` keyed per entity type.
type FleetManifest = BTreeMap<String, FleetManifestEntry>;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct FleetManifestEntry {
    content_hash: String,
    #[serde(default)]
    version: i64,
}

fn load_manifest(path: &Path) -> FleetManifest {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Read the `id:` field from a profile.yaml body (its stable logical id).
fn profile_logical_id(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("id:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Build fleet changes for agent profiles by diffing `~/.mur/agents/*/profile.yaml`
/// against the manifest. Never reads `identity.key`.
pub fn build_fleet_profile_changes(mur_dir: &Path, manifest_path: &Path) -> Result<Vec<FleetChange>> {
    let manifest = load_manifest(manifest_path);
    let agents_dir = mur_dir.join("agents");
    let mut changes = Vec::new();
    if !agents_dir.exists() {
        return Ok(changes);
    }
    for entry in std::fs::read_dir(&agents_dir)? {
        let dir = entry?.path();
        let profile = dir.join("profile.yaml");
        if !profile.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&profile)?;
        let Some(id) = profile_logical_id(&body) else { continue };
        let ch = hash(&body);
        if manifest.get(&id).map(|m| m.content_hash.as_str()) != Some(ch.as_str()) {
            changes.push(FleetChange {
                action: "upsert".into(),
                logical_id: id,
                content_hash: ch,
                payload: Some(body),
            });
        }
    }
    Ok(changes)
}
```

Add to `mur-core/Cargo.toml` if not present: `sha2` and `tempfile` (dev). Check first — `sha2` is likely already a dependency.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet_sync::tests::build_changes_detects`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet_sync.rs mur-core/src/cmd/mod.rs mur-core/Cargo.toml
git commit -m "feat(fleet): manifest + profile change builder (key-free payload)"
```

---

## Task 7: Model-binding change builder (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/fleet_sync.rs`
- Test: same file inline tests

> `models.yaml` entries (`ModelEntry`) hold `secret: Option<SecretRef>`, which serializes as `env:VAR` / `keychain:svc/acct` etc. — a reference, never plaintext. So serializing a `ModelEntry` for sync is inherently ref-only; this task just confirms and wires it.

- [ ] **Step 1: Write the failing test**

Add to `fleet_sync.rs` tests:

```rust
#[test]
fn build_model_binding_changes_keeps_secret_as_ref() {
    use mur_common::model::{ModelEntry, ModelRegistry};
    use mur_common::secret::SecretRef;

    let mut reg = ModelRegistry::default();
    reg.models.insert("gpt5".into(), ModelEntry {
        provider: "openai".into(),
        model: "gpt-5".into(),
        secret: Some(SecretRef::Keychain { service: "mur".into(), account: "openai".into() }),
        ..Default::default()
    });

    let changes = build_fleet_model_changes(&reg, &Default::default()).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].logical_id, "gpt5");
    let payload = changes[0].payload.as_ref().unwrap();
    assert!(payload.contains("keychain")); // ref form
    assert!(!payload.to_lowercase().contains("sk-")); // no plaintext key
}
```

(Adjust `ModelEntry { .. }` construction to its real field set / `Default` availability — confirm with `mur-common/src/model.rs`. If `ModelEntry` lacks `Default`, build it via the registry's normal constructor.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet_sync::tests::build_model_binding`
Expected: FAIL — `build_fleet_model_changes not found`.

- [ ] **Step 3: Implement**

Add to `fleet_sync.rs`:

```rust
use mur_common::model::ModelRegistry;

/// Build fleet changes for model bindings by diffing models.yaml entries
/// against the manifest. Secrets are `SecretRef`s (env/keychain/file/cmd) —
/// serialized as references, so no plaintext leaves the device.
pub fn build_fleet_model_changes(reg: &ModelRegistry, manifest: &FleetManifest) -> Result<Vec<FleetChange>> {
    let mut changes = Vec::new();
    for (key, entry) in &reg.models {
        let body = serde_yaml::to_string(entry)?;
        let ch = hash(&body);
        if manifest.get(key).map(|m| m.content_hash.as_str()) != Some(ch.as_str()) {
            changes.push(FleetChange {
                action: "upsert".into(),
                logical_id: key.clone(),
                content_hash: ch,
                payload: Some(body),
            });
        }
    }
    Ok(changes)
}
```

Change `load_manifest` to be reused; signature here takes `&FleetManifest` so callers load once per entity type.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet_sync::tests::build_model_binding`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet_sync.rs
git commit -m "feat(fleet): model-binding change builder (ref-only secrets)"
```

---

## Task 8: Apply pull + per-device key generation (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/fleet_sync.rs`
- Test: same file inline tests

> On pull, write the profile to `~/.mur/agents/<slug>/profile.yaml`. If that agent has **no** local `identity.key`, generate a fresh Ed25519 key (key_version 0) so each device has its own signing key under the shared owner. Reuse the keygen used by `mur agent create` — grep `mur-core`/`mur-agent-runtime` for the Ed25519 generation helper (likely in an `identity`/`rekey` module) and call it; do not hand-roll crypto.

- [ ] **Step 1: Write the failing test**

Add to `fleet_sync.rs` tests:

```rust
#[test]
fn apply_pull_writes_profile_and_generates_missing_key() {
    use mur_common::sync_types::{FleetEntity, FleetEntityType};
    let mur = tempdir().unwrap();
    let ent = FleetEntity {
        logical_id: "agent-scout".into(),
        content_hash: "h".into(),
        version: 1,
        deleted: false,
        payload: Some("id: agent-scout\nname: scout\n".into()),
    };
    let report = apply_fleet_pull(mur.path(), FleetEntityType::AgentProfile, &[ent]).unwrap();

    let slug = "scout"; // derive slug from name/id — see impl note
    assert!(mur.path().join(format!("agents/{slug}/profile.yaml")).exists());
    assert!(mur.path().join(format!("agents/{slug}/identity.key")).exists());
    assert_eq!(report.written, 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet_sync::tests::apply_pull_writes_profile`
Expected: FAIL — `apply_fleet_pull not found`.

- [ ] **Step 3: Implement**

Add to `fleet_sync.rs`:

```rust
use mur_common::sync_types::{FleetEntity, FleetEntityType};

#[derive(Default, Debug)]
pub struct ApplyReport {
    pub written: usize,
    pub keys_generated: usize,
    /// logical_ids whose model-binding secret-ref does not resolve locally.
    pub unresolved_secrets: Vec<String>,
}

/// Write pulled entities to disk. For agent profiles, ensure a per-device
/// identity key exists (generate one if absent).
pub fn apply_fleet_pull(mur_dir: &Path, etype: FleetEntityType, ents: &[FleetEntity]) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    match etype {
        FleetEntityType::AgentProfile => {
            for e in ents {
                if e.deleted { continue; }
                let Some(body) = &e.payload else { continue };
                let slug = profile_slug(body); // name-derived dir slug; mirror agent create
                let dir = mur_dir.join("agents").join(&slug);
                std::fs::create_dir_all(&dir)?;
                // atomic write (temp + rename), consistent with store/yaml.rs
                write_atomic(&dir.join("profile.yaml"), body.as_bytes())?;
                report.written += 1;
                if !dir.join("identity.key").exists() {
                    generate_device_identity_key(&dir)?; // reuses agent-create keygen
                    report.keys_generated += 1;
                }
            }
        }
        FleetEntityType::ModelBinding => {
            // merge each entry into ~/.mur/models.yaml; flag unresolved secret-refs
            apply_model_bindings(mur_dir, ents, &mut report)?;
        }
    }
    Ok(report)
}
```

Implement `profile_slug`, `write_atomic` (or reuse the helper in `store/yaml.rs`), `generate_device_identity_key` (call the existing Ed25519 keygen), and `apply_model_bindings` (deserialize each payload into `ModelEntry`, insert into the loaded `ModelRegistry`, save; for each, attempt `entry.secret.as_ref().map(|s| s.resolve())` and on `Err` push `logical_id` to `report.unresolved_secrets`). Keep each helper small and unit-tested if non-trivial.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet_sync::tests::apply_pull_writes_profile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet_sync.rs
git commit -m "feat(fleet): apply pull + per-device identity key generation"
```

---

## Task 9: Push/pull flow with conflict retry (mur-core)

**Files:**
- Modify: `mur-core/src/cmd/fleet_sync.rs`
- Test: `mur-core/tests/cli_fleet.rs` (wiremock round-trip)

> Mirror the pattern push retry loop in `sync_cmd.rs` (~lines 320–430): push with `base_version`; on `conflict: true`, pull latest, resolve (profile = LWW unless `--force-local`; manifest update), rebuild changes, retry once. Persist server version to `~/.mur/.fleet_version_<entity_type>`.

- [ ] **Step 1: Write the failing test**

Add to `mur-core/tests/cli_fleet.rs`: a wiremock scenario where the first `POST /api/v1/core/fleet/agent_profile` returns `{"ok":false,"conflict":true}`, the subsequent `GET .../agent_profile?since=0` returns one entity at version 4, and the retried `POST` returns `{"ok":true,"version":5}`. Assert `fleet_push(...)` returns `Ok(5)` and writes `.fleet_version_agent_profile` = `5`.

```rust
#[tokio::test]
async fn fleet_push_resolves_conflict_then_succeeds() {
    // ... mount the three mocks described above against a MockServer ...
    let mur = tempdir().unwrap();
    // seed one local profile so there is a change to push
    // ...
    let v = mur_core::cmd::fleet_sync::fleet_push(
        &server.uri(), "tok", mur.path(),
        mur_common::sync_types::FleetEntityType::AgentProfile, false,
    ).await.unwrap();
    assert_eq!(v, 5);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cli_fleet fleet_push_resolves_conflict`
Expected: FAIL — `fleet_push not found`.

- [ ] **Step 3: Implement push + pull**

Add `pub async fn fleet_pull(...)` and `pub async fn fleet_push(...)` to `fleet_sync.rs`:

```rust
fn version_path(mur_dir: &Path, etype: FleetEntityType) -> std::path::PathBuf {
    mur_dir.join(format!(".fleet_version_{}", etype.path_segment()))
}
fn read_version(mur_dir: &Path, etype: FleetEntityType) -> i64 {
    std::fs::read_to_string(version_path(mur_dir, etype)).ok()
        .and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

pub async fn fleet_pull(base: &str, token: &str, mur_dir: &Path, etype: FleetEntityType) -> Result<ApplyReport> {
    let since = read_version(mur_dir, etype);
    let url = format!("{}/api/v1/core/fleet/{}?since={}", base, etype.path_segment(), since);
    let resp: mur_common::sync_types::FleetPullResponse = reqwest::Client::new()
        .get(url).bearer_auth(token).send().await?.error_for_status()?.json().await?;
    let report = apply_fleet_pull(mur_dir, etype, &resp.entities)?;
    std::fs::write(version_path(mur_dir, etype), resp.version.to_string())?;
    Ok(report)
}

pub async fn fleet_push(base: &str, token: &str, mur_dir: &Path,
    etype: FleetEntityType, force_local: bool) -> Result<i64> {
    use mur_common::sync_types::{FleetPushRequest, FleetPushResponse};
    let manifest_path = mur_dir.join(format!(".fleet_manifest_{}.json", etype.path_segment()));
    let manifest = load_manifest(&manifest_path);
    let changes = match etype {
        FleetEntityType::AgentProfile => build_fleet_profile_changes(mur_dir, &manifest_path)?,
        FleetEntityType::ModelBinding => {
            let reg = ModelRegistry::load_from(&mur_dir.join("models.yaml")).unwrap_or_default();
            build_fleet_model_changes(&reg, &manifest)?
        }
    };
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/core/fleet/{}", base, etype.path_segment());
    let mut base_version = read_version(mur_dir, etype);

    for attempt in 0..2 {
        let req = FleetPushRequest { base_version, entity_type: etype, changes: changes.clone(), force_local };
        let resp: FleetPushResponse = client.post(&url).bearer_auth(token).json(&req)
            .send().await?.error_for_status()?.json().await?;
        if let Some(v) = resp.version {
            std::fs::write(version_path(mur_dir, etype), v.to_string())?;
            update_fleet_manifest(&manifest_path, &changes, v)?;
            return Ok(v);
        }
        if resp.conflict.unwrap_or(false) && attempt == 0 && !force_local {
            fleet_pull(base, token, mur_dir, etype).await?; // LWW: server state pulled, manifest+version updated
            base_version = read_version(mur_dir, etype);
            continue;
        }
        anyhow::bail!("fleet push failed (conflict unresolved)");
    }
    anyhow::bail!("fleet push exhausted retries")
}
```

Implement `update_fleet_manifest` (write each change's `logical_id → { content_hash, version }`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core --test cli_fleet fleet_push_resolves_conflict`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/fleet_sync.rs mur-core/tests/cli_fleet.rs
git commit -m "feat(fleet): push/pull flow with conflict retry + version tracking"
```

---

## Task 10: CLI surface — `mur sync fleet` + `mur sync status` (mur-core)

**Files:**
- Modify: `mur-core/src/cli/mod.rs` (extend `SyncAction`, ~line 66)
- Modify: `mur-core/src/cmd/sync_cmd.rs` (dispatch `Fleet`; extend status)
- Test: `mur-core/tests/cli_fleet.rs` (CLI dispatch + entitlement gate)

- [ ] **Step 1: Write the failing test**

Add to `mur-core/tests/cli_fleet.rs`: with `/auth/me` mocked to `effective_plan: "free"`, invoking the fleet sync entry returns an error mentioning Pro. (Drive via the public dispatch fn, e.g. `sync_cmd::fleet_sync_cmd(direction, force_local).await`, with env pointing at the mock server and a fake token in the auth store — mirror how `cli_drafts.rs` injects base URL + token.)

```rust
#[tokio::test]
async fn fleet_sync_refused_for_free_plan() {
    // mock /auth/me → {"effective_plan":"free"}
    // ...
    let err = mur_core::cmd::sync_cmd::fleet_sync_cmd(
        mur_core::cmd::sync_cmd::DeviceSyncDirection::Both, false,
    ).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("pro"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core --test cli_fleet fleet_sync_refused_for_free_plan`
Expected: FAIL — `fleet_sync_cmd not found`.

- [ ] **Step 3: Add the clap subcommand**

In `mur-core/src/cli/mod.rs`, extend the `SyncAction` enum:

```rust
#[derive(Subcommand)]
pub enum SyncAction {
    // ... existing variants ...
    /// Sync your evolved fleet (agent profiles + model bindings) across devices (Pro).
    Fleet {
        #[arg(long, conflicts_with = "push")]
        pull: bool,
        #[arg(long)]
        push: bool,
        #[arg(long)]
        force_local: bool,
    },
}
```

- [ ] **Step 4: Implement dispatch + status**

In `mur-core/src/cmd/sync_cmd.rs`:

```rust
/// Entry for `mur sync fleet`. Gates on Pro, then syncs each entity type.
pub async fn fleet_sync_cmd(direction: DeviceSyncDirection, force_local: bool) -> anyhow::Result<()> {
    let (base, token) = resolve_server_and_token()?; // same helper device_sync uses
    let plan = crate::auth::fetch_effective_plan(&base, &token).await?;
    if !crate::auth::plan_allows_fleet(&plan) {
        anyhow::bail!("fleet sync requires a Pro plan (current: {plan}). Upgrade at https://app.mur.run");
    }
    let mur_dir = crate::default_mur_dir();
    use mur_common::sync_types::FleetEntityType::*;
    for etype in [AgentProfile, ModelBinding] {
        if matches!(direction, DeviceSyncDirection::Pull | DeviceSyncDirection::Both) {
            let r = crate::cmd::fleet_sync::fleet_pull(&base, &token, &mur_dir, etype).await?;
            for id in &r.unresolved_secrets {
                eprintln!("  ⚠ {id}: secret not resolvable on this device (agent will run unbound)");
            }
        }
        if matches!(direction, DeviceSyncDirection::Push | DeviceSyncDirection::Both) {
            crate::cmd::fleet_sync::fleet_push(&base, &token, &mur_dir, etype, force_local).await?;
        }
    }
    Ok(())
}
```

Wire the `Fleet { pull, push, force_local }` arm in the `mur sync` handler to call `fleet_sync_cmd` with the matching `DeviceSyncDirection`. Extend `mur sync status` to print, per entity type, the local `.fleet_version_*` vs a `?since=local` pull count, and list unresolved secrets (call a read-only variant that does not write).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core --test cli_fleet`
Then: `cargo build -p mur-core` and `cargo run -p mur-core -- sync fleet --help` (confirm help renders).
Expected: PASS; help shows `--pull/--push/--force-local`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/mod.rs mur-core/src/cmd/sync_cmd.rs mur-core/tests/cli_fleet.rs
git commit -m "feat(fleet): mur sync fleet CLI + Pro gate + status"
```

---

## Task 11: Degraded-mode integration test (mur-core)

**Files:**
- Test: `mur-core/tests/cli_fleet.rs`

- [ ] **Step 1: Write the test**

Add a wiremock round-trip: pull a `model_binding` whose payload references `keychain:mur/absent-account` (guaranteed unresolved in CI). Assert `fleet_pull` returns an `ApplyReport` with the binding's `logical_id` in `unresolved_secrets`, the entry is still written to `models.yaml`, and the call succeeds (no error).

```rust
#[tokio::test]
async fn missing_secret_ref_is_degraded_not_fatal() {
    // mock GET /api/v1/core/fleet/model_binding?since=0 →
    //   {"entities":[{"logical_id":"gpt5","content_hash":"h","version":1,"deleted":false,
    //     "payload":"provider: openai\nmodel: gpt-5\nsecret: keychain:mur/absent-account\n"}],
    //    "version":1}
    let report = mur_core::cmd::fleet_sync::fleet_pull(
        &server.uri(), "tok", mur.path(),
        mur_common::sync_types::FleetEntityType::ModelBinding,
    ).await.unwrap();
    assert!(report.unresolved_secrets.contains(&"gpt5".to_string()));
    assert!(mur.path().join("models.yaml").exists());
}
```

- [ ] **Step 2: Run test to verify it fails (or passes if Task 8 already covers it)**

Run: `cargo test -p mur-core --test cli_fleet missing_secret_ref_is_degraded`
Expected: FAIL if `apply_model_bindings` does not yet record unresolved refs; then fix `apply_model_bindings` to push to `report.unresolved_secrets`.

- [ ] **Step 3: Make it pass**

Ensure `apply_model_bindings` (Task 8) attempts `secret.resolve()` and records failures without erroring. Adjust until green.

- [ ] **Step 4: Run full fleet test suite + clippy/fmt**

Run:
```bash
cargo test -p mur-core --test cli_fleet
cargo test -p mur-common fleet_tests
cargo clippy -p mur-core -p mur-common -- -D warnings
cargo fmt --check
```
Expected: all PASS / clean.

- [ ] **Step 5: Commit**

```bash
git add mur-core/tests/cli_fleet.rs mur-core/src/cmd/fleet_sync.rs
git commit -m "test(fleet): degraded mode for unresolved secret-refs"
```

---

## Phase 2 (blocked — separate plan)

**Do not implement here.** Syncing the unified skill corpus (`~/.mur/skills/<name>/` = signed `skill.yaml` + derived `stats.yaml` + append-only `events.jsonl`) via **`events.jsonl` set-union + deterministic re-reduce** (spec §6.A) depends on the Pattern→Skill migration landing first (`Pattern` removed, `~/.mur/patterns/` deleted, the shared reducer + event schema finalized — `2026-05-28-mur-notes-design.md` + Workflow Engine v2). Once that migration is merged, write `docs/superpowers/plans/<date>-fleet-sync-pro-phase2.md` covering:
- A third `FleetEntityType::Skill` reusing the Phase 1 substrate (store, endpoints, manifest, version, Pro gate, CLI loop).
- Client-side `events.jsonl` union + re-reduce on conflict, and signed-`skill.yaml` LWW (spec §6.B).
- Tests: event-union commutativity/idempotency, signed-manifest integrity after LWW, two-device usage-history convergence.

---

## Self-Review

**Spec coverage (Phase 1 scope):**
- §3 entities (profiles + model bindings) → Tasks 6, 7, 8. Skill corpus → Phase 2 (correctly deferred).
- §4 substrate (extend server-sync, opaque blob, `/api/v1/core/fleet/`, Pro gate) → Tasks 2, 3, 4.
- §5 identity (no private key synced; per-device key) + secrets (ref-only) → Tasks 6 (key-free payload), 7 (SecretRef-only), 8 (per-device keygen + degraded), 11.
- §6 conflict (LWW on signed/profile blob; `--force-local`) → Task 9. Event-union (§6.A) → Phase 2.
- §7 CLI (`mur sync fleet`, status) → Task 10.
- §8 data flow (push diff/base_version/retry; pull since/apply/version) → Tasks 9, 10.
- §9 testing (secret-missing degraded, identity isolation, entitlement gate) → Tasks 5, 8, 11.

**Placeholder scan:** Each code step shows real code; helper fns that mirror existing pattern-sync are named with the file/line of their model. The few "match the existing helper name" notes point at concrete reference files (`sync.go`, `store/yaml.rs`, `sync_cmd.rs`) rather than leaving logic undefined.

**Type consistency:** `FleetEntityType`/`FleetChange`/`FleetPushRequest`/`FleetPushResponse`/`FleetPullResponse`/`FleetEntity` defined in Task 1 are used unchanged in Tasks 4, 8, 9. `ApplyReport.unresolved_secrets` defined in Task 8 is consumed in Tasks 10, 11. `path_segment()` (Task 1) is used in Tasks 4 (route param), 9 (URLs/version files).
