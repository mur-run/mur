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
/// Still disabled, and the reason has moved twice as the work landed. The
/// probe is answered; MUR does now serve its tools over MCP (`tools/list`,
/// `tools/call`, and the shim that forwards to them). What is missing is the
/// other half: nothing writes the per-turn `--mcp-config` and nothing runs
/// the CLI, so this row describes a backend that could work rather than one
/// that does.
///
/// Keep this sentence true. A row whose stated reason outlives the thing it
/// described is worse than a bare `false` — it explains itself confidently
/// and wrongly, and a user reading the panel has no way to tell.
pub const CLAUDE: CliBackend = CliBackend {
    key: "claude",
    binary: "claude",
    headless_invocation: &["-p"],
    stream_flags: &["--output-format", "stream-json"],
    tool_disable_flags: &["--tools", "", "--strict-mcp-config"],
    mcp_mount: McpMount::PerCall,
    home_env_var: "CLAUDE_CONFIG_DIR",
    activation: Activation::Disabled {
        reason: "no spawn path: nothing writes the per-turn --mcp-config or runs the CLI",
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

/// A backend whose binary was found, plus whether MUR may drive it.
///
/// `usable == false` is a backend that is present and listed but must not be
/// spawned; the caller renders it with `Activation::Disabled`'s reason. It is
/// deliberately not filtered out — the disabled entry is what carries the
/// explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAvailability {
    pub backend: &'static CliBackend,
    pub path: std::path::PathBuf,
    pub usable: bool,
}

/// Which backends the user actually has, given a binary resolver.
///
/// `resolve` is injected rather than calling a `which` helper directly: this
/// crate is consumed by the Hub, the runtime and the CLI, each of which
/// resolves binaries differently (the Hub must ask an interactive login shell,
/// because a Finder-launched app inherits a bare PATH). Injection also makes
/// every case below testable without touching the real PATH.
pub fn available<F>(resolve: F) -> Vec<BackendAvailability>
where
    F: Fn(&str) -> Option<std::path::PathBuf>,
{
    REGISTRY
        .iter()
        .filter_map(|b| {
            resolve(b.binary).map(|path| BackendAvailability {
                backend: b,
                path,
                usable: matches!(b.activation, Activation::Enabled),
            })
        })
        .collect()
}

/// `<mur_home>/cli-homes/<key>/` — this backend's private CLI home.
///
/// `mur_home` is a parameter rather than a call to `trust::mur_home()` so the
/// path is a pure function of its inputs and every test runs against a temp
/// dir. Same shape as `local_llm::local_model_dir`.
pub fn home_dir(mur_home: &std::path::Path, key: &str) -> std::path::PathBuf {
    mur_home.join("cli-homes").join(key)
}

/// Create this backend's private home if absent and return the environment
/// variable that points the CLI at it.
///
/// The home starts empty and stays MUR's: the user authenticates once inside
/// it, and their own `~/.claude` / `~/.codex` is never read or written. We do
/// not copy `auth.json` — two holders of one refresh-token lineage each
/// rotating would log the user out of their own CLI, which is why the gateway
/// is the sole token holder on the other track.
pub fn ensure_home(
    mur_home: &std::path::Path,
    b: &CliBackend,
) -> std::io::Result<(&'static str, std::path::PathBuf)> {
    let dir = home_dir(mur_home, b.key);
    std::fs::create_dir_all(&dir)?;
    Ok((b.home_env_var, dir))
}

/// The flags that make a spawned CLI see MUR's tools and nothing else.
///
/// One constant, not three arguments assembled at the call site. Measured
/// 2026-09-16: `--tools ""` alone still left 44 tools mounted — every MCP
/// server in the user's own config — and none of those pass MUR's handler,
/// entitlements or HITL gate. `--strict-mcp-config` is what empties the
/// list, so the three travel together or the isolation is not there.
pub const ISOLATION_FLAGS: &[&str] = &["--tools", "", "--strict-mcp-config"];

/// The `--mcp-config` document for one turn.
///
/// Names the shim, the agent socket it dials back on, and the task it
/// belongs to. `task_id` is what binds a spawned `bash` job to an owner and
/// routes an approval prompt, so it is an argument rather than something the
/// shim could infer.
pub fn mcp_config_json(
    shim_bin: &str,
    socket: &std::path::Path,
    task_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "mur": {
                "command": shim_bin,
                "args": [
                    "mcp-shim",
                    "--socket", socket.to_string_lossy(),
                    "--task-id", task_id,
                ],
            }
        }
    })
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
        // The activation gate, as a test. The blocker has moved once already:
        // first the tool probe, then serving tools over MCP, now the spawn
        // itself. Matching on "spawn path" would have kept passing through
        // that middle change while the sentence went stale, so this asserts
        // the part that is actually specific to what is missing.
        match CLAUDE.activation {
            Activation::Disabled { reason } => assert!(
                reason.contains("--mcp-config") && reason.contains("runs the CLI"),
                "the reason no longer names what is missing: {reason}"
            ),
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

    use std::path::PathBuf;

    fn found(_: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/homebrew/bin/claude"))
    }

    fn missing(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn an_absent_binary_produces_no_entry() {
        // "Backends whose binary is absent do not appear in the UI at all."
        assert!(available(missing).is_empty());
    }

    #[test]
    fn a_present_binary_is_listed_with_the_path_that_was_resolved() {
        let got = available(found);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].backend.key, "claude");
        assert_eq!(got[0].path, PathBuf::from("/opt/homebrew/bin/claude"));
    }

    #[test]
    fn a_present_but_unverified_backend_is_listed_and_not_usable() {
        // The distinction this task exists for: absent is gone, unverified is
        // shown-and-disabled. Folding the second into the first would remove
        // the only surface that explains the unmet requirement.
        let got = available(found);
        assert!(!got[0].usable);
    }

    #[test]
    fn usable_tracks_activation_and_nothing_else() {
        // Guards against a future row being enabled by the mere fact that its
        // binary resolved.
        for a in available(found) {
            assert_eq!(
                a.usable,
                matches!(a.backend.activation, Activation::Enabled)
            );
        }
    }

    #[test]
    fn home_dir_is_namespaced_under_cli_homes() {
        let got = home_dir(std::path::Path::new("/tmp/murhome"), "claude");
        assert_eq!(got, PathBuf::from("/tmp/murhome/cli-homes/claude"));
    }

    #[test]
    fn ensure_home_creates_the_dir_and_returns_the_env_var() {
        let tmp = std::env::temp_dir().join(format!("mur-cli-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (var, dir) = ensure_home(&tmp, &CLAUDE).expect("create");
        assert_eq!(var, "CLAUDE_CONFIG_DIR");
        assert_eq!(dir, tmp.join("cli-homes").join("claude"));
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ensure_home_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("mur-cli-home-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        ensure_home(&tmp, &CLAUDE).expect("first");
        let marker = home_dir(&tmp, "claude").join("settings.json");
        std::fs::write(&marker, b"{}").expect("write marker");
        ensure_home(&tmp, &CLAUDE).expect("second");
        assert_eq!(std::fs::read(&marker).expect("read marker"), b"{}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ensure_home_never_touches_the_users_own_cli_config() {
        // The Global Constraint, asserted rather than assumed. A stand-in for
        // ~/.claude sits OUTSIDE the mur home; creating the private home must
        // leave its bytes untouched.
        let base = std::env::temp_dir().join(format!("mur-cli-iso-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let user_cfg = base.join("user-claude");
        std::fs::create_dir_all(&user_cfg).expect("user cfg");
        let cred = user_cfg.join(".credentials.json");
        std::fs::write(&cred, b"user-token").expect("seed");

        ensure_home(&base.join("murhome"), &CLAUDE).expect("create");

        assert_eq!(std::fs::read(&cred).expect("still there"), b"user-token");
        assert!(
            !base
                .join("murhome")
                .join("cli-homes")
                .join("claude")
                .join(".credentials.json")
                .exists()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_isolation_flags_stay_together() {
        // Each of the three is load-bearing and the middle row of the table
        // in the plan is why: dropping --strict-mcp-config re-mounts the
        // user's own MCP servers, and the spawn still looks correct.
        assert_eq!(ISOLATION_FLAGS, &["--tools", "", "--strict-mcp-config"]);
    }

    #[test]
    fn the_mcp_config_names_the_shim_the_socket_and_the_task() {
        let v = mcp_config_json(
            "/usr/local/bin/mur_agent_x",
            std::path::Path::new("/tmp/x/agent.sock"),
            "t-9",
        );
        let s = &v["mcpServers"]["mur"];
        assert_eq!(s["command"], "/usr/local/bin/mur_agent_x");
        let args: Vec<String> = s["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|a| a.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(args[0], "mcp-shim");
        assert!(args.contains(&"/tmp/x/agent.sock".to_string()));
        assert!(args.contains(&"t-9".to_string()));
    }

    #[test]
    fn the_config_declares_exactly_one_server() {
        // `--strict-mcp-config` means this document is the whole tool
        // surface. A second entry here would be a second unaudited source.
        let v = mcp_config_json("bin", std::path::Path::new("/s"), "t");
        assert_eq!(v["mcpServers"].as_object().expect("obj").len(), 1);
    }
}
