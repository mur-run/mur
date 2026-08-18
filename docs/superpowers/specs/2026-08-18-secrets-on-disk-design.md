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

## Decisions

1. **(B) first, or (A) first, or neither?**
2. **Does `mur model connect` keep offering `file:` at all**, or does it become
   keychain-only with `file:` accepted for import and immediately migrated?
3. **Does the doctor check block anything**, or only report? Blocking a working
   setup because its secret is a file is the kind of gate that gets switched
   off.
