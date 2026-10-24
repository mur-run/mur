//! `mur code-nav doctor` (#1983): read the setup manifest back and check it
//! still matches the disk and the agent's profile.
//!
//! Offline and read-only: no network, no install, no profile or manifest
//! write. It only reports; `mur code-nav setup` repairs. Exit is always 0
//! unless the inputs cannot be read at all, like `mur model doctor`.
//!
//! [`audit`] is pure over its [`Inputs`]; [`run`] only loads them and prints.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_agent_runtime::mcp::serena::{
    DEFAULT_ENTRY_NAME, SERENA_ENTRY_HOMES_DIR, SERENA_HOME_DIR, serena_paths_for,
};
use mur_common::AgentProfile;

use super::consent::{self, Manifest};
use super::plan;
use super::serena_config::project_folder;
use super::serena_install::Installed;
use super::serena_project::{PROJECT_FILE, PROJECT_OWNED_KEYS};
use super::{ast_grep_install, node_ls_install, pyright_install, serena_install};

/// The shared npm cache a manual workaround grants; #1981 installs into
/// MUR's own cache under the tools root instead.
const SHARED_NPM_CACHE: &str = ".npm";
/// `project.yml` key holding the language list.
const LANGS_KEY: &str = PROJECT_OWNED_KEYS[1];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub subject: String,
    pub detail: String,
    /// The command that repairs it, when there is one.
    pub fix: Option<String>,
}

impl Finding {
    fn ok(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            subject: subject.into(),
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(subject: impl Into<String>, detail: impl Into<String>, fix: String) -> Self {
        Self {
            level: Level::Warn,
            subject: subject.into(),
            detail: detail.into(),
            fix: Some(fix),
        }
    }
}

/// Everything [`audit`] looks at, already loaded.
pub struct Inputs<'a> {
    pub agent: &'a str,
    pub mur_home: &'a Path,
    pub agent_home: &'a Path,
    /// The user's home, for `~/` grants.
    pub user_home: &'a Path,
    pub manifest: &'a Manifest,
    pub profile: &'a AgentProfile,
    /// The ast-grep build pinned for this host; `None` where MUR pins none,
    /// so its version cannot be verified offline.
    pub ast_grep_pin: Option<ast_grep_install::Pinned>,
}

pub fn audit(i: &Inputs) -> Vec<Finding> {
    let mut out = Vec::new();
    check_installs(i, &mut out);
    check_grants(i, &mut out);
    check_unrecorded(i, &mut out);
    check_entries(i, &mut out);
    check_vue_links(i, &mut out);
    out
}

/// #1982: each entry's `vue-lsp` link into the tools root, re-checked with
/// the same function the runtime preflight runs. Silent for an entry with
/// neither Vue recorded nor a link on disk.
fn check_vue_links(i: &Inputs, out: &mut Vec<Finding>) {
    use mur_common::serena_vue::{LinkState, check_link, resources_link};
    let tools = i.mur_home.join(mur_common::config::MUR_TOOLS_DIR);
    let vue = mur_common::serena_ls_cmd::VUE_LS_ID;
    for (entry, langs) in &i.manifest.entry_languages {
        let Ok(paths) = serena_paths_for(i.agent_home, entry) else {
            continue;
        };
        let subject = format!("entry {entry} vue link");
        let link = resources_link(&paths.home);
        match check_link(&paths.home, &tools) {
            Ok(LinkState::Linked) => out.push(Finding::ok(subject, link.display().to_string())),
            Ok(LinkState::Absent) if langs.contains(vue) => out.push(Finding::warn(
                subject,
                format!(
                    "{} is missing; serena would npm-install in the sandbox",
                    link.display()
                ),
                setup_cmd(i),
            )),
            Ok(LinkState::Absent) => {}
            Err(why) => out.push(Finding::warn(subject, why, setup_cmd(i))),
        }
    }
}

/// The setup run that re-verifies installs, with serena's flags when the
/// manifest says serena was set up.
fn setup_cmd(i: &Inputs) -> String {
    match &i.manifest.project {
        Some(p) => format!(
            "mur code-nav setup --agent {} --with-serena --project {}",
            i.agent,
            p.display()
        ),
        None => format!("mur code-nav setup --agent {}", i.agent),
    }
}

/// Where the recorded `name@version` install must be, and the version MUR
/// pins today. `None` for a tool doctor does not know.
fn install_target(i: &Inputs, name: &str) -> Option<(Option<PathBuf>, &'static str)> {
    use mur_common::serena_ls_cmd::{INTELEPHENSE, ManagedNodeLs, TYPESCRIPT, VUE};
    let m = i.manifest;
    let node = |ls: &ManagedNodeLs, r: &Option<super::node_ls_install::Record>| {
        (r.as_ref().map(|r| r.entry.clone()), ls.pin())
    };
    Some(match name {
        plan::AST_GREP => (
            Some(mur_common::config::ast_grep_binary_path(i.mur_home)),
            mur_common::config::AST_GREP_PINNED_VERSION,
        ),
        plan::SERENA => (m.serena.as_ref().map(|r| r.bin.clone()), plan::SERENA_PIN),
        plan::PYRIGHT => (m.pyright.as_ref().map(|r| r.bin.clone()), plan::PYRIGHT_PIN),
        plan::INTELEPHENSE => node(&INTELEPHENSE, &m.intelephense),
        plan::TS_LS => node(&TYPESCRIPT, &m.typescript),
        plan::VUE_LS => node(&VUE, &m.vue),
        _ => return None,
    })
}

fn check_installs(i: &Inputs, out: &mut Vec<Finding>) {
    for key in &i.manifest.installed {
        let subject = format!("install {key}");
        let Some((name, version)) = key.rsplit_once('@') else {
            out.push(Finding::warn(
                subject,
                "not a name@version key",
                setup_cmd(i),
            ));
            continue;
        };
        let Some((path, pin)) = install_target(i, name) else {
            out.push(Finding::warn(
                subject,
                "unknown tool; this MUR cannot check it",
                setup_cmd(i),
            ));
            continue;
        };
        let Some(path) = path else {
            out.push(Finding::warn(
                subject,
                "recorded as installed but the manifest has no path for it",
                setup_cmd(i),
            ));
            continue;
        };
        if !path.exists() {
            out.push(Finding::warn(
                subject,
                format!("{} is missing", path.display()),
                setup_cmd(i),
            ));
        } else if version != pin {
            out.push(Finding::warn(
                subject,
                format!("this MUR pins {name}@{pin}"),
                setup_cmd(i),
            ));
        } else {
            out.push(match on_disk_state(i, name) {
                Some(Installed::Verified) => Finding::ok(subject, path.display().to_string()),
                Some(Installed::Mismatch) => Finding::warn(
                    subject,
                    format!(
                        "{} is present but what is installed is not {name}@{pin}",
                        path.display()
                    ),
                    setup_cmd(i),
                ),
                Some(Installed::Missing) => Finding::warn(
                    subject,
                    format!(
                        "{} is present but its version record is missing",
                        path.display()
                    ),
                    setup_cmd(i),
                ),
                None => Finding::ok(
                    subject,
                    format!(
                        "{} (present; version not verifiable offline on this platform)",
                        path.display()
                    ),
                ),
            });
        }
    }
}

/// The installed artifact's own version, judged the way setup judges it
/// (npm `package.json`, uv dist-info, binary hash) against the pin. `None`
/// when there is no offline way to tell.
fn on_disk_state(i: &Inputs, name: &str) -> Option<Installed> {
    use mur_common::serena_ls_cmd::{INTELEPHENSE, TYPESCRIPT, VUE};
    let tools = node_ls_install::tools_dir(i.mur_home);
    Some(match name {
        plan::AST_GREP => {
            let dest = mur_common::config::ast_grep_binary_path(i.mur_home);
            match ast_grep_install::installed_state(&dest, i.ast_grep_pin.as_ref()?) {
                ast_grep_install::Installed::Verified => Installed::Verified,
                ast_grep_install::Installed::Mismatch => Installed::Mismatch,
                ast_grep_install::Installed::Missing => Installed::Missing,
            }
        }
        plan::SERENA => serena_install::installed_state(&serena_install::serena_dir(i.mur_home)),
        plan::PYRIGHT => {
            pyright_install::installed_state(&pyright_install::pyright_dir(i.mur_home))
        }
        plan::INTELEPHENSE => node_ls_install::installed_state(&INTELEPHENSE, &tools),
        plan::TS_LS => node_ls_install::installed_state(&TYPESCRIPT, &tools),
        // Vue's server and its companion TypeScript packages, all pinned.
        plan::VUE_LS => node_ls_install::installed_state(&VUE, &tools),
        _ => return None,
    })
}

/// A recorded grant: its kind, its value, and the profile list holding it.
fn grant_list<'p>(
    p: &'p AgentProfile,
    key: &'_ str,
) -> Option<(&'static str, String, &'p [String])> {
    let (kind, value) = key.split_once(' ')?;
    let list: &[String] = match kind {
        "spawn" => &p.entitlements.processes.spawn.allowed,
        "read" => &p.entitlements.filesystem.read,
        "spawn-dir" => &p.entitlements.processes.spawn.allowed_dirs,
        _ => return None,
    };
    let kind = match kind {
        "spawn" => "spawn",
        "read" => "read",
        _ => "spawn-dir",
    };
    Some((kind, value.to_owned(), list))
}

fn grant_fix(agent: &str, kind: &str, value: &str) -> String {
    let verb = match kind {
        "spawn" => "allow-spawn",
        "read" => "allow-read",
        _ => "allow-spawn-dir",
    };
    format!("mur agent perm {verb} {agent} {value}")
}

fn check_grants(i: &Inputs, out: &mut Vec<Finding>) {
    for key in &i.manifest.granted {
        let subject = format!("grant {key}");
        let Some((kind, value, list)) = grant_list(i.profile, key) else {
            out.push(Finding::warn(subject, "unknown grant kind", setup_cmd(i)));
            continue;
        };
        if !list.iter().any(|g| g == &value) {
            out.push(Finding::warn(
                subject,
                "recorded by setup but missing from the agent profile",
                grant_fix(i.agent, kind, &value),
            ));
        } else if kind != "spawn" && !Path::new(&value).exists() {
            out.push(Finding::warn(
                subject,
                format!("granted, but {value} no longer exists"),
                setup_cmd(i),
            ));
        } else {
            out.push(Finding::ok(subject, "in profile"));
        }
    }
}

fn expand(user_home: &Path, s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => user_home.join(rest),
        None if s == "~" => user_home.to_path_buf(),
        None => PathBuf::from(s),
    }
}

/// Profile grants that look like code-nav but setup never recorded.
fn check_unrecorded(i: &Inputs, out: &mut Vec<Finding>) {
    let tools = super::node_ls_install::tools_dir(i.mur_home);
    let roots = [
        tools.clone(),
        i.agent_home.join(SERENA_HOME_DIR),
        i.agent_home.join(SERENA_ENTRY_HOMES_DIR),
    ];
    let npm = i.user_home.join(SHARED_NPM_CACHE);
    let recorded: BTreeSet<&str> = i.manifest.granted.iter().map(String::as_str).collect();
    let fs = &i.profile.entitlements.filesystem;
    let spawn = &i.profile.entitlements.processes.spawn;
    let lists: [(&str, &[String], &str); 3] = [
        ("read", &fs.read, "remove-path {a} read"),
        ("write", &fs.write, "remove-path {a} write"),
        ("spawn-dir", &spawn.allowed_dirs, "deny-spawn-dir {a}"),
    ];
    for (kind, list, revoke) in lists {
        for g in list {
            let p = expand(i.user_home, g);
            let fix = format!("mur agent perm {} {g}", revoke.replace("{a}", i.agent));
            if p.starts_with(&npm) {
                out.push(Finding::warn(
                    format!("{kind} {g}"),
                    format!(
                        "manual grant on the shared npm cache; setup installs node servers \
                         with its own cache under {}, so this is no longer needed",
                        tools.display()
                    ),
                    fix,
                ));
            } else if roots.iter().any(|r| p.starts_with(r))
                && !recorded.contains(format!("{kind} {g}").as_str())
            {
                out.push(Finding::warn(
                    format!("{kind} {g}"),
                    "code-nav path in the profile that setup never recorded (manual workaround?)",
                    fix,
                ));
            }
        }
    }
}

/// The project an entry serves, per the manifest.
fn entry_project<'m>(m: &'m Manifest, entry: &str) -> Option<&'m PathBuf> {
    if entry == DEFAULT_ENTRY_NAME {
        m.project.as_ref().or_else(|| m.entry_projects.get(entry))
    } else {
        m.entry_projects.get(entry)
    }
}

/// `language_servers` in a `project.yml`.
fn read_langs(path: &Path) -> Result<BTreeSet<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let Some(seq) = v.get(LANGS_KEY).and_then(|s| s.as_sequence()) else {
        bail!("{} has no `{LANGS_KEY}` list", path.display());
    };
    Ok(seq
        .iter()
        .filter_map(|l| l.as_str().map(str::to_owned))
        .collect())
}

fn entry_fix(i: &Inputs, entry: &str, project: &Path, langs: &BTreeSet<String>) -> String {
    entry_cmd(i, entry, project, "--lsp", langs)
}

/// `flag` is `--lsp` (re-add what is recorded) or `--lsp-only` (narrow the
/// record to what is listed, #1992).
fn entry_cmd(
    i: &Inputs,
    entry: &str,
    project: &Path,
    flag: &str,
    langs: &BTreeSet<String>,
) -> String {
    let mut s = format!(
        "mur code-nav setup --agent {} --with-serena --project {} --entry {entry}",
        i.agent,
        project.display()
    );
    for l in langs {
        s.push(' ');
        s.push_str(flag);
        s.push(' ');
        s.push_str(l);
    }
    s
}

fn check_entries(i: &Inputs, out: &mut Vec<Finding>) {
    for (entry, want) in &i.manifest.entry_languages {
        let subject = format!("entry {entry}");
        let Some(project) = entry_project(i.manifest, entry) else {
            out.push(Finding::warn(
                subject,
                "languages recorded but no project",
                setup_cmd(i),
            ));
            continue;
        };
        let fix = entry_fix(i, entry, project, want);
        let file = serena_paths_for(i.agent_home, entry)
            .map_err(anyhow::Error::from)
            .and_then(|p| project_folder(&p, project))
            .map(|d| d.join(PROJECT_FILE));
        let file = match file {
            Ok(f) => f,
            Err(e) => {
                out.push(Finding::warn(subject, format!("{e:#}"), fix));
                continue;
            }
        };
        match read_langs(&file) {
            Err(e) => out.push(Finding::warn(subject, format!("{e:#}"), fix)),
            Ok(have) if &have != want => {
                let list = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(",");
                let detail = format!(
                    "{} lists [{}], setup recorded [{}]",
                    file.display(),
                    list(&have),
                    list(want)
                );
                let missing: BTreeSet<String> = want.difference(&have).cloned().collect();
                if missing.is_empty() {
                    out.push(Finding::warn(subject, detail, fix));
                } else {
                    // A language dropped from project.yml is usually one whose
                    // server cannot start, so re-adding it with `--lsp` would
                    // stop serena again (#1989). Narrow the record to what the
                    // file lists instead (#1992). `--lsp-only` refuses an empty
                    // set, so an empty file gets no fix.
                    let narrow = (!have.is_empty())
                        .then(|| entry_cmd(i, entry, project, "--lsp-only", &have));
                    out.push(Finding {
                        level: Level::Warn,
                        subject,
                        detail: format!(
                            "{detail}; missing [{}] -- re-adding them with setup may stop \
                             serena from starting if their servers cannot launch",
                            list(&missing)
                        ),
                        fix: narrow,
                    });
                }
            }
            Ok(_) => out.push(Finding::ok(subject, format!("{}", file.display()))),
        }
    }
}

/// Print findings one per line, then a summary. Returns the warning count.
pub fn print(out: &mut dyn Write, agent: &str, findings: &[Finding]) -> Result<usize> {
    writeln!(out, "agent '{agent}':")?;
    for f in findings {
        let tag = match f.level {
            Level::Ok => "ok   ",
            Level::Warn => "warn ",
            Level::Error => "error",
        };
        writeln!(out, "  {tag} {}: {}", f.subject, f.detail)?;
        if let Some(fix) = &f.fix {
            writeln!(out, "        fix: {fix}")?;
        }
    }
    let bad = findings.iter().filter(|f| f.level != Level::Ok).count();
    writeln!(out, "  {} check(s), {bad} need attention", findings.len())?;
    Ok(bad)
}

/// Agents with a manifest, by file stem. Only an absent manifest directory
/// means "nothing set up"; any other read error is an error, not empty.
fn manifest_agents(mur_home: &Path) -> Result<Vec<String>> {
    let mut dir = mur_home.to_path_buf();
    dir.extend(consent::MANIFEST_DIR);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    let mut names = Vec::new();
    for e in rd {
        let p = e
            .with_context(|| format!("read an entry of {}", dir.display()))?
            .path();
        if p.extension().is_some_and(|x| x == "json")
            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// One agent. A missing manifest means setup never ran; a corrupt one is
/// an error, never treated as empty.
fn doctor_agent(
    mur_home: &Path,
    user_home: &Path,
    agent: &str,
    ast_grep_pin: Option<&ast_grep_install::Pinned>,
    out: &mut dyn Write,
) -> Result<usize> {
    let path = consent::manifest_path(mur_home, agent);
    if !path.exists() {
        writeln!(
            out,
            "agent '{agent}': `mur code-nav setup` has not run (no {})",
            path.display()
        )?;
        return Ok(0);
    }
    let manifest = match consent::load_manifest(&path) {
        Ok(m) => m,
        Err(e) => {
            let f = Finding {
                level: Level::Error,
                subject: "manifest".into(),
                detail: format!("{e:#}; not treated as empty"),
                fix: None,
            };
            return print(out, agent, &[f]);
        }
    };
    let agent_home = mur_home.join("agents").join(agent);
    let ppath = agent_home.join("profile.yaml");
    let yaml =
        std::fs::read_to_string(&ppath).with_context(|| format!("read {}", ppath.display()))?;
    let profile: AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", ppath.display()))?;
    let findings = audit(&Inputs {
        agent,
        mur_home,
        agent_home: &agent_home,
        user_home,
        manifest: &manifest,
        profile: &profile,
        ast_grep_pin: ast_grep_pin.cloned(),
    });
    print(out, agent, &findings)
}

pub fn run(agent: Option<&str>, out: &mut dyn Write) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let user_home = dirs::home_dir().context("no home directory")?;
    let agents = match agent {
        Some(a) => {
            let a = crate::a2a_dial::canonicalize_agent_name(&mur_home, a);
            if !mur_home
                .join("agents")
                .join(&a)
                .join("profile.yaml")
                .is_file()
            {
                bail!("agent '{a}' not found");
            }
            vec![a]
        }
        None => manifest_agents(&mur_home)?,
    };
    if agents.is_empty() {
        writeln!(out, "no agent has run `mur code-nav setup` yet")?;
        return Ok(());
    }
    let pin = ast_grep_install::pinned_for_host();
    let mut bad = 0;
    for a in &agents {
        bad += doctor_agent(&mur_home, &user_home, a, pin.as_ref(), out)?;
    }
    writeln!(
        out,
        "\n{bad} finding(s) need attention, nothing was changed"
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
