//! Companion network-egress audit (B0 rule 12 / M8.3).
//!
//! Roadmap §6.1 rule 12 requires that the companion subsystem has no
//! direct network egress — the only outbound code path is the
//! agent's already-opted-in model provider via `crate::llm::LlmClient`.
//!
//! This module embeds every companion source file at compile time
//! via `include_str!` and asserts none of them imports a known HTTP
//! client crate or a raw socket type. The test fails the build the
//! moment a future change introduces a forbidden dependency, so the
//! invariant is enforced even if the rule-12 reviewer forgets to
//! re-audit.
//!
//! The allowed exception is `crate::llm::LlmClient` (and its sister
//! types `LlmError` / `LlmMessage` / `LlmRequest`); the import is
//! whitelisted by name in the test below.

#[cfg(test)]
const COMPANION_FILES: &[(&str, &str)] = &[
    ("clock.rs", include_str!("clock.rs")),
    ("earned_permission.rs", include_str!("earned_permission.rs")),
    ("i18n.rs", include_str!("i18n.rs")),
    ("inbox.rs", include_str!("inbox.rs")),
    ("linter.rs", include_str!("linter.rs")),
    ("mod.rs", include_str!("mod.rs")),
    ("notifier.rs", include_str!("notifier.rs")),
    ("outbox/mod.rs", include_str!("outbox/mod.rs")),
    ("outbox/deliver.rs", include_str!("outbox/deliver.rs")),
    ("outbox/generate.rs", include_str!("outbox/generate.rs")),
    ("outbox/i18n.rs", include_str!("outbox/i18n.rs")),
    ("picker.rs", include_str!("picker.rs")),
    ("schedule.rs", include_str!("schedule.rs")),
    ("situations.rs", include_str!("situations.rs")),
    ("telemetry.rs", include_str!("telemetry.rs")),
    ("voice.rs", include_str!("voice.rs")),
];

/// Substrings that indicate a direct or transitive network egress
/// path in companion source. Match is substring + case-sensitive
/// against the file body (not its tokenised AST), so it intentionally
/// catches commented-out forms too — the audit treats commented
/// imports as a warning we should not have.
#[cfg(test)]
const FORBIDDEN_TOKENS: &[&str] = &[
    "use reqwest",
    "use hyper",
    "use surf",
    "use ureq",
    "use isahc",
    "use tokio::net::",
    "use tokio::io::AsyncReadExt", // rules out raw socket reads
    "::TcpStream",
    "::TcpListener",
    "::UnixStream",
    "::UnixListener",
    "use std::net::",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_companion_file_imports_a_network_client() {
        let mut violations: Vec<String> = Vec::new();
        for (name, body) in COMPANION_FILES {
            for needle in FORBIDDEN_TOKENS {
                if body.contains(needle) {
                    violations.push(format!("{name} contains forbidden token: {needle}"));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "Companion network-egress audit failed (B0 rule 12 / M8.3):\n  {}\n\
             The only allowed outbound is `crate::llm::LlmClient` (the agent's\n\
             already-opted-in model provider). If a new outbound is genuinely\n\
             needed, update roadmap §6.1 rule 12 + this audit + the privacy\n\
             statement before adding the import.",
            violations.join("\n  "),
        );
    }

    #[test]
    fn audit_runs_against_every_companion_file() {
        // Sanity: if a new file lands in src/companion/ without being
        // added to COMPANION_FILES the audit silently skips it.
        // We can't list directory contents at compile-time, but we
        // assert at runtime that the count matches what `mod.rs`
        // declared. If you add `pub mod foo;` to mod.rs you must add
        // ("foo.rs", include_str!("foo.rs")) here too.
        let mod_rs = include_str!("mod.rs");
        let declared = mod_rs
            .lines()
            .filter(|l| l.trim_start().starts_with("pub mod "))
            .count();
        // outbox is split into a folder; count its production submodules
        // (excluding `mod tests;`) so each one shows up in COMPANION_FILES.
        let outbox_mod_rs = include_str!("outbox/mod.rs");
        let outbox_subs = outbox_mod_rs
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("mod ") && t != "mod tests;"
            })
            .count();
        // mod.rs itself is not a `pub mod` line; +1 to include it.
        let expected = declared + 1 + outbox_subs;
        assert_eq!(
            COMPANION_FILES.len(),
            expected,
            "COMPANION_FILES count ({}) ≠ pub mod count in mod.rs ({}) + mod.rs itself + outbox submodules ({}). \
             Did you add a new companion file without updating network_audit::COMPANION_FILES?",
            COMPANION_FILES.len(),
            declared,
            outbox_subs,
        );
    }

    #[test]
    fn llm_client_remains_the_only_outbound_indirection() {
        // Companion files MAY import `crate::llm::LlmClient` (the
        // model-provider abstraction). This test asserts the only
        // outbound symbols imported under that path are recognised —
        // defends against someone adding e.g. `crate::llm::HttpFetcher`
        // and claiming the audit still passes.
        //
        // Strategy: extract every identifier referenced after
        // `crate::llm::` (handling both `crate::llm::LlmClient` and
        // `use crate::llm::{LlmClient, LlmError}` forms), then assert
        // each is in the allow-list. Adding a new sister type requires
        // updating both this list and roadmap §6.1 rule 12.
        // Allowed identifiers referenced under `crate::llm::`. Splits
        // into:
        //   - public types of the LlmClient abstraction (the runtime
        //     interface companion code uses);
        //   - sub-modules under `crate::llm` whose contents are real
        //     LLM provider clients (network egress IS allowed for the
        //     agent's configured model provider) or test stubs.
        // Adding a new symbol here requires roadmap §6.1 rule 12 to
        // be re-checked first.
        let allowed: &[&str] = &[
            // LlmClient surface
            "BackgroundKind",
            "LlmClient",
            "LlmError",
            "LlmMessage",
            "LlmRequest",
            "LlmResponse",
            "RequestIntent",
            "RichMessage",
            "StopReason",
            "ToolCallResult",
            "ToolDef",
            "ToolResultEntry",
            // Provider sub-modules + their test stubs
            "anthropic",
            "ollama",
            "openai",
            "stub",
            "StubLlm",
        ];

        let mut found_any_outbound = false;
        for (file, body) in COMPANION_FILES {
            for (line_no, line) in body.lines().enumerate() {
                let line_no = line_no + 1; // human 1-based
                let Some(rest) = line.split_once("crate::llm::") else {
                    continue;
                };
                found_any_outbound = true;
                let rest = rest.1;
                // Possible forms after `crate::llm::`:
                //   - `LlmClient`           (single type)
                //   - `{LlmClient, LlmError}` (multi-import)
                let names: Vec<String> = if rest.starts_with('{') {
                    let close = rest.find('}').unwrap_or(rest.len());
                    rest[1..close]
                        .split(',')
                        .map(|s| s.trim().trim_end_matches(';').to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    let end = rest
                        .find(|c: char| !c.is_alphanumeric() && c != '_')
                        .unwrap_or(rest.len());
                    vec![rest[..end].to_string()]
                };
                for name in names {
                    assert!(
                        allowed.contains(&name.as_str()),
                        "{file}:{line_no} imports an unrecognised crate::llm symbol: {name}\n  \
                         {line}\n\
                         If this is genuinely an LLM-only addition, append it to \
                         `allowed` in companion::network_audit AND update roadmap \
                         §6.1 rule 12. If it's a new outbound capability, the rule \
                         and the privacy statement must change first.",
                    );
                }
            }
        }
        assert!(
            found_any_outbound,
            "Audit precondition: at least one companion file should reference \
             crate::llm::*. If none do, the audit's allow-list is stale — \
             re-verify the rule-12 claim from scratch.",
        );
    }
}
