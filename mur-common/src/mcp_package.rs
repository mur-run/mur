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
    match runner {
        Runner::Npm => {
            let out = std::process::Command::new("npm")
                .args(["view", name, "version"])
                .output()
                .map_err(|e| anyhow::anyhow!("run `npm view {name} version`: {e}"))?;
            if !out.status.success() {
                bail!(
                    "`npm view {name} version` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                );
            }
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if v.is_empty() {
                bail!("`npm view {name} version` returned nothing");
            }
            Ok(v)
        }
        // uv has no "what version would you pick" query, so resolve the way an
        // install would and read the answer back out of the resolution. Using
        // the resolver rather than a registry query means the recorded version
        // is the one uv would actually install, including any yanked-release
        // or requires-python filtering it applies.
        Runner::Python => {
            let dir = tempfile::tempdir().map_err(|e| anyhow::anyhow!("temp dir: {e}"))?;
            let req_in = dir.path().join("req.in");
            std::fs::write(&req_in, format!("{name}\n"))
                .map_err(|e| anyhow::anyhow!("write {}: {e}", req_in.display()))?;
            let out = std::process::Command::new("uv")
                .args(["pip", "compile", "req.in", "-o", "req.lock"])
                .current_dir(dir.path())
                .output()
                .map_err(|e| anyhow::anyhow!("run uv pip compile: {e} (is uv on PATH?)"))?;
            if !out.status.success() {
                bail!(
                    "`uv pip compile` could not resolve `{name}`: {}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                );
            }
            let body = std::fs::read_to_string(dir.path().join("req.lock"))
                .map_err(|e| anyhow::anyhow!("read resolved lockfile: {e}"))?;
            pinned_version_of(&body, name)
                .ok_or_else(|| anyhow::anyhow!("`{name}` did not appear in uv's resolution"))
        }
    }
}

/// Find `name==version` for `name` in a `uv pip compile` lockfile.
///
/// Distribution names normalise loosely — `Foo.Bar` and `foo-bar` are the same
/// project — so matching has to normalise too, or a package would resolve fine
/// and then look absent.
pub fn pinned_version_of(lockfile: &str, name: &str) -> Option<String> {
    let want = normalize_dist_name(name);
    for line in lockfile.lines() {
        let line = line.trim();
        // A lockfile is mostly not pins: blank lines, `# via` comments, and
        // `--hash=` continuations. Each has to be skipped, not treated as the
        // end of the search — `?` here would abandon the whole file at the
        // first one.
        if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
            continue;
        }
        let Some(spec) = line.split_whitespace().next() else {
            continue;
        };
        let Some((pkg, version)) = spec.split_once("==") else {
            continue;
        };
        if normalize_dist_name(pkg) == want {
            return Some(version.trim_end_matches('\\').trim().to_string());
        }
    }
    None
}

/// PEP 503 name normalisation: lowercase, runs of `-_.` collapse to `-`.
fn normalize_dist_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        } else {
            out.extend(c.to_lowercase());
            last_dash = false;
        }
    }
    out
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

    // ── uv resolution ───────────────────────────────────────────────────────

    /// Real `uv pip compile --generate-hashes` output: continuation
    /// backslashes, hash lines, and `# via` comments around the spec.
    const UV_LOCK: &str = r#"
# This file was autogenerated by uv via the following command:
#    uv pip compile req.in --generate-hashes -o req.lock
annotated-types==0.8.0 \
    --hash=sha256:13b2beaad985e05e2d6407ee4c4f35590b11f8d693a258a561055cac8f64cab7
    # via pydantic
mcp-server-time==0.6.2 \
    --hash=sha256:5d38af6cd620f2ae3849fb44fd4879e0890aa1febe8d47eb355fb45d93fe6a5b
    # via -r req.in
"#;

    #[test]
    fn reads_the_pinned_version_out_of_a_uv_lockfile() {
        assert_eq!(
            pinned_version_of(UV_LOCK, "mcp-server-time").as_deref(),
            Some("0.6.2"),
        );
        assert_eq!(
            pinned_version_of(UV_LOCK, "annotated-types").as_deref(),
            Some("0.8.0"),
            "transitive deps are pinned in the same file",
        );
        assert_eq!(pinned_version_of(UV_LOCK, "absent-pkg"), None);
    }

    /// PEP 503: `Foo.Bar`, `foo_bar` and `foo-bar` are one project. Without
    /// normalising, a package would resolve fine and then read as missing.
    #[test]
    fn distribution_names_match_across_spelling() {
        for spelling in ["mcp_server_time", "MCP-Server-Time", "mcp.server.time"] {
            assert_eq!(
                pinned_version_of(UV_LOCK, spelling).as_deref(),
                Some("0.6.2"),
                "`{spelling}` names the same project",
            );
        }
    }

    #[test]
    fn comments_are_never_mistaken_for_a_pin() {
        let lock = "# uv pip compile foo==1.0.0\nbar==2.0.0\n";
        assert_eq!(pinned_version_of(lock, "foo"), None, "that was a comment");
        assert_eq!(pinned_version_of(lock, "bar").as_deref(), Some("2.0.0"));
    }
}
