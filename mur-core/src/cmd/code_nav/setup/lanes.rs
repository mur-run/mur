//! serena's exec lanes: which interpreter dirs its entry points need, and
//! the second consent ask for any the manifest has not recorded (#1942).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::super::consent::{self, Manifest};
use crate::cmd::browser::setup::Consent;

/// serena's entry point is a script whose shebang is the venv interpreter,
/// a symlink to the Python uv chose. The seal must exec both (Layer B
/// granted the same two lanes). Returns the directories to grant; each must
/// canonicalize under `tools_root` (`<mur_home>/tools`), so a venv whose
/// Python lives elsewhere — or a symlink leading out — is refused, not
/// granted (plan 3.6c).
pub fn interpreter_lanes(serena_bin: &Path, tools_root: &Path) -> Result<Vec<PathBuf>> {
    let text =
        std::fs::read(serena_bin).with_context(|| format!("read {}", serena_bin.display()))?;
    let first = text.split(|b| *b == b'\n').next().unwrap_or_default();
    let shebang = std::str::from_utf8(first)
        .ok()
        .and_then(|l| l.strip_prefix("#!"))
        .map(str::trim)
        .with_context(|| format!("{} has no shebang", serena_bin.display()))?;
    let python = PathBuf::from(shebang);
    let venv_bin = python
        .parent()
        .context("shebang has no parent")?
        .to_path_buf();
    let real = std::fs::canonicalize(&python)
        .with_context(|| format!("resolve serena's interpreter {}", python.display()))?;
    let mut lanes = vec![venv_bin];
    if let Some(d) = real.parent()
        && !lanes.iter().any(|l| l == d)
    {
        lanes.push(d.to_path_buf());
    }
    let root = std::fs::canonicalize(tools_root)
        .with_context(|| format!("resolve {}", tools_root.display()))?;
    for lane in &lanes {
        let canon = std::fs::canonicalize(lane)
            .with_context(|| format!("resolve exec lane {}", lane.display()))?;
        if !canon.starts_with(&root) {
            bail!(
                "exec lane {} (for {}) resolves outside {}; refusing to grant it. \
                 Re-run setup to reinstall with a MUR-managed Python",
                canon.display(),
                serena_bin.display(),
                root.display()
            );
        }
    }
    Ok(lanes)
}

/// How `apply` asks about what the plan could not list.
pub(super) struct Ask<'a> {
    pub(super) consent: Consent,
    pub(super) input: &'a mut dyn BufRead,
}

/// #1942: exec lanes are known only after install, so the plan diff never
/// saw them. Any lane the manifest has not recorded is shown by its path
/// and consented to under the same rules as the plan, before it is granted.
pub(super) fn consent_to_lanes(
    lanes: &[PathBuf],
    m: &Manifest,
    consent: Consent,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    let new = consent::new_lanes(lanes, m);
    if new.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nExec lanes to grant (resolved after install):")?;
    for l in &new {
        writeln!(out, "    {}", consent::lane_grant_key(l))?;
    }
    let listed = || {
        new.iter()
            .map(|l| l.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match consent {
        Consent::Given => Ok(()),
        Consent::Impossible => bail!(
            "new exec lanes need consent and there is no terminal to ask: {}. \
             Pass --yes to confirm them; nothing was granted",
            listed()
        ),
        Consent::Ask if crate::cmd::consent::literal_yes(input, out)? => Ok(()),
        Consent::Ask => {
            writeln!(
                out,
                "  skipped — no exec lane or profile entry was granted."
            )?;
            bail!(
                "code-nav setup not applied: exec lanes not approved ({})",
                listed()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A shebang whose target is a venv symlink is a unix layout; a Windows
    // venv has `Scripts\serena.exe` and no shebang to follow.
    #[cfg(unix)]
    #[test]
    fn lanes_are_venv_bin_and_real_interpreter_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("pythons/3.12/bin");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("python3.12"), b"").unwrap();
        let venv_bin = tmp.path().join("venv/bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::os::unix::fs::symlink(real_dir.join("python3.12"), venv_bin.join("python")).unwrap();
        let bin = venv_bin.join("serena");
        std::fs::write(
            &bin,
            format!("#!{}\nimport x\n", venv_bin.join("python").display()),
        )
        .unwrap();
        let lanes = interpreter_lanes(&bin, tmp.path()).unwrap();
        assert_eq!(lanes[0], venv_bin);
        assert_eq!(lanes[1], std::fs::canonicalize(&real_dir).unwrap());
    }

    /// The pre-3.6c layout: venv under tools, its Python in uv's shared dir.
    #[cfg(unix)]
    #[test]
    fn interpreter_outside_tools_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let foreign = tmp.path().join("uv/python/cpython-3.13.2/bin");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("python3.13"), b"").unwrap();
        let venv_bin = tools.join("serena/v/venv/bin");
        std::fs::create_dir_all(&venv_bin).unwrap();
        std::os::unix::fs::symlink(foreign.join("python3.13"), venv_bin.join("python")).unwrap();
        let bin = venv_bin.join("serena");
        std::fs::write(&bin, format!("#!{}\n", venv_bin.join("python").display())).unwrap();
        let e = interpreter_lanes(&bin, &tools).unwrap_err();
        assert!(e.to_string().contains("outside"), "{e:#}");
    }

    /// A lane dir that is itself a symlink out of tools is refused too.
    #[cfg(unix)]
    #[test]
    fn venv_bin_symlinked_out_of_tools_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let outside = tmp.path().join("elsewhere/bin");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("python"), b"").unwrap();
        std::fs::create_dir_all(tools.join("serena/v")).unwrap();
        let venv_bin = tools.join("serena/v/bin");
        std::os::unix::fs::symlink(&outside, &venv_bin).unwrap();
        let bin = outside.join("serena");
        std::fs::write(&bin, format!("#!{}\n", venv_bin.join("python").display())).unwrap();
        assert!(interpreter_lanes(&bin, &tools).is_err());
    }

    #[test]
    fn no_shebang_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("serena");
        std::fs::write(&bin, b"\x7fELF").unwrap();
        assert!(interpreter_lanes(&bin, tmp.path()).is_err());
    }
}
