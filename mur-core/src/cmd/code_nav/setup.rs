//! Task 3.6 (impure half): `mur code-nav setup`. Detect, plan, print, ask
//! once, apply, record.
//!
//! Consent follows `mur browser setup` (`cmd::consent`): a literal `yes` on a
//! terminal, or `--yes`. `--yes` confirms the printed plan; flags alone
//! decide its scope. No terminal and no `--yes` refuses before anything is
//! installed, written or granted. A re-run whose plan adds nothing over the
//! manifest asks nothing and only re-verifies.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_agent_runtime::mcp::serena::serena_paths;
use serde_yaml_ng::Value;

use super::consent::{self, Manifest};
use super::plan::{self, Detected, Flags, Lang, LspStatus, Permission, UV};
use super::{ast_grep_install, serena_config, serena_entry, serena_install, serena_project};
use crate::cmd::browser::setup::Consent;

pub struct Args {
    pub agent: String,
    pub project: Option<PathBuf>,
    pub flags: Flags,
}

/// `PATH` lookup without a `which` dependency.
fn on_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(format!("{tool}{}", std::env::consts::EXE_SUFFIX)))
        .find(|c| c.is_file())
}

fn detect() -> Detected {
    let mut tools: BTreeSet<&str> = Lang::ALL
        .iter()
        .flat_map(|l| l.prerequisites().iter().copied())
        .collect();
    tools.insert(UV);
    Detected {
        on_path: tools
            .into_iter()
            .filter(|t| on_path(t).is_some())
            .map(str::to_owned)
            .collect(),
    }
}

/// serena's entry point is a script whose shebang is the venv interpreter,
/// a symlink to the Python uv chose. The seal must exec both (Layer B
/// granted the same two lanes). Returns the directories to grant.
pub fn interpreter_lanes(serena_bin: &Path) -> Result<Vec<PathBuf>> {
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
    Ok(lanes)
}

pub fn run(
    args: Args,
    consent: Consent,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let agent = crate::a2a_dial::canonicalize_agent_name(&mur_home, &args.agent);
    let agent_home = mur_home.join("agents").join(&agent);
    if !agent_home.join("profile.yaml").is_file() {
        bail!("agent '{agent}' not found");
    }
    let project = match (&args.project, args.flags.with_serena) {
        (Some(p), true) => Some(serena_entry::checked_project(p)?),
        (None, true) => bail!("--with-serena needs --project <dir>: the repo serena will serve"),
        (Some(_), false) => bail!("--project only applies with --with-serena"),
        (None, false) => None,
    };

    let plan = plan::plan(&mur_home, &args.flags, &detect())?;
    let mut permissions = plan.permissions.clone();
    if let Some(p) = &project
        && plan
            .install
            .iter()
            .any(|r| r.name == plan::SERENA && r.missing.is_none())
    {
        permissions.push(Permission::Read(p.clone()));
    }
    let plan = plan::Plan {
        permissions,
        ..plan
    };

    writeln!(out, "mur code-nav setup for agent '{agent}'\n")?;
    consent::print_tables(out, &plan)?;
    if let Some(p) = &project {
        writeln!(out, "\nserena project: {}", p.display())?;
        writeln!(
            out,
            "    also granted after install: exec of serena's own Python (its venv bin dir and \
             the interpreter uv chose)"
        )?;
    }

    let manifest_path = consent::manifest_path(&mur_home, &agent);
    let prior = consent::load_manifest(&manifest_path)?;
    let first_run = prior == Manifest::default();
    let diff = consent::diff(&plan, project.as_deref(), &prior);
    consent::print_diff(out, &diff, first_run)?;

    if diff.is_empty() {
        writeln!(
            out,
            "\nNothing new to consent to; re-verifying what is installed."
        )?;
    } else {
        match consent {
            Consent::Impossible => bail!(
                "`mur code-nav setup` needs a terminal to ask for consent. Pass --yes to \
                 confirm the plan above (flags decide what it contains)"
            ),
            Consent::Ask if !crate::cmd::consent::literal_yes(input, out)? => {
                writeln!(
                    out,
                    "  skipped — nothing was installed, written or granted."
                )?;
                bail!("code-nav setup not applied");
            }
            _ => {}
        }
    }

    let manifest = apply(
        &mur_home,
        &agent,
        &agent_home,
        &plan,
        project.as_deref(),
        prior,
        out,
    )?;
    consent::save_manifest(&manifest_path, &manifest)?;
    writeln!(out, "\nRecorded in {}", manifest_path.display())?;
    Ok(())
}

fn apply(
    mur_home: &Path,
    agent: &str,
    agent_home: &Path,
    plan: &plan::Plan,
    project: Option<&Path>,
    prior: Manifest,
    out: &mut dyn Write,
) -> Result<Manifest> {
    let mut m = prior;
    m.disabled = plan.disabled_lines();
    for row in plan.install.iter().filter(|r| r.missing.is_none()) {
        if row.name == plan::AST_GREP {
            let dest = mur_common::config::ast_grep_binary_path(mur_home);
            let o = ast_grep_install::install(&dest)?;
            writeln!(
                out,
                "  ✓ ast-grep {} ({o:?}) at {}",
                row.version,
                dest.display()
            )?;
            m.installed
                .insert(consent::install_key(row.name, row.version));
        }
    }
    let Some(project) = project else {
        return Ok(m);
    };
    if plan
        .install
        .iter()
        .any(|r| r.name == plan::SERENA && r.missing.is_some())
    {
        writeln!(out, "  ! serena not set up: {UV} not found on PATH")?;
        return Ok(m);
    }

    let uv = on_path(UV).with_context(|| format!("{UV} disappeared from PATH"))?;
    let dir = serena_install::serena_dir(mur_home);
    let (o, record) = serena_install::install_with(&uv, &dir)?;
    writeln!(
        out,
        "  ✓ serena {} ({o:?}) at {}",
        record.version,
        record.bin.display()
    )?;
    m.installed
        .insert(consent::install_key(plan::SERENA, &record.version));

    // Union with earlier runs: setup never revokes, so a language the user
    // consented to before stays in serena's list until they remove it.
    let langs: Vec<Lang> = Lang::ALL
        .into_iter()
        .filter(|l| {
            m.languages.contains(l.flag())
                || plan
                    .lsp
                    .iter()
                    .any(|r| r.lang == *l && r.status == LspStatus::Enabled)
        })
        .collect();
    let paths = serena_paths(agent_home);
    let ptpl = serena_project::read_pinned_project_template(&dir)?;
    let pfile = serena_project::write_project(&paths, project, &ptpl, &langs)?;
    writeln!(out, "  ✓ wrote {}", pfile.display())?;
    let tpl = serena_config::read_pinned_template(&dir)?;
    let secret =
        existing_auth_secret(&paths.config_file).unwrap_or_else(serena_config::new_auth_secret);
    let cfg = serena_config::write_config(&paths, project, &tpl, &secret)?;
    writeln!(out, "  ✓ wrote {} (preflight passed)", cfg.display())?;

    let (ppath, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let change = serena_entry::upsert(&mut profile, serena_entry::build(&record, project)?)?;
    crate::cmd::agent::save_profile(&ppath, &mut profile)?;
    writeln!(out, "  ✓ profile entry `serena`: {change:?}")?;

    for p in &plan.permissions {
        match p {
            Permission::Spawn(b) => crate::cmd::agent::cmd_perm_allow_spawn(agent, b)?,
            Permission::Read(d) => crate::cmd::agent::cmd_perm_allow_read(agent, path_str(d)?)?,
        }
        m.granted.insert(consent::grant_key(p));
    }
    for lane in interpreter_lanes(&record.bin)? {
        crate::cmd::agent::cmd_perm_allow_spawn_dir(agent, path_str(&lane)?)?;
        m.granted.insert(format!("spawn-dir {}", lane.display()));
    }
    writeln!(out, "  ✓ permissions granted")?;

    m.languages
        .extend(langs.iter().map(|l| l.flag().to_owned()));
    m.project = Some(project.to_path_buf());
    m.serena = Some(record);
    Ok(m)
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path {} is not UTF-8", p.display()))
}

/// Keep serena's `auth_secret` across re-runs so a re-run that changes
/// nothing writes a byte-identical config.
fn existing_auth_secret(config: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config).ok()?;
    let doc: Value = serde_yaml_ng::from_str(&text).ok()?;
    doc.get("auth_secret")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
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
        let lanes = interpreter_lanes(&bin).unwrap();
        assert_eq!(lanes[0], venv_bin);
        assert_eq!(lanes[1], std::fs::canonicalize(&real_dir).unwrap());
    }

    #[test]
    fn no_shebang_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("serena");
        std::fs::write(&bin, b"\x7fELF").unwrap();
        assert!(interpreter_lanes(&bin).is_err());
    }

    #[test]
    fn auth_secret_is_reused_only_when_set() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("c.yml");
        std::fs::write(&f, "auth_secret: abc\n").unwrap();
        assert_eq!(existing_auth_secret(&f).as_deref(), Some("abc"));
        std::fs::write(&f, "auth_secret:\n").unwrap();
        assert_eq!(existing_auth_secret(&f), None);
        assert_eq!(existing_auth_secret(&tmp.path().join("none")), None);
    }
}
