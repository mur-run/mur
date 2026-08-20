# Choosing a secret backend per context

**Date:** 2026-08-20
**Status:** discussion — no decision, nothing implemented
**Issue:** #866 direction 3
**Follows:** #1019 (the `mur doctor` probe that makes the failure visible)

## The failure this is about

Keychain Always-Allow binds to the code-signing **identity**. An ad-hoc
signature has no certificate, so the identity degenerates to the binary's
CDHash and every upgrade reads as a different app.

An interactive run can re-prompt. A launchd- or Hub-spawned agent **cannot** —
and the resolution path turns that into silence:

```rust
pub fn resolve_to_string_blocking(&self) -> Option<String> {
    self.resolve_blocking().ok().map(...)      // Err -> None
}
```

`None` is indistinguishable from "no secret configured", so the caller falls
through and the agent runs without a key. Nothing is logged as a failure.

#1019 now warns before the upgrade. This document is about whether the *choice
of backend* should change, not about the warning.

## The constraint that shapes every option

**`secret` is a property of the MODEL, not of the agent.**

`~/.mur/models.yaml` holds one `SecretRef` per model entry, and every agent that
references that model resolves the same ref. So "Keychain when interactive,
file when unattended" cannot be expressed today for a single model — there is
one slot, shared.

Any design has to answer: **what does the backend choice key on?**

| keys on | consequence |
|---|---|
| the model | today's schema; one backend for all consumers of that model |
| the agent | needs a per-agent override layer that does not exist |
| the launch context | the runtime would have to pick at resolve time, from two refs stored side by side |

That is the decision. The rest follows from it.

## Options

### (a) Leave the schema alone; change only the default `mur model connect` writes

`model_connect.rs:207` writes `SecretRef::Keychain` unconditionally. Change it
to `file:` and the whole class disappears — no signing identity is involved.

Cost: at-rest protection drops from Keychain to 0600 file permissions, for
**every** user including those who never run an unattended agent. The
laptop-with-FileVault case and the always-on-server case get the same answer,
and it is the weaker one.

Cheapest to build. Weakest security story.

### (b) Per-agent override

Add an optional `secret` to the agent profile that shadows the model's. Service
agents get `file:`; interactive ones inherit Keychain.

Honest about the difference, and each agent's setup is legible in one place.

Cost: a second place secrets can be configured, so "where does this key come
from" gains a lookup order. Given how much of this week was spent on rules that
existed in two places and drifted (`protects_credential` vs
`credential_paths()`, #1018), that is a real cost, not a nominal one.

### (c) Two refs, chosen at resolve time

Store both and let the runtime pick based on whether it can prompt.

Most convenient in principle. Two problems:

1. **The runtime cannot reliably know.** There is no "am I interactive" signal
   that survives launchd, Hub spawning, `mur agent cli`, and a terminal.
   Guessing it makes the backend nondeterministic, and a wrong guess is the
   silent failure this whole issue is about.
2. Every secret now has two copies to keep in sync, doubling the surface for
   the "guard file and written file are not the same file" class (#1011).

Recorded to be rejected explicitly, since it is the intuitive answer.

### (d) Do nothing beyond #1019

The probe now warns before the upgrade, and the workaround (one foreground run,
click Allow) is documented. If direction 2 lands, the ad-hoc case disappears for
release users and this whole question becomes narrow.

Worth stating: **direction 2's feasibility check (#866 comment) found both of
its stated blockers absent** — there are no bottles, and CI already signs with a
Developer ID. If a CI-signed tarball survives `brew install`, most users never
hit this, and (a)–(c) are solving a problem that mostly is not there.

## What would make this decidable

1. **Finish direction 2's last experiment.** If brew preserves the CI
   signature, the affected population is small and (d) may be the answer.
2. **Decide what the backend keys on** (the table above). Everything else is
   implementation.
3. **State the at-rest threat model.** "0600 file" versus "Keychain" only
   differ against an attacker who can read the user's files but not prompt as
   them. Whether that attacker is in scope has never been written down, and
   without it (a) cannot be evaluated — it is a security trade with no stated
   baseline.

## Who should weigh in

Point 3 is not an engineering call. It needs whoever owns the security posture
to say what at-rest protection is for, because the answer changes which of
(a)–(d) is even acceptable.
