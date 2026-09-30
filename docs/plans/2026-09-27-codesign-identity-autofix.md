# Design draft: detect and auto-fix a missing codesign identity

Date: 2026-09-27
Status: draft (needs review)
Related: #1587, #866 (keychain grants break after an ad-hoc re-sign)

## Problem

When `mur update` re-signs the installed binaries and `update.codesign_identity` is
not set, it falls back to ad-hoc signing. An ad-hoc signature has a different
signing identity every time, so the macOS keychain no longer recognises the new
binary and agents using `keychain:` secrets **fail silently** until the user
re-authorises them in the foreground.

Today `mur update` only prints a warning after the fact, which the user has
usually already scrolled past. And the `apple-signing` check in `mur doctor` only
inspects the `MUR_APPLE_DEVELOPER_ID` / `MUR_APPLE_TEAM_ID` environment variables
used by the release/export flow — it **never looks at `update.codesign_identity`**,
so nothing warns the user ahead of time.

## Goals

1. Detect the gap before the user hits it: `mur init`, `mur doctor`, MUR Hub home.
2. Offer a one-click fix: create a local self-signed code signing certificate and
   write it into `update.codesign_identity`.
3. Do not pretend it is fully automatic — importing a *trusted* certificate raises
   a system admin authorisation dialog, and that one interaction cannot be skipped.

## New check: `codesign-identity`

Lives in `mur-core/src/cmd/doctor.rs`, alongside the existing `apple-signing`
check but with different meaning (that one is about **release signing**, this one
about **local re-signing on update**). The docs must spell out the difference so
the two are not confused.

Decision order:

| Condition | Result |
|---|---|
| Not macOS | skip |
| `update.codesign_identity` set and found by `security find-identity -v -p codesigning` | ok |
| Set, but the certificate is missing or expired | fail (pointing at a non-existent identity is worse than not setting it) |
| Unset, a Developer ID Application certificate exists | warn + suggest that identity |
| Unset, only Apple Development certificates exist | warn + suggest one (enough for local re-signing) |
| Unset and no code signing certificate at all | warn + suggest `--fix` to create a self-signed one |

## `mur doctor --fix`

doctor currently has **no `--fix` framework at all**; that is the main engineering
cost of this proposal. Minimal design:

- Add `fix: Option<FixAction>` to the `Check` struct, where `FixAction` exposes
  `describe()` and `apply()`.
- `mur doctor --fix` runs only the checks that are non-ok and carry a fix, printing
  what it is about to do and asking for confirmation per item.
- Every fix must be idempotent, and must back up any config file it edits
  (`config.yaml.bak`) before writing.

### The fix for this case

`certtool` has no option for the code signing EKU, so the flow is:

1. `openssl req -x509 -newkey rsa:2048 -nodes` to generate the key and certificate
   with `extendedKeyUsage = codeSigning`, CN `MUR Local Signing`, 10-year validity.
2. Bundle as PKCS#12 and `security import` into the login keychain, authorising
   codesign with `-T /usr/bin/codesign`.
3. `security add-trusted-cert -d -r trustRoot -p codeSign` — **this step raises the
   admin authorisation dialog.**
4. Read back the actual SHA-1 / CN and write it to `update.codesign_identity`.
5. Verify: `codesign -s <identity>` a temporary file, then `codesign -v` it. Only a
   successful verification counts as fixed.

Failure must roll back: if the certificate import fails, do not touch `config.yaml`.

## Surfaces

- **`mur doctor`**: as above.
- **`mur init`**: run the same check; on warn, point at `mur doctor --fix` rather
  than raising an authorisation dialog inside `init`.
- **MUR Hub home**: read the doctor check result, show a card with a "Fix" button
  that calls the same daemon path (do not reimplement the shell flow in the GUI).
  The authorisation dialog comes from the system, so the GUI must handle the user
  cancelling it.

## Open questions

- Is adding a self-signed certificate to `trustRoot` acceptable? The alternative is
  to import it untrusted — codesign still works, but the Gatekeeper semantics are
  weaker. We need to confirm that we only care about a *stable* signing identity,
  not a *trusted* one. If so, step 3 can be dropped entirely and the one-click fix
  becomes genuinely non-interactive. **This is the thing most worth settling first.**
- Should `mur update` also point at `mur doctor --fix` when it detects it just
  signed ad-hoc?
- Behaviour on Linux / Windows (currently skip).
