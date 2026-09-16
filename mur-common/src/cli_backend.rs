//! CLI-spawn backend registry.
//!
//! One row per coding CLI MUR can drive. The registry is data, not code
//! paths: Gemini CLI was replaced by Antigravity inside a year, and the
//! design records that hardcoding per-CLI flags guarantees rewriting this on
//! the next replacement.
//!
//! A row exists only when every field is known. `agy`'s home env var and the
//! `codex` / `agy` streaming envelopes are still open questions in
//! `docs/superpowers/specs/2026-09-16-cli-spawn-backends-design.md`, so those
//! rows are absent rather than half-filled — an unknown field here would be
//! read as fact by every consumer.
//!
//! Nothing in this module spawns a process or reads the user's CLI config.

/// How a backend's MCP configuration reaches the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMount {
    /// Flags on every invocation; nothing is written to disk.
    PerCall,
    /// Written once into the backend's private home.
    Persistent,
}

/// Whether MUR may actually drive this backend.
///
/// Separate from binary presence on purpose. The spec's activation gate reads:
/// "Failure or unknown results keep the backend disabled; binary presence is
/// insufficient." A disabled backend is still listed and still rendered — it
/// is the panel that carries the explanation and the controls, so hiding it
/// would strand the user exactly as hiding a subscription provider did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    Enabled,
    /// `reason` is shown to the user. It names the unmet requirement.
    Disabled {
        reason: &'static str,
    },
}

/// One CLI-spawn backend, as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliBackend {
    /// Stable identifier used in paths and UI keys.
    pub key: &'static str,
    /// Executable name, resolved against the user's shell PATH by the caller.
    pub binary: &'static str,
    /// Flags that put the CLI in headless mode.
    pub headless_invocation: &'static [&'static str],
    /// Flags that select a machine-readable streaming envelope.
    pub stream_flags: &'static [&'static str],
    /// Flags that disable the CLI's own built-in tools.
    pub tool_disable_flags: &'static [&'static str],
    pub mcp_mount: McpMount,
    /// Environment variable that relocates this CLI's home.
    pub home_env_var: &'static str,
    pub activation: Activation,
    /// Free text for the panel: what is known, and what is not.
    pub capability_notes: &'static str,
}

/// `claude`, measured at 2.1.273 by running it, not only by reading `--help`.
///
/// `tool_disable_flags` is `--tools ""`, not `--disallowedTools`: the latter
/// is a named deny list, and denying `Bash` merely sent the model to `Glob`.
///
/// Both halves of the disable are required. `--tools ""` alone left 44 tools
/// mounted — every MCP server in the user's own config — so the flags below
/// are the pair, and `--strict-mcp-config` is load-bearing. Verified from the
/// `system init` event's `tools` array, which reported `[]`.
///
/// Still disabled, for a different reason than the earlier draft: the probe
/// is answered, but nothing can spawn this yet. MUR does not serve its tools
/// over MCP, so there is no loop for the CLI to call back into.
pub const CLAUDE: CliBackend = CliBackend {
    key: "claude",
    binary: "claude",
    headless_invocation: &["-p"],
    stream_flags: &["--output-format", "stream-json"],
    tool_disable_flags: &["--tools", "", "--strict-mcp-config"],
    mcp_mount: McpMount::PerCall,
    home_env_var: "CLAUDE_CONFIG_DIR",
    activation: Activation::Disabled {
        reason: "spawn path not implemented: MUR does not yet serve its tools over MCP",
    },
    capability_notes: "--tools \"\" disables built-ins but NOT the user's own MCP \
                       servers; --strict-mcp-config is what empties the tool list",
};

/// Every backend whose record is complete. Absence is a statement: a CLI
/// missing here has an unanswered probe, not a missing implementation.
pub const REGISTRY: &[CliBackend] = &[CLAUDE];

/// Look up a backend by key.
pub fn backend(key: &str) -> Option<&'static CliBackend> {
    REGISTRY.iter().find(|b| b.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_row_matches_the_measured_capabilities() {
        assert_eq!(CLAUDE.binary, "claude");
        assert_eq!(CLAUDE.headless_invocation, &["-p"]);
        assert_eq!(CLAUDE.stream_flags, &["--output-format", "stream-json"]);
        assert_eq!(
            CLAUDE.tool_disable_flags,
            &["--tools", "", "--strict-mcp-config"]
        );
        assert_eq!(CLAUDE.mcp_mount, McpMount::PerCall);
        assert_eq!(CLAUDE.home_env_var, "CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn claude_is_disabled_because_nothing_can_spawn_it_yet() {
        // The activation gate, as a test. The blocker is no longer the tool
        // probe — that is answered — but the missing spawn path.
        match CLAUDE.activation {
            Activation::Disabled { reason } => assert!(reason.contains("spawn path")),
            Activation::Enabled => panic!("nothing can spawn a backend yet"),
        }
    }

    #[test]
    fn the_tool_disable_carries_both_halves() {
        // Regression guard for the measured hazard: `--tools ""` on its own
        // left 44 of the user's own MCP tools mounted. Dropping
        // --strict-mcp-config here would silently reopen that hole.
        assert!(CLAUDE.tool_disable_flags.contains(&"--tools"));
        assert!(CLAUDE.tool_disable_flags.contains(&"--strict-mcp-config"));
        assert!(
            !CLAUDE.tool_disable_flags.contains(&"--disallowedTools"),
            "--disallowedTools is a named deny list, not a disable"
        );
    }

    #[test]
    fn every_row_is_fully_specified() {
        // The rule the registry exists to enforce: no half-filled row. A
        // backend with an unknown field belongs outside the registry, not
        // inside it with a plausible-looking guess.
        for b in REGISTRY {
            assert!(!b.key.is_empty(), "{}: empty key", b.key);
            assert!(!b.binary.is_empty(), "{}: empty binary", b.key);
            assert!(
                !b.headless_invocation.is_empty(),
                "{}: no headless flags",
                b.key
            );
            assert!(!b.stream_flags.is_empty(), "{}: no stream flags", b.key);
            assert!(!b.home_env_var.is_empty(), "{}: no home env var", b.key);
        }
    }

    #[test]
    fn unprobed_backends_are_absent_rather_than_guessed() {
        assert!(
            backend("agy").is_none(),
            "agy's home env var is open question 1"
        );
        assert!(
            backend("codex").is_none(),
            "codex's stream envelope is open question 2"
        );
    }

    #[test]
    fn lookup_finds_claude_and_rejects_unknown_keys() {
        assert_eq!(backend("claude"), Some(&CLAUDE));
        assert!(backend("nope").is_none());
    }
}
