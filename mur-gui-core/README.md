# mur-gui-core

Shared GUI core library consumed by `mur-hub-gui` (the new MuR Hub desktop
app) and during migration also by `mur-agent-gui` (legacy per-agent app).

This crate hosts code that must not fork between the two GUIs:

- `sidecar` — spawn / supervise `mur-agent-runtime` child processes
- `companion_bridge` — debounced filesystem watcher on
  `~/.mur/agents/<name>/companion/inbox/`
- `a2a` — A2A v0.3 unix-socket client

M-h0 is the empty scaffold. Later milestones populate the modules above.

See `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md` §3.1.
