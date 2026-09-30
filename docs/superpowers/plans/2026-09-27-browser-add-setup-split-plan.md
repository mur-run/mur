# Plan: Split `/browser --add` from permission grants, move grants into `mur browser setup`

## Problem

`/browser --add` installs the skill only (`browser_cmd.rs:29` → `manage::skill_add`);
it never touches permissions. The skill cannot actually run without three grants:

- `mur agent perm allow-spawn <agent> playwright-mcp`
- `mur agent perm allow-spawn <agent> chrome-headless-shell`
- `mur agent perm allow-spawn-dir <agent> ~/.mur/artifacts/<agent>/shim/probe`

The third one is the `~/.mur` not-executable root cause: writes succeed there,
exec is denied (verified by copying `/bin/echo` into the directory). Only
`target/` and `.cache/uv` are currently in the exec lane.

Silently granting these at `--add` time is privilege escalation without review,
which this repo does not do.

Secondary blocker: `mur browser setup` bails in non-TTY contexts
(`dispatch.rs:664` passes `stdin.is_terminal()`), so it cannot be run from
murmur today. The blocker is the TTY check, **not** a password: there is no
`sudo`/`ASKPASS` anywhere in the repo. The macOS password dialog observed
earlier is the Keychain GUI prompt for the age identity in
`mur-browser/src/state.rs`, unrelated to these grants.

## Design

Two steps, consent stays human.

1. **`/browser --add`** — unchanged behavior (install the skill), plus a closing
   hint line: run `mur browser setup` to finish. No permission writes.
2. **`mur browser setup`** — owns the three grants. Instead of requiring a TTY,
   it raises the consent question over the existing HITL / `elicitation/create`
   transport (`mur-agent-runtime/src/mcp_shim.rs:276`), so the prompt renders
   inside murmur. Precedent: `grant_egress(…, yes: bool)` in
   `deep_research/provision.rs`. On approval it applies the three grants, then
   runs the existing `live_check` to verify.

Net user flow: `--add` → hint → `mur browser setup` → one HITL approval → grants
applied → `live_check` passes. No terminal switch, one confirmation.

## Steps

1. Add the hint line to `--add` (no TTY dependency; lands independently).
2. Add a `yes: bool` / non-interactive consent path to `browser setup`, replacing
   the hard `is_terminal()` bail.
3. Wire that consent path to `elicitation/create` so murmur renders the prompt.
4. Apply the three grants on approval; surface `allow-spawn-dir` for the probe
   directory.
5. Re-run `live_check` as the success gate; tests for the denied and approved
   branches.

## Out of scope

- Keychain / age identity prompting (GUI, separate path).
- Widening the exec lane globally for `~/.mur`; this plan grants one directory.
