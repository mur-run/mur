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
#[cfg(unix)]
use mur_common::serena_vue::{LS_RESOURCES_REL, VUE_LS_CLASS, VUE_LSP_DIR};
pub use mur_common::serena_vue::{MARKER_FILE, marker, resources_link, ts_shim, vue_shim};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// File extension that marks a project as Vue.
pub const VUE_EXT: &str = "vue";
/// Directories never searched for `.vue` files (serena ignores them too).
const SKIP_DIRS: [&str; 7] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "coverage",
    ".nuxt",
    ".output",
];
/// How deep `project_has_vue` looks below the project root. A Vue app's
/// components sit a handful of levels down; the cap bounds the walk on a
/// large tree that holds no Vue at all.
const MAX_SCAN_DEPTH: usize = 12;
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
/// write, so a component the agent swapped for a symlink must not steer a
/// single write out of `serena_home` (#1998 review). `serena_home` itself
/// is the trusted root; every dir below it is created and opened relative
/// to its parent's fd with `O_NOFOLLOW`, so a planted symlink is refused
/// before anything is made through it, and a later swap of the path cannot
/// redirect the fd-relative steps. The new link is staged beside the old
/// one and renamed into place in one step; a real directory there is
/// renamed aside first and removed without following links inside it.
#[cfg(unix)]
pub fn link(serena_home: &Path, tools_dir: &Path) -> Result<LinkOutcome> {
    use rustix::fs::{AtFlags, FileType, Mode};
    let target = VUE.dir(tools_dir);
    std::fs::create_dir_all(serena_home)
        .with_context(|| format!("mkdir {}", serena_home.display()))?;
    let mut dir = rustix::fs::open(serena_home, dir_flags(), Mode::empty())
        .with_context(|| format!("open {}", serena_home.display()))?;
    let mut shown = serena_home.to_path_buf();
    for name in LS_RESOURCES_REL.iter().chain([&VUE_LS_CLASS]) {
        shown.push(name);
        dir = descend(&dir, name, &shown)?;
    }
    let link = VUE_LSP_DIR;
    let shown_link = shown.join(link);

    let outcome = match rustix::fs::statat(&dir, link, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => match FileType::from_raw_mode(st.st_mode as _) {
            FileType::Symlink => {
                let now = rustix::fs::readlinkat(&dir, link, Vec::new()).ok();
                if now.as_deref().map(|c| c.to_bytes()) == Some(target.as_os_str().as_bytes()) {
                    return Ok(LinkOutcome::Unchanged);
                }
                LinkOutcome::Replaced
            }
            FileType::Directory => {
                let aside = sibling(link, ASIDE_SUFFIX);
                rustix::fs::renameat(&dir, link, &dir, &aside)
                    .with_context(|| format!("move {} aside", shown_link.display()))?;
                remove_tree_at(&dir, &aside)
                    .with_context(|| format!("remove {}", shown.join(&aside).display()))?;
                LinkOutcome::Replaced
            }
            _ => LinkOutcome::Replaced,
        },
        Err(rustix::io::Errno::NOENT) => LinkOutcome::Created,
        Err(e) => return Err(e).with_context(|| format!("inspect {}", shown_link.display())),
    };
    let stage = sibling(link, STAGE_SUFFIX);
    let _ = rustix::fs::unlinkat(&dir, &stage, AtFlags::empty());
    rustix::fs::symlinkat(&target, &dir, &stage).with_context(|| {
        format!(
            "link {} -> {}",
            shown.join(&stage).display(),
            target.display()
        )
    })?;
    if let Err(e) = rustix::fs::renameat(&dir, &stage, &dir, link) {
        let _ = rustix::fs::unlinkat(&dir, &stage, AtFlags::empty());
        return Err(e).with_context(|| format!("move link into {}", shown_link.display()));
    }
    Ok(outcome)
}

#[cfg(unix)]
fn dir_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC
}

/// Make `name` under `parent` if missing, then open it without following a
/// symlink. A symlink (or anything not a directory) there is refused, and
/// nothing has been created through it.
#[cfg(unix)]
fn descend(parent: &rustix::fd::OwnedFd, name: &str, shown: &Path) -> Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(e).with_context(|| format!("mkdir {}", shown.display())),
    }
    rustix::fs::openat(parent, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty()).map_err(|e| {
        anyhow::anyhow!(
            "{} is not a real directory inside SERENA_HOME ({e}); refusing to touch it",
            shown.display()
        )
    })
}

/// `rm -rf name` relative to `parent`, never following a symlink: links
/// are unlinked, only real dirs are entered (opened `O_NOFOLLOW`).
#[cfg(unix)]
fn remove_tree_at(parent: &rustix::fd::OwnedFd, name: &str) -> rustix::io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    let fd = rustix::fs::openat(parent, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty())?;
    let mut entries = Vec::new();
    for entry in rustix::fs::Dir::read_from(&fd)? {
        let entry = entry?;
        let n = entry.file_name().to_bytes();
        if n != b"." && n != b".." {
            entries.push((entry.file_name().to_owned(), entry.file_type()));
        }
    }
    for (n, kind) in entries {
        let n = n.to_string_lossy().into_owned();
        let is_dir = match kind {
            FileType::Directory => true,
            FileType::Unknown => rustix::fs::statat(&fd, &n, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|st| FileType::from_raw_mode(st.st_mode as _) == FileType::Directory),
            _ => false,
        };
        if is_dir {
            remove_tree_at(&fd, &n)?;
        } else {
            rustix::fs::unlinkat(&fd, &n, AtFlags::empty())?;
        }
    }
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)
}

#[cfg(unix)]
fn sibling(name: &str, suffix: &str) -> String {
    format!("{name}{suffix}-{}", std::process::id())
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
        .max_depth(MAX_SCAN_DEPTH)
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
