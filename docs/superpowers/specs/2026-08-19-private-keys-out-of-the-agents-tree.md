# Move private keys out of the agents tree (#850 option (c))

**Date:** 2026-08-19
**Status:** design — nothing implemented
**Issue:** #850
**Follows:** #975, #1003, #1006, #1010
**Supersedes the "option (c)" sketch in** `2026-08-18-agent-read-confinement-audit.md` §4

## Why this exists

Two sandbox rules currently work by **enumerating** `<mur_home>/agents/*`:

| rule | shipped in | enumerates |
|---|---|---|
| deny reads of sibling signing keys | #975 | `agents/*/identity.key` |
| grant reads of peer public material | #1006 | `agents/*/identity.pub`, `agents/*/rotations.jsonl` |

Both are sealed when the policy is built, so both carry the same defect: **an
agent created afterwards is not in the list.** For the deny that means a new
sibling's key stays readable to an already-running agent; for the grant it means
a new peer's events cannot be verified. Neither is fixable by a better list —
the list is the problem.

Neither backend can express "a file with this name under any agent home": SBPL
emits `(subpath "literal")` here and Landlock is path-fd based. So the rule has
to become a *subtree*, and that means the private and public material cannot
share a directory.

## The load-bearing fact, verified

**An agent never needs to read its private key after the sandbox seals.**

```
mur-agent-runtime/src/supervisor.rs:174   AgentIdentity::load(&agent_home)
mur-agent-runtime/src/supervisor.rs:314   sandbox::apply(...)
```

The identity is loaded 140 lines before the seal and held in an `Arc`; nothing
in the runtime reloads it (`AgentIdentity::load` appears exactly once in
`mur-agent-runtime`, at that line).

This is what makes the design cheap: `<mur_home>/keys/` can be denied **whole,
on both backends, with no re-allow and no exception for the agent's own key**.
Today's macOS tier-3 self-protection already denies an agent its own
`identity.key` for the same reason, and works.

## The shape

```
~/.mur/keys/<agent>/identity.key        private, 0600, denied to every sandbox
~/.mur/agents/<agent>/identity.pub      public
~/.mur/agents/<agent>/rotations.jsonl   public
~/.mur/agents/<agent>/profile.yaml      …everything else, unchanged
```

Then both rules collapse into subtrees:

| rule | today | after |
|---|---|---|
| private keys | enumerate `agents/*/identity.key` | `deny (subpath "<mur_home>/keys")` |
| peer public material | enumerate 2 files × N agents | grant `agents/` read whole |

No enumeration, no after-seal gap, and `LaunchChain::sibling_signing_keys()` and
`peer_public_key_material()` both disappear.

## Migration surface, measured

`identity.key` appears in 26 files. Most are sandbox rules or tests. The paths
that actually construct or move the file:

| file | sites | what it does |
|---|---|---|
| `mur-common/src/identity.rs` | 2 | **the chokepoint** — `save()` / `load()` join the name |
| `mur-core/src/cmd/agent_rekey.rs` | 7 | writes `identity.key.prev`, renames from a scratch dir |
| `mur-common/src/muragent/installer.rs` | 10 | preserves the local keypair across a bundle update (mostly tests) |
| `mur-core/src/cmd/fleet_sync.rs` | 5 | existence check ("is this dir a real agent") |
| `mur-core/src/cmd/agent/lifecycle.rs` | 3 | existence check |
| `mur-core/src/cmd/fleet/import.rs` | 6 | import side |

**Export does not carry private keys** — `fleet/export.rs:146` says so in as
many words ("Bundle each member's `profile.yaml` (never the private
`identity.key`)"), and `agent/export.rs` has zero mentions. That removes the
largest feared cost: `.muragent` / `.fleet` bundle formats do not change.

Because `save`/`load` are a genuine chokepoint, most call sites need no edit at
all — they pass a directory and let identity.rs join the name. The work is
teaching those two functions that the private half lives elsewhere.

## Proposed API

Rather than thread a second path through every caller, keep the existing
`dir: &Path` signature and resolve the private half from it:

```rust
/// `<mur_home>/agents/<name>` → `<mur_home>/keys/<name>/identity.key`
fn private_key_path(agent_dir: &Path) -> PathBuf
```

That keeps `AgentIdentity::save(&agent_home)` / `load(&agent_home)` working
verbatim at all ~15 call sites. The mapping is derivable because every caller
already passes a canonical `agents/<name>` path.

**A live bug this survey turned up.** `skill_publish::resolve_publisher_identity`
guards on one file and writes another:

```rust
let key_path = home.join(".mur").join("publisher-identity.key");
if key_path.exists() {
    AgentIdentity::load(&home.join(".mur"))          // reads ~/.mur/identity.key
} else {
    AgentIdentity::generate().save(&home.join(".mur"))   // WRITES ~/.mur/identity.key
```

`publisher-identity.key` is never created by anything, so the guard is always
false; `save()` is `fs::write`, which truncates. On a machine that has a host
key — this one does — `mur skill publish` therefore **overwrites
`~/.mur/identity.key` with a freshly generated key**. That is unrecoverable:
events signed by the old key stop attributing and the rotation chain breaks.

It is filed separately rather than folded in here, but it is the sharpest
possible illustration of why this spec treats key moves as the risky part.

**Sharp edge:** three callers pass something that is *not* under `agents/` —
`commander.rs:136` (`<mur_home>/commander`), `skill_publish.rs:166`
(`~/.mur/publisher-identity.key`), and the top-level host `~/.mur/identity.key`.
The mapping must leave those alone; they are already covered by
`credential_paths()` as fixed subtrees and have no enumeration problem. This is
the main correctness risk in the change and wants a test per case.

## What does NOT get better

- `channel_writer::append_as_writer` loads the **router's** key
  (`agents/mur/identity.key`) from a possibly-sandboxed `mur`. That is denied
  today (#975) and stays denied — #1010 made the resulting downgrade loud
  instead of silent. Option (c) does not change it; it makes the denial uniform
  rather than enumerated.
- `commander/signing.key`, `auth.json`, `secrets/` — fixed paths already, no gap.

## Migration mechanics

On first run after upgrade, for each `agents/<name>/identity.key`: create
`keys/<name>/`, move the file (0600, `rename` within the same filesystem), leave
`identity.pub` and `rotations.jsonl` in place.

Two properties to preserve, each a test:

1. **Idempotent** — a second run finds nothing to move.
2. **Never lossy** — if `keys/<name>/identity.key` already exists and differs
   from the one in `agents/`, stop and report rather than overwrite. A wrong
   merge here silently changes an agent's identity, which forges attribution on
   every channel it writes to.

Where it runs is an open decision (below).

## Open decisions

1. **Where the migration runs.** `mur update` is explicit and observable but
   skippable; the runtime's startup path is self-healing but does key I/O
   before the seal in a process that is about to be confined. The trust-store
   migration (#1004) chose startup and it worked — but that store is
   reconstructible and a private key is not.
2. **Rollback.** Is a `--revert` worth building, or is "restore from backup" the
   answer? A half-migrated tree where some agents have keys in `keys/` and some
   in `agents/` must not be a silent state.
3. **Sequencing against the deny.** The subtree deny cannot ship before every
   key has moved, or agents lose access to their own identity at load. Either
   two releases, or one release where the deny is derived from what is actually
   on disk.

## Why this is worth doing

It is the only variant that removes the gap rather than shrinking it, and it
*deletes* two enumeration routines plus their after-seal caveats instead of
adding a third. Against that: it moves a private key, which is the one file in
the tree where a mistake is not recoverable.
