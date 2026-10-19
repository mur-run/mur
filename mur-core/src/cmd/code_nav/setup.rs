//! Task 3.6 (impure half): `mur code-nav setup`. Detect, plan, print, ask
//! once, apply, record.
//!
//! Consent follows `mur browser setup` (`cmd::consent`): a literal `yes` on a
//! terminal, or `--yes`. `--yes` confirms the printed plan; flags alone
//! decide its scope. No terminal and no `--yes` refuses before anything is
//! installed, written or granted. A re-run whose plan adds nothing over the
//! manifest asks nothing and only re-verifies.
//!
//! The exec lanes are the one thing the plan cannot list: after install,
//! any lane the manifest has not recorded is printed by path and asked
//! about again, under the same rules, before it is granted (#1942). A run
//! that fails part way saves no manifest, so the next run re-lists what it
//! had already applied: conservative, never silent.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_agent_runtime::mcp::serena::serena_paths_for;
use serde_yaml_ng::Value;

use super::consent::{self, Manifest};
use super::plan::{self, Detected, Flags, Lang, LspStatus, Permission, UV};
use super::{
    ast_grep_install, ast_grep_wire, node_ls_install, pyright_install, serena_config, serena_entry,
    serena_install, serena_project, smoke, smoke_fetch,
};
use crate::cmd::browser::setup::Consent;

pub struct Args {
    pub agent: String,
    pub project: Option<PathBuf>,
    /// Name of the `kind: serena` entry this run creates or updates.
    pub entry: String,
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
    // Refuse a name serena_paths_for would refuse at spawn, before anything
    // is installed: the home is derived from the name alone.
    serena_paths_for(&agent_home, &args.entry)?;
    let entry = args.entry.as_str();
    let project = match (&args.project, args.flags.with_serena) {
        (Some(p), true) => Some(serena_entry::checked_project(p)?),
        (None, true) => bail!("--with-serena needs --project <dir>: the repo serena will serve"),
        (Some(_), false) => bail!("--project only applies with --with-serena"),
        (None, false) => None,
    };
    // A same-named server the user owns would only be refused at the final
    // profile save, after uv downloads and config writes. Refuse it here,
    // before the plan is even printed, so nothing changes on disk.
    if project.is_some() {
        let (_, profile) = crate::cmd::agent::load_profile_for_edit(&agent)?;
        serena_entry::check_slot(&profile, entry)?;
    }

    let plan = plan::plan(&mur_home, &args.flags, &detect())?;
    let mut permissions = plan.permissions.clone();
    // #1984: what wiring ast-grep to the agent grants, decided (and any
    // refusal raised) before anything is installed.
    if plan.install.iter().any(|r| r.name == plan::AST_GREP) {
        let slot = ast_grep_wire::preflight(&agent)?;
        permissions.extend(ast_grep_wire::permissions(&mur_home, &slot));
    }
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
        writeln!(out, "\nserena entry `{entry}` project: {}", p.display())?;
        writeln!(
            out,
            "    also needed after install: exec of serena's (and pyright's) own Python \
             (each venv bin dir and the interpreter uv chose); any not granted before \
             is shown by path and asked about then"
        )?;
    }

    let manifest_path = consent::manifest_path(&mur_home, &agent);
    let prior = consent::load_manifest(&manifest_path)?;
    let first_run = prior == Manifest::default();
    let diff = consent::diff(&plan, entry, project.as_deref(), &prior);
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
        &Target {
            mur_home: &mur_home,
            agent: &agent,
            agent_home: &agent_home,
            entry,
        },
        &plan,
        project.as_deref(),
        prior,
        &mut Ask { consent, input },
        out,
    )?;
    consent::save_manifest(&manifest_path, &manifest)?;
    writeln!(out, "\nRecorded in {}", manifest_path.display())?;

    // #1981 Layer 3: success means the servers start under the agent's seal,
    // not that the files landed. Run after the manifest is saved, so a
    // failure here keeps the installs and grants recorded above.
    if let Some(project) = &project
        && manifest.serena.is_some()
    {
        let langs: Vec<Lang> = Lang::ALL
            .into_iter()
            .filter(|l| {
                manifest
                    .languages_of(entry)
                    .is_some_and(|s| s.contains(l.flag()))
            })
            .collect();
        let serena_home = serena_paths_for(&agent_home, entry)?.home;
        let (targets, unfetched) = smoke_targets(&manifest, &langs, &mur_home, &serena_home, out)?;
        let fetch_dir = smoke::fetch_dir(&serena_home);
        smoke::verify(&agent, project, &targets, &unfetched, &fetch_dir, out)?;
    }
    Ok(())
}

/// Servers to launch, and the ones that could not be, with why.
type SmokeTargets = (Vec<smoke::Target>, Vec<(Lang, String)>);

/// Why a server neither MUR nor setup's fetch provides goes unlaunched.
const NOT_MANAGED: &str = "serena starts it from a system install setup does not manage";

/// What setup's smoke launch starts for `langs`: MUR's own installs, plus the
/// servers serena fetches itself, fetched now into the entry's `serena_home`
/// (#1993). The second list is what could not be launched, and why.
fn smoke_targets(
    manifest: &Manifest,
    langs: &[Lang],
    mur_home: &Path,
    serena_home: &Path,
    out: &mut dyn Write,
) -> Result<SmokeTargets> {
    let mut targets = smoke::targets(manifest, langs, mur_home);
    let rest: Vec<Lang> = langs
        .iter()
        .copied()
        .filter(|l| !targets.iter().any(|t| t.lang == *l))
        .collect();
    let to_fetch: Vec<Lang> = rest
        .iter()
        .copied()
        .filter(|l| l.serena_fetched())
        .collect();
    let mut unfetched: Vec<(Lang, String)> = rest
        .iter()
        .copied()
        .filter(|l| !l.serena_fetched())
        .map(|l| (l, NOT_MANAGED.into()))
        .collect();
    if !to_fetch.is_empty() {
        let names: Vec<_> = to_fetch.iter().map(|l| l.flag()).collect();
        writeln!(
            out,
            "\nFetching {} the way serena does, to smoke them…",
            names.join(", ")
        )?;
        let dir = serena_install::serena_dir(mur_home);
        for f in smoke_fetch::fetch(&dir, serena_home, &to_fetch) {
            match f {
                smoke_fetch::Fetched::Ready(t) => targets.push(t),
                smoke_fetch::Fetched::Failed { lang, detail } => unfetched.push((
                    lang,
                    format!("serena could not fetch it during setup: {detail}"),
                )),
            }
        }
    }
    Ok((targets, unfetched))
}

/// How `apply` asks about what the plan could not list.
struct Ask<'a> {
    consent: Consent,
    input: &'a mut dyn BufRead,
}

/// #1942: exec lanes are known only after install, so the plan diff never
/// saw them. Any lane the manifest has not recorded is shown by its path
/// and consented to under the same rules as the plan, before it is granted.
fn consent_to_lanes(
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

/// Whose profile and which serena entry a run applies to.
struct Target<'a> {
    mur_home: &'a Path,
    agent: &'a str,
    agent_home: &'a Path,
    entry: &'a str,
}

fn apply(
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
    let node_ls: Vec<_> = [
        (&intelephense, mur_common::serena_ls_cmd::INTELEPHENSE),
        (&typescript, mur_common::serena_ls_cmd::TYPESCRIPT),
    ]
    .into_iter()
    .filter_map(|(r, ls)| r.as_ref().map(|_| ls))
    .collect();

    let langs = entry_langs(&m, entry, plan);
    let paths = serena_paths_for(agent_home, entry)?;
    let ptpl = serena_project::read_pinned_project_template(&dir)?;
    let pfile = serena_project::write_project(&paths, project, &ptpl, &langs)?;
    writeln!(out, "  ✓ wrote {}", pfile.display())?;
    let tpl = serena_config::read_pinned_template(&dir)?;
    let secret =
        existing_auth_secret(&paths.config_file).unwrap_or_else(serena_config::new_auth_secret);
    let managed = serena_config::ManagedLs {
        python: pyright.as_ref().map(|r| r.bin.as_path()),
        node: &node_ls,
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

#[cfg(test)]
#[path = "setup_tests.rs"]
mod run_tests;
