//! `mur-browser` — the piece of MUR that sits between an agent and
//! `@playwright/mcp`.
//!
//! Slice 1 (this crate's first cut) ships the **MCP stdio proxy** with a
//! pure-forward hook, plus the frozen contracts the parallel slices build on:
//!
//! | module      | slice | what is frozen here                                  |
//! |-------------|-------|------------------------------------------------------|
//! | `proxy`     | 1     | JSON-RPC line relay + `Hook` trait (intercept points) |
//! | `recorder`  | 2     | `Step` schema written to `runs/<name>/actions.yaml`   |
//! | `locator`   | 3     | `role:` / `testid:` / `text:` / `label:` / `css:` grammar |
//! | `broker`    | 5     | `transform_input` / `redact_output` wire types        |
//! | `paths`     | 1     | `~/.mur/browser/...` layout                            |
//!
//! Nothing in here touches Playwright directly — every browser action goes
//! over MCP, so the crate has no Node or Chromium dependency at build time.
//! Design source: `docs/superpowers/specs/2026-09-10-browser-phase1-spec.md`.

pub mod auth;
pub mod broker;
pub mod export;
pub mod guard;
pub mod locator;
pub mod paths;
pub mod proxy;
pub mod recorder;
pub mod replay;
pub mod state;

/// npm package spawned as the downstream MCP server.
pub const PLAYWRIGHT_MCP_PKG: &str = "@playwright/mcp@latest";

/// Name the agent sees for this server (`mur agent mcp add <agent> browser …`).
pub const SERVER_NAME: &str = "browser";
