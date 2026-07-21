# MUR Official Catalog — Server Endpoints (`/catalog` + `/download`) — Design

**Status:** Design / spec
**Date:** 2026-07-21
**Builds on:** `2026-07-20-official-catalog-design.md` (overall), the shipped client (PR #738: `OfficialLicense`, `check_license`, import gates, `mur official`), the producer + CI pipeline (`2026-07-21-official-catalog-publish-pipeline-design.md` — bundles published as GitHub Releases on private `mur-run/official-catalog`, `index.json` committed to that repo). Server = `github.com/mur-run/mur-server` (Go, chi router, Postgres, Stripe billing, device-flow auth).

## 1. Goal

Serve the official catalog to the `mur` client: a public `GET /api/v1/core/catalog` (browse) and an authenticated `GET /api/v1/core/catalog/{id}/download` (entitlement-gated) that returns the signed bundle plus an account-bound, license-key-signed `OfficialLicense`.

## 2. Decisions (settled during brainstorm)

| Question | Decision |
|---|---|
| License signing key | **Separate online license key** — the root bundle-signing key (`ed25519-861d2acb`) stays CI-only/offline and NEVER touches the server. A distinct license keypair signs licenses; the client pins its fingerprint as a new `MUR_OFFICIAL_LICENSE_KEY_FP` and verifies licenses against it. A server compromise can forge only licenses (bounded), never official code bundles. |
| Catalog data source | **Live-fetch `index.json`** from the private repo (GitHub API) with a 60 s in-memory cache; degrade to last-cached on GitHub outage. No DB table, no sync job. |
| GitHub access | **GitHub App** (not a PAT) — the server mints short-lived (1 h) installation tokens from the App's private key, auto-refreshed, so there is no annual PAT rotation. App has `Contents: Read` on `mur-run/official-catalog`. |
| License key custody (server) | Env secret `OFFICIAL_LICENSE_SIGNING_KEY` (base64 32-byte Ed25519 secret) for v1; KMS/HSM is a hardening follow-up (blast radius already bounded by the key split). |
| Entitlement | Reuse `BillingService.GetSubscription(userID)`; pro allowed when `Status ∈ {Active, PastDue}`. `expires_at` = subscription `CurrentPeriodEnd` + 30-day grace; free items get a long-lived license too (uniform flow). |

## 3. Scope — two parts, ordered

1. **Client key-split (prerequisite, `mur` repo — separate small plan, ships FIRST).** Without it, licenses signed by the new license key would fail the client's `check_license` (which today pins `MUR_OFFICIAL_PUBLISHER_KEY_FP` for licenses). See §6.
2. **Server endpoints (`mur-server`, this plan's focus).** §4–§5.

## 4. `GET /api/v1/core/catalog` (public)

- Mounted in the existing `/api/v1/core` chi route group, **no auth** (powers web browsing + the Hub store + `mur official list`).
- Handler fetches `index.json` from `mur-run/official-catalog` via the GitHub API (raw content of the repo's `index.json` on `main`), authenticated with a GitHub App installation token (minted from the App private key, cached ~55 min). Result cached in-memory 60 s (TTL). On GitHub error, serve the last good cache; if no cache yet, `503`.
- Translate each `index.json` item → the client's fixed `CatalogItem` shape: `{id, tier, version, description}` (drop `storage_key`/`sha256`/`size` — server-internal). Response: `{"items":[...]}`.

## 5. `GET /api/v1/core/catalog/{id}/download` (authenticated)

- Wrapped in `authMiddleware` (Bearer → user in context). `id` is `agents/<name>` or `fleets/<name>`.
- Flow (fail-closed at each step):
  1. Resolve the item from the cached `index.json`; `404` if unknown.
  2. If `tier == "pro"`: `BillingService.GetSubscription(userID)`; allow iff `Status ∈ {Active, PastDue}`, else `403` with a "requires an active MUR Pro subscription" body. Free items skip this.
  3. Fetch the release asset bytes: GitHub API on `mur-run/official-catalog`, release tag `official/<id>/<version>` (e.g. `official/agents/researcher/1.0.0` — exactly the tag the CI publish job creates, where `id` already carries the `agents/` or `fleets/` prefix), download the `.muragent`/`.fleet` asset with the App installation token.
  4. Build `OfficialLicense { format_version, user_id: <caller>, item: id, version, expires_at, signer_pubkey: <license pubkey base64>, sig: None }`, then sign with the license key (`sign_license`); `expires_at` = pro → `CurrentPeriodEnd + 30d`; free → a far-future/long TTL.
  5. Respond `{"license": <OfficialLicense JSON>, "bundle_base64": <base64 asset>}` — the exact shape the shipped client (#738) parses.
- The bundle bytes are proxied inline (never a public URL), so the private release asset stays private and pro bytes are never freely fetchable.

## 6. Client key-split (prerequisite — `mur` repo)

- `mur-common/src/skill/publisher_trust.rs`: add `MUR_OFFICIAL_LICENSE_KEY_FP` (the license key's `ed25519-<8hex>` fingerprint).
- **License-verification** sites switch from the publisher fp to the license fp:
  - `cmd_official_install`'s `check_license(&license, id, &user_id, MUR_OFFICIAL_LICENSE_KEY_FP)`.
  - the import-gate license check `require_license_against(mur_home, item, user, MUR_OFFICIAL_LICENSE_KEY_FP)` (in both `official_gate` and `official_gate_agent`).
- **Bundle-signer** checks are UNCHANGED — `official_gate`/`official_gate_agent` still verify the bundle signature against `MUR_OFFICIAL_PUBLISHER_KEY_FP`. Clean split: bundle trust = publisher key; license trust = license key.
- Ships in a coordinated `mur` release BEFORE the server issues license-key-signed licenses.

## 7. Config / secrets (mur-server)

- `OFFICIAL_LICENSE_SIGNING_KEY` — base64 32-byte Ed25519 secret (the license private key). Its fp must equal the client's `MUR_OFFICIAL_LICENSE_KEY_FP`.
- **GitHub App** (auto-refreshing access, no PAT rotation): `GITHUB_APP_ID` (numeric), `GITHUB_APP_INSTALLATION_ID` (the installation on `mur-run/official-catalog`), `GITHUB_APP_PRIVATE_KEY` (the App's RSA private key PEM — a secret). The server mints a short JWT (RS256, signed with the PEM) → exchanges it for a 1 h installation token at `POST /app/installations/{id}/access_tokens`, cached until ~5 min before expiry. Uses the existing `golang-jwt/jwt/v5` dep.
- `OFFICIAL_CATALOG_REPO` — `mur-run/official-catalog` (configurable).
- Added via the existing `getEnv` config pattern (`internal/config/config.go`).

## 8. Errors / edge cases

- `401` missing/invalid bearer; `403` pro without active subscription; `404` unknown id; `503` catalog with no cache and GitHub down; `502` download when the release asset can't be fetched.
- License verification happens ONLY at download/install time (client side), never at runtime — a lapsed subscription never disables installed content (local-first).
- Never log the license private key or the bundle bytes.

## 9. Testing

- **Unit:** license sign→verify roundtrip against the license key (mirrors the client's `check_license`); `index.json`→`CatalogItem` translation; entitlement gate (pro allowed for Active/PastDue, denied otherwise; free bypasses).
- **Integration:** a mock GitHub (httptest) serving `index.json` + a release asset; a test user with and without an active subscription hitting `/download`; assert `403` vs a well-formed `{license, bundle_base64}` whose license verifies against the license pubkey.

## 10. Out of scope

- Client key-split is a separate `mur`-repo plan (prerequisite).
- KMS/HSM custody for the license key (hardening follow-up).
- Hub GUI store (consumes these same endpoints; separate).
- Real license-key generation + secret provisioning (operator action).
