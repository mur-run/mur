//! How serena's Vue server finds its install, shared by `mur code-nav
//! setup` (which builds it), the runtime preflight C7 (which refuses a
//! launch unless it is intact) and `mur code-nav doctor` (#1982).
//!
//! serena's Vue server has no launch override. At serena-agent 2.0.0.dev0
//! (`solidlsp/language_servers/vue_language_server.py`,
//! `VueLanguageServer._setup_runtime_dependencies`) it always execs
//! `<SERENA_HOME>/language_servers/static/VueLanguageServer/vue-lsp/
//! node_modules/.bin/vue-language-server --stdio`, and npm-installs into
//! `vue-lsp/` unless both `.bin` shims exist and `vue-lsp/.installed_version`
//! reads `<vue>_<typescript>_<typescript-language-server>`. Those three
//! versions come from `ls_specific_settings` (`vue.vue_language_server_version`,
//! `typescript.typescript_version`, `typescript.typescript_language_server_version`)
//! with defaults hard-coded in serena. MUR writes all three explicitly
//! ([`version_settings`]), so a serena upgrade that changes a default cannot
//! silently turn a complete install into a reinstall through the link.
//!
//! setup makes `vue-lsp` a symlink to [`VUE`]'s install under the tools root.
//! The link lives in the agent-writable `SERENA_HOME`, so [`check_link`]
//! re-verifies it before every launch. Lives here because
//! `mur-agent-runtime` must not depend on `mur-core`.

use std::path::{Path, PathBuf};

use crate::serena_ls_cmd::{
    TS_LS_ID, TS_LS_PACKAGE, TS_LS_PIN, TYPESCRIPT_PIN, VUE, VUE_LS_ID, VUE_LS_PIN,
};

/// serena's resources dir under `SERENA_HOME`
/// (`SolidLSPSettings.ls_resources_dir`).
pub const LS_RESOURCES_REL: [&str; 2] = ["language_servers", "static"];
/// serena's per-server subdir: the class name (`ls_resources_dir`).
pub const VUE_LS_CLASS: &str = "VueLanguageServer";
/// The install dir serena expects inside it.
pub const VUE_LSP_DIR: &str = "vue-lsp";
/// serena's version marker inside `vue-lsp/`.
pub const MARKER_FILE: &str = ".installed_version";
/// The shim serena execs for the Vue server.
pub const VUE_SHIM_REL: [&str; 3] = ["node_modules", ".bin", "vue-language-server"];
/// serena refuses the install unless this shim exists too.
pub const TS_SHIM_REL: [&str; 3] = ["node_modules", ".bin", TS_LS_PACKAGE];

/// `ls_specific_settings.vue` key for the Vue server version.
pub const VUE_VERSION_KEY: &str = "vue_language_server_version";
/// `ls_specific_settings.typescript` key for the `typescript` package.
pub const TYPESCRIPT_VERSION_KEY: &str = "typescript_version";
/// `ls_specific_settings.typescript` key for typescript-language-server.
pub const TS_LS_VERSION_KEY: &str = "typescript_language_server_version";

/// `(ls_specific_settings section, key, pinned value)` for every version
/// serena folds into the marker. Written by setup, required by C7.
pub fn version_settings() -> [(&'static str, &'static str, &'static str); 3] {
    [
        (VUE_LS_ID, VUE_VERSION_KEY, VUE_LS_PIN),
        (TS_LS_ID, TYPESCRIPT_VERSION_KEY, TYPESCRIPT_PIN),
        (TS_LS_ID, TS_LS_VERSION_KEY, TS_LS_PIN),
    ]
}

/// What serena compares `vue-lsp/.installed_version` against.
pub fn marker() -> String {
    format!("{VUE_LS_PIN}_{TYPESCRIPT_PIN}_{TS_LS_PIN}")
}

/// `<serena_home>/language_servers/static/VueLanguageServer`: the dir that
/// holds the link. Everything from `serena_home` down is agent-writable.
pub fn resources_parent(serena_home: &Path) -> PathBuf {
    let mut p = serena_home.to_path_buf();
    p.extend(LS_RESOURCES_REL);
    p.join(VUE_LS_CLASS)
}

/// `<serena_home>/language_servers/static/VueLanguageServer/vue-lsp`.
pub fn resources_link(serena_home: &Path) -> PathBuf {
    resources_parent(serena_home).join(VUE_LSP_DIR)
}

/// The shim serena execs, as seen through `dir`.
pub fn vue_shim(dir: &Path) -> PathBuf {
    let mut p = dir.to_path_buf();
    p.extend(VUE_SHIM_REL);
    p
}

/// The companion shim serena also requires, as seen through `dir`.
pub fn ts_shim(dir: &Path) -> PathBuf {
    let mut p = dir.to_path_buf();
    p.extend(TS_SHIM_REL);
    p
}

/// What [`check_link`] found at `vue-lsp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// Nothing there: serena would npm-install if Vue were enabled.
    Absent,
    /// MUR's link to a complete, current install.
    Linked,
}

/// Check `vue-lsp` under `serena_home`: absent, or a symlink that resolves
/// to exactly [`VUE`]'s install under `tools_dir`, with both shims present
/// and the marker reading [`marker`]. `Err` says what is wrong, for a
/// message that ends in "re-run setup".
pub fn check_link(serena_home: &Path, tools_dir: &Path) -> Result<LinkState, String> {
    let link = resources_link(serena_home);
    let meta = match std::fs::symlink_metadata(&link) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LinkState::Absent),
        Err(e) => return Err(format!("{}: {e}", link.display())),
    };
    if !meta.file_type().is_symlink() {
        return Err(format!(
            "{} is not MUR's link (serena's own download, or replaced)",
            link.display()
        ));
    }
    let want = VUE.dir(tools_dir);
    let real = link
        .canonicalize()
        .map_err(|e| format!("{} ({e})", link.display()))?;
    let want_real = want
        .canonicalize()
        .map_err(|e| format!("{} ({e})", want.display()))?;
    if real != want_real {
        return Err(format!(
            "{} -> {}, not {}",
            link.display(),
            real.display(),
            want.display()
        ));
    }
    for shim in [vue_shim(&real), ts_shim(&real)] {
        if !shim.exists() {
            return Err(format!("{} is missing", shim.display()));
        }
    }
    let marker_path = real.join(MARKER_FILE);
    match std::fs::read_to_string(&marker_path) {
        Ok(s) if s.trim() == marker() => Ok(LinkState::Linked),
        Ok(s) => Err(format!(
            "{} reads {:?}, expected {:?}",
            marker_path.display(),
            s.trim(),
            marker()
        )),
        Err(e) => Err(format!("{} ({e})", marker_path.display())),
    }
}

#[cfg(all(test, unix))]
#[path = "serena_vue_tests.rs"]
mod tests;
