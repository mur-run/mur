//! Task 3.4: generate `<agent>/serena/serena_config.yml` (#1688).
//!
//! serena re-saves the whole file as a "migration" when any field it maps is
//! absent, and its loader raises when `projects` is missing. So the file
//! starts from serena's own template, which lists every mapped field (checked
//! against `SerenaConfig` at the pinned commit), with the keys MUR owns
//! replaced by MUR's values. `auth_secret` is filled too: serena generates
//! and re-saves one when it is empty.
//!
//! The template is read from the pinned install (task 3.3), not vendored:
//! serena is GPL-3.0-or-later. Its sha256 is pinned, so a template whose
//! layout this generator was not written against is refused, never guessed.
//!
//! The written file must pass the runtime's own preflight (C1–C9), which is
//! called here on the result.

use anyhow::{Context, Result, bail};
use mur_agent_runtime::mcp::serena::{SERENA_TOOL_ALLOWLIST, SerenaPaths, preflight};
use serde_yaml_ng::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// sha256 of `serena/resources/serena_config.template.yml` at
/// `serena_install::SERENA_GIT_REV`. Bump with the pin.
pub const SERENA_CONFIG_TEMPLATE_SHA256: &str =
    "8dcb555c1e422db974dc206840442ab1eed440293cf2f450ac85e62496959415";
/// Template location inside the serena package.
const TEMPLATE_REL: [&str; 3] = ["serena", "resources", "serena_config.template.yml"];
const TEMPLATE_SEARCH_DEPTH: usize = 8;

/// serena's placeholder for the project directory's name.
const FOLDER_NAME_PLACEHOLDER: &str = "$projectFolderName";
/// serena's language-server id for clangd, and the C8 lock-down.
const CPP_LS_ID: &str = "cpp";
/// serena's language-server id for pyright, and the key that makes serena
/// launch a given binary instead of running uv (3.6b; C7 confines it).
const PYTHON_LS_ID: &str = "python";
const LS_PATH: &str = "ls_path";
const CLANGD_LOCKDOWN: &str = "--enable-config=false";
/// serena's own mode for this file (it holds `auth_secret`).
#[cfg(unix)]
const PRIVATE_MODE: u32 = 0o600;

const HEADER: &str = "\n# --- Managed by MUR (`mur code-nav setup`). The keys below are checked \
                      at every serena spawn;\n# --- an edit that breaks a check refuses startup.\n";

/// Top-level keys MUR sets; every other template key keeps serena's value.
pub const OWNED_KEYS: [&str; 10] = [
    "trusted_project_path_patterns",
    "project_serena_folder_location",
    "fixed_tools",
    "excluded_tools",
    "included_optional_tools",
    "web_dashboard",
    "web_dashboard_open_on_launch",
    "ls_specific_settings",
    "auth_secret",
    "projects",
];

/// Find the template inside a serena install dir from 3.3.
pub fn template_path(install_dir: &Path) -> Option<PathBuf> {
    find_resource(install_dir, &TEMPLATE_REL)
}

/// Read the template and refuse it unless it is the pinned one.
pub fn read_pinned_template(install_dir: &Path) -> Result<String> {
    read_pinned_resource(
        install_dir,
        &TEMPLATE_REL,
        SERENA_CONFIG_TEMPLATE_SHA256,
        "serena config template",
    )
}

/// Find a file shipped in the serena package (`rel` ends at the file).
pub(super) fn find_resource(install_dir: &Path, rel: &[&str]) -> Option<PathBuf> {
    let rel: PathBuf = rel.iter().collect();
    walkdir::WalkDir::new(install_dir)
        .max_depth(TEMPLATE_SEARCH_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .find(|p| p.is_file() && p.ends_with(&rel))
}

/// Read a packaged file and refuse it unless its sha256 is `want`.
pub(super) fn read_pinned_resource(
    install_dir: &Path,
    rel: &[&str],
    want: &str,
    what: &str,
) -> Result<String> {
    let path = find_resource(install_dir, rel)
        .with_context(|| format!("{what} not found under {}", install_dir.display()))?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let got = hex::encode(Sha256::digest(&bytes));
    if got != want {
        bail!(
            "{}: sha256 {got}, expected {want} (the pinned serena's {what}); refusing to \
             generate from an unknown template",
            path.display()
        );
    }
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

/// The MUR folder serena uses for `project_root`:
/// `<projects_dir>/<project folder name>`.
pub fn project_folder(paths: &SerenaPaths, project_root: &Path) -> Result<PathBuf> {
    let name = project_root
        .file_name()
        .with_context(|| format!("project {} has no folder name", project_root.display()))?;
    Ok(paths.projects_dir.join(name))
}

/// The values MUR owns, in `OWNED_KEYS` order. `python_ls` is the
/// pre-installed `pyright-langserver` (3.6b), when Python is enabled.
pub fn owned_values(
    paths: &SerenaPaths,
    project_root: &Path,
    auth_secret: &str,
    python_ls: Option<&Path>,
) -> Result<Mapping> {
    let s = |p: &Path| -> Result<Value> {
        p.to_str()
            .map(|s| Value::String(s.to_owned()))
            .with_context(|| format!("path {} is not UTF-8", p.display()))
    };
    let strings = |xs: &[&str]| Value::Sequence(xs.iter().map(|x| Value::from(*x)).collect());
    let projects_dir = paths
        .projects_dir
        .to_str()
        .with_context(|| format!("path {} is not UTF-8", paths.projects_dir.display()))?;
    // C8 applies whenever serena may auto-detect C/C++, so the clangd
    // lock-down is always written; it is inert for other languages.
    let mut cpp = Mapping::new();
    cpp.insert("ls_extra_args".into(), strings(&[CLANGD_LOCKDOWN]));
    cpp.insert(
        "compile_commands_dir".into(),
        s(&project_folder(paths, project_root)?)?,
    );
    let mut ls = Mapping::new();
    ls.insert(CPP_LS_ID.into(), Value::Mapping(cpp));
    if let Some(bin) = python_ls {
        let mut py = Mapping::new();
        py.insert(LS_PATH.into(), s(bin)?);
        ls.insert(PYTHON_LS_ID.into(), Value::Mapping(py));
    }

    let values: [Value; 10] = [
        Value::Sequence(vec![]),
        Value::String(format!("{projects_dir}/{FOLDER_NAME_PLACEHOLDER}")),
        strings(&SERENA_TOOL_ALLOWLIST),
        Value::Sequence(vec![]),
        Value::Sequence(vec![]),
        Value::Bool(false),
        Value::Bool(false),
        Value::Mapping(ls),
        Value::from(auth_secret),
        Value::Sequence(vec![s(project_root)?]),
    ];
    Ok(OWNED_KEYS
        .iter()
        .map(|k| Value::from(*k))
        .zip(values)
        .collect())
}

/// Template with every owned top-level key (and its block) removed, then
/// MUR's values appended. Fails if the template lacks an owned key, since
/// that means it is not the layout this was written against.
pub fn render(template: &str, owned: &Mapping) -> Result<String> {
    render_owned(
        template,
        &OWNED_KEYS,
        owned,
        HEADER,
        "serena config template",
    )
}

/// [`render`] over any key set: drop each of `keys` (with its block) from
/// `template`, then append `header` and `owned`.
pub(super) fn render_owned(
    template: &str,
    keys: &[&str],
    owned: &Mapping,
    header: &str,
    what: &str,
) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut seen: Vec<&str> = Vec::new();
    let mut skipping = false;
    // Blank lines and column-0 comments met while skipping: inside the
    // dropped block if more of it follows, else they lead the next key.
    let mut held = String::new();
    for line in template.split_inclusive('\n') {
        if skipping && is_interstitial(line) {
            held.push_str(line);
            continue;
        }
        if skipping && is_block_continuation(line) {
            held.clear();
            continue;
        }
        skipping = false;
        out.push_str(&std::mem::take(&mut held));
        if let Some(key) = keys
            .iter()
            .find(|k| line.strip_prefix(**k).is_some_and(|r| r.starts_with(':')))
        {
            seen.push(key);
            skipping = true;
            continue;
        }
        out.push_str(line);
    }
    out.push_str(&held);
    if let Some(missing) = keys.iter().find(|k| !seen.contains(k)) {
        bail!("{what} has no top-level `{missing}`; refusing to generate");
    }
    if let Some(dup) = seen
        .iter()
        .enumerate()
        .find(|(i, k)| seen[..*i].contains(k))
    {
        bail!("{what} sets `{}` twice", dup.1);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(header);
    out.push_str(&serde_yaml_ng::to_string(owned).context("serialize MUR-owned keys")?);
    Ok(out)
}

/// A line that belongs to the previous top-level key's block value:
/// indented, or a column-0 sequence item (`key:\n- x` is valid YAML).
fn is_block_continuation(line: &str) -> bool {
    line.starts_with([' ', '\t']) || line.starts_with("- ") || line.trim_end() == "-"
}

/// A line that neither continues nor ends a block: blank, or a column-0
/// comment.
fn is_interstitial(line: &str) -> bool {
    line.trim().is_empty() || line.starts_with('#')
}

/// A fresh value for `auth_secret` (serena's own format: a v4 UUID).
pub fn new_auth_secret() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Write the config for `project_root` and run the runtime preflight on it
/// (whose C7 confines `python_ls` under the MUR tools root).
pub fn write_config(
    paths: &SerenaPaths,
    project_root: &Path,
    template: &str,
    auth_secret: &str,
    python_ls: Option<&Path>,
) -> Result<PathBuf> {
    let folder = project_folder(paths, project_root)?;
    std::fs::create_dir_all(&folder).with_context(|| format!("create {}", folder.display()))?;
    let owned = owned_values(paths, project_root, auth_secret, python_ls)?;
    let text = render(template, &owned)?;
    write_private(&paths.config_file, text.as_bytes())?;
    preflight(paths, project_root).with_context(|| {
        format!(
            "generated {} does not pass the serena preflight",
            paths.config_file.display()
        )
    })?;
    Ok(paths.config_file.clone())
}

/// Temp + rename, with the temp file created owner-only so the secret is
/// never readable by others, even briefly. serena itself chmods to 0600.
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("yml.tmp");
    // The dir is one the sandboxed serena child can write. A leftover tmp
    // (or a planted symlink) is removed, never opened: `create_new` refuses
    // to follow a link and guarantees `mode` applies at creation, so the
    // secret is never readable by others even for an instant.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("remove stale {}", tmp.display())),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, PRIVATE_MODE);
    let mut f = opts
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(PRIVATE_MODE))
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))
}

#[cfg(test)]
#[path = "serena_config_tests.rs"]
mod tests;
