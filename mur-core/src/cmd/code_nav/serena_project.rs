//! Task 3.6: the MUR-owned `project.yml` in serena's per-project folder.
//!
//! The runtime's C8 check reads this file's `language_servers`: without it
//! serena auto-detects languages, so C/C++ cannot be ruled out and startup
//! is refused. Setup therefore writes the list the user consented to.
//!
//! Like the global config (3.4) the text comes from the pinned install's own
//! template (GPL, not vendored), refused unless its sha256 matches. Every
//! template field is kept, so serena finds the file complete and does not
//! re-save it on load (`ProjectConfig.load`, `was_complete`).

use anyhow::{Context, Result};
use mur_agent_runtime::mcp::serena::SerenaPaths;
use serde_yaml_ng::{Mapping, Value};
use std::path::{Path, PathBuf};

use super::plan::Lang;
use super::serena_config::{project_folder, read_pinned_resource, render_owned, write_private};

/// sha256 of `serena/resources/project.template.yml` at `SERENA_GIT_REV`.
pub const PROJECT_TEMPLATE_SHA256: &str =
    "443c9dd25f1cb09ddfcfa4b63f207808e1969ab73202f25814797a5be50ca54f";
const PROJECT_TEMPLATE_REL: [&str; 3] = ["serena", "resources", "project.template.yml"];
/// serena's file name inside the project folder.
pub const PROJECT_FILE: &str = "project.yml";

/// Keys MUR sets; every other template key keeps serena's default.
pub const PROJECT_OWNED_KEYS: [&str; 2] = ["project_name", "language_servers"];

const HEADER: &str = "\n# --- Managed by MUR (`mur code-nav setup`): the language list is the one \
                      you consented to.\n# --- The runtime refuses C/C++ unless it was enabled.\n";

pub fn read_pinned_project_template(install_dir: &Path) -> Result<String> {
    read_pinned_resource(
        install_dir,
        &PROJECT_TEMPLATE_REL,
        PROJECT_TEMPLATE_SHA256,
        "serena project template",
    )
}

/// The project's folder name, which serena also uses as `project_name`.
fn project_name(project_root: &Path) -> Result<String> {
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .with_context(|| {
            format!(
                "project {} has no UTF-8 folder name",
                project_root.display()
            )
        })
}

/// Template with `project_name` and `language_servers` replaced.
pub fn render_project(template: &str, project_root: &Path, langs: &[Lang]) -> Result<String> {
    let mut owned = Mapping::new();
    owned.insert(
        PROJECT_OWNED_KEYS[0].into(),
        Value::String(project_name(project_root)?),
    );
    owned.insert(
        PROJECT_OWNED_KEYS[1].into(),
        Value::Sequence(langs.iter().map(|l| Value::from(l.flag())).collect()),
    );
    render_owned(
        template,
        &PROJECT_OWNED_KEYS,
        &owned,
        HEADER,
        "serena project template",
    )
}

/// Write `<projects_dir>/<folder>/project.yml`. Must run before
/// `write_config`, whose preflight reads the language list (C8).
pub fn write_project(
    paths: &SerenaPaths,
    project_root: &Path,
    template: &str,
    langs: &[Lang],
) -> Result<PathBuf> {
    let folder = project_folder(paths, project_root)?;
    std::fs::create_dir_all(&folder).with_context(|| format!("create {}", folder.display()))?;
    let path = folder.join(PROJECT_FILE);
    write_private(
        &path,
        render_project(template, project_root, langs)?.as_bytes(),
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# name comment
project_name: \"project_name\"

# languages comment
language_servers: [\"python\"]
encoding: \"utf-8\"
ignored_paths: []
";

    fn top(text: &str) -> Mapping {
        serde_yaml_ng::from_str(text).unwrap()
    }

    #[test]
    fn owned_keys_replaced_rest_kept() {
        let text =
            render_project(FIXTURE, Path::new("/r/myrepo"), &[Lang::Go, Lang::Python]).unwrap();
        let m = top(&text);
        assert_eq!(m["project_name"], Value::from("myrepo"));
        assert_eq!(
            m["language_servers"],
            Value::Sequence(vec!["go".into(), "python".into()])
        );
        assert_eq!(m["encoding"], Value::from("utf-8"));
        assert_eq!(text.matches("language_servers:").count(), 1);
        assert!(text.contains("# languages comment"));
    }

    #[test]
    fn cpp_listed_only_when_enabled() {
        let without = render_project(FIXTURE, Path::new("/r/x"), &[Lang::Python]).unwrap();
        assert!(
            !top(&without)["language_servers"]
                .as_sequence()
                .unwrap()
                .contains(&Value::from("cpp"))
        );
        let with = render_project(FIXTURE, Path::new("/r/x"), &[Lang::Cpp]).unwrap();
        assert_eq!(
            top(&with)["language_servers"],
            Value::Sequence(vec!["cpp".into()])
        );
    }

    #[test]
    fn template_without_language_list_is_refused() {
        let err = render_project("project_name: x\n", Path::new("/r/x"), &[Lang::Python])
            .unwrap_err()
            .to_string();
        assert!(err.contains("language_servers"), "{err}");
    }
}
