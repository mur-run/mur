# Secrets on disk: replace the deny list with a rule

**Date:** 2026-08-18
**Status:** design, decision requested — nothing implemented
**Issues:** #850 (read confinement), #979 (unredacted capture)
**Follows:** #975, #976, #978

## Why this exists

The deny list has been wrong three times in one day:

| round | what was added | what it missed |
|---|---|---|
| #976 | `secrets/`, `auth.json`, `identity.key` | `commander/signing.key`, `mobile/pair-token`, `actions-runner/` |
| #976 (2nd) | those three | `queue/` and the other capture stores |
| #978 (2nd) | capture stores | `commander/.env`, `runtime/vlc.json` — **in a directory the list had already touched** |

Each round was found by looking harder, not by the mechanism catching anything.
A list only covers what someone remembered; the next credential lands somewhere
new and is readable until a human notices. That is the property to change.

## What is actually on disk

Measured on a live install, not assumed:

```
models.yaml secret refs:   file × 6,   env × 0,   keychain × 0
  file: → ~/.mur/secrets/anthropic.key
  file: → ~/.mur/secrets/deepseek.key
  file: → ~/.mur/secrets/omlx.key
```

**Every model credential is a plaintext file**, referenced by absolute path.
Keychain is supported (`SecretRef::Keychain`) and documented as the path the
Hub uses — and is used for none of them here.

Also on disk, none of it MUR-managed:

```
commander/.env           SLACK_BOT_TOKEN, SLACK_SIGNING_SECRET,
                         SLACK_APP_TOKEN, ANTHROPIC_API_KEY
actions-runner/.creds    self-hosted CI runner credentials
runtime/vlc.json         VLC control password
queue/events.jsonl       934 MB of unredacted command lines (#979)
```

Note `~/.mur/secrets/` is **not a MUR convention** — nothing in the Rust code
reads or writes it. It is simply where this user's `file:` refs happen to point.
A design that assumes MUR owns that directory would be assuming something
untrue.

## Two candidate rules

### (A) One private root

`~/.mur/private/`, denied as a single subpath on both backends. Everything
secret moves under it.

Replaces the list with a rule, and no enumeration — a new file inside it is
covered the moment it is created.

What it does not solve: things MUR does not own. `actions-runner/` is a GitHub
runner install, `commander/.env` belongs to the commander bridge, and neither
will move because MUR would like them to. So (A) shrinks the list without
eliminating it.

Cost: a migration that rewrites every `file:` ref, plus every doc and script
that names a path under `~/.mur/secrets/`.

### (B) Take the plaintext off disk

Migrate `file:` refs to `keychain:`. A Keychain-backed secret is not a file, so
no sandbox rule is needed for it and no deny can be forgotten.

`mur model connect` already writes to the Keychain; `SecretRef::Keychain`
already resolves. The gap is that existing `file:` refs are never migrated, and
nothing warns that a plaintext ref is a plaintext ref.

What it does not solve: the same non-MUR files as (A), plus the capture stores,
which are not secrets but records.

Cost: a migration command plus a warning. Much smaller than (A); the Keychain
path already works headless here (Developer ID signing, one Always Allow).

## Recommendation

**(B), then (A) for the remainder.**

(B) removes the largest class — model credentials — from disk entirely, and it
is the only option where forgetting to add something to a list cannot hurt,
because there is no file to protect. It is also the smaller change.

(A) then covers what genuinely must stay on disk. Its value is lower once (B)
has run, which is an argument for doing (B) first rather than both at once.

Neither replaces the deny list immediately: `queue/`, `actions-runner/` and
`commander/.env` stay where they are under either, so the list survives — just
shorter, and no longer the only thing standing between an agent and an API key.

## What would make this measurable

A check — `mur model doctor` is the natural home — that reports every
`SecretRef::File` as "plaintext on disk, readable by anything that can read the
path", with the `keychain:` equivalent to run. Today nothing says a `file:` ref
is weaker than a `keychain:` one, so the six on this machine look like a
configuration choice rather than a default nobody revisited.

That check is worth building even if neither migration is: it turns "we have a
deny list" into "here is what is still exposed and why".

## Decisions — settled

### 1. (B) first, then (A). Settled.

### 2. `file:` stays. The defect is that it is silent, not that it exists.

Half of this was already true in the code: **`mur model connect` writes
`SecretRef::Keychain`** (`cmd/model_connect.rs:207`) and never offers `file:`.
The six `file:` refs on this machine came from `model add` or a hand-edit, not
from `connect`.

And `file:` cannot be removed, for reasons that are checkable rather than
hypothetical:

- `keyring` v3 is built with `apple-native`, `linux-native`,
  `sync-secret-service`, `windows-native` — cross-platform, but a **headless
  Linux box with no Secret Service daemon has no keyring at all**. `file:` is
  the only backend that works there.
- `mur model import` carries refs from another machine; a ref format the
  importer cannot represent breaks that path.
- Containers and CI materialise secrets as files by convention — Vault agent,
  SOPS, a k8s secret mount. Those are files on purpose.

Removing the variant leaves those with no substitute.

So the rule is **loud, not absent**:

- `model add` warns at the moment it accepts a `file:` ref, naming the
  `keychain:` equivalent. Warn, not refuse — see decision 3.
- `model doctor` reports standing ones (below).
- The migration in (B) moves the ones that can move, and leaves the rest with
  the warning attached.

What changes is that a plaintext ref becomes a **stated choice** instead of a
default nobody revisited.

### 3. Report. Never block — and the repo already argued this.

`cmd_model_doctor` returns `Ok(())` on every path, prints
`"nothing was changed"`, and already has a `Level::Warn` / error split in its
output. It is a read-only reporter by contract; making it fail would change
what the command *is*, not just what it says.

The reasoning is already written down in this repo, in `.github/workflows/eval.yml`:

> A gate that fires for things you cannot fix is a gate that gets switched off,
> and then the replacement never gets built.

A `file:` ref on headless Linux is precisely a thing the user cannot fix. A
check that fails it teaches people to stop running the check.

So: a new `Level::Warn` finding per `SecretRef::File`, with the exact
`keychain:` command to run. **No change to the exit code**, which stays zero.

If enforcement is ever wanted — a CI job asserting no plaintext secrets — it
belongs behind an explicit opt-in (`--fail-on-plaintext`) chosen by someone who
knows their environment can satisfy it. Default useful, strict mode available,
and the strict mode is never the thing that greets a new user.
