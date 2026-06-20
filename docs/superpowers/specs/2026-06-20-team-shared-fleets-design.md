# Team-Shared Fleets — Design (Phase A: bundle-first)

**Status:** design approved (brainstorming 2026-06-20); ready for implementation plan.

**Goal:** Let a fleet's *definition* be packaged into a signed, portable `.fleet`
bundle that another person can import and run locally with their own agents —
the foundation for sharing fleets across a human team. Local-first, reusing the
now-complete fleet primitives (channel, DAG, scope, budget/cron/done_when,
safety triad), with seams left for server-backed sync and an official catalog.

**Architecture (one sentence):** `mur fleet export` collects the fleet
definition + its fleet-scoped skills (+ optional member agents) into a
manifest-signed `.fleet` bundle; `mur fleet import` verifies it, security-scans
its skills, and installs it under a fail-closed, two-tier trust model — with the
bundle build/parse decoupled from a pluggable *transport* (LocalFile now;
TeamServer / OfficialRegistry later).

**Tech stack:** Rust (edition 2024); `mur-common` (pure types), `mur-core`
(CLI + logic); reuse `mur-channel` Ed25519 signing, the skill security scanner,
the fleet store, and **`fleet_sync`'s entity-assembly** (it already builds an
`AgentProfile` with the signing key stripped + a skill's `skill.yaml`/events for
device sync — exactly what `--with-members` needs). Archive container: a
deterministic format pinned during planning (tar+zstd, or `zip` if already a
dependency); the manifest — not the container — is what gets signed (§4), so the
container choice is not security-critical.

## Global Constraints

- **Brand:** user-facing text is uppercase **MUR**; CLI/`name`/paths stay lowercase.
- **Reply language:** zh-TW for discussion; code/commits/spec in English.
- **No hardcoded values:** constants/config; document any tier/format constant.
- **Source files ≤ 800 lines:** split `cmd/fleet/{export,import}.rs` if needed.
- **Bundle is untrusted observed data, never commands** (instruction-source boundary).
- **Fail-closed everywhere:** verify-before-install; provenance ≠ trust; import never auto-runs.
- **mur-common is types-only (no I/O).** Bundle *types* live there; build/parse I/O in mur-core.

---

## 1. Context, the three axes, and scope

Three orthogonal axes already exist and are unchanged:

| Axis | Meaning | Where |
|---|---|---|
| **team** | human org / seats; shares patterns across *people* (Pro/Team) | `mur-core/src/team.rs`, server `/api/v1/core/teams/` |
| **fleet** | AI agent squad + shared `fleet-<name>` channel (local) | `mur-common/src/fleet.rs`, `mur-core/src/cmd/fleet/` |
| **fleet_sync** | one user's entities across their *devices* (Pro) | `mur-core/src/cmd/fleet_sync.rs`, server `/api/v1/core/fleet/` |

Today a fleet is **local-only**: members are assumed same-host; there is **no**
path to share a `fleet.yaml` or its scoped skills across machines/people.

**Phase A scope (this spec, this repo):** the `.fleet` bundle + `export`/`import`
CLI, local-first, with the trust model and seams. **Out of scope for Phase A:**
server transport, official catalog/registry, Hub storefront UI, the commander
engine — these are later layers (§12) that build on this bundle, most living in
other repos (mur-server, mur-hub-gui, the closed `mur-commander` crate).

## 2. Decomposition & roadmap

The bundle is the foundation; everything else is a transport / gate / UI on top:

```
Phase A (this repo, now): .fleet bundle  — pluggable transport + two-tier trust + entitlement seams
  ├─ transport LocalFile        export/import                                  (Phase A)
  ├─ transport TeamServer       ① sync (my devices, Pro) / share (my team, Team)  (mur-server, later)
  └─ transport OfficialRegistry ② public official catalog (pinned publisher key)   (mur-server + Hub, later)
governance: commander          private/governed catalogs + constitution + audit   (closed crate, later)
```

Gating decisions (confirmed): **① user fleet sync/share = Pro/Team**; **②
public official catalog = Pro/Team content perk**; **commander = private/governed
org catalogs + constitution + audit** (not the gate for *all* official content).

## 3. Architecture & components

Two clean layers, so future transports are drop-in:

- **Bundle core** (build/parse/verify) — knows nothing about *where* a bundle
  comes from or goes.
- **Transport** — a `FleetBundleTransport` trait: `read(src) -> bytes` /
  `write(dst, bytes)`. Phase A ships `LocalFile` only. The CLI calls
  core::build → transport.write (export) and transport.read → core::parse →
  core::install (import).

| File | Responsibility |
|---|---|
| `mur-common/src/fleet_bundle.rs` | `FleetBundle`/`BundleManifest`/`BundleEntry` types, format version const, content-hash + canonical-manifest serialization. **No I/O.** |
| `mur-core/src/cmd/fleet/export.rs` | gather fleet.yaml + fleet-scoped skills (+ optional member exports) → hash → sign manifest → archive → write |
| `mur-core/src/cmd/fleet/import.rs` | read → verify signature + hashes → resolve trust tier → scan skills → HITL confirm → install (skills, fleet.yaml, members) → report |
| `mur-core/src/cmd/fleet/bundle_transport.rs` | `FleetBundleTransport` trait + `LocalFile` impl (seam for TeamServer/OfficialRegistry) |
| reuse | skill install + security scan (`mur skill`), `fleet_sync` entity-assembly for `--with-members` (profile-minus-key + skills), `store::{load_fleet,save_fleet}`, `a2a_dial::canonicalize_agent_name`, `mur-channel` sign/verify |

## 4. Bundle format

A `.fleet` file is an archive (tar + zstd) with this internal layout:

```
bundle.yaml                 # BundleManifest (see below) — the ONLY signed object
fleet.yaml                  # fleet definition (host-specific .last_run/.stopped excluded)
skills/<skill>/skill.yaml   # each fleet-scoped skill (scope:Fleet, fleet=<name>)
members/<agent>/...         # only with --with-members: single-agent export per member
```

`BundleManifest` (in `bundle.yaml`):

```yaml
format_version: 1               # const FLEET_BUNDLE_FORMAT = 1
fleet_name: devteam
created_at: "2026-06-20T..Z"    # stamped by the CLI after build (not in workflow)
signer_pubkey: <multibase>      # exporter's concierge identity pubkey (§7)
signer_fingerprint: "ab12-cd34" # short, human-checkable
includes_members: false
members: [pm, qa, rustsmith]    # declared member names (always listed)
entries:                        # every file pinned by content hash
  - { path: fleet.yaml,                 sha256: ... }
  - { path: skills/triage/skill.yaml,   sha256: ... }
sig: <multibase Ed25519 over canonical(manifest-without-sig)>
```

**Signing rule:** the **manifest** is the signed object; it pins every file by
SHA-256. The archive container itself need not be byte-deterministic — verifying
each extracted file's hash against the manifest, plus the manifest signature, is
sufficient. Canonical sign-input excludes the `sig` field itself.

## 5. CLI surface

```
mur fleet export <name> [--with-members] [-o <file>]
    # default output: <name>.fleet in cwd
mur fleet import <file.fleet> [--force] [--no-members] [--yes]
    # --force: overwrite an existing fleet/skill of the same name
    # --no-members: skip member-agent install even if the bundle has them
    # --yes: pre-approve the install confirmation (still verifies + scans; for scripts)
```

(Wired into `cli/actions.rs` `FleetAction` + `dispatch.rs`, mirroring existing
fleet subcommands.)

## 6. Export flow (`mur fleet export`)

1. `store::load_fleet(name)` (error if absent).
2. Collect this fleet's fleet-scoped skills: scan installed skills, keep those
   with `scope == Fleet && fleet == <name>` (reuses the B-full scope fields).
3. If `--with-members`: for each member, assemble its agent export reusing
   `fleet_sync`'s entity-assembly — `AgentProfile` with the **signing private key
   stripped** (the same stripping `fleet_sync` already does for device sync) +
   the member's skills + its entitlements.
4. Strip host-specific state from `fleet.yaml` (no `.last_run`/`.stopped`;
   `channel_id` is derived from the name so it travels fine).
5. Compute each entry's SHA-256 → build `BundleManifest` → **sign** the canonical
   manifest with the exporter's concierge identity (§7) → archive to `<name>.fleet`.
6. Print: output path, signer fingerprint, # skills, # members, members-included flag.

## 7. Signing identity

The exporter signs with the **local concierge agent identity**
(`~/.mur/agents/mur/identity`) — present on every install (the self-contained Hub
ships the `mur` concierge), stable, and already an Ed25519 keypair via
`AgentIdentity`. The manifest carries `signer_pubkey` + a short
`signer_fingerprint`; import surfaces the fingerprint for out-of-band verification
(TOFU). (Alternative considered: a dedicated device key — deferred; the concierge
key avoids introducing a new identity in Phase A.)

## 8. Trust model (two-tier)

The signature proves **who sent it, not that it is safe** — provenance ≠ trust.

- **Tier 1 — peer bundle (default):** signer key is unknown → **TOFU**. Import
  shows the signer fingerprint, runs the skill security scan, and requires
  **explicit HITL confirmation**. Imported skills land at the **lowest trust
  tier** (Draft/untrusted) regardless of any `trust:` they claim; the user
  curates up later (`mur skill curate`/`trust`).
- **Tier 2 — official bundle:** signer key matches a **pinned publisher key**
  shipped with the client → higher trust; the scan still runs, but a clean
  official bundle may install without per-skill interrogation. (The pinned-key
  list + the official catalog are Phase B / server; Phase A defines the *check*
  and ships an empty/dev pin set so the mechanism is testable.)

`--yes` pre-approves the confirmation but **never** skips signature verification
or the security scan.

## 9. Import flow (`mur fleet import`)

1. Read archive (via the `LocalFile` transport) → extract to a temp dir.
2. Parse `bundle.yaml`; **verify** the manifest signature and **every** entry's
   SHA-256 against the manifest. Invalid/tampered → **refuse**. Unsigned →
   refuse unless `--force` (and then still scan + confirm, marked untrusted).
3. Resolve trust tier (§8) from `signer_pubkey` vs the pinned set.
4. Show provenance: signer fingerprint + tier, fleet name, skills to install,
   members (declared + which are bundled), entitlements any bundled member
   requests.
5. Run the **skill security scanner** on each bundled skill; surface findings.
6. **HITL confirm** (unless `--yes`): nothing is written before approval.
7. Install:
   - skills → installed with `scope:Fleet, fleet=<name>`, **trust downgraded**
     per tier (§8); name collision → skip + report (or overwrite with `--force`).
   - `fleet.yaml` → `store::save_fleet`; name collision → refuse unless `--force`.
   - members → §10.
8. Print a summary: installed skills, fleet, members installed vs **missing**
   (and how to supply them). **Never auto-runs the fleet.**

## 10. Member handling (hybrid, least-privilege)

- **Default (no `--with-members` in the bundle, or `--no-members`):** install the
  fleet definition + skills; for each member, `canonicalize_agent_name` against
  local agents; **report** which are missing ("missing qa, rustsmith — create
  them or import a `--with-members` bundle"). The fleet installs; running it later
  just needs the members present.
- **`--with-members` bundle:** for each **missing** member, install the bundled
  agent export — with the **signing identity regenerated locally** (private keys
  never travel), **no secrets** carried, and its **entitlements shown +
  confirmed** before install. An existing local agent of the same name is **never
  overwritten** (skip + report, or `--force`).

## 11. Conflict / re-import

Name-keyed. Re-importing an existing fleet/skill **refuses** unless `--force`
(mirrors `mur skill install`). Phase A does **not** do version merge — that is
the server-sync seam's job (LWW), §12.

## 12. Future-layer seams (defined, not implemented in Phase A)

- **Transport seam:** `FleetBundleTransport` (§3). `TeamServer` and
  `OfficialRegistry` impls slot in later without touching build/parse.
- **① sync/share (Pro/Team):** `mur fleet share --team <id>` / `mur fleet pull`
  push/pull the **same** bundle bytes via a team-scoped server endpoint
  (proposed contract: `POST/GET /api/v1/core/teams/{id}/fleets/`), reusing
  `fleet_sync`'s manifest/LWW for conflict. Entitlement-gated. *Server work,
  separate repo — Phase A only specs the contract.*
- **② official catalog (Pro/Team):** `mur fleet install <official-name>` pulls a
  bundle from a registry and verifies the **pinned publisher key** (Tier 2).
  Catalog/publishing/Hub storefront are separate efforts.
- **commander (governance):** private/governed org catalogs + constitution
  enforcement + audit emission, in the closed crate. Phase A's two-tier trust +
  signed manifest are the substrate it builds on.
- **entitlement seam:** a single check point (`fn require_fleet_sync_entitlement`)
  the server transports call; Phase A's LocalFile path is ungated (local export
  is free, like exporting your own agent).

## 13. Error handling

- Missing fleet / bad archive / missing manifest → clear, actionable error.
- Signature or any file hash mismatch → **refuse** (no partial install).
- Unsigned bundle → refuse unless `--force` (then untrusted + scan + confirm).
- Skill name collision → skip that skill + report; fleet name collision → refuse
  unless `--force`.
- Member missing → **report, do not block** (fleet still installs).
- Member agent name collision → never overwrite; skip + report (or `--force`).

## 14. Testing

- **Pure (`mur-common`):** `FleetBundle`/manifest serde roundtrip; canonical
  manifest hashing; sign → verify; **tamper → verify fails** (flip a byte in an
  entry, flip the sig); missing-member detection over a member list vs a local set.
- **Security:** a bundle whose skill trips the scanner is flagged; an
  exporter-claimed `trust: Canonical` skill installs at the **lowest** tier;
  unsigned bundle refused without `--force`.
- **Real CLI smoke (temp `MUR_HOME`s):** export a fleet (+ scoped skills) →
  import into a fresh `MUR_HOME` → assert `fleet.yaml` + skills present, `scope:Fleet`
  stamped, missing members reported; `--force` overwrite; `--with-members`
  round-trip installs a regenerated-identity member with no secrets.

## 15. Out of scope (explicit)

Shared *live execution* (one channel across hosts, remote member dial); server
transport implementation; official catalog/registry + publishing pipeline; Hub
storefront UI; the commander engine; version-merge/LWW on re-import. All are
future layers (§12) on this bundle foundation.
