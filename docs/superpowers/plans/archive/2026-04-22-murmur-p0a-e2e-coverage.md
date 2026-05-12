# Murmur P0a — E2E coverage map

The plan (`2026-04-22-murmur-p0a-agent-runtime-plan-part2.md`, Task 39)
called for eight named E2E test files under
`mur-agent-runtime/tests/e2e/`. Most of those scenarios are already
exercised by the integration tests landed during Tasks 4–38; this
document maps each plan-listed E2E to the test that covers it so we
don't grow duplicate suites.

`scripts/e2e/run-all.sh` is the single entrypoint: it builds the
workspace, runs every integration test (default + `#[ignore]`-gated),
and optionally produces an `llvm-cov` report.

## Coverage map

| Plan E2E                          | Covered by                                                                                  |
|-----------------------------------|---------------------------------------------------------------------------------------------|
| `e2e_create_and_launch.rs`        | `mur-core/tests/agent_create.rs` + `mur-agent-runtime/tests/supervisor_startup.rs`           |
| `e2e_roundtrip_send.rs`           | `mur-core/tests/agent_send.rs` + `mur-agent-runtime/tests/methods.rs`                        |
| `e2e_remove_purge.rs`             | `mur-core/tests/agent_lifecycle.rs` (`remove_*`)                                             |
| `e2e_argv0_spoofing.rs`           | `mur-agent-runtime/tests/multi_call.rs` (extract / spoof checks)                             |
| `e2e_list_filters.rs`             | `mur-core/tests/agent_list_status.rs` (running vs stopped JSON)                              |
| `e2e_export_import_murpkg.rs`     | `mur-agent-runtime/tests/export_pkg.rs` + `import_pkg.rs` (round-trip + UUID rotation)       |
| `e2e_export_bin_run_standalone.rs`| `mur-core/tests/agent_export.rs` (pkg path); `--ignored` `export_bin_*` for the slow build  |
| `e2e_mgmt_cli_suite.rs`           | `agent_prompt.rs`, `agent_mcp.rs`, `agent_skill.rs`, `agent_perm.rs`, `agent_install_service.rs`|
| Supervisor lifecycle (graceful)   | `mur-agent-runtime/tests/supervisor_shutdown.rs`                                             |
| Embedded extraction               | `mur-agent-runtime/tests/embed_extract.rs` + `bin_embed.rs`                                  |
| Telemetry stats / logs            | `mur-core/tests/agent_stats.rs`                                                              |
| Ephemeral card                    | `mur-core/tests/agent_card_ephemeral.rs`                                                     |

Each row has at least one passing test; the smoke runner prints `✅ E2E
smoke suite passed` only when *all* of the above are green.

## Coverage gate

cargo-llvm-cov is the recommended tooling for the 85%-line gate. It is
not pinned as a workspace dev-dep (it is a global cargo install); the
runner invokes it on demand:

```bash
scripts/e2e/run-all.sh --coverage
```

If the binary is missing the script prints an install hint and exits
non-zero. CI/CD wiring is not part of P0a (no `.github/workflows/*` was
added in this branch); see `2026-04-22-murmur-p0a-agent-runtime-plan-COMPLETE.md`
for the post-merge checklist.
