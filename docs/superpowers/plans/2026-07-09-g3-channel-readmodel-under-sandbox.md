# G3: Sandboxed Members vs the Channel Read-Model — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let sandboxed fleet members complete the v3d-2 peer-writes-own path cleanly — their signed self-reply already lands in the channel's `events.jsonl`, but the follow-up SQLite read-model refresh (`~/.mur/index/channels.db`) fails under the kernel sandbox and poisons the whole append with a misleading error.

**Architecture:** Two small, complementary fixes. (1) The sandbox policy already carves out `<mur_home>/channels` as runtime-owned shared state (`policy.rs:157` — "peer-writes-own, v3d-2"); extend the same carve-out to `<mur_home>/index` (the SQLite read-model + lance stores are the same trust tier: runtime-owned, droppable, rebuildable — `index.rs:9`). (2) In `mur-channel`'s `ChannelService`, a failed manifest/index refresh AFTER a successful event append becomes a warn, not an error — per the crate's own "droppable & rebuildable" contract, a projection failure must not fail a write whose event is already durable.

**Tech Stack:** Rust (edition 2024), `mur-agent-runtime` + `mur-channel`. No new dependencies (`tracing` already in mur-channel).

## Root Cause (empirically pinned, 2026-07-09 live fleet run)

`channel/delegate` on a sandboxed worker logged
`failed to append self-reply to channel error=attempt to write a readonly database`
— yet the fleet channel's `events.jsonl` **contains the worker's signed
`Agent{dr_worker_1}` events**. Dissection of `append_signed`
(`mur-channel/src/service.rs:220`):

1. `store.append_event` → `~/.mur/channels/<id>/events.jsonl` — **succeeds**
   (the sandbox already grants `<mur_home>/channels`, `policy.rs:157-175`).
2. `store.save_manifest` → `channel.yaml` — succeeds (same carve-out).
3. `index.upsert` → SQLite `<mur_home>/index/channels.db` — **fails**:
   `~/.mur/index` is NOT in the sandbox write allowlist, SQLite maps the
   denied write to `SQLITE_READONLY` ("attempt to write a readonly
   database"), and the `?` at `service.rs:250` turns a stale-projection
   condition into a hard `Err` — even though the event of record is durable.

Consequences today: misleading "failed to append" logs on every delegated
turn, and a stale `channels.db` read-model (Hub/`channel_query` summaries,
`updated_at` ordering) until an unsandboxed writer touches the channel.

The index is BY DESIGN disposable: "SQLite read-model … Droppable &
rebuildable" (`mur-channel/src/index.rs:9`).

## Global Constraints

- **Fail-closed where it matters:** the event append itself (`store.append_event`) and channel creation/membership/deletion (where the manifest IS the primary record) keep their hard `?` error propagation. ONLY the post-append manifest+index refresh becomes non-fatal.
- **Landlock gotcha:** path rules on paths that don't exist at seal time are silently skipped — the index-dir grant must `create_dir_all` before granting, exactly like the existing channels carve-out (`policy.rs:168-174` idiom).
- **Same trust tier only:** the new grant is `<mur_home>/index` (runtime-owned rebuildable projections: channels.db, *.lance, capabilities.json). It must NOT grant anything else outside `agent_home`.
- No hardcoded values; single source file ≤ 800 lines; `cargo clippy --workspace -- -D warnings` + `cargo fmt --check` clean.
- Tests: `export ORT_STRATEGY=download`; plain `cargo test -p <crate> <filter>`. Permission-flip tests are `#[cfg(unix)]`.

---

### Task 1: Sandbox carve-out for `<mur_home>/index`

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (immediately after the channels carve-out block ending at line ~175, inside `from_entitlements`)

**Interfaces:**
- Produces: worker `SandboxPolicy.fs_write` containing `<mur_home>/index`. No new API.

- [ ] **Step 1: Write the failing test** (append to `policy.rs` tests; follow the file's existing test idiom for building `Entitlements` + calling `from_entitlements` — there are sibling tests asserting `fs_write` contents; copy their construction)

```rust
    #[test]
    fn index_dir_is_granted_like_channels() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents").join("w1");
        std::fs::create_dir_all(&agent_home).unwrap();
        let ent = mur_common::agent::Entitlements::default();
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
        assert!(
            policy.fs_write.contains(&mur_home.join("channels")),
            "pre-existing channels carve-out must remain"
        );
        assert!(
            policy.fs_write.contains(&mur_home.join("index")),
            "index read-model dir must be granted alongside channels"
        );
        // The grant idiom creates the dir so Landlock rules stick.
        assert!(mur_home.join("index").is_dir());
    }
```

(If `policy.rs` tests don't already import `tempfile`, check the crate's dev-dependencies — `tempfile` is used elsewhere in mur-agent-runtime tests; adapt to however sibling `from_entitlements` tests set up paths.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime index_dir_is_granted`
Expected: FAIL — `fs_write` does not contain `<mur_home>/index`.

- [ ] **Step 3: Implement the grant**

Insert directly after the channels carve-out block (after line ~175), same shape:

```rust
        // The channel read-model (`<mur_home>/index`: channels.db + lance
        // stores) is the same runtime-owned, rebuildable tier as the channel
        // store above: a delegated agent's self-append refreshes
        // channels.db's `updated_at` row. Without this grant SQLite maps the
        // denied write to SQLITE_READONLY and every peer-writes-own append
        // reports a false failure (G3, live fleet run 2026-07-09). Same
        // create-before-grant idiom as `channels` (Landlock skips rules on
        // paths that don't exist at seal time).
        if let Some(index_dir) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join("index"))
            && !fs_write.contains(&index_dir)
        {
            let _ = std::fs::create_dir_all(&index_dir);
            fs_write.push(index_dir);
        }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-agent-runtime index_dir_is_granted && cargo test -p mur-agent-runtime sandbox && cargo clippy -p mur-agent-runtime -- -D warnings`
Expected: new + all pre-existing sandbox tests PASS, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/policy.rs
git commit -m "fix(sandbox): grant the channel read-model dir (~/.mur/index) like the channel store (G3)"
```

---

### Task 2: Post-append read-model refresh becomes non-fatal

**Files:**
- Modify: `mur-channel/src/service.rs` — add one private helper; switch exactly FOUR call sites to it: `append_message` (~line 163-167), `append` (~209-213), `append_signed` (~247-251), `transition` (~300-305).
- Do NOT touch the fatal sites: `create_for_agent` (:97), `create_for_fleet` (:143), `create_for_workflow` (:187), `add_participant` (:356), `remove_participant` (:370), `delete_channel` (:378) — there the manifest/index write IS the operation.

**Interfaces:**
- Produces: `fn refresh_read_model(&self, ch: &Channel)` (private). No public API change; the four append-family methods keep their exact signatures and success payloads.

- [ ] **Step 1: Write the failing test** (append to `service.rs` tests, following the file's existing test setup idiom — `append_signed_stores_verifiable_sig` at ~line 442 shows how a test service + channel are built; reuse that setup)

```rust
    // Unix-only: exercises a read-only index via fs permission bits.
    #[cfg(unix)]
    #[test]
    fn append_survives_readonly_index() {
        use std::os::unix::fs::PermissionsExt;
        let (svc, mur_home, ch, identity) = /* same setup as
            append_signed_stores_verifiable_sig: tempdir MUR_HOME, open
            service, create a channel, build a test AgentIdentity — copy
            that test's construction verbatim */;

        // Freeze the read-model: db file read-only + dir non-writable so
        // SQLite cannot write pages or create -wal/-shm.
        let index_dir = mur_home.join("index");
        let db = index_dir.join("channels.db");
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // The append must still succeed — the event log is the record.
        let ev = svc
            .append_signed(
                &ch.id,
                &identity,
                1,
                mur_common::channel::ChannelActor::Agent { id: "w1".into() },
                mur_common::channel::EventKind::Message,
                serde_json::json!({ "text": "hi" }),
                None,
            )
            .expect("append must not fail on a read-only read-model");

        // Restore perms so tempdir cleanup works.
        std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();

        // The event is durably in the log.
        let events = svc.load_events(&ch.id).unwrap();
        assert!(events.iter().any(|e| e.seq == ev.seq));
    }
```

Adaptation note: fill the setup from the sibling test; if SQLite's already-open connection can still write despite the permission flip on some platform (WAL pre-created), ALSO chmod any existing `channels.db-wal`/`channels.db-shm` to 0o444 in the same block — the assertion that matters is `append_signed` returning `Ok` while the log gains the event.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-channel append_survives_readonly_index`
Expected: FAIL — `append_signed` currently propagates the SQLite error.

- [ ] **Step 3: Implement the helper + switch the four sites**

Add to `impl ChannelService` (near the private helpers at the bottom, before the tests):

```rust
    /// Refresh the manifest + SQLite read-model after a successful event
    /// append. Both are rebuildable projections of `events.jsonl` — a
    /// refresh failure must not fail an append whose event is already
    /// durable. Concretely: a sandboxed delegate (peer-writes-own, v3d-2)
    /// may be able to write the channel store but not the shared index;
    /// SQLite reports the denied write as "attempt to write a readonly
    /// database" (G3, live fleet run 2026-07-09).
    fn refresh_read_model(&self, ch: &Channel) {
        if let Err(e) = self
            .store
            .save_manifest(ch)
            .and_then(|()| self.index.upsert(ch))
        {
            tracing::warn!(
                channel_id = %ch.id,
                error = %e,
                "read-model refresh failed after append (event persisted; index is rebuildable)"
            );
        }
    }
```

Then in `append_message`, `append`, and `append_signed`, replace:

```rust
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
```

with:

```rust
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.refresh_read_model(&ch);
        }
```

And in `transition` (which also sets state), replace:

```rust
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.state = new_state;
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
```

with:

```rust
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.state = new_state;
            ch.updated_at = ev.ts;
            self.refresh_read_model(&ch);
        }
```

(If `save_manifest` returns a non-`()` Ok type, adjust the `.and_then(|()| …)` closure binding accordingly — keep the two operations sequenced with the combined error branch.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-channel && cargo clippy -p mur-channel -- -D warnings && cargo fmt --check`
Expected: new test + all pre-existing mur-channel tests PASS (signature/fold/idempotency tests unaffected — the append path's success payload is unchanged), clippy + fmt clean.

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/service.rs
git commit -m "fix(channel): post-append read-model refresh is non-fatal (event log is the record) (G3)"
```

---

### Task 3: Documentation

**Files:**
- Modify: `docs/architecture/runtime-overview.md` — in the channel/delegate (v3d-2 peer-writes-own) description, add: "Sandboxed members get write access to both the channel store (`~/.mur/channels`) and the read-model dir (`~/.mur/index`); independently, a failed manifest/SQLite refresh after a successful append is a warning, not an error — `events.jsonl` is the record and the index is rebuildable."

**Interfaces:** none (docs only).

- [ ] **Step 1: Make the edit** (verbatim sentence, inside the existing section — search "peer-writes-own" or "channel/delegate"; do not create a new section)

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/runtime-overview.md
git commit -m "docs(channel): document index-dir grant + non-fatal read-model refresh (G3)"
```

---

## Operator Verification (manual, after merge)

1. Rebuild the runtime (`cargo build --release -p mur-agent-runtime`) — worker symlinks point at `target/release/mur-agent-runtime`.
2. Start `dr_worker_1..4`, then run one live iteration: `mur deep-research run deep-research --max-iterations 1`.
3. **Expected (fixed):** worker stderr has NO `failed to append self-reply` / `readonly database` lines; `~/.mur/channels/fleet-deep-research/events.jsonl` gains new signed `Agent{dr_worker_N}` events; `~/.mur/index/channels.db` mtime updates during the run.
4. This is also the first post-G1 live iteration — watch whether the intermittent `Step sN failed (exit 1)` recurs now that tool calls succeed; if it does, capture the loop stderr for the follow-up diagnosis (the failure was never explained by G3: the self-append is best-effort and cannot fail the RPC).

## Out of Scope (tracked separately)

- **G4** skill loader rejects `fleet:<name>` scoped refs — next in queue.
- **G2** gateway search tier (`spawn agent-browser` under sandbox).
- Rebuild-on-drift tooling for `channels.db` (a `mur internals reindex`-style command) — the index already self-heals on the next unsandboxed write; only worth building if staleness is observed in practice.

## Self-Review Notes

- Fatal-vs-non-fatal boundary is explicit: only the four post-append refresh sites change; creation/membership/deletion keep hard errors (manifest is primary there).
- Task 1's grant reuses the exact channels-carve-out idiom incl. the Landlock create-before-grant rule; test asserts both grants coexist.
- Task 2's test pins the contract (append Ok + event durable under a frozen read-model) rather than the SQLite error string, so it stays valid across SQLite versions.
