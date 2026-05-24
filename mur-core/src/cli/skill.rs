//! `mur skill` subcommand surface (M0: validate + fmt only).

use clap::Subcommand;

#[derive(Subcommand)]
pub enum SkillAction {
    /// Run schema validation + full security content scan on a skill file.
    Validate {
        /// Path to skill.yaml or skill.md. Defaults to ./skill.yaml.
        #[arg(default_value = "skill.yaml")]
        path: String,
        /// Exit non-zero only on schema errors; print scan findings but
        /// don't fail the command on them (useful for CI gating step 1).
        #[arg(long)]
        warnings_only: bool,
    },
    /// Convert between canonical YAML and markdown frontmatter forms.
    Fmt {
        /// Input file (yaml or md, auto-detected by extension).
        path: String,
        /// Target format: `yaml` or `md`. If omitted, flips the input format.
        #[arg(long)]
        to: Option<String>,
        /// Write the result back to the file in-place; otherwise stdout.
        #[arg(long)]
        write: bool,
    },
}
