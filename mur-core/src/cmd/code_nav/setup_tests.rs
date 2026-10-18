//! #1916: `setup::run` as a whole, against an isolated `MUR_HOME`. The unit
//! tests on `check_slot` cannot show that a refusal happens *before* setup
//! touches anything; these do, by comparing the whole home before and after.

use super::*;
use mur_common::agent::McpServerEntry;
use std::collections::BTreeMap;

/// Every file under `root` with its bytes, so "nothing was written" is a
/// plain equality rather than a list of paths someone remembered to check.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let bytes = std::fs::read(e.path()).unwrap();
            (e.path().strip_prefix(root).unwrap().to_path_buf(), bytes)
        })
        .collect()
}

struct Home {
    _tmp: tempfile::TempDir,
    _env: mur_common::test_env::EnvGuard,
    mur_home: PathBuf,
    repo: PathBuf,
}

/// An isolated `MUR_HOME` holding agent `carol`, whose profile already has
/// the given user-owned MCP servers, plus an empty repo to point serena at.
fn home_with_servers(names: &[&str]) -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("mur-home");
    let agent_home = mur_home.join("agents").join("carol");
    std::fs::create_dir_all(&agent_home).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let mut env = mur_common::test_env::EnvGuard::hold();
    env.set_var("MUR_HOME", &mur_home);

    let mut profile = mur_common::AgentProfile::default_for_tests();
    profile.name = "carol".into();
    for n in names {
        profile.mcp_servers.push(McpServerEntry {
            name: (*n).into(),
            command: "/bin/true".into(),
            ..Default::default()
        });
    }
    std::fs::write(
        agent_home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    Home {
        _tmp: tmp,
        _env: env,
        mur_home,
        repo,
    }
}

fn args(h: &Home, entry: &str) -> Args {
    Args {
        agent: "carol".into(),
        project: Some(h.repo.clone()),
        entry: entry.into(),
        flags: Flags {
            with_serena: true,
            ..Default::default()
        },
    }
}

fn run_setup(h: &Home, entry: &str) -> (Result<()>, String) {
    let mut out = Vec::new();
    let r = run(
        args(h, entry),
        Consent::Given,
        &mut std::io::empty(),
        &mut out,
    );
    (r, String::from_utf8(out).unwrap())
}

/// `code/nav` and `code_nav` both become the tool prefix `mcp__code_nav__`.
/// The refusal must leave the profile, the serena home and the consent
/// manifest exactly as they were: no bytes written anywhere under `MUR_HOME`.
#[test]
fn a_shared_tool_prefix_is_refused_before_anything_is_written() {
    let h = home_with_servers(&["code/nav"]);
    let before = snapshot(&h.mur_home);

    let (r, out) = run_setup(&h, "code_nav");

    let err = format!("{:#}", r.expect_err("a prefix clash must be refused"));
    assert!(err.contains("same tool prefix"), "{err}");
    assert!(err.contains("code/nav"), "names the other server: {err}");
    assert_eq!(snapshot(&h.mur_home), before, "setup wrote before refusing");
    assert!(
        !out.contains("serena entry"),
        "refused before the plan was printed: {out}"
    );
}

/// A user's own non-serena server holding the entry's exact name is refused
/// the same way, and equally early.
#[test]
fn a_user_server_holding_the_entry_name_is_refused_before_anything_is_written() {
    let h = home_with_servers(&["serena"]);
    let before = snapshot(&h.mur_home);

    let (r, _) = run_setup(&h, "serena");

    let err = format!("{:#}", r.expect_err("the slot is taken"));
    assert!(err.contains("not kind: serena"), "{err}");
    assert_eq!(snapshot(&h.mur_home), before, "setup wrote before refusing");
}

/// A name `sanitize_server` would rewrite is refused by the path check, which
/// also runs before the profile is even read.
#[test]
fn an_entry_name_that_sanitizes_to_another_is_refused_before_anything_is_written() {
    for bad in ["serena_", "repo__a", "Repo", "a/b", ""] {
        let h = home_with_servers(&[]);
        let before = snapshot(&h.mur_home);
        let (r, _) = run_setup(&h, bad);
        assert!(r.is_err(), "`{bad}` must be refused");
        assert_eq!(snapshot(&h.mur_home), before, "`{bad}`: wrote anything");
    }
}

/// The success path needs serena's two pinned templates, which are GPL and so
/// not vendored: copy them from a real install (`MUR_SERENA_INSTALL_DIR`).
/// Everything else is laid out here, in the isolated `MUR_HOME`, so no network
/// and no `uv` run is involved.
#[cfg(unix)]
mod success {
    use super::*;
    use crate::cmd::code_nav::serena_config::find_resource;
    use crate::cmd::code_nav::serena_install::{
        BIN_SUBDIR, PACKAGE, PYTHON_SUBDIR, PYVENV_CFG, SERENA_GIT_REV, SERENA_GIT_URL,
        UV_TOOL_SUBDIR, serena_binary_path_in, serena_dir,
    };
    use std::os::unix::fs::PermissionsExt;

    const TEMPLATES: [(&[&str], &str); 2] = [
        (
            &["serena", "resources", "serena_config.template.yml"],
            "config",
        ),
        (&["serena", "resources", "project.template.yml"], "project"),
    ];

    /// A verified-looking serena install under `<mur_home>/tools`, whose entry
    /// point's shebang names an interpreter inside it (so the exec lanes pass
    /// the "must stay under the tools root" check), plus a `uv` stub on a
    /// `PATH` that holds nothing else, so the plan is the same on every host.
    fn install_fake_serena(h: &mut Home, real: &Path) {
        let dir = serena_dir(&h.mur_home);
        let env = dir.join(UV_TOOL_SUBDIR).join(PACKAGE);
        let site = env.join("lib/python3.13/site-packages");
        let dist = site.join(format!(
            "serena_agent-{}.dist-info",
            crate::cmd::code_nav::plan::SERENA_PIN
        ));
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(
            dist.join("direct_url.json"),
            format!(
                r#"{{"url":"{SERENA_GIT_URL}","vcs_info":{{"vcs":"git","commit_id":"{SERENA_GIT_REV}"}}}}"#
            ),
        )
        .unwrap();
        let py_home = dir.join(PYTHON_SUBDIR).join("cpython-3.13.2/bin");
        std::fs::create_dir_all(&py_home).unwrap();
        std::fs::write(
            env.join(PYVENV_CFG),
            format!("home = {}\nimplementation = CPython\n", py_home.display()),
        )
        .unwrap();
        let python = env.join(BIN_SUBDIR).join("python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, b"").unwrap();
        let bin = serena_binary_path_in(&dir);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, format!("#!{}\n", python.display())).unwrap();
        for (rel, _) in TEMPLATES {
            let src = find_resource(real, rel).expect("template in the real install");
            let dest = site.join(rel.iter().collect::<PathBuf>());
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::copy(src, dest).unwrap();
        }
        let stub = h.mur_home.parent().unwrap().join("path");
        std::fs::create_dir_all(&stub).unwrap();
        let uv = stub.join("uv");
        std::fs::write(&uv, b"#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        h._env.set_var("PATH", &stub);
    }

    fn run_ok(h: &Home, entry: &str) {
        let mut a = args(h, entry);
        a.flags.no_ast_grep = true;
        // One language with no prerequisites: the plan must not depend on
        // which of node / go / rust-analyzer this host happens to have.
        a.flags.lsp = vec!["lua".into()];
        let mut out = Vec::new();
        let r = run(a, Consent::Given, &mut std::io::empty(), &mut out);
        let out = String::from_utf8_lossy(&out);
        r.unwrap_or_else(|e| panic!("setup `{entry}` failed: {e:#}\n{out}"));
    }

    /// #1916: a valid named entry writes its own serena home and nothing in
    /// any other entry's, and the profile entry points at the repo.
    #[test]
    #[ignore = "needs a pinned serena install for its templates (MUR_SERENA_INSTALL_DIR)"]
    fn a_valid_named_entry_writes_only_its_own_paths() {
        let real = PathBuf::from(
            std::env::var_os("MUR_SERENA_INSTALL_DIR").expect("MUR_SERENA_INSTALL_DIR"),
        );
        let mut h = home_with_servers(&[]);
        install_fake_serena(&mut h, &real);
        let agent_home = h.mur_home.join("agents").join("carol");
        let before = snapshot(&h.mur_home);

        run_ok(&h, "repo-b");

        let own = agent_home.join("serena-homes").join("repo-b");
        let repo = std::fs::canonicalize(&h.repo).unwrap();
        let folder = own.join("projects").join(repo.file_name().unwrap());
        assert!(
            own.join("serena_config.yml").is_file(),
            "config in own home"
        );
        assert!(
            folder.join("project.yml").is_file(),
            "project.yml in own home"
        );
        assert!(
            !agent_home.join("serena").exists(),
            "default home untouched"
        );

        // Every new file is under this entry's home, or is bookkeeping that
        // is per agent rather than per entry: the consent manifest and the
        // grant pins. Nothing may land in another entry's home or the default.
        let manifest = consent::manifest_path(&h.mur_home, "carol");
        let pins = h.mur_home.join("entitlement-pins").join("carol.json");
        let after = snapshot(&h.mur_home);
        let new: Vec<PathBuf> = after
            .keys()
            .filter(|p| !before.contains_key(*p))
            .map(|p| h.mur_home.join(p))
            .collect();
        for abs in &new {
            assert!(
                abs.starts_with(&own) || *abs == manifest || *abs == pins,
                "stray file written: {}",
                abs.display()
            );
        }
        assert!(new.contains(&manifest), "manifest recorded: {new:?}");

        let cfg: Value = serde_yaml_ng::from_str(
            &std::fs::read_to_string(own.join("serena_config.yml")).unwrap(),
        )
        .unwrap();
        let loc = cfg["project_serena_folder_location"].as_str().unwrap();
        assert!(
            loc.starts_with(own.join("projects").to_str().unwrap()),
            "folder location outside the entry's home: {loc}"
        );

        let (_, profile) = crate::cmd::agent::load_profile_for_edit("carol").unwrap();
        let e = profile
            .mcp_servers
            .iter()
            .find(|e| e.name == "repo-b")
            .expect("profile entry `repo-b`");
        assert_eq!(e.kind, Some(mur_common::agent::McpServerKind::Serena));
        assert_eq!(e.project.as_deref(), Some(repo.as_path()));
        assert_eq!(
            e.command,
            serena_binary_path_in(&serena_dir(&h.mur_home))
                .to_str()
                .unwrap()
        );
        assert!(e.binary_sha256.is_some(), "entry point is pinned");

        let m = consent::load_manifest(&manifest).unwrap();
        assert_eq!(m.entry_projects.get("repo-b"), Some(&repo));
        assert_eq!(m.project, None, "the default entry was not set up");

        // A second entry on the same repo gets a different home, and the
        // first is left exactly as it was.
        let first = snapshot(&own);
        run_ok(&h, "repo-c");
        assert_eq!(snapshot(&own), first, "a second entry changed the first");
        let other = agent_home.join("serena-homes").join("repo-c");
        assert!(other.join("serena_config.yml").is_file());
        assert_ne!(own, other);
    }
}

/// #1943: setting up a second entry must not widen the first entry's
/// `language_servers`, and vice versa.
#[test]
fn entry_languages_do_not_leak_across_entries() {
    let plan_for = |lsp: &[&str]| {
        let flags = Flags {
            with_serena: true,
            no_ast_grep: true,
            lsp: lsp.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        // node + npm: php's intelephense needs them, and the test must not
        // depend on what this host has on PATH.
        let detected = Detected {
            on_path: ["node", "npm", UV].into_iter().map(String::from).collect(),
        };
        super::plan::plan(Path::new("/mh"), &flags, &detected).unwrap()
    };
    let mut m = Manifest::default();
    let site = entry_langs(&m, "site", &plan_for(&["php"]));
    assert_eq!(site, [Lang::Php]);
    m.set_languages("site", site.iter().map(|l| l.flag().to_owned()));

    let repo = entry_langs(&m, "serena", &plan_for(&["lua"]));
    assert_eq!(repo, [Lang::Lua], "php leaked from another entry");
    m.set_languages("serena", repo.iter().map(|l| l.flag().to_owned()));

    // Re-running an entry keeps its own earlier languages (never revokes).
    assert_eq!(
        entry_langs(&m, "site", &plan_for(&["python"])),
        [Lang::Python, Lang::Php]
    );
}

/// #1992: `--lsp-only` sets the entry's languages to exactly what this run
/// enables, dropping what the entry listed before, instead of the union.
#[test]
fn lsp_only_replaces_the_entry_languages() {
    let plan_for = |lsp: &[&str], exact: bool| {
        let flags = Flags {
            with_serena: true,
            no_ast_grep: true,
            lsp: lsp.iter().map(|s| s.to_string()).collect(),
            exact_lsp: exact,
        };
        let detected = Detected {
            on_path: ["node", "npm", UV].into_iter().map(String::from).collect(),
        };
        super::plan::plan(Path::new("/mh"), &flags, &detected).unwrap()
    };
    let mut m = Manifest::default();
    m.set_languages(
        "mur",
        ["php".to_owned(), "lua".to_owned(), "cpp".to_owned()],
    );
    assert_eq!(
        entry_langs(&m, "mur", &plan_for(&["php"], true)),
        [Lang::Php],
        "--lsp-only must not keep lua/cpp"
    );
    // Plain --lsp still unions.
    assert_eq!(
        entry_langs(&m, "mur", &plan_for(&["php"], false)),
        [Lang::Php, Lang::Lua, Lang::Cpp]
    );
}

/// #1942: manifest records lane X, uv now resolves Y, the plan diff is
/// empty. Setup must ask about Y (or refuse), never grant it silently.
mod lane_consent {
    use super::*;

    fn lanes() -> (Manifest, Vec<PathBuf>) {
        let x = PathBuf::from("/mh/tools/serena/python/cpython-3.13.2/bin");
        let y = PathBuf::from("/mh/tools/serena/python/cpython-3.13.5/bin");
        let m = Manifest {
            granted: [consent::lane_grant_key(&x)].into_iter().collect(),
            ..Default::default()
        };
        (m, vec![x, y])
    }

    fn check(c: Consent, answer: &str) -> (Result<()>, String) {
        let (m, l) = lanes();
        let mut out = Vec::new();
        let r = consent_to_lanes(&l, &m, c, &mut answer.as_bytes(), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn no_terminal_and_no_yes_refuses_a_new_lane() {
        let (r, _) = check(Consent::Impossible, "");
        let e = format!("{:#}", r.expect_err("must refuse"));
        assert!(e.contains("cpython-3.13.5"), "names the lane: {e}");
    }

    #[test]
    fn a_new_lane_is_asked_with_its_concrete_path() {
        let (r, out) = check(Consent::Ask, "no\n");
        assert!(r.is_err(), "anything but `yes` refuses");
        assert!(out.contains("cpython-3.13.5"), "{out}");
        assert!(
            !out.contains("cpython-3.13.2"),
            "recorded lane not re-asked: {out}"
        );
        let (r, _) = check(Consent::Ask, "yes\n");
        r.unwrap();
    }

    #[test]
    fn yes_flag_accepts_but_still_prints_the_path() {
        let (r, out) = check(Consent::Given, "");
        r.unwrap();
        assert!(out.contains("cpython-3.13.5"), "{out}");
    }

    #[test]
    fn recorded_lanes_ask_nothing_even_without_a_terminal() {
        let (m, l) = lanes();
        let mut out = Vec::new();
        consent_to_lanes(
            &l[..1],
            &m,
            Consent::Impossible,
            &mut std::io::empty(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
    }
}
