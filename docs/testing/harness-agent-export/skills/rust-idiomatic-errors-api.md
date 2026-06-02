---
name: rust-idiomatic-errors-api
description: Idiomatic Rust best practices for error handling, API/error-type design, and testing. Use when implementing or reviewing Rust code.
---

# Rust Engineer: Idiomatic Errors, API & Testing

Distilled from 2026 Rust guidance (The Rust Book, mmapped.blog, howtocodeit.com, oneuptime).

## Error handling
- `Result<T,E>` for recoverable failure the caller must handle; `Option<T>` when absence is **not** an error.
- Propagate with `?`; use `if let` / `while let` for readable `Option` handling; combinators `map`/`and_then`/`or_else`.
- **No `unwrap`/`expect` in production paths** — fine only in tests/prototypes. Reserve `panic!` for unrecoverable bugs and tests.

## Error type design (API)
- Model failures in the type system with **domain-specific enums** so callers can `match` variants.
- Express variants in **problem-domain terms**, not implementation/dependency details — keeps the public error API stable.
- **`thiserror` for libraries, `anyhow` for applications**. Prefer concrete enums in libs; boxed/`anyhow` at app top-level.
- Be granular: a distinct error type per function is rarely "too much".

## Testing & tooling
- Every bug-fix ships with a **regression test that reproduces the original failure**.
- `unwrap`/`expect` acceptable in tests as a deliberate fail signal.
- Run `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo audit`; let exhaustiveness lints catch unhandled arms.

## Handoff rule
End with: `HANDOFF -> <role>: <what to verify/deploy>`.
