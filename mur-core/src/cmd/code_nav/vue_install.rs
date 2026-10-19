//! The Vue language server (#1982): pre-installed like the other node
//! servers ([`super::node_ls_install`]), then handed to serena by linking
//! serena's own resources dir to the install.
//!
//! serena's Vue server has no launch override. At serena-agent 2.0.0.dev0,
//! `solidlsp/language_servers/vue_language_server.py`
//! (`VueLanguageServer._setup_runtime_dependencies`) always runs
//! `<ls_resources_dir>/VueLanguageServer/vue-lsp/node_modules/.bin/
//! vue-language-server --stdio`, and npm-installs into `vue-lsp/` unless
//! both `.bin` shims exist and `vue-lsp/.installed_version` reads
//! `<vue>_<typescript>_<typescript-language-server>`. Left alone that is the
//! two sandbox failures of #1981 again (npm cache EPERM; a shebang shim
//! under the agent-writable `SERENA_HOME` the seal will not exec).
//!
//! So setup installs the three pinned packages under the tools root, writes
//! the marker there, and makes `vue-lsp` a symlink to that install. The
//! seal checks exec on the resolved script, which is under the tools root,
//! and only the server's own `bin/` dir is granted. The companion
//! TypeScript server is not launched from here: serena gives it the
//! `ls_specific_settings.typescript` `ls_base_cmd` written for #1981.
//!
//! What serena compares the marker against, and the check the runtime
//! repeats before every launch, live in [`mur_common::serena_vue`].

use anyhow::{Context, Result, bail};
use mur_common::serena_ls_cmd::VUE;
pub use mur_common::serena_vue::{MARKER_FILE, marker, resources_link, ts_shim, vue_shim};
use std::path::{Path, PathBuf};

/// File extension that marks a project as Vue.
pub const VUE_EXT: &str = "vue";
/// Directories never searched for `.vue` files (serena ignores them too).
const SKIP_DIRS: [&str; 6] = [
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".nuxt",
    ".output",
];
/// Sibling names `link` stages through, suffixed with the pid.
const STAGE_SUFFIX: &str = ".mur-stage";
const ASIDE_SUFFIX: &str = ".mur-old";

/// The only directory the seal must exec in: the server's own `bin/`.
pub fn exec_dir(tools_dir: &Path) -> PathBuf {
    let entry = VUE.entry(tools_dir);
    entry.parent().map(Path::to_path_buf).unwrap_or(entry)
}

/// Write the marker into the install, so serena sees a complete install.
pub fn write_marker(tools_dir: &Path) -> Result<()> {
    let dir = VUE.dir(tools_dir);
    for shim in [vue_shim(&dir), ts_shim(&dir)] {
        if !shim.exists() {
            bail!("{} is missing; re-run setup to reinstall", shim.display());
        }
    }
    let path = dir.join(MARKER_FILE);
    std::fs::write(&path, marker()).with_context(|| format!("write {}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Unchanged,
    Created,
    /// A previous `vue-lsp` (serena's own download, or a stale link) was
    /// replaced.
    Replaced,
}

/// Point serena's `vue-lsp` at the install. Idempotent; a version bump
/// moves the link to the new install dir.
///
/// Setup runs as the user, outside the seal, on a path the agent can
/// write. So the parent dir must canonicalize inside `serena_home` (a
/// planted symlink cannot steer the rename or the delete elsewhere), the
/// new link is staged beside it and renamed into place in one step, and a
/// real directory there is first renamed aside, never deleted in place.
#[cfg(unix)]
pub fn link(serena_home: &Path, tools_dir: &Path) -> Result<LinkOutcome> {
    let link = resources_link(serena_home);
    let target = VUE.dir(tools_dir);
    let parent = link
        .parent()
        .context("serena resources link has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    let home_real = serena_home
        .canonicalize()
        .with_context(|| format!("resolve {}", serena_home.display()))?;
    let parent_real = parent
        .canonicalize()
        .with_context(|| format!("resolve {}", parent.display()))?;
    if !parent_real.starts_with(&home_real) {
        bail!(
            "{} resolves to {}, outside SERENA_HOME {}; refusing to touch it",
            parent.display(),
            parent_real.display(),
            home_real.display()
        );
    }
    let link = parent_real.join(mur_common::serena_vue::VUE_LSP_DIR);

    let outcome = match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(&link).ok().as_deref() == Some(target.as_path()) {
                return Ok(LinkOutcome::Unchanged);
            }
            LinkOutcome::Replaced
        }
        Ok(meta) if meta.is_dir() => {
            let aside = sibling(&link, ASIDE_SUFFIX);
            std::fs::rename(&link, &aside)
                .with_context(|| format!("move {} aside", link.display()))?;
            // `aside` is a real directory inside the checked parent;
            // `remove_dir_all` does not follow symlinks inside it.
            std::fs::remove_dir_all(&aside)
                .with_context(|| format!("remove {}", aside.display()))?;
            LinkOutcome::Replaced
        }
        Ok(_) => LinkOutcome::Replaced,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LinkOutcome::Created,
        Err(e) => return Err(e).with_context(|| format!("inspect {}", link.display())),
    };
    let stage = sibling(&link, STAGE_SUFFIX);
    let _ = std::fs::remove_file(&stage);
    std::os::unix::fs::symlink(&target, &stage)
        .with_context(|| format!("link {} -> {}", stage.display(), target.display()))?;
    if let Err(e) = std::fs::rename(&stage, &link) {
        let _ = std::fs::remove_file(&stage);
        return Err(e).with_context(|| format!("move link into {}", link.display()));
    }
    Ok(outcome)
}

#[cfg(unix)]
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!("{suffix}-{}", std::process::id()));
    path.with_file_name(name)
}

#[cfg(not(unix))]
pub fn link(_serena_home: &Path, _tools_dir: &Path) -> Result<LinkOutcome> {
    bail!("--lsp vue is supported on unix hosts only")
}

/// Whether `root` holds a `.vue` file outside the skipped dirs. Used only
/// to hint at `--lsp vue`; Vue is High tier, so it is never enabled by
/// detection alone.
pub fn project_has_vue(root: &Path) -> bool {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || e.file_name()
                    .to_str()
                    .is_some_and(|n| !n.starts_with('.') && !SKIP_DIRS.contains(&n))
        })
        .filter_map(Result::ok)
        .any(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|x| x.to_str()) == Some(VUE_EXT)
        })
}

#[cfg(all(test, unix))]
#[path = "vue_install_tests.rs"]
mod tests;
