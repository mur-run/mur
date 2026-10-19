//! C7 for serena's Vue server (#1982). It has no `ls_base_cmd` or
//! `ls_path` for the generic C7 to match: serena always execs the shim
//! under `<SERENA_HOME>/…/VueLanguageServer/vue-lsp`, which `mur code-nav
//! setup` makes a symlink into the tools root. That link is
//! agent-writable, so it is re-checked here before every launch, along
//! with the version settings serena compares the install marker against.
//! A mismatch there would make serena npm-install through the link.

use std::path::Path;

use mur_common::serena_ls_cmd::VUE_LS_ID;
use mur_common::serena_vue::{LinkState, check_link, resources_link, version_settings};
use serde_yaml_ng::{Mapping, Value};

use super::{
    ABSENT, KEY_LS_SETTINGS, PROJECT_FILE, PROJECT_LOCAL_FILE, SerenaPaths, SerenaPreflightError,
    project_languages, project_yaml, show,
};

const LINK_KEY: &str = "vue-lsp";
const FIX: &str = "re-run `mur code-nav setup --with-serena --lsp vue`";

/// `ls` is the global `ls_specific_settings`; `folder` is the MUR-owned
/// project folder, whose `project.yml` says whether Vue is enabled.
pub(super) fn check(
    ls: Option<&Mapping>,
    cfg_file: &Path,
    folder: &Path,
    paths: &SerenaPaths,
) -> Result<(), SerenaPreflightError> {
    let link = resources_link(&paths.home);
    let expected = format!("MUR's link into {} ({FIX})", paths.tools_dir.display());
    let state = match check_link(&paths.home, &paths.tools_dir) {
        Ok(s) => s,
        Err(found) => return fail!(C7LsExec, link, LINK_KEY, found, expected),
    };
    let listed = project_languages(folder)
        .is_some_and(|l| l.iter().any(|x| x.eq_ignore_ascii_case(VUE_LS_ID)));
    if listed && state == LinkState::Absent {
        let found = format!("{ABSENT} (serena would npm-install in the sandbox)");
        return fail!(C7LsExec, link, LINK_KEY, found, expected);
    }
    if state == LinkState::Absent {
        return Ok(());
    }
    for (section, key, pin) in version_settings() {
        let full = format!("{KEY_LS_SETTINGS}.{section}.{key}");
        let want = format!("{pin:?} (MUR's pin; {FIX})");
        let got = ls.and_then(|m| m.get(section)).and_then(|s| s.get(key));
        if got.and_then(Value::as_str) != Some(pin) {
            let found = got.map_or(ABSENT.to_owned(), show);
            return fail!(C7LsExec, cfg_file, full, found, want);
        }
        // serena ignores project files while C3 holds; a different pin
        // there is refused anyway, as the generic C7 does.
        for name in [PROJECT_FILE, PROJECT_LOCAL_FILE] {
            let file = folder.join(name);
            let v = project_yaml(&file).and_then(|doc| {
                doc.get(KEY_LS_SETTINGS)
                    .and_then(|m| m.get(section))
                    .and_then(|s| s.get(key))
                    .cloned()
            });
            if let Some(v) = v
                && v.as_str() != Some(pin)
            {
                return fail!(C7LsExec, file, full, show(&v), want);
            }
        }
    }
    Ok(())
}
