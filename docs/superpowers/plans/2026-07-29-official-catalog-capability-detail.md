# Official catalog capability detail — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An official agent's Library page says what it can do and what it can touch, derived from the artifact rather than written by hand.

**Architecture:** The catalog build already parses `profile.yaml` into `mur_common::AgentProfile` to construct the bundle; a pure function turns that same value into a flat `CapabilitySummary` which is stored in `index.json`. The server exposes one item at `GET /api/v1/core/catalog/{id}`, and the dashboard's detail page renders the summary — degrading to today's page when the field is absent.

**Tech Stack:** Rust (catalog build), Go + chi (server), Next.js/React + TypeScript (dashboard).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-29-official-catalog-capability-detail.md` in the `mur` repo. Read it before Task 1.
- **Three repositories.** Every task names its own; never edit outside it.
  - Build: `/Volumes/Firecuda4tb/Projects/official-sign` — despite the directory name this is a clone of `mur-run/official-catalog`; the tool lives in `tools/official-sign/`.
  - Server + dashboard: `/Volumes/Firecuda4tb/Projects/mur-server`.
- **The summary carries values, not sentences.** The build emits modes and lists (`"restricted"`, `["~/.ssh"]`); the UI owns the wording. Three languages must not each invent their own copy.
- **An item without a `capabilities` block must render exactly today's page** — no empty panel, no error. This is the version-skew rule from the spec, and it is what lets these tasks ship in any order.
- Rust edition 2024. Tests via `cargo test` in the catalog repo (it is not a nextest repo), `go test` in the server, `npx tsc --noEmit` + `npx eslint` in the dashboard.
- Commit per task; `git add` only the files the task names.

---

## File Structure

| File | Repo | Responsibility |
|---|---|---|
| `tools/official-sign/src/capability.rs` **(new)** | catalog | `CapabilitySummary` + the pure `derive()` from `AgentProfile` |
| `tools/official-sign/src/index.rs` | catalog | `IndexItem` gains `capabilities` |
| `tools/official-sign/src/sign.rs` | catalog | `build_official_muragent` returns the profile it already parsed |
| `tools/official-sign/src/main.rs` | catalog | threads the profile into `make_item` |
| `internal/api/handlers/official_catalog.go` | server | per-item handler + `capabilities` passthrough |
| `internal/services/officialcatalog/catalog.go` | server | `IndexItem` gains the block |
| `internal/api/server.go` | server | the new route |
| `dashboard/src/lib/library.ts` | server | fetch one item |
| `dashboard/src/app/(protected)/mur/library/[type]/[name]/page.tsx` | server | render the panel |

---

### Task 1: derive the summary (pure, catalog repo)

**Repo:** `/Volumes/Firecuda4tb/Projects/official-sign`

**Files:**
- Create: `tools/official-sign/src/capability.rs`
- Modify: `tools/official-sign/src/lib.rs` (add `pub mod capability;`)

**Interfaces:**
- Produces: `pub struct CapabilitySummary` (fields below) and `pub fn derive(profile: &AgentProfile) -> CapabilitySummary`.

- [ ] **Step 1: Write the failing tests**

Create `tools/official-sign/src/capability.rs` with only this test module plus the imports:

```rust
//! Capability summary derived from an agent's profile.
use mur_common::agent::AgentProfile;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    /// The restrictive shape the catalog's own researcher ships with.
    fn restrictive() -> AgentProfile {
        serde_yaml_ng::from_str(
            r#"
schema: 1
id: 00000000-0000-0000-0000-000000000000
name: researcher
display_name: Researcher
version: "1.0.0"
persona: { category: research, description: d, traits: {} }
sys_prompt_file: prompt.md
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
capabilities: ["a2a.message.send"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: ["~/.ssh"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
"#,
        )
        .expect("restrictive fixture must parse")
    }

    /// The same agent with every permission opened up.
    fn permissive() -> AgentProfile {
        let mut p = restrictive();
        p.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::Unrestricted;
        p.entitlements.network.outbound.allow_hosts = vec!["api.example.com".into()];
        p.entitlements.filesystem.read = vec!["~/Documents".into()];
        p.entitlements.filesystem.write = vec!["~/tmp".into()];
        p.entitlements.processes.spawn.mode = mur_common::agent::SpawnMode::Any;
        p.mcp_servers = vec![mur_common::agent::McpServerEntry {
            name: "filesystem".into(),
            ..Default::default()
        }];
        p.skills = vec!["deep-research".into()];
        p
    }

    #[test]
    fn a_permissive_profile_does_not_summarise_like_a_restrictive_one() {
        // The whole point of the panel: these two must never read the same.
        let r = derive(&restrictive());
        let p = derive(&permissive());
        assert_ne!(r, p);
        assert_eq!(r.network_mode, "restricted");
        assert_eq!(p.network_mode, "unrestricted");
        assert!(r.filesystem_write.is_empty());
        assert_eq!(p.filesystem_write, vec!["~/tmp".to_string()]);
        assert_eq!(r.spawn_mode, "allowlist");
        assert_eq!(p.spawn_mode, "any");
    }

    #[test]
    fn carries_the_lists_a_reader_needs() {
        let p = derive(&permissive());
        assert_eq!(p.mcp_servers, vec!["filesystem".to_string()]);
        assert_eq!(p.skills, vec!["deep-research".to_string()]);
        assert_eq!(p.capabilities, vec!["a2a.message.send".to_string()]);
        assert_eq!(p.network_allow_hosts, vec!["api.example.com".to_string()]);
        assert_eq!(p.filesystem_deny, vec!["~/.ssh".to_string()]);
    }

    #[test]
    fn model_reads_as_provider_slash_name_and_prefers_an_explicit_ref() {
        let mut p = restrictive();
        assert_eq!(derive(&p).model.as_deref(), Some("ollama/m"));
        p.model_ref = Some("anthropic_opus".into());
        assert_eq!(derive(&p).model.as_deref(), Some("anthropic_opus"));
    }

    #[test]
    fn limits_come_through_verbatim() {
        let s = derive(&restrictive());
        assert_eq!(s.memory_mb, 512);
    }
}
```

- [ ] **Step 2: Register the module and watch the tests fail**

Add to `tools/official-sign/src/lib.rs`:

```rust
pub mod capability;
```

Run: `cd tools/official-sign && cargo test capability`
Expected: FAIL — `cannot find function derive`.

- [ ] **Step 3: Implement the derivation**

Above the test module in `capability.rs`:

```rust
/// What an agent can do, and what it can touch — read off its profile.
///
/// Values, not sentences: the mode strings and path lists are what the profile
/// says, and the wording belongs to whatever renders them. Flat on purpose, so
/// the same shape survives JSON into Go and TypeScript without three nested
/// type definitions to keep in step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySummary {
    /// `provider/name`, or the registry alias when the profile names one.
    pub model: Option<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub capabilities: Vec<String>,
    /// `unrestricted` | `restricted` | `proxy-only` | `off`
    pub network_mode: String,
    pub network_allow_hosts: Vec<String>,
    pub filesystem_read: Vec<String>,
    pub filesystem_write: Vec<String>,
    pub filesystem_deny: Vec<String>,
    /// `allowlist` | `any` | `none` | `strict`
    pub spawn_mode: String,
    pub spawn_allowed: Vec<String>,
    pub memory_mb: u64,
}

/// Lowercase, hyphenated rendering of an enum whose Debug is CamelCase.
/// `ProxyOnly` → `proxy-only`. Derived rather than matched arm by arm so a new
/// variant appears in the summary instead of silently becoming "unknown" — an
/// unnamed permission is still a permission the reader should see.
fn kebab(v: impl std::fmt::Debug) -> String {
    let s = format!("{v:?}");
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

pub fn derive(profile: &AgentProfile) -> CapabilitySummary {
    let ent = &profile.entitlements;
    CapabilitySummary {
        model: profile.model_ref.clone().or_else(|| {
            let m = &profile.model;
            (!m.provider.is_empty() || !m.name.is_empty())
                .then(|| format!("{}/{}", m.provider, m.name))
        }),
        mcp_servers: profile.mcp_servers.iter().map(|s| s.name.clone()).collect(),
        skills: profile
            .skills
            .iter()
            .cloned()
            .chain(profile.installed_skills.iter().map(|s| s.name.clone()))
            .collect(),
        capabilities: profile.capabilities.clone(),
        network_mode: kebab(&ent.network.outbound.mode),
        network_allow_hosts: ent.network.outbound.allow_hosts.clone(),
        filesystem_read: ent.filesystem.read.clone(),
        filesystem_write: ent.filesystem.write.clone(),
        filesystem_deny: ent.filesystem.deny.clone(),
        spawn_mode: kebab(&ent.processes.spawn.mode),
        spawn_allowed: ent.processes.spawn.allowed.clone(),
        memory_mb: ent.limits.memory_mb,
    }
}
```

If `SkillCardEntry`'s field is not `name`, use whatever names the skill and say so in your report — do not invent a field.

- [ ] **Step 4: Run the tests**

Run: `cd tools/official-sign && cargo test capability`
Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/official-sign/src/capability.rs tools/official-sign/src/lib.rs
git commit -m "feat(catalog): derive a capability summary from an agent profile"
```

---

### Task 2: put the summary in index.json (catalog repo)

**Repo:** `/Volumes/Firecuda4tb/Projects/official-sign`

**Files:**
- Modify: `tools/official-sign/src/index.rs`, `src/sign.rs`, `src/main.rs`
- Test: `tools/official-sign/src/index.rs` (existing `mod tests`), `tests/end_to_end.rs`

**Interfaces:**
- Consumes: `capability::{CapabilitySummary, derive}` from Task 1.
- Produces: `IndexItem.capabilities: Option<CapabilitySummary>`; `build_official_muragent` now returns `Result<AgentProfile>`; `make_item(entry, bundle_path, profile: Option<&AgentProfile>)`.

- [ ] **Step 1: Write the failing test**

In `tools/official-sign/src/index.rs`'s `mod tests`, and note the existing helper `item()` builds an `IndexItem` — it needs the new field:

```rust
    #[test]
    fn make_item_carries_the_capability_summary_when_a_profile_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("b.muragent");
        std::fs::write(&bundle, b"not a real bundle, only hashed here").unwrap();
        let entry = CatalogEntry {
            id: "agents/researcher".into(),
            kind: "agent".into(),
            name: "researcher".into(),
            version: "1.0.0".into(),
            tier: "free".into(),
            description: "d".into(),
        };

        let without = make_item(&entry, &bundle, None).unwrap();
        assert!(without.capabilities.is_none());

        let profile: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(crate::capability::tests_fixture_yaml()).unwrap();
        let with = make_item(&entry, &bundle, Some(&profile)).unwrap();
        let cap = with.capabilities.expect("capabilities must be present");
        assert_eq!(cap.network_mode, "restricted");
        // The download fields are unaffected by the addition.
        assert_eq!(with.sha256, without.sha256);
        assert_eq!(with.storage_key, without.storage_key);
    }
```

For the fixture, expose the YAML from Task 1's tests as a `pub(crate) fn tests_fixture_yaml() -> &'static str` in `capability.rs` (outside `#[cfg(test)]` is unnecessary — put it behind `#[cfg(test)] pub(crate)`), and have Task 1's `restrictive()` use it too, so one fixture serves both files.

- [ ] **Step 2: Run it and watch it fail**

Run: `cd tools/official-sign && cargo test index`
Expected: FAIL — `make_item` takes 2 arguments.

- [ ] **Step 3: Add the field and the parameter**

In `index.rs`:

```rust
pub struct IndexItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub version: String,
    pub tier: String,
    pub description: String,
    pub storage_key: String,
    pub sha256: String,
    pub size: u64,
    /// Derived at publish time from the item's profile. `None` for items
    /// published before this existed, and for kinds with no profile — the
    /// consumers treat its absence as "no panel", never as an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::capability::CapabilitySummary>,
}
```

```rust
pub fn make_item(
    entry: &CatalogEntry,
    bundle_path: &Path,
    profile: Option<&mur_common::agent::AgentProfile>,
) -> Result<IndexItem> {
```
and inside the constructed value, after `size`:
```rust
        capabilities: profile.map(crate::capability::derive),
```

Add `capabilities: None` to the existing `item()` test helper.

- [ ] **Step 4: Return the profile the build already parsed**

`sign.rs` parses `profile.yaml` into an `AgentProfile` on its way to building the bundle. Hand it back rather than reading the file twice:

```rust
pub fn build_official_muragent(
    source_dir: &Path,
    out_path: &Path,
    identity: &AgentIdentity,
    mur_version: &str,
) -> Result<AgentProfile> {
```
and end the function with `Ok(profile)` instead of `Ok(())`. The profile is moved into `MuragentWriter::new(...)` as YAML text, not as the struct, so returning the struct costs nothing — verify that as you go and, if the struct is consumed, clone it before the writer takes it.

In `main.rs`:

```rust
    let profile = official_sign::sign::build_official_muragent(
        &source_dir,
        &bundle,
        &identity,
        env!("CARGO_PKG_VERSION"),
    )?;
    official_sign::sign::verify_official(&bundle, &entry, &expect_fp)?;

    let item = official_sign::index::make_item(&entry, &bundle, Some(&profile))?;
```

- [ ] **Step 5: Run the whole suite, including the end-to-end test**

Run: `cd tools/official-sign && cargo test`
Expected: all PASS. `tests/end_to_end.rs` exercises the real build path — if it asserts on `index.json`'s shape, extend it to assert the capabilities block is present rather than loosening it.

- [ ] **Step 6: Commit**

```bash
git add tools/official-sign/src/index.rs tools/official-sign/src/sign.rs tools/official-sign/src/main.rs tools/official-sign/src/capability.rs
git commit -m "feat(catalog): publish each item's capability summary in index.json"
```

---

### Task 3: serve one item (server repo)

**Repo:** `/Volumes/Firecuda4tb/Projects/mur-server`

**Files:**
- Modify: `internal/services/officialcatalog/catalog.go`, `internal/api/handlers/official_catalog.go`, `internal/api/server.go`
- Test: `internal/api/handlers/official_catalog_test.go`

**Interfaces:**
- Produces: `GET /api/v1/core/catalog/{id...}` → `{"item": {...}}` including `capabilities` when the index has it.

- [ ] **Step 1: Write the failing test**

In `official_catalog_test.go`, beside the existing List tests:

```go
func TestOfficialCatalogHandler_Get_ReturnsOneItemWithCapabilities(t *testing.T) {
	items := testCatalogItems()
	items[1].Capabilities = map[string]any{"network_mode": "restricted"}
	h := &OfficialCatalogHandler{cat: stubCatalogIndexer{items: items}}

	req := httptest.NewRequest(http.MethodGet, "/api/v1/core/catalog/agent-bar", nil)
	rctx := chi.NewRouteContext()
	rctx.URLParams.Add("*", "agent-bar")
	req = req.WithContext(context.WithValue(req.Context(), chi.RouteCtxKey, rctx))
	rec := httptest.NewRecorder()

	h.Get(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d (%s)", rec.Code, rec.Body.String())
	}
	var body struct {
		Item map[string]json.RawMessage `json:"item"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if _, ok := body.Item["capabilities"]; !ok {
		t.Errorf("expected a capabilities block, got: %s", rec.Body.String())
	}
	// The server-internal download fields stay internal here too.
	for _, k := range []string{"storage_key", "sha256", "size"} {
		if _, leaked := body.Item[k]; leaked {
			t.Errorf("%q must not appear in a public response", k)
		}
	}
}

func TestOfficialCatalogHandler_Get_UnknownID_Returns404(t *testing.T) {
	h := &OfficialCatalogHandler{cat: stubCatalogIndexer{items: testCatalogItems()}}
	req := httptest.NewRequest(http.MethodGet, "/api/v1/core/catalog/nope", nil)
	rctx := chi.NewRouteContext()
	rctx.URLParams.Add("*", "nope")
	req = req.WithContext(context.WithValue(req.Context(), chi.RouteCtxKey, rctx))
	rec := httptest.NewRecorder()

	h.Get(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("expected 404, got %d", rec.Code)
	}
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `go test ./internal/api/handlers/ -run OfficialCatalogHandler_Get`
Expected: FAIL — `h.Get` undefined, `Capabilities` undefined.

- [ ] **Step 3: Carry the block through the service type**

In `internal/services/officialcatalog/catalog.go`, add to `IndexItem`:

```go
	// Derived by the catalog build from the item's profile. Absent for items
	// published before it existed; passed through verbatim rather than typed
	// field by field, so a new key added by the build reaches the client
	// without a server release.
	Capabilities map[string]any `json:"capabilities,omitempty"`
```

- [ ] **Step 4: Add the handler**

In `internal/api/handlers/official_catalog.go`, extend the response DTO and add `Get`:

```go
type officialCatalogItemResponse struct {
	ID           string         `json:"id"`
	Kind         string         `json:"kind"`
	Name         string         `json:"name"`
	Tier         string         `json:"tier"`
	Version      string         `json:"version"`
	Description  string         `json:"description"`
	Capabilities map[string]any `json:"capabilities,omitempty"`
}
```

```go
// Get returns a single catalog item, including its capability summary.
//
// The list endpoint deliberately does not carry capabilities: a Library page
// draws cards from the index and has no use for every item's permission table.
func (h *OfficialCatalogHandler) Get(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "*")
	items, err := h.cat.Index(r.Context())
	if err != nil {
		writeError(w, "official catalog unavailable: "+err.Error(), http.StatusServiceUnavailable)
		return
	}
	for _, item := range items {
		if item.ID == id {
			writeJSON(w, map[string]interface{}{"item": toItemResponse(item)}, http.StatusOK)
			return
		}
	}
	writeError(w, "no such catalog item", http.StatusNotFound)
}
```

Extract the existing List mapping into `toItemResponse(item officialcatalog.IndexItem) officialCatalogItemResponse` and use it from both, so one place decides what is public.

- [ ] **Step 5: Route it**

In `internal/api/server.go`, beside the existing catalog routes (the comment there explains why the exact `/catalog` route precedes the `/catalog/*` wildcard — the same ordering applies here; the download route is auth-gated and mounted separately, so mount this one where the public `GET /catalog` lives):

```go
				// Public: GET /api/v1/core/catalog/<id...> — one item with its
				// capability summary. Mounted after the exact "/catalog" route
				// and before the auth-gated download wildcard.
				r.Get("/catalog/{id}", handlers.NewOfficialCatalogHandler(catSvc).Get)
```

A catalog id contains a slash (`agents/researcher`), so a single `{id}` param will not match it. Use the wildcard form the download route already uses and read `chi.URLParam(r, "*")` — mirror whatever `official_download.go` does, since it solved this exact problem, and say in your report which form you used.

- [ ] **Step 6: Run the tests**

Run: `go test ./internal/api/handlers/ ./internal/api/`
Expected: all PASS, including the existing exact-key List assertion.

- [ ] **Step 7: Commit**

```bash
git add internal/services/officialcatalog/catalog.go internal/api/handlers/official_catalog.go internal/api/handlers/official_catalog_test.go internal/api/server.go
git commit -m "feat(catalog): serve one item, with its capability summary"
```

---

### Task 4: render the two questions (dashboard, server repo)

**Repo:** `/Volumes/Firecuda4tb/Projects/mur-server`

**Files:**
- Modify: `dashboard/src/lib/library.ts`, `dashboard/src/app/(protected)/mur/library/[type]/[name]/page.tsx`

**Interfaces:**
- Consumes: `GET /catalog/{id}` from Task 3.
- Produces: `loadCatalogItem(id: string): Promise<CapabilitySummary | null>`.

- [ ] **Step 1: Add the fetch**

In `library.ts`:

```ts
/** The capability summary the catalog build derives from an item's profile. */
export interface CapabilitySummary {
  model?: string;
  mcp_servers?: string[];
  skills?: string[];
  capabilities?: string[];
  network_mode?: string;
  network_allow_hosts?: string[];
  filesystem_read?: string[];
  filesystem_write?: string[];
  filesystem_deny?: string[];
  spawn_mode?: string;
  spawn_allowed?: string[];
  memory_mb?: number;
}

/**
 * One catalog item's capability summary, or null when there is none.
 *
 * Null covers every reason the panel should simply not appear: an item
 * published before the build derived summaries, a server that predates the
 * endpoint, a network failure. None of them is an error the reader can act on,
 * and an empty panel would be worse than no panel.
 */
export async function loadCapabilities(id: string): Promise<CapabilitySummary | null> {
  try {
    const data = await api.get<{ item?: { capabilities?: CapabilitySummary } }>(
      `/catalog/${id}`
    );
    return data.item?.capabilities ?? null;
  } catch {
    return null;
  }
}
```

- [ ] **Step 2: Fetch it on the detail page**

In the detail page, beside the existing item effect:

```tsx
  const [caps, setCaps] = useState<CapabilitySummary | null>(null);
  useEffect(() => {
    if (!item?.id) return;
    loadCapabilities(item.id).then(setCaps);
  }, [item?.id]);
```

- [ ] **Step 3: Render the panel**

Below the install command block, and only when `caps` is present:

```tsx
      {caps && (
        <Card className="mt-4">
          <CardContent className="p-5 space-y-4 text-sm">
            <div>
              <h2 className="font-semibold text-primary mb-2">What it can do</h2>
              <dl className="space-y-1">
                <CapRow label="Model" value={caps.model} />
                <CapRow label="Skills" value={caps.skills} />
                <CapRow label="Tools (MCP)" value={caps.mcp_servers} />
                <CapRow label="Capabilities" value={caps.capabilities} />
              </dl>
            </div>
            <div>
              <h2 className="font-semibold text-primary mb-2">What it can touch</h2>
              <dl className="space-y-1">
                <CapRow
                  label="Network"
                  value={
                    caps.network_mode === 'unrestricted'
                      ? 'any host'
                      : caps.network_allow_hosts?.length
                        ? `${caps.network_mode} — ${caps.network_allow_hosts.join(', ')}`
                        : `${caps.network_mode} — no hosts allowed`
                  }
                />
                <CapRow label="Reads" value={caps.filesystem_read} empty="no files" />
                <CapRow label="Writes" value={caps.filesystem_write} empty="no files" />
                <CapRow label="Refused" value={caps.filesystem_deny} />
                <CapRow
                  label="Runs programs"
                  value={
                    caps.spawn_mode === 'any'
                      ? 'any program'
                      : caps.spawn_allowed?.length
                        ? caps.spawn_allowed.join(', ')
                        : 'none'
                  }
                />
                <CapRow label="Memory limit" value={caps.memory_mb ? `${caps.memory_mb} MB` : undefined} />
              </dl>
            </div>
          </CardContent>
        </Card>
      )}
```

with this helper above the component:

```tsx
/**
 * One row of the capability panel. An empty list renders its `empty` wording
 * rather than disappearing: a missing row reads as "unknown", while "no files"
 * is the most reassuring thing this page can say.
 */
function CapRow({ label, value, empty }: { label: string; value?: string | string[]; empty?: string }) {
  const text = Array.isArray(value) ? (value.length ? value.join(', ') : empty) : value;
  if (!text) return null;
  return (
    <div className="flex gap-2">
      <dt className="text-muted-foreground w-32 shrink-0">{label}</dt>
      <dd className="font-mono text-xs pt-0.5">{text}</dd>
    </div>
  );
}
```

- [ ] **Step 4: Check it degrades**

Run the gates:

```bash
cd dashboard && npx tsc --noEmit && npx eslint "src/app/(protected)/mur/library/[type]/[name]/page.tsx" src/lib/library.ts
```
Expected: both clean, exit 0.

Then prove the degraded path against the live API, which still predates all of this:

```bash
node -e '(async()=>{const r=await fetch("https://api.mur.run/api/v1/core/catalog/agents/researcher");console.log(r.status, (await r.text()).slice(0,200));})()'
```
Expected: a 404 or an error body — and therefore `loadCapabilities` returning `null` and no panel. That is the version-skew rule holding.

- [ ] **Step 5: Commit**

```bash
git add dashboard/src/lib/library.ts "dashboard/src/app/(protected)/mur/library/[type]/[name]/page.tsx"
git commit -m "feat(dashboard): show what an official agent can do and can touch"
```

---

## Self-Review

**Spec coverage**

| spec section | task |
|---|---|
| §1 derive, never hand-write | 1 |
| §1 the per-field derivation table | 1 |
| §2 per-item endpoint, index stays lean | 3 |
| §3 two questions, in order | 4 |
| §3 empty entitlements stated plainly | 4 (`CapRow`'s `empty`) |
| failure: no capabilities → today's page | 4 (`loadCapabilities` returns null; panel is conditional) — proved in Task 4 Step 4 |
| failure: profile missing fields | 1 (every field is a plain clone of an already-defaulted struct) |
| failure: unrecognised shape | 1 (`kebab()` renders any new enum variant instead of matching arms) |
| testing: permissive vs restrictive fixtures | 1 |
| testing: server key-set assertion | 3 |
| testing: dashboard degradation | 4 |

**Not covered, deliberately:** fleets. The spec says they derive the equivalent from `fleet.yaml`, but the catalog holds no fleet item today (`agents/` is the only kind directory), so there is nothing to derive from and no way to test it. Add it when the first fleet is published; the endpoint and the panel are already shaped for it.

**Type consistency:** `CapabilitySummary`'s field names are identical in Rust (`snake_case` structs), Go (`map[string]any`, passthrough), and TypeScript (`snake_case` interface). The JSON is the contract; only Rust names them twice, and Task 4's interface mirrors Task 1's struct field for field.
