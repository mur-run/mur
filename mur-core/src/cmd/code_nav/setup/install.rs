//! Phase 2 of setup's apply (#2016): download and install, wire ast-grep,
//! write serena's files, and resolve the exec lanes. Grants nothing for
//! serena; that is [`commit`](super::apply::commit). The one manifest save
//! here records the ast-grep wiring as soon as it reaches the profile (#2018).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_agent_runtime::mcp::serena::serena_paths_for;
use serde_yaml_ng::Value;

use super::super::consent::{self, Manifest};
use super::super::plan::{self, Lang, UV};
use super::super::{
    ast_grep_install, ast_grep_wire, node_ls_install, pyright_install, serena_config,
    serena_install, serena_project, vue_install,
};
use super::apply::{Prepared, SerenaPlan, SerenaStep, Target};
use super::lanes::interpreter_lanes;
use super::on_path;

/// What [`install`] did, for the lane question and [`commit`](super::apply::commit).
pub(super) struct Installed<'p> {
    /// The prior manifest plus this run's installs and ast-grep grants.
    pub(super) manifest: Manifest,
    /// `None` when serena was not set up (no project, or no uv).
    pub(super) serena: Option<SerenaInstalled<'p>>,
}

pub(super) struct SerenaInstalled<'p> {
    pub(super) project: &'p Path,
    pub(super) record: serena_install::Record,
    pub(super) pyright: Option<pyright_install::Record>,
    pub(super) intelephense: Option<node_ls_install::Record>,
    pub(super) typescript: Option<node_ls_install::Record>,
    pub(super) vue: Option<node_ls_install::Record>,
    pub(super) langs: Vec<Lang>,
    /// The interpreter dirs serena's (and pyright's) entry points need,
    /// deduplicated; not yet consented to or granted.
    pub(super) lanes: Vec<PathBuf>,
}

impl Installed<'_> {
    /// The exec lanes to ask about before commit; empty without serena.
    pub(super) fn lanes(&self) -> &[PathBuf] {
        self.serena.as_ref().map_or(&[], |s| &s.lanes)
    }
}

/// Install everything `prepared` lists. `install_ast_grep` is
/// [`ast_grep_install::install`] outside tests.
pub(super) fn install_with<'p>(
    t: &Target<'_>,
    prepared: Prepared<'p>,
    out: &mut dyn Write,
    install_ast_grep: &dyn Fn(&Path) -> Result<ast_grep_install::Outcome>,
) -> Result<Installed<'p>> {
    let Target {
        mur_home,
        agent,
        manifest_path,
        ..
    } = *t;
    let Prepared {
        manifest: mut m,
        ast_grep,
        serena,
    } = prepared;
    if let Some(version) = ast_grep {
        let dest = mur_common::config::ast_grep_binary_path(mur_home);
        let o = install_ast_grep(&dest)?;
        writeln!(
            out,
            "  ✓ ast-grep {} ({o:?}) at {}",
            version,
            dest.display()
        )?;
        m.installed
            .insert(consent::install_key(plan::AST_GREP, version));
        ast_grep_wire::apply(mur_home, agent, &mut m, out)?;
        // #2018: `ast_grep_wire::apply` saved the profile before recording
        // its grants in `m`, so the profile now holds them. Record them
        // durably too: a later failure must not leave the agent holding
        // grants the manifest does not know about (the next run would ask
        // for them again). Never before the profile save.
        consent::save_manifest(manifest_path, &m)?;
    }
    let s = match serena {
        SerenaStep::Off => None,
        SerenaStep::NoUv => {
            writeln!(out, "  ! serena not set up: {UV} not found on PATH")?;
            None
        }
        SerenaStep::Install(s) => Some(install_serena(t, s, &mut m, out)?),
    };
    Ok(Installed {
        manifest: m,
        serena: s,
    })
}

fn install_serena<'p>(
    t: &Target<'_>,
    s: SerenaPlan<'p>,
    m: &mut Manifest,
    out: &mut dyn Write,
) -> Result<SerenaInstalled<'p>> {
    let Target {
        mur_home,
        agent_home,
        entry,
        ..
    } = *t;
    let project = s.project;
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

    // 3.6b: pyright is installed only when Python is enabled in this plan.
    // An earlier run's install stays recorded (setup never revokes) and its
    // `ls_path` is kept, so a re-run without Python does not fall back to uvx.
    let pyright = if s.pyright {
        let (o, r) = pyright_install::install_with(&uv, &pyright_install::pyright_dir(mur_home))?;
        writeln!(
            out,
            "  ✓ pyright {} ({o:?}) at {}",
            r.version,
            r.bin.display()
        )?;
        m.installed
            .insert(consent::install_key(plan::PYRIGHT, &r.version));
        Some(r)
    } else {
        m.pyright.clone()
    };

    // #1945 / #1981: each managed node server (intelephense, TypeScript) is
    // pre-installed only when its language is enabled; serena then gets the
    // canonical `ls_base_cmd`, so it never npm-installs in the sandbox. An
    // earlier install stays, like pyright's, so a re-run without that
    // language keeps the same launch command.
    let prior = m.intelephense.clone();
    let intelephense = install_node_ls(
        &mur_common::serena_ls_cmd::INTELEPHENSE,
        s.intelephense,
        prior,
        mur_home,
        m,
        out,
    )?;
    let prior = m.typescript.clone();
    let typescript = install_node_ls(
        &mur_common::serena_ls_cmd::TYPESCRIPT,
        s.typescript,
        prior,
        mur_home,
        m,
        out,
    )?;
    // #1982: Vue is installed like the others but handed to serena by
    // linking its resources dir; it has no `ls_base_cmd`, so it is not in
    // `node_ls`. Its companion TypeScript server is (the plan implies it).
    let prior = m.vue.clone();
    let vue = install_node_ls(
        &mur_common::serena_ls_cmd::VUE,
        s.vue,
        prior,
        mur_home,
        m,
        out,
    )?;
    let node_ls: Vec<_> = [
        (&intelephense, mur_common::serena_ls_cmd::INTELEPHENSE),
        (&typescript, mur_common::serena_ls_cmd::TYPESCRIPT),
    ]
    .into_iter()
    .filter_map(|(r, ls)| r.as_ref().map(|_| ls))
    .collect();

    let langs = s.langs;
    let paths = serena_paths_for(agent_home, entry)?;
    if vue.is_some() {
        let tools = node_ls_install::tools_dir(mur_home);
        vue_install::write_marker(&tools)?;
        let l = vue_install::link(&paths.home, &tools)?;
        writeln!(
            out,
            "  ✓ serena's Vue server dir ({l:?}): {}",
            vue_install::resources_link(&paths.home).display()
        )?;
    }
    let ptpl = serena_project::read_pinned_project_template(&dir)?;
    let pfile = serena_project::write_project(&paths, project, &ptpl, &langs)?;
    writeln!(out, "  ✓ wrote {}", pfile.display())?;
    let tpl = serena_config::read_pinned_template(&dir)?;
    let secret =
        existing_auth_secret(&paths.config_file).unwrap_or_else(serena_config::new_auth_secret);
    let managed = serena_config::ManagedLs {
        python: pyright.as_ref().map(|r| r.bin.as_path()),
        node: &node_ls,
        vue: vue.is_some(),
    };
    let cfg = serena_config::write_config(&paths, project, &tpl, &secret, managed)?;
    writeln!(out, "  ✓ wrote {} (preflight passed)", cfg.display())?;

    // Both entry points are scripts run by a venv interpreter uv chose;
    // the seal must exec each one (requirement 3). Resolved before anything
    // is written, so a lane outside the tools dir refuses with no half-applied
    // profile or grants. A grant that fails *after* `save_profile` does leave
    // the serena entry with partial grants; re-running setup completes them
    // (the manifest records only what succeeded).
    let entry_points = std::iter::once(&record.bin).chain(pyright.as_ref().map(|r| &r.bin));
    let tools_root = mur_home.join(mur_common::config::MUR_TOOLS_DIR);
    let mut lanes: Vec<PathBuf> = Vec::new();
    for bin in entry_points {
        for lane in interpreter_lanes(bin, &tools_root)? {
            if !lanes.contains(&lane) {
                lanes.push(lane);
            }
        }
    }
    Ok(SerenaInstalled {
        project,
        record,
        pyright,
        intelephense,
        typescript,
        vue,
        langs,
        lanes,
    })
}

/// Install `ls` when `wanted` (the plan lists it); otherwise keep `prior`.
fn install_node_ls(
    ls: &mur_common::serena_ls_cmd::ManagedNodeLs,
    wanted: bool,
    prior: Option<node_ls_install::Record>,
    mur_home: &Path,
    m: &mut Manifest,
    out: &mut dyn Write,
) -> Result<Option<node_ls_install::Record>> {
    if !wanted {
        return Ok(prior);
    }
    let npm = on_path(plan::NPM).with_context(|| format!("{} not found on PATH", plan::NPM))?;
    let (o, r) = node_ls_install::install_with(ls, &npm, &node_ls_install::tools_dir(mur_home))?;
    writeln!(
        out,
        "  ✓ {} {} ({o:?}) at {}",
        ls.tool_dir,
        r.version,
        r.entry.display()
    )?;
    m.installed
        .insert(consent::install_key(ls.tool_dir, &r.version));
    Ok(Some(r))
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
