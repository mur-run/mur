//! The apply step of `mur code-nav setup`: install, write serena's files,
//! save the profile entry, grant, and return the manifest to record.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_agent_runtime::mcp::serena::serena_paths_for;
use serde_yaml_ng::Value;

use super::super::consent::{self, Manifest};
use super::super::plan::{self, Lang, LspStatus, Permission, UV};
use super::super::{
    ast_grep_install, ast_grep_wire, node_ls_install, pyright_install, serena_config, serena_entry,
    serena_install, serena_project, vue_install,
};
use super::lanes::{Ask, consent_to_lanes, interpreter_lanes};
use super::on_path;

/// Whose profile and which serena entry a run applies to.
pub(super) struct Target<'a> {
    pub(super) mur_home: &'a Path,
    pub(super) agent: &'a str,
    pub(super) agent_home: &'a Path,
    pub(super) entry: &'a str,
}

pub(super) fn apply(
    t: &Target<'_>,
    plan: &plan::Plan,
    project: Option<&Path>,
    prior: Manifest,
    ask: &mut Ask<'_>,
    out: &mut dyn Write,
) -> Result<Manifest> {
    let Target {
        mur_home,
        agent,
        agent_home,
        entry,
    } = *t;
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
            ast_grep_wire::apply(mur_home, agent, &mut m, out)?;
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

    // 3.6b: pyright is installed only when Python is enabled in this plan.
    // An earlier run's install stays recorded (setup never revokes) and its
    // `ls_path` is kept, so a re-run without Python does not fall back to uvx.
    let pyright = if plan.install.iter().any(|r| r.name == plan::PYRIGHT) {
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
    let intelephense = install_node_ls(
        &mur_common::serena_ls_cmd::INTELEPHENSE,
        m.intelephense.clone(),
        plan,
        mur_home,
        &mut m,
        out,
    )?;
    let typescript = install_node_ls(
        &mur_common::serena_ls_cmd::TYPESCRIPT,
        m.typescript.clone(),
        plan,
        mur_home,
        &mut m,
        out,
    )?;
    // #1982: Vue is installed like the others but handed to serena by
    // linking its resources dir; it has no `ls_base_cmd`, so it is not in
    // `node_ls`. Its companion TypeScript server is (the plan implies it).
    let vue = install_node_ls(
        &mur_common::serena_ls_cmd::VUE,
        m.vue.clone(),
        plan,
        mur_home,
        &mut m,
        out,
    )?;
    let node_ls: Vec<_> = [
        (&intelephense, mur_common::serena_ls_cmd::INTELEPHENSE),
        (&typescript, mur_common::serena_ls_cmd::TYPESCRIPT),
    ]
    .into_iter()
    .filter_map(|(r, ls)| r.as_ref().map(|_| ls))
    .collect();

    let langs = entry_langs(&m, entry, plan);
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
    consent_to_lanes(&lanes, &m, ask.consent, ask.input, out)?;
    let (ppath, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let change = serena_entry::upsert(
        &mut profile,
        serena_entry::build_named(&record, entry, project)?,
    )?;
    crate::cmd::agent::save_profile(&ppath, &mut profile)?;
    writeln!(out, "  ✓ profile entry `{entry}`: {change:?}")?;

    // The quiet `grant_*` forms: the consent table above already named each
    // grant, and the running-agent warning is printed once, below, instead
    // of once per grant.
    for p in &plan.permissions {
        match p {
            Permission::Spawn(b) => crate::cmd::agent::grant_spawn(agent, b)?,
            Permission::Read(d) => crate::cmd::agent::grant_read(agent, path_str(d)?)?,
            Permission::SpawnDir(d) => crate::cmd::agent::grant_spawn_dir(agent, path_str(d)?)?,
            // Applied by `ast_grep_wire` right after the ast-grep install.
            Permission::ReadWrite(_) | Permission::Mcp { .. } => continue,
        }
        m.granted.insert(consent::grant_key(p));
    }
    for lane in &lanes {
        crate::cmd::agent::grant_spawn_dir(agent, path_str(lane)?)?;
        m.granted.insert(consent::lane_grant_key(lane));
        writeln!(out, "  ✓ exec lane {}", lane.display())?;
    }
    writeln!(out, "  ✓ permissions granted")?;
    crate::cmd::agent::warn_if_running(agent);

    m.set_languages(entry, langs.iter().map(|l| l.flag().to_owned()));
    m.set_project(entry, project);
    m.serena = Some(record);
    m.pyright = pyright;
    m.intelephense = intelephense;
    m.typescript = typescript;
    m.vue = vue;
    Ok(m)
}

/// Install `ls` when the plan lists it; otherwise keep `prior`.
fn install_node_ls(
    ls: &mur_common::serena_ls_cmd::ManagedNodeLs,
    prior: Option<node_ls_install::Record>,
    plan: &plan::Plan,
    mur_home: &Path,
    m: &mut Manifest,
    out: &mut dyn Write,
) -> Result<Option<node_ls_install::Record>> {
    if !plan.install.iter().any(|r| r.name == ls.tool_dir) {
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

/// The languages `entry`'s `project.yml` lists: what this plan enables,
/// unioned with what this entry listed before (setup never revokes), or,
/// with `--lsp-only` (#1992), exactly what this plan enables. Only
/// this entry's: serena refuses to start if any listed server fails, so
/// another project's languages must not leak in (#1943).
pub(super) fn entry_langs(m: &Manifest, entry: &str, plan: &plan::Plan) -> Vec<Lang> {
    let own = m.languages_of(entry).filter(|_| !plan.exact_lsp);
    Lang::ALL
        .into_iter()
        .filter(|l| {
            own.is_some_and(|o| o.contains(l.flag()))
                || plan
                    .lsp
                    .iter()
                    .any(|r| r.lang == *l && r.status == LspStatus::Enabled)
        })
        .collect()
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
