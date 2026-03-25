# Changelog

## v2.2.0 (2026-03-25)

### 🚀 New Features

- **Variable System** — `mur var set/get/list/delete` for user-defined variables with `{{var}}` template expansion in workflows
- **Parameterize** — Auto-detect URLs, tokens, API keys, paths in workflows and suggest variable replacements
- **mur exit / mur quit** — Stop recording without export

### 🐛 Bug Fixes

- **Pattern Inject Scoring** — Fix detect_emails to skip git@ SSH URLs, fix detect_database_urls/detect_api_keys for KEY=value format
- **Hook Formatting** — Orphaned header guard, kind-aware rendering (Preference→bullet, Procedure→steps, Technical→numbered)
- **Variable.rs** — Fix unsafe set_var/remove_var in Rust 2024

### 📊 Tests

- 5 new hook formatting tests (494 total, all pass)

## v2.1.6

- Previous stable release
