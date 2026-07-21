# Official Catalog Server Endpoints (`/catalog` + `/download`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement `GET /api/v1/core/catalog` (public) and `GET /api/v1/core/catalog/{id}/download` (authenticated, entitlement-gated) in `mur-server` (Go), returning the signed bundle plus a license-key-signed `OfficialLicense` byte-compatible with the shipped Rust client's `check_license`.

**Architecture:** A Go `OfficialLicense` signer that reproduces the Rust `license_sign_input` bytes EXACTLY (compact JSON, struct-field order, no HTML escaping, `sig` omitted) then Ed25519-signs with a dedicated license key (NOT the root bundle key). Catalog data is `index.json` live-fetched from the private `mur-run/official-catalog` repo (60 s cache). `/download` verifies entitlement via the existing `BillingService`, fetches the release asset via the GitHub API, and proxies the bytes inline as base64.

**Tech Stack:** Go, chi router, `crypto/ed25519`, `encoding/json` + `encoding/base64`, `net/http` (GitHub API), existing `internal/{config,services,api}` patterns.

## Global Constraints

- **License byte-contract (load-bearing — a mismatch makes every client install fail).** The Rust client verifies the license signature over `serde_json::to_vec(license_with_sig_None)`. That is: **compact JSON** (no spaces), keys in this exact order `format_version, user_id, item, version, expires_at, signer_pubkey`, the `sig` key **omitted**, and **no HTML escaping** of `<`/`>`/`&`. Go must produce identical bytes via `json.Encoder` with `SetEscapeHTML(false)` and the trailing `\n` trimmed (NOT `json.Marshal`, which HTML-escapes). Field order = Go struct definition order.
- The license key is a SEPARATE key from the root bundle key. Its fingerprint must equal the client's `MUR_OFFICIAL_LICENSE_KEY_FP` (client key-split is a prerequisite — see the spec §6). The root `ed25519-861d2acb` bundle key NEVER appears in mur-server.
- Response shapes are frozen by the shipped client (PR #738): `/catalog` → `{"items":[{"id","tier","version","description"}]}`; `/download` → `{"license":<OfficialLicense JSON>,"bundle_base64":"..."}`.
- Module `github.com/mur-run/mur-server`; routes mount in the existing `/api/v1/core` chi group; auth via `authMiddleware`, user via `middleware.GetUserFromContext(ctx)`.
- TDD with Go's `testing`; run `go test ./internal/...`. Never log the license private key or bundle bytes.

---

### Task 1: `OfficialLicense` Go signer (byte-exact with Rust)

**Files:**
- Create: `internal/services/officialcatalog/license.go`
- Test: `internal/services/officialcatalog/license_test.go`

**Interfaces:**
- Produces:
  - `type OfficialLicense struct { FormatVersion uint32 \`json:"format_version"\`; UserID string \`json:"user_id"\`; Item string \`json:"item"\`; Version string \`json:"version"\`; ExpiresAt string \`json:"expires_at"\`; SignerPubkey string \`json:"signer_pubkey"\`; Sig string \`json:"sig,omitempty"\` }`
  - `func SignInput(l OfficialLicense) ([]byte, error)` — marshals `l` WITHOUT `sig` as compact, non-HTML-escaped JSON in field order.
  - `func Sign(l *OfficialLicense, priv ed25519.PrivateKey)` — sets `SignerPubkey` (base64 of pubkey), computes `SignInput`, sets `Sig` = base64(ed25519.Sign).
  - `func Verify(l OfficialLicense) bool` — mirrors the Rust `verify_license_sig` for tests.

- [ ] **Step 1: Write the golden-bytes failing test**

```go
func TestSignInput_GoldenBytes(t *testing.T) {
    l := OfficialLicense{FormatVersion: 1, UserID: "u1", Item: "agents/researcher",
        Version: "1.0.0", ExpiresAt: "2027-01-01T00:00:00Z", SignerPubkey: "PUB"}
    got, err := SignInput(l)
    if err != nil { t.Fatal(err) }
    want := `{"format_version":1,"user_id":"u1","item":"agents/researcher","version":"1.0.0","expires_at":"2027-01-01T00:00:00Z","signer_pubkey":"PUB"}`
    if string(got) != want {
        t.Fatalf("sign-input mismatch:\n got=%s\nwant=%s", got, want)
    }
}

func TestSignVerify_Roundtrip(t *testing.T) {
    pub, priv, _ := ed25519.GenerateKey(rand.Reader)
    _ = pub
    l := OfficialLicense{FormatVersion: 1, UserID: "u1", Item: "agents/x", Version: "1.0.0", ExpiresAt: "2027-01-01T00:00:00Z"}
    Sign(&l, priv)
    if !Verify(l) { t.Fatal("valid signature must verify") }
    l.Item = "agents/tampered"
    if Verify(l) { t.Fatal("tampered license must not verify") }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `go test ./internal/services/officialcatalog/ -run TestSignInput`
Expected: build/undefined errors (types/functions missing).

- [ ] **Step 3: Implement**

```go
package officialcatalog

import (
    "bytes"
    "crypto/ed25519"
    "encoding/base64"
    "encoding/json"
)

type OfficialLicense struct {
    FormatVersion uint32 `json:"format_version"`
    UserID        string `json:"user_id"`
    Item          string `json:"item"`
    Version       string `json:"version"`
    ExpiresAt     string `json:"expires_at"`
    SignerPubkey  string `json:"signer_pubkey"`
    Sig           string `json:"sig,omitempty"`
}

// SignInput reproduces Rust `license_sign_input`: serde_json::to_vec of the
// license with `sig` cleared — compact, field-order, NO HTML escaping.
func SignInput(l OfficialLicense) ([]byte, error) {
    l.Sig = "" // omitted via omitempty
    var buf bytes.Buffer
    enc := json.NewEncoder(&buf)
    enc.SetEscapeHTML(false)
    if err := enc.Encode(&l); err != nil {
        return nil, err
    }
    return bytes.TrimRight(buf.Bytes(), "\n"), nil // Encoder appends '\n'
}

func Sign(l *OfficialLicense, priv ed25519.PrivateKey) error {
    pub := priv.Public().(ed25519.PublicKey)
    l.SignerPubkey = base64.StdEncoding.EncodeToString(pub)
    l.Sig = ""
    input, err := SignInput(*l)
    if err != nil {
        return err
    }
    l.Sig = base64.StdEncoding.EncodeToString(ed25519.Sign(priv, input))
    return nil
}

func Verify(l OfficialLicense) bool {
    pub, err := base64.StdEncoding.DecodeString(l.SignerPubkey)
    if err != nil || len(pub) != ed25519.PublicKeySize {
        return false
    }
    sig, err := base64.StdEncoding.DecodeString(l.Sig)
    if err != nil {
        return false
    }
    input, err := SignInput(l)
    if err != nil {
        return false
    }
    return ed25519.Verify(pub, input, sig)
}
```

> CONFIRMED against `mur-common/src/official.rs`: Rust `sign_license` uses `base64 STANDARD` (== Go `base64.StdEncoding`) for BOTH `signer_pubkey` and `sig`, sets `signer_pubkey` BEFORE computing the sign-input (so it's inside the signed payload), and `verify_license_sig` uses the non-strict `vk.verify` (== Go `ed25519.Verify`). The golden string in Step 1 is exactly `serde_json::to_vec`'s output for that fixture (compact, field order, `sig` omitted). No further base64/format uncertainty — implement as written.

- [ ] **Step 4: Run to verify pass**

Run: `go test ./internal/services/officialcatalog/`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add internal/services/officialcatalog/license.go internal/services/officialcatalog/license_test.go
git commit -m "feat(official): OfficialLicense Go signer byte-compatible with Rust client"
```

---

### Task 2: Config additions

**Files:**
- Modify: `internal/config/config.go`

**Interfaces:**
- Produces: config fields `OfficialLicenseSigningKey string` (base64 32-byte Ed25519 secret), `GitHubAppID string`, `GitHubAppInstallationID string`, `GitHubAppPrivateKey string` (RSA PEM), `OfficialCatalogRepo string` (default `mur-run/official-catalog`), loaded via the existing `getEnv` pattern. Two helpers: `func (c *Config) OfficialLicensePrivateKey() (ed25519.PrivateKey, error)` (base64-decode → validate 32 bytes → `ed25519.NewKeyFromSeed`); `func (c *Config) GitHubAppRSAKey() (*rsa.PrivateKey, error)` (parse the PEM via `jwt.ParseRSAPrivateKeyFromPEM`).

- [ ] **Step 1: Failing test** — `config_test.go`: set the license-key env to a base64 32-byte seed and the App-key env to a test RSA PEM; load config; assert `OfficialLicensePrivateKey()` returns a usable key and `GitHubAppRSAKey()` parses.
- [ ] **Step 2: Run — fails (fields/methods missing).**
- [ ] **Step 3: Implement** — add the `getEnv(...)` lines for `OFFICIAL_LICENSE_SIGNING_KEY`, `GITHUB_APP_ID`, `GITHUB_APP_INSTALLATION_ID`, `GITHUB_APP_PRIVATE_KEY`, `OFFICIAL_CATALOG_REPO` (default `"mur-run/official-catalog"`), mirroring existing lines like `BitLDMGBaseURL`. Add the `OfficialLicensePrivateKey` helper (`crypto/ed25519` + `encoding/base64`) and `GitHubAppRSAKey` helper (`jwt.ParseRSAPrivateKeyFromPEM` from `github.com/golang-jwt/jwt/v5`). Also add the three new keys to `.env.example`.
- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Commit** `feat(official): config for license key + GitHub App catalog access`.

---

### Task 2b: GitHub App installation-token minter

**Files:**
- Create: `internal/services/officialcatalog/githubapp.go`
- Test: `internal/services/officialcatalog/githubapp_test.go`

**Interfaces:**
- Produces:
  - `type InstallationTokenSource struct {...}` + `func NewInstallationTokenSource(httpClient *http.Client, appID, installationID string, rsaKey *rsa.PrivateKey) *InstallationTokenSource`.
  - `func (s *InstallationTokenSource) Token(ctx context.Context) (string, error)` — returns a valid installation token, minting a new one when the cached token is within 5 min of expiry.
- Mechanism: build a JWT (`jwt.NewWithClaims(jwt.SigningMethodRS256, claims)` with `iss = appID`, `iat = now-60s`, `exp = now+9min`), sign with `rsaKey`; `POST https://api.github.com/app/installations/<installationID>/access_tokens` with `Authorization: Bearer <jwt>`, `Accept: application/vnd.github+json`; parse `{"token","expires_at"}`; cache under a mutex until `expires_at - 5min`.

- [ ] **Step 1: Failing test** — `httptest` server asserts the incoming `Authorization: Bearer <jwt>` parses+verifies against the test RSA public key with `iss==appID`, and returns `{"token":"ghs_test","expires_at":"<now+1h>"}`. Assert `Token()` returns `ghs_test`, and a second call within the window does not re-hit the server (cached). Inject the API base URL for tests.
- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement** — the JWT mint + exchange + mutex-guarded cache (`token string`, `exp time.Time`), base URL field defaulting to `https://api.github.com`.
- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Commit** `feat(official): GitHub App installation-token minter`.

---

### Task 3: Catalog fetcher (index.json + 60s cache)

**Files:**
- Create: `internal/services/officialcatalog/catalog.go`
- Test: `internal/services/officialcatalog/catalog_test.go`

**Interfaces:**
- Produces:
  - `type IndexItem struct { ID, Kind, Name, Version, Tier, Description, StorageKey, Sha256 string; Size int64 }` (json tags matching the CI-produced `index.json`).
  - `type CatalogService struct {...}` with `func NewCatalogService(httpClient *http.Client, repo string, tokens *InstallationTokenSource) *CatalogService` (holds the Task 2b token source for auth).
  - `func (s *CatalogService) Index(ctx context.Context) ([]IndexItem, error)` — GETs `https://api.github.com/repos/<repo>/contents/index.json` (Accept `application/vnd.github.raw`, `Authorization: Bearer <tokens.Token(ctx)>`), parses the JSON array, caches the parsed slice for 60 s. On fetch error with a warm cache, returns the cache; with no cache, returns the error.

- [ ] **Step 1: Failing test** — `httptest` server returns a fixed `index.json` array; assert `Index` parses it and a second call within the TTL does not re-hit the server (count requests). Add a case: server errors after a warm cache → `Index` returns the cached slice.
- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement** — the fetch (inject the base URL for tests via a struct field defaulting to `https://api.github.com`), a `sync.Mutex`-guarded `cached []IndexItem` + `fetchedAt time.Time`, TTL const `60 * time.Second`.
- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Commit** `feat(official): catalog index.json fetcher with cache`.

---

### Task 4: `GET /api/v1/core/catalog` handler

**Files:**
- Create: `internal/api/handlers/official_catalog.go`
- Test: `internal/api/handlers/official_catalog_test.go`

**Interfaces:**
- Consumes: `CatalogService.Index`.
- Produces: `type OfficialCatalogHandler struct {...}` + `NewOfficialCatalogHandler(cat *officialcatalog.CatalogService, ...)` + `func (h *OfficialCatalogHandler) List(w, r)` — calls `Index`, maps each `IndexItem` → `{id, tier, version, description}`, writes `{"items":[...]}`. On `Index` error with no cache → `503`.

- [ ] **Step 1: Failing test** — construct the handler over a `CatalogService` backed by an httptest GitHub mock; `httptest` the handler; assert `200` + JSON `items` with exactly `id/tier/version/description` keys (no `storage_key`).
- [ ] **Step 2–4: red → implement → green.** Follow an existing handler (e.g. `community.go`) for the response-writing idiom.
- [ ] **Step 5: Commit** `feat(official): GET /catalog handler`.

---

### Task 5: Release-asset fetcher

**Files:**
- Modify: `internal/services/officialcatalog/catalog.go` (add asset fetch)
- Test: same package test

**Interfaces:**
- Produces: `func (s *CatalogService) FetchAsset(ctx context.Context, item IndexItem) ([]byte, error)` — resolves the release by tag `official/<item.ID>/<item.Version>` (`GET /repos/<repo>/releases/tags/<tag>`), finds the asset whose name ends in `.muragent`/`.fleet`, and downloads it (`GET .../releases/assets/<id>` with `Accept: application/octet-stream`). All calls authenticate with `Authorization: Bearer <s.tokens.Token(ctx)>` (the Task 2b installation token). Returns raw bytes.

- [ ] **Step 1: Failing test** — httptest mock serving the release JSON (with an asset) + the asset bytes; assert `FetchAsset` returns the exact bytes. Add a `404`-release case → error.
- [ ] **Step 2–4: red → implement → green.**
- [ ] **Step 5: Commit** `feat(official): release asset fetcher by tag`.

---

### Task 6: `GET /api/v1/core/catalog/{id}/download` handler

**Files:**
- Create: `internal/api/handlers/official_download.go`
- Test: `internal/api/handlers/official_download_test.go`

**Interfaces:**
- Consumes: `CatalogService.{Index,FetchAsset}`, `officialcatalog.Sign`, `BillingService.GetSubscription`, `middleware.GetUserFromContext`, `config.OfficialLicensePrivateKey`.
- Produces: `type OfficialDownloadHandler struct {...}` + constructor + `func (h *OfficialDownloadHandler) Download(w, r)`.

- [ ] **Step 1: Write the failing integration test**

Set up: httptest GitHub mock (index.json with a `pro` item + a release asset); a fake billing lookup (inject an interface `entitlementChecker` the handler calls — `HasActiveSubscription(ctx, userID) (bool, error)` — so the test controls it without a real Stripe/DB). Build a request with a user in context via `middleware.WithUser(ctx, user)`.

```go
func TestDownload_ProWithoutSubscription_403(t *testing.T) { /* entitlement=false → 403 */ }
func TestDownload_ProWithSubscription_ReturnsSignedLicenseAndBundle(t *testing.T) {
    // entitlement=true → 200; decode {license, bundle_base64};
    // assert bundle_base64 decodes to the mock asset bytes;
    // assert officialcatalog.Verify(license) == true and license.UserID == the caller;
}
func TestDownload_UnknownID_404(t *testing.T) {}
```

- [ ] **Step 2: Run — fails.**
- [ ] **Step 3: Implement the handler**

Flow: `id` from `chi.URLParam(r, "*")` (the id contains a slash — mount the route so the wildcard captures `agents/researcher`); look it up in `Index`; `404` if absent. `user := middleware.GetUserFromContext(r.Context())`. If `item.Tier == "pro"` and `!entitlement.HasActiveSubscription(ctx, user.ID)` → `403`. `assetBytes := FetchAsset(...)`. Build `OfficialLicense{FormatVersion:1, UserID:user.ID.String(), Item:id, Version:item.Version, ExpiresAt: expiry}` where `expiry` = pro → subscription `CurrentPeriodEnd + 30d` (RFC3339), free → a fixed far-future (e.g. `now+100y`). `officialcatalog.Sign(&lic, priv)`. Write `{"license":lic, "bundle_base64": base64.StdEncoding.EncodeToString(assetBytes)}`.

> The real `entitlementChecker` implementation wraps `BillingService.GetSubscription` and returns `sub.Status == SubscriptionStatusActive || sub.Status == SubscriptionStatusPastDue` and reads `sub.CurrentPeriodEnd` for the expiry; the handler takes it as an interface so tests inject a stub.

- [ ] **Step 4: Run — pass.**
- [ ] **Step 5: Commit** `feat(official): GET /download handler (entitlement + signed license)`.

---

### Task 7: Wire routes + docs

**Files:**
- Modify: `internal/api/server.go` (construct the services/handlers, register routes)
- Modify: `CLAUDE.md` (mur-server) or the docs page note

- [ ] **Step 1:** In `server.go`, construct the token source + catalog service from config, the two handlers, and register inside the `/api/v1/core` group:
  ```go
  rsaKey, _ := cfg.GitHubAppRSAKey()
  tokens := officialcatalog.NewInstallationTokenSource(httpClient, cfg.GitHubAppID, cfg.GitHubAppInstallationID, rsaKey)
  catSvc := officialcatalog.NewCatalogService(httpClient, cfg.OfficialCatalogRepo, tokens)
  // construct officialCatalogHandler + officialDownloadHandler over catSvc + the license key, then:
  r.Get("/catalog", officialCatalogHandler.List)                 // public
  r.Route("/catalog", func(r chi.Router) {
      r.Group(func(r chi.Router) {
          r.Use(authMiddleware)
          r.Get("/{id}/download", officialDownloadHandler.Download) // wildcard id may contain '/'
      })
  })
  ```
  > NOTE: `{id}` must capture `agents/researcher` (contains a slash). chi needs a wildcard route (`/{id}/download` won't match a slashed id) — use `r.Get("/catalog/*", ...)` and parse `strings.TrimSuffix(chi.URLParam(r,"*"), "/download")`, or register `/catalog/{kind}/{name}/download` and reassemble `id = kind+"/"+name`. Pick the one that keeps the public `List` route unambiguous; verify with a routing test.
- [ ] **Step 2:** `go build ./...` + `go test ./internal/...` — all green.
- [ ] **Step 3:** Add a one-paragraph note to the server docs describing the two endpoints + the required env (`OFFICIAL_LICENSE_SIGNING_KEY`, `GITHUB_APP_ID`, `GITHUB_APP_INSTALLATION_ID`, `GITHUB_APP_PRIVATE_KEY`).
- [ ] **Step 4: Commit** `feat(official): wire /catalog + /download routes`.

---

## Prerequisite (separate `mur`-repo plan — must ship first)

Client key-split (spec §6): add `MUR_OFFICIAL_LICENSE_KEY_FP`; switch the license-verification sites (`check_license` in `cmd_official_install`, `require_license_against` in both import gates) to the license fp; leave bundle-signer checks on `MUR_OFFICIAL_PUBLISHER_KEY_FP`. Without it, license-key-signed licenses fail the client's verify.

## Out of scope

- KMS/HSM custody for the license key (hardening).
- Real key generation + secret provisioning (operator).
- Hub GUI store (same endpoints).
