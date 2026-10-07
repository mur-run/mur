//! The broker's private bare repo (F12 items 2/6/7, F28): created with a config the broker
//! writes itself, inspected for anything that could change what git believes about ancestry,
//! digested over its control files, and frozen read-only before any untrusted pack touches it.
use crate::{
    constants::{
        ALLOWED_REPO_EXTENSIONS, CONTROL_PATHS, DEFAULT_GIT_TIMEOUT_SECS, FORBIDDEN_REPO_PATHS,
        PRIVATE_REPO_DIR,
    },
    error::BrokerError,
    git::GitRunner,
    oid::ObjectFormat,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct PrivateRepo {
    path: PathBuf,
    runner: GitRunner,
}

fn storage(e: impl std::fmt::Display) -> BrokerError {
    BrokerError::Storage(e.to_string())
}
fn unprovable(why: impl Into<String>) -> BrokerError {
    BrokerError::AncestryUnprovable(why.into())
}

/// The only config the broker ever runs git against. SHA-256 needs format version 1 plus the
/// `objectformat` extension; everything else is version 0.
fn minimal_config(fmt: ObjectFormat) -> String {
    match fmt {
        ObjectFormat::Sha1 => "[core]\n\trepositoryformatversion = 0\n\tbare = true\n".to_owned(),
        ObjectFormat::Sha256 => "[core]\n\trepositoryformatversion = 1\n\tbare = true\n\
             [extensions]\n\tobjectformat = sha256\n"
            .to_owned(),
    }
}

impl PrivateRepo {
    pub fn create(
        root: &Path,
        fmt: ObjectFormat,
        git_bin: &Path,
    ) -> Result<PrivateRepo, BrokerError> {
        let path = root.join(PRIVATE_REPO_DIR);
        GitRunner::init_bare(git_bin, &path, fmt).map_err(|e| storage(format!("{e:?}")))?;
        fs::write(path.join("config"), minimal_config(fmt)).map_err(storage)?;
        // `git init` copies sample hooks and an `info/` dir from its template; neither is ours.
        let hooks = path.join("hooks");
        fs::remove_dir_all(&hooks).map_err(storage)?;
        fs::create_dir(&hooks).map_err(storage)?;
        let info = path.join("info");
        if info.exists() {
            fs::remove_dir_all(&info).map_err(storage)?;
        }
        let runner = GitRunner::new(git_bin.to_path_buf(), path.clone());
        Ok(PrivateRepo { path, runner })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn runner(&self) -> &GitRunner {
        &self.runner
    }

    /// Reject (never repair) anything that could alter ancestry or object lookup: grafts,
    /// shallow files, alternates, commit-graphs, a multi-pack-index, an unknown format version
    /// or an extension outside the allow-list. Reporting only, so the evidence survives.
    pub fn inspect_forbidden(&self) -> Result<(), BrokerError> {
        for rel in FORBIDDEN_REPO_PATHS {
            if fs::symlink_metadata(self.path.join(rel)).is_ok() {
                return Err(unprovable(format!("forbidden path present: {rel}")));
            }
        }
        self.inspect_config()
    }

    fn inspect_config(&self) -> Result<(), BrokerError> {
        let timeout = Duration::from_secs(DEFAULT_GIT_TIMEOUT_SECS);
        let out = self
            .runner
            .run(&["config", "--local", "--list", "-z"], timeout)
            .map_err(|e| storage(format!("{e:?}")))?;
        if out.code != 0 {
            // git itself refuses the repo (e.g. an unsupported format version).
            return Err(unprovable("repository config is not readable by git"));
        }
        for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let text = String::from_utf8_lossy(entry);
            let (key, value) = text.split_once('\n').unwrap_or((&text, ""));
            if key == "core.repositoryformatversion" && !matches!(value, "0" | "1") {
                return Err(unprovable(format!("repositoryformatversion {value}")));
            }
            if let Some(ext) = key.strip_prefix("extensions.")
                && !ALLOWED_REPO_EXTENSIONS.contains(&ext)
            {
                return Err(unprovable(format!("unknown extension: {ext}")));
            }
        }
        Ok(())
    }

    /// SHA-256 over the sorted `(kind, relative path, bytes)` of every control path, recursively.
    /// Object and pack data is deliberately outside it. A symlink anywhere inside is an error.
    pub fn control_digest(&self) -> Result<String, BrokerError> {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for rel in CONTROL_PATHS {
            collect(&self.path, Path::new(rel), &mut files)?;
        }
        files.sort();
        let mut h = Sha256::new();
        for (rel, bytes) in &files {
            h.update((rel.len() as u64).to_be_bytes());
            h.update(rel.as_bytes());
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
        }
        Ok(hex::encode(h.finalize()))
    }

    /// Make every control path read-only and return the digest of what was frozen.
    pub fn freeze(&self) -> Result<String, BrokerError> {
        let before = self.control_digest()?;
        for rel in CONTROL_PATHS {
            set_writable(&self.path.join(rel), false)?;
        }
        let after = self.control_digest()?;
        if before != after {
            return Err(unprovable("control files changed while freezing"));
        }
        Ok(after)
    }

    /// Remove the repo, including read-only control paths left behind by `freeze`.
    pub fn destroy(self) {
        let _ = set_writable(&self.path, true);
        for rel in CONTROL_PATHS {
            let _ = set_writable(&self.path.join(rel), true);
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Directories contribute an entry (`d:<path>`) so adding or removing an empty one changes the
/// digest; files contribute `f:<path>` and their bytes.
fn collect(base: &Path, rel: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), BrokerError> {
    let full = base.join(rel);
    let meta = match fs::symlink_metadata(&full) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(storage(e)),
    };
    let key = rel.to_string_lossy();
    if meta.file_type().is_symlink() {
        return Err(unprovable(format!("symlink in control path: {key}")));
    }
    if meta.is_dir() {
        out.push((format!("d:{key}"), Vec::new()));
        for child in fs::read_dir(&full).map_err(storage)? {
            let child = child.map_err(storage)?;
            collect(base, &rel.join(child.file_name()), out)?;
        }
    } else {
        out.push((format!("f:{key}"), fs::read(&full).map_err(storage)?));
    }
    Ok(())
}

/// chmod the tree rooted at `p` without following symlinks. Directories keep their execute
/// bit so a frozen tree stays traversable (and so `destroy` can walk it).
fn set_writable(p: &Path, writable: bool) -> Result<(), BrokerError> {
    let meta = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(storage(e)),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let mode = meta.permissions().mode();
    let new = if writable {
        mode | 0o200
    } else {
        mode & !0o222
    };
    if meta.is_dir() && writable {
        fs::set_permissions(p, fs::Permissions::from_mode(new)).map_err(storage)?;
    }
    if meta.is_dir() {
        for child in fs::read_dir(p).map_err(storage)? {
            set_writable(&child.map_err(storage)?.path(), writable)?;
        }
    }
    if !(meta.is_dir() && writable) {
        fs::set_permissions(p, fs::Permissions::from_mode(new)).map_err(storage)?;
    }
    Ok(())
}
