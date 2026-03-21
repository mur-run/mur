# Changelog

All notable changes to mur-core will be documented in this file.

## [2.1.6] - 2026-03-21

### Schedule System
- Schedule claim/release protocol (Phase 5) — daemon coordination
- System cron/launchd integration for `mur schedule`
- `mur workflow schedule` CLI (Phase 2-3)
- Schedules stored in `schedules.yaml`

### Workflow Execution
- `mur run` now executes workflows (not just prints)
- Extended `mur sync` to include schedules and workflows
- Unified Schedule and Workflow types in `mur-common`

### Security
- Shell injection fix in command execution
- Secret scrubbing for session transcripts before cloud push
- `auth.json` permissions set to 0600 (owner-only)
- `.env` loaded from `~/.mur/.env` on startup

### Code Quality
- Code review fixes for Phase 1-5
- Removed dead code, async cleanup, typed errors
- Step Default trait, model updates, curl → reqwest migration

## [2.1.3] - 2026-03-10

### Workflow & Session
- Step extensions, workflow publish/install, sync push workflows
- Session cloud push + LLM workflow extraction
- Dashboard review URL after Analyze/Export in post-session menu
- Load `.env` from `~/.mur/.env` on startup
