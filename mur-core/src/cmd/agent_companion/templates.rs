//! `mur agent companion templates eject` — write embedded voice templates to disk.

use anyhow::Result;
use clap::{Args, Subcommand};
use mur_common::companion::Relationship;
use std::path::{Path, PathBuf};

use super::util::{agent_home_for, atomic_write_bytes};

// ─── CLI types ────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct TemplatesArgs {
    #[command(subcommand)]
    pub cmd: TemplatesCmd,
}

#[derive(Subcommand, Debug)]
pub enum TemplatesCmd {
    /// Eject embedded voice templates to disk for editing.
    Eject {
        /// Agent name (required when --scope=agent).
        name: Option<String>,
        /// Where to write: agent (per-agent dir) or user (mur-home shared).
        #[arg(long, default_value = "agent", value_parser = ["agent", "user"])]
        scope: String,
        /// Optional `<relationship>.<locale>` selector (e.g. `friend.zh-TW`).
        /// Without it, ejects ALL combinations.
        selector: Option<String>,
    },
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(args: TemplatesArgs) -> Result<()> {
    match args.cmd {
        TemplatesCmd::Eject {
            name,
            scope,
            selector,
        } => {
            let dest_root = resolve_dest_root(name.as_deref(), &scope)?;
            eject_to(&dest_root, selector.as_deref())
        }
    }
}

// ─── Path-taking implementation (also used by tests) ──────────────────────────

pub(crate) fn eject_to(dest_root: &Path, selector: Option<&str>) -> Result<()> {
    std::fs::create_dir_all(dest_root)?;

    let all = mur_common::companion::voice_template::all_templates();
    let filtered: Vec<_> = match selector {
        Some(sel) => {
            let (rel_str, locale) = sel.split_once('.').ok_or_else(|| {
                anyhow::anyhow!("selector must be `relationship.locale`, got `{sel}`")
            })?;
            all.into_iter()
                .filter(|(rel, loc, _)| rel_segment(rel) == rel_str && *loc == locale)
                .collect()
        }
        None => all,
    };

    if filtered.is_empty() {
        anyhow::bail!("no templates matched");
    }

    for (rel, locale, body) in &filtered {
        let filename = format!("{}.{}.md", rel_segment(rel), locale);
        let path = dest_root.join(&filename);
        atomic_write_bytes(&path, body.as_bytes())?;
        println!("✓ {}", path.display());
    }
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn resolve_dest_root(name: Option<&str>, scope: &str) -> Result<PathBuf> {
    match scope {
        "agent" => {
            let n = name.ok_or_else(|| anyhow::anyhow!("--scope=agent requires <name>"))?;
            Ok(agent_home_for(n)?.join("companion/templates"))
        }
        "user" => Ok(crate::paths::mur_root(None).join("companion/templates")),
        _ => unreachable!(),
    }
}

fn rel_segment(r: &Relationship) -> &'static str {
    use Relationship::*;
    match r {
        Friend => "friend",
        Coach => "coach",
        AccountabilityBuddy => "accountability_buddy",
        Mentor => "mentor",
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn eject_all_writes_10_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("companion/templates");
        eject_to(&dest, None).unwrap();
        let count = std::fs::read_dir(&dest).unwrap().count();
        assert_eq!(
            count, 10,
            "expected 10 templates (friend×4 + coach×2 + accountability_buddy×2 + mentor×2)"
        );
    }

    #[test]
    fn eject_with_selector_writes_one_file() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("companion/templates");
        eject_to(&dest, Some("friend.zh-TW")).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["friend.zh-TW.md"]);
    }

    #[test]
    fn eject_with_unknown_selector_errors() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("companion/templates");
        let err = eject_to(&dest, Some("friend.klingon")).unwrap_err();
        assert!(
            err.to_string().contains("no templates matched"),
            "expected 'no templates matched', got: {err}"
        );
    }

    #[test]
    fn eject_with_malformed_selector_errors() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("companion/templates");
        let err = eject_to(&dest, Some("nodot")).unwrap_err();
        assert!(
            err.to_string().contains("relationship.locale"),
            "expected format hint, got: {err}"
        );
    }
}
