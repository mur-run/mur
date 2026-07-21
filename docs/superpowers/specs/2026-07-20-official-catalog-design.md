# MUR Official Agents/Fleets Catalog — Design

**Status:** Design / spec
**Date:** 2026-07-20
**Builds on:** quill P2.1 pinned publisher root (`MUR_OFFICIAL_PUBLISHER_KEY_FP`), quill P2.2 registry-CI signing, `mur agent export` / `mur fleet export` signed bundles, `mur login` device flow, mur-server auth/billing.

## 1. Goal

Ship an **official catalog of Agents and Fleets** under a freemium model:

- Official content is downloadable **only via app.mur.run after login** (free tier drives registration; pro tier requires an active subscription).
- Official content is **not shareable** between users; user-created exports remain fully shareable, unchanged.
- Publishing is restricted to official staff — with **no publish tool that could leak**: publishing is a private repo + CI signing pipeline.

## 2. Decisions (settled during brainstorm)

| Question | Decision |
|---|---|
| Business model | **Freemium** — free official items (login-gated) + pro items (subscription) |
| Anti-sharing strength | **License binding + offline tolerance** — no runtime DRM, no phone-home; make sharing fail cleanly, accept that a forked client can bypass (those users would never pay) |
| Publish tooling | **Private repo + CI signing** — no shippable tool exists at all |
| Install surfaces | **CLI first + Hub GUI store** (Hub as second phase) |
| Payment model | **Subscription** — active sub = full pro catalog; lapse keeps installed content usable (local-first promise), blocks new downloads/updates |

## 3. Architecture

### 3.1 Publish pipeline (official side)

- New **private** repo `mur-run/official-catalog`:
  - `agents/<name>/` and `fleets/<name>/` hold unsigned bundle sources, produced by staff with the existing `mur agent export` / `mur fleet export --with-members` commands, submitted via PR.
  - `catalog.yaml` — per-item metadata: name, description, `tier: free|pro`, version, marketing metadata.
- CI on merge-to-main (same model as quill P2.2):
  1. Sign each bundle with the **official private key** held in GitHub Secrets (DSSE/Ed25519).
  2. Stamp `distribution: official` inside the signed manifest (see §3.3).
  3. Upload signed bundles to mur-server storage + regenerate the catalog index.
- Properties: publish permission = repo permission; private key never touches a laptop; every release has a PR review trail; **there is no tool to leak**.
- The public `mur-run/skill-registry` is unaffected. Paid content must not live in any public repo — the private repo + entitlement-gated delivery is what makes "pro" mean anything.

### 3.2 Server side (mur-server, Go)

- `GET /api/catalog` — catalog metadata only (no content). Public: powers web browsing/marketing and the Hub store list.
- `GET /api/catalog/<item>/download` — requires auth (existing session/token from `mur login` device flow).
  - `tier: pro` → additionally check entitlement = existing billing subscription is active. No new data model; entitlement is a query against billing state.
  - Response: the signed official bundle **plus a license token** — a DSSE envelope signed by the official key containing `{user_id, item, version, expires_at}` where `expires_at` = subscription period end + 30-day grace. Free-tier items get a license too (uniform pipeline; no entitlement check).

### 3.3 Client side (mur CLI + Hub)

- New commands: `mur official list`, `mur official install <name>` (resolves agent or fleet from the catalog). Uses the stored `mur login` credentials to call the API.
- Install verification, fail-closed:
  1. Bundle signature verifies against the pinned official root (`MUR_OFFICIAL_PUBLISHER_KEY_FP` — already compiled into the client).
  2. License token signature verifies against the same root.
  3. License `user_id` == locally logged-in account; `expires_at` not passed.
  4. License + install record stored locally so updates re-verify the same way.
- **Verification happens only at download/update time — never at runtime.** Unsubscribing never disables installed content (local-first promise kept).
- License renewal: any authenticated catalog call refreshes tokens; offline use is fine until `expires_at`.

**Anti-sharing enforcement (the one critical rule):**
The signed manifest carries `distribution: official`. The file-import paths (`mur agent import`, `mur fleet import`) reject any bundle bearing this marker unless a matching valid license (this user, this item) exists locally — with the message "get this from app.mur.run". Because the marker is inside the signed payload, stripping it invalidates the official signature: the bundle degrades to an untrusted peer bundle (TOFU low-trust flow, official identity lost). User-created bundles never carry the marker; their share/import flow is untouched.

### 3.4 Hub GUI store (phase 2)

Model-Library-style store page consuming the same two API endpoints: browse catalog, one-click install, show license/subscription state. No additional server surface.

## 4. Threats accepted / out of scope

- A modified client can skip license checks — accepted by design (Q2 decision).
- Local clock rollback extends offline grace — accepted (bounded by next online contact).
- Deliberately **not** doing: runtime phone-home, kill-on-unsubscribe, DRM.
- Deferred to later iterations: rollback/version-downgrade protection, license revocation lists, team-seat sharing of entitlements, web deep-link install.

## 5. Implementation phases

1. **Phase 1 — pipeline + CLI:** `official-catalog` repo + CI signing; mur-server catalog/download/license endpoints; `mur official list|install`; import-path official-marker gate.
2. **Phase 2 — Hub store:** store page in mur-hub-gui on the same API.

## 6. Testing

- Unit: license envelope sign/verify; marker-stripped bundle loses official trust; import gate rejects official bundle without/with-mismatched license; expired license blocks download-time verify but not installed content.
- Integration: end-to-end free-tier download+install with a test account; pro download denied without entitlement, allowed with.
- CI pipeline: dry-run signing job on PRs in `official-catalog` (sign with a test key, verify structure) so breakage surfaces before merge.
