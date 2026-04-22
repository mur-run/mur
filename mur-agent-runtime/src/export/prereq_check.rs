//! Prerequisite checker — validates runtime dependencies before agent start.
//!
//! For self-contained binaries (Task 34/35), the supervisor calls
//! `check_mcp_prereqs` against the bundled manifest before spinning up;
//! a missing MCP command yields a friendly error and exit code 2.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Iterate `prerequisites.mcp_servers[*].command_basename` from a YAML
/// manifest string and return the names of binaries not found on $PATH.
pub fn check_mcp_prereqs(manifest_yaml: &str) -> Result<Vec<String>> {
    let v: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(manifest_yaml).context("parse manifest yaml")?;
    let mut missing = Vec::new();
    if let Some(servers) = v["prerequisites"]["mcp_servers"].as_sequence() {
        for s in servers {
            let basename = s["command_basename"].as_str().unwrap_or("");
            if basename.is_empty() {
                continue;
            }
            if !which_exists(basename) {
                missing.push(basename.to_string());
            }
        }
    }
    Ok(missing)
}

/// Best-effort PATH walk. Absolute targets check existence directly.
pub fn which_exists(bin: &str) -> bool {
    let p = PathBuf::from(bin);
    if p.is_absolute() {
        return p.exists();
    }
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            if Path::new(dir).join(bin).exists() {
                return true;
            }
        }
    }
    false
}

/// Render the spec §11.3 friendly error format.
pub fn format_missing_error(missing: &[String]) -> String {
    let mut buf = String::from("error: missing MCP prerequisites:\n");
    for b in missing {
        buf.push_str(&format!("  - {b}\n"));
    }
    buf.push_str("hint: install the missing binaries and rerun.\n");
    buf
}
