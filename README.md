# MUR — Continuous Learning for AI Assistants

> ⚠️ **v2 Rust rewrite** — in development. For the stable Go version, see [mur-core](https://github.com/mur-run/mur-core).

MUR observes how you work with AI tools, extracts reusable patterns, and injects the right knowledge at the right time.

## Architecture

```
mur-common/     Shared types (Pattern, Config, Event, LLM traits)
mur-core/       CLI + library (capture → store → retrieve → evolve → inject)
mur-commander/  Resident daemon for workflow extraction (future)
```

## Key Differences from v1 (Go)

| | v1 (Go) | v2 (Rust) |
|---|---|---|
| Retrieval | Tag matching, full dump | Semantic + BM25 hybrid, top-5 with scoring |
| Lifecycle | Patterns live forever | Auto promote/deprecate with decay |
| Feedback | None (0 injections tracked) | Full closed loop: inject → track → evolve |
| Quality | No filter, junk accumulates | Noise filter + dedup + verify pipeline |
| Pattern format | Flat YAML | Dual-layer content + evidence + links |
| Storage | YAML only | YAML (source of truth) + LanceDB index |

## Status

🚧 Phase 1 — Foundation

See [plans/007-mur-core-v2-rust-rewrite.md](../mur-core/plans/007-mur-core-v2-rust-rewrite.md) for the full spec.

## License

MIT
