//! Package specs for interpreter-launched MCP servers.
//!
//! `command: npx, args: ["@yawlabs/fetch-mcp"]` runs whatever the registry
//! serves at spawn time. The binary pin cannot help here — it hashes `npx`
//! (see [`crate::exec::is_interpreter_command`]) — so the first thing that can
//! is knowing *which release* the user approved.
//!
//! This module only parses and resolves the spec. Verifying the bytes that
//! actually get executed needs package-manager cache introspection or a
//! MUR-owned install, which is tracked separately.

use anyhow::{Result, bail};

/// A package spec found in an interpreter entry's args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    /// Index into `args` where the spec sits, so a caller can rewrite it.
    pub arg_index: usize,
    /// Package name, including any `@scope/` prefix.
    pub name: String,
    /// Version suffix, if the spec carries one. `None` means the spec floats:
    /// the package manager resolves it fresh on every start.
    pub version: Option<String>,
}

impl PackageSpec {
    /// `true` when no version is recorded — the resolved code can change
    /// between two starts with no user action and no signal.
    pub fn floats(&self) -> bool {
        self.version.is_none()
    }

    /// The spec as it appears (and would be written) in args.
    pub fn to_arg(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{v}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Package runners whose first non-flag argument is a package spec.
fn runner_kind(command: &str) -> Option<Runner> {
    let first = command.split_whitespace().next().unwrap_or(command);
    let stem = std::path::Path::new(first)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(first)
        .to_ascii_lowercase();
    match stem.as_str() {
        "npx" | "bunx" => Some(Runner::Npm),
        "uvx" => Some(Runner::Python),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Npm,
    Python,
}

/// Find the package spec an interpreter entry will run.
///
/// Returns `None` for anything whose shape isn't unambiguous — `node
/// server.js`, `python -m pkg`, or a runner invoked with a flag that takes a
/// separate value (`npx -p a b`). Guessing wrong here would mean rewriting the
/// wrong argument, so an unrecognised shape is left alone.
pub fn parse_spec(command: &str, args: &[String]) -> Option<PackageSpec> {
    runner_kind(command)?;
    let mut idx = 0usize;
    while idx < args.len() {
        let a = &args[idx];
        // A flag that takes a separate value makes the position of the spec
        // ambiguous — bail rather than rewrite the wrong argument.
        if matches!(a.as_str(), "-p" | "--package" | "-c" | "--call") {
            return None;
        }
        if a.starts_with('-') {
            idx += 1; // valueless flag (-y, --yes, --quiet, --package=x)
            continue;
        }
        return Some(split_spec(idx, a));
    }
    None
}

/// Split `name[@version]`, keeping a leading `@scope/` intact.
fn split_spec(arg_index: usize, spec: &str) -> PackageSpec {
    // A leading '@' is a scope, not a version separator; look after it.
    let search_from = usize::from(spec.starts_with('@'));
    match spec[search_from..].rfind('@') {
        Some(rel) => {
            let at = search_from + rel;
            PackageSpec {
                arg_index,
                name: spec[..at].to_string(),
                version: Some(spec[at + 1..].to_string()),
            }
        }
        None => PackageSpec {
            arg_index,
            name: spec.to_string(),
            version: None,
        },
    }
}

/// Ask the package manager which version it would resolve right now.
///
/// This records what the user is approving at pin time; it is not a trust
/// decision about the registry.
pub fn resolve_current_version(runner: Runner, name: &str) -> Result<String> {
    let (program, args) = match runner {
        Runner::Npm => (
            "npm",
            vec!["view".to_string(), name.to_string(), "version".to_string()],
        ),
        Runner::Python => bail!(
            "resolving a current version for uvx packages is not implemented; \
             pass an explicit `{name}==<version>` in args"
        ),
    };
    let out = std::process::Command::new(program)
        .args(&args)
        .output()
        .map_err(|e| anyhow::anyhow!("run `{program} view {name} version`: {e}"))?;
    if !out.status.success() {
        bail!(
            "`{program} view {name} version` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        bail!("`{program} view {name} version` returned nothing");
    }
    Ok(v)
}

/// The runner behind a command, for callers that already know it's one.
pub fn runner_for(command: &str) -> Option<Runner> {
    runner_kind(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finds_a_floating_scoped_package() {
        let s = parse_spec("npx", &args(&["@yawlabs/fetch-mcp"])).unwrap();
        assert_eq!(s.name, "@yawlabs/fetch-mcp");
        assert_eq!(s.version, None);
        assert!(s.floats(), "no version means npx resolves it every start");
        assert_eq!(s.arg_index, 0);
    }

    #[test]
    fn a_scope_prefix_is_not_a_version_separator() {
        let s = parse_spec("npx", &args(&["@scope/pkg@1.2.3"])).unwrap();
        assert_eq!(s.name, "@scope/pkg");
        assert_eq!(s.version.as_deref(), Some("1.2.3"));
        assert!(!s.floats());
        assert_eq!(s.to_arg(), "@scope/pkg@1.2.3");
    }

    #[test]
    fn handles_unscoped_and_valueless_flags() {
        let s = parse_spec("npx", &args(&["-y", "--quiet", "some-mcp@0.4.0"])).unwrap();
        assert_eq!(s.name, "some-mcp");
        assert_eq!(s.version.as_deref(), Some("0.4.0"));
        assert_eq!(
            s.arg_index, 2,
            "index must point at the spec, not the flags"
        );
    }

    /// Rewriting the wrong argument would corrupt the launch command, so an
    /// ambiguous shape must decline rather than guess.
    #[test]
    fn declines_ambiguous_and_non_runner_shapes() {
        assert!(parse_spec("npx", &args(&["-p", "typescript", "tsc"])).is_none());
        assert!(parse_spec("npx", &args(&["--package", "a", "b"])).is_none());
        assert!(parse_spec("node", &args(&["server.js"])).is_none());
        assert!(parse_spec("python3", &args(&["-m", "pkg"])).is_none());
        assert!(parse_spec("mur-mcp-server", &args(&[])).is_none());
        assert!(parse_spec("npx", &args(&["-y"])).is_none(), "flags only");
        assert!(parse_spec("npx", &args(&[])).is_none());
    }

    #[test]
    fn recognises_runners_by_path_and_case() {
        assert_eq!(runner_for("/opt/homebrew/bin/npx"), Some(Runner::Npm));
        assert_eq!(runner_for("BUNX"), Some(Runner::Npm));
        assert_eq!(runner_for("uvx"), Some(Runner::Python));
        assert_eq!(runner_for("node"), None);
    }

    #[test]
    fn to_arg_round_trips_what_was_parsed() {
        for raw in ["@scope/pkg@1.2.3", "@scope/pkg", "pkg@2.0.0-beta.1", "pkg"] {
            let s = split_spec(0, raw);
            assert_eq!(s.to_arg(), raw);
        }
    }
}
