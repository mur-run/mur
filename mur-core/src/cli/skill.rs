//! `mur skill` subcommand surface.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum SkillAction {
    /// Run schema validation + full security content scan on a skill file.
    Validate {
        #[arg(default_value = "skill.yaml")]
        path: String,
        #[arg(long)]
        warnings_only: bool,
    },
    /// Convert between canonical YAML and markdown frontmatter.
    Fmt {
        path: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        write: bool,
    },
    /// List installed skills (from ~/.mur/skills/).
    List,
    /// Show full content of an installed skill.
    Show { name: String },
    /// Uninstall a skill.
    Remove { name: String },
    /// Search installed skills (--local) or remote registry.
    Search {
        query: String,
        #[arg(long)]
        local: bool,
    },
    /// Show Layer 1+2 summary of an installed skill.
    Info {
        name: String,
        #[arg(long)]
        full: bool,
    },
    /// Run full security scan + signature check on an installed skill.
    Audit { name: String },
    /// Promote or demote a skill's trust level.
    Trust {
        name: String,
        #[arg(long)]
        level: String,
    },
    /// Install a skill from registry, file, or URL.
    Install {
        /// Skill name (registry), local path, or git URL.
        source: String,
    },
    /// Publish a local skill to the default registry.
    Publish {
        /// Path to skill.yaml to publish.
        path: String,
    },
    /// Update an installed skill to the latest registry version.
    Update {
        /// Name of installed skill to update.
        name: String,
    },
    /// Print the resolved dependency tree for an installed skill.
    Deps {
        /// Name of installed skill.
        name: String,
    },
}
