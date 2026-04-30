# Model Registry + Secret References — COMPLETE

Plan: `2026-04-29-model-registry-and-secret-refs-plan.md`
Spec: `2026-04-29-model-registry-and-secret-refs-design.md`
Branch: `feat/model-registry-and-secret-refs` (worktree
`.worktrees/model-registry`).

All 5 PRs of the plan landed on the branch as 22 commits. Workspace
builds, clippy `--all-targets` clean, fmt clean, tests pass (lib + 3
new integration tests). The GUI shell (mur-agent-gui) is workspace-
excluded; verified independently via `cargo check` + `tsc -b` + `vite
build` from inside the worktree.

## PR-1 — `mur-common::secret::SecretRef`

| Task | Commit |
|---|---|
| 1.1 deps (kept from plan; corrected in 1.4) | `b6b4ff8`, then `61a5171` (pin keyring v3) |
| 1.2 SecretRef enum + serde | `863f336` |
| 1.3 Env variant | `4604c3e` |
| 1.4 Keychain variant | `ca1d964` |
| 1.5 File variant (plaintext + .age) | `c6ea2c9` |
| 1.6 Cmd variant | `b3072f1` |
| 1.7 + 1.8 SecretRef::check + keychain_{set,delete} | `5be4679` |
| 1.9 Workspace gate | `d2d2b0f`, `ec6f773` (fmt sweeps) |

**Notable deviations from the plan:**

- **keyring v4 → v3.** The plan was written when keyring v4 was new;
  v4.0.0 turned out to be a "samples + CLI" wrapper with no `[features]`
  and a fundamentally different API surface (real API moved to
  `keyring-core`). Pinned to v3.6.3 (already in lockfile as a transitive
  dep). Plan doc now records the trap so the next reader doesn't repeat
  it.
- **Custom test fixture for keychain.** v3's stock `keyring::mock`
  advertises `CredentialPersistence::EntryOnly`, so each `Entry::new`
  gets its own private storage and our resolve-via-fresh-Entry pattern
  sees no data. Wrote a `SharedMockBuilder` (~80 LoC, test-only) backed
  by an `Arc<Mutex<HashMap>>` that all entries share. Lifted to a
  reusable `keychain_test_fixture` module so 1.4 + 1.7 + 1.8 tests all
  use it.
- **age 0.11 API.** `Decryptor` is a struct now, not an enum
  (no `Decryptor::Recipients(_)`). `Encryptor::with_recipients` takes
  `impl Iterator<Item = &'a dyn Recipient>` not `Vec<Box<...>>`. Code +
  tests updated accordingly.

## PR-2 — `mur-common::model` + runtime resolution

| Task | Commit |
|---|---|
| 2.1 + 2.2 ModelEntry / Registry / load+save | `3855bf0` |
| 2.3 + 2.4 AgentProfile.model_ref + supervisor wiring | `2d1bec8` |
| 2.4 drive-by clippy fix in agent_rekey_cli | `607e197` |
| 2.5 e2e smoke script | `accc1e4` |

`AnthropicClient::from_secret_string` + `OpenAiClient::from_secret_string`
sit alongside the existing `from_env`. Supervisor's resolve_model_entry
prefers profile.model_ref → ~/.mur/models.yaml; falls back to the inline
`model:` block. Integration tests cover legacy, registry-hit, and
registry-miss; HOME mutation is serialized via a static Mutex.

## PR-3 — CLI verbs

| Task | Commit |
|---|---|
| 3.1 + 3.2 `mur model {add,list,show,remove,migrate}` | `41bdfe1` |
| 3.3 `mur agent secret {set,list,delete}` | `fa327bd` |

3 integration tests in `mur-core/tests/cmd_model.rs` (round trip, sad
paths). Set/delete delegate to the Phase-1 helpers; list cross-references
the registry to display the SecretRef + status without surfacing values.
New deps: `rpassword` (hidden prompt for `set` without an inline value).

## PR-4 — GUI sidecar env injection

| Task | Commit |
|---|---|
| 4.1 sidecar.rs resolve + inject | `eea2597` |
| 4.2 manual acceptance doc | `eea2597` (same commit) |

`SidecarManager::start` now blocks (via `tauri::async_runtime::block_on`)
on `resolve_secrets_for_agent`, which walks profile.model_ref → registry
→ SecretRef. Resolved value is forwarded to the sidecar as the
provider-appropriate env var (`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`).
The sidecar's existing `from_env` path picks it up unchanged. Resolution
is best-effort — failures fall through to the sidecar's own resolution
or the echo runner. Env keys (not values) are info-logged.

**Build hygiene:** added empty `[workspace]` table to
`mur-agent-gui/src-tauri/Cargo.toml` so cargo doesn't walk past the
worktree's root Cargo.toml (which excludes mur-agent-gui) and accidentally
attach to the main repo's root workspace when built from a worktree.

Acceptance doc: `scripts/e2e/p1-gui-secret-injection.md`.

## PR-5 — GUI Model tab

| Task | Commit |
|---|---|
| 5.1 Tauri commands | `f4ce1e8` |
| 5.2 React Model tab | `4aef6df` |
| 5.3 acceptance doc update | `a2da016` |

Four new commands: `list_models`, `get_active_model_ref`,
`set_active_model_ref`, `set_secret`. ModelEntryView is a serializable
projection — exposes `secret_ref` as a display string, `secret_status`
as the `check()` boolean, but never the value. set_secret rejects
non-keychain refs.

The Model tab lists the registry, lets the user switch active model
via radio, shows ✓/✗/no-secret-needed status pills per entry, and opens
a hidden-input modal for keychain: refs. CRUD on the registry stays in
the CLI by design (YAGNI for v1). Empty state shows the CLI command
to add the first entry.

Verified: `tsc -b` clean, `vite build` produces 170 kB / 52 kB gz.

## Out of scope (per plan)

- Auto OAuth refresh (P2 follow-up).
- Lifting secret/model crates to a published crate so mur-commander
  can drop its parallel implementation (do that only when a real
  cross-process need lands).
- GUI CRUD on the registry (Add new entry from GUI).
- `Literal` SecretRef variant.

## Suggested follow-up

- Run the manual acceptance flow end-to-end against a real signed
  Kelp.app to validate PR-4 + PR-5 together.
- Land the [-COMPLETE] marker, push, open the 5 PRs in order.
