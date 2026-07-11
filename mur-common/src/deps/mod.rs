//! Portable program dependencies: declaring, detecting, and (curated)
//! installing the external programs a shared MUR artifact needs.
//! See docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md

use serde::{Deserialize, Serialize};

pub mod detect;
pub mod registry;

/// One external-program requirement declared by a skill / MCP entry / agent
/// profile / fleet. Data only — no I/O.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProgramDep {
    /// Stable lowercase identifier (also the registry key when `registry` is None).
    pub name: String,
    /// How to check whether the program is present.
    pub detect: DetectMethod,
    /// Human-readable "why this is needed", shown in the doctor report.
    pub reason: String,
    /// Manual-install guidance (URL/command). Display only — never executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Optional key into MUR's curated registry (enables auto-install).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

/// Exactly one detection method (serde picks the arm by which field is present).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum DetectMethod {
    /// A file exists at this (tilde/`$MUR_HOME`-expanded) path.
    File { file: String },
    /// A command resolves on `PATH`.
    Command { command: String },
    /// A command's reported version is `>= min`.
    Version { version: VersionCheck },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VersionCheck {
    /// Full command line to run, e.g. "node --version".
    pub command: String,
    /// Minimum acceptable semver, e.g. "18.0.0".
    pub min: String,
}

/// Result of detecting a `ProgramDep`.
#[derive(Debug, Clone, PartialEq)]
pub enum DepStatus {
    Present,
    Missing,
    PresentWrongVersion { found: String },
}

/// Current platform key, `<arch>-<os>` (e.g. "aarch64-macos"), matching the
/// curated registry's per-platform keys. Uses the compiled target via
/// `std::env::consts` (no subprocess).
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_dep_parses_all_detect_methods() {
        let y = r#"
- name: lightpanda
  detect: { file: "~/.mur/aura/lightpanda" }
  reason: "render tier"
  hint: "https://lightpanda.io/download"
  registry: lightpanda
- name: gh
  detect: { command: "gh" }
  reason: "github ops"
- name: node
  detect: { version: { command: "node --version", min: "18.0.0" } }
  reason: "js runtime"
"#;
        let deps: Vec<ProgramDep> = serde_yaml::from_str(y).unwrap();
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "lightpanda");
        assert!(
            matches!(&deps[0].detect, DetectMethod::File { file } if file == "~/.mur/aura/lightpanda")
        );
        assert_eq!(deps[0].registry.as_deref(), Some("lightpanda"));
        assert!(matches!(&deps[1].detect, DetectMethod::Command { command } if command == "gh"));
        assert!(deps[1].hint.is_none());
        assert!(
            matches!(&deps[2].detect, DetectMethod::Version { version } if version.min == "18.0.0")
        );
    }

    #[test]
    fn current_platform_is_arch_dash_os() {
        let p = current_platform();
        assert!(p.contains('-'));
        // arch and os are non-empty
        let (arch, os) = p.split_once('-').unwrap();
        assert!(!arch.is_empty() && !os.is_empty());
    }
}
