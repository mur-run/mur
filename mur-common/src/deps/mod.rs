//! Portable program dependencies: declaring, detecting, and (curated)
//! installing the external programs a shared MUR artifact needs.
//! See docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// Author-declared install recipe (Phase 2). Present only in signed
    /// bundles; auto-installed at import ONLY from a trusted publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ProgramRecipe>,
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

/// Author-declared, per-platform install recipe carried on a `ProgramDep`
/// inside a SIGNED bundle. Its integrity flows from the bundle signature; its
/// authorization from the publisher's trust classification at import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProgramRecipe {
    /// `<arch>-<os>` → recipe. Only the current platform's entry is installed.
    pub platforms: BTreeMap<String, PlatformRecipe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlatformRecipe {
    pub url: String,
    pub sha256: String,
    /// Bare-binary install target (relative to mur_home); `None` when `archive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_to: Option<String>,
    #[serde(default)]
    pub executable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<registry::ArchiveSpec>,
}

impl ProgramRecipe {
    /// Convert this recipe's entry for `platform` into the Phase 1
    /// `CuratedRecipe` the installer consumes. `None` if no entry for the
    /// platform. `description` is a fixed author-provenance label.
    pub fn for_platform(&self, platform: &str) -> Option<registry::CuratedRecipe> {
        let p = self.platforms.get(platform)?;
        Some(registry::CuratedRecipe {
            description: "author-declared recipe".to_string(),
            url: p.url.clone(),
            sha256: p.sha256.clone(),
            install_to: p.install_to.clone(),
            executable: p.executable,
            archive: p.archive.clone(),
        })
    }
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

    #[test]
    fn program_dep_parses_optional_recipe_and_defaults_none() {
        let with = r#"
name: some-tool
detect: { command: some-tool }
reason: "x"
recipe:
  platforms:
    aarch64-macos: { url: "u", sha256: "s", install_to: "aura/some-tool", executable: true }
"#;
        let d: ProgramDep = serde_yaml::from_str(with).unwrap();
        assert!(d.recipe.is_some());
        assert!(d.recipe.unwrap().for_platform("aarch64-macos").is_some());

        // Phase 1 dep without recipe → None (back-compat).
        let without = "name: x\ndetect: { command: x }\nreason: r\n";
        let d2: ProgramDep = serde_yaml::from_str(without).unwrap();
        assert!(d2.recipe.is_none());
    }
}

#[cfg(test)]
mod recipe_tests {
    use super::*;

    #[test]
    fn program_recipe_for_platform_converts_to_curated() {
        let y = r#"
platforms:
  aarch64-macos: { url: "https://x/tool", sha256: "abc123", install_to: "aura/tool", executable: true }
"#;
        let r: ProgramRecipe = serde_yaml::from_str(y).unwrap();
        let cur = r.for_platform("aarch64-macos").expect("platform present");
        assert_eq!(cur.url, "https://x/tool");
        assert_eq!(cur.sha256, "abc123");
        assert_eq!(cur.install_to.as_deref(), Some("aura/tool"));
        assert!(cur.executable);
        assert!(cur.archive.is_none());
        assert!(r.for_platform("sparc-solaris").is_none());
    }
}
