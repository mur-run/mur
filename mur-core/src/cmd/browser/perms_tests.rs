use super::*;

fn home() -> PathBuf {
    PathBuf::from("/tmp/murhome")
}

/// The expected probe dir, spelled the way the platform spells it:
/// `probe_dir` joins components, so the separator is `\` on Windows.
fn probe(agent: &str) -> String {
    probe_dir(&home(), agent).to_string_lossy().into_owned()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_string()).collect()
}

fn st(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The cache a mac/Linux install resolves; tests pass it explicitly so
/// they do not depend on the host's `HOME` or `PLAYWRIGHT_BROWSERS_PATH`.
fn cache() -> PathBuf {
    home().join("ms-playwright")
}

fn shell_build() -> PathBuf {
    cache().join("chromium_headless_shell-1246")
}

fn full_build() -> PathBuf {
    cache().join("chromium-1246")
}

fn registry() -> PathBuf {
    cache().join("b")
}

fn browsers() -> Browsers {
    Browsers {
        cache: Some(cache()),
        builds: vec![shell_build(), full_build()],
        registry: Some(registry()),
    }
}

fn server() -> String {
    st(&mur_browser::server::install_dir(&home()))
}

fn all_dirs(agent: &str) -> Vec<String> {
    vec![probe(agent), st(&shell_build()), st(&full_build())]
}

#[test]
fn empty_profile_needs_every_grant() {
    let p = plan(&home(), "mur", &browsers(), &[], &[], &[], &[]);
    assert_eq!(p.binaries, s(&["node", "chrome-headless-shell"]));
    assert_eq!(p.dirs, all_dirs("mur"));
    assert_eq!(p.read, Some(server()));
    assert_eq!(p.write, Some(st(&registry())));
    assert_eq!(p.revoke_write, None);
    assert_eq!(p.revoke_dir, None);
    assert!(!p.is_empty());
}

#[test]
fn fully_granted_profile_is_a_no_op() {
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &s(&["node", "chrome-headless-shell"]),
        &all_dirs("mur"),
        &[server()],
        &[st(&registry())],
    );
    assert!(p.is_empty(), "{p:?}");
}

/// The probe dir is per-agent: another agent's grant must not count.
#[test]
fn dir_grant_is_not_shared_between_agents() {
    let dirs = all_dirs("other");
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &[],
        &dirs,
        &[server()],
        &[st(&registry())],
    );
    assert_eq!(p.dirs, vec![probe("mur")]);
}

#[test]
fn partial_profile_only_lists_what_is_missing() {
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &s(&["node"]),
        &[st(&shell_build()), st(&full_build())],
        &[server()],
        &[st(&registry())],
    );
    assert_eq!(p.binaries, s(&["chrome-headless-shell"]));
    let cmds = commands("mur", &p);
    assert_eq!(cmds.len(), 2, "{cmds:?}");
    assert!(cmds[0].ends_with("allow-spawn mur chrome-headless-shell"));
    assert!(cmds[1].contains(&format!("allow-spawn-dir mur {}", probe("mur"))));
}

/// #1730: the exec lanes are the build dirs and the only write is the
/// server registry — never the shared cache root.
#[test]
fn browsers_cache_root_is_never_granted() {
    let p = plan(&home(), "mur", &browsers(), &[], &[], &[], &[]);
    let root = st(&cache());
    for c in commands("mur", &p) {
        assert!(!c.ends_with(&format!(" {root}")), "root granted: {c}");
    }
    assert!(commands("mur", &p).contains(&format!(
        "mur agent perm allow-write mur {}",
        st(&registry())
    )));
}

/// #1730: a path both writable and an exec lane lets the agent plant a
/// binary it (or another agent, or the user) later runs. Nothing `plan`
/// grants may overlap that way.
#[test]
fn no_path_is_both_write_and_spawn_granted() {
    let grants = plan(&home(), "mur", &browsers(), &[], &[], &[], &[]).grants();
    let writes: Vec<&String> = grants
        .iter()
        .filter_map(|g| match g {
            Grant::Write(w) => Some(w),
            _ => None,
        })
        .collect();
    for g in &grants {
        if let Grant::Dir(d) = g {
            for w in &writes {
                let (d, w) = (Path::new(d), Path::new(w));
                assert!(
                    !d.starts_with(w) && !w.starts_with(d),
                    "{} is an exec lane overlapping write grant {}",
                    d.display(),
                    w.display()
                );
            }
        }
    }
}

/// A profile set up before #1730 holds write + exec on the cache root.
/// Setup must offer both back, revocations first, or the new registry
/// write grant would still sit inside an exec lane.
#[test]
fn legacy_root_grants_are_revoked_first() {
    let root = st(&cache());
    let mut dirs = all_dirs("mur");
    dirs.push(root.clone());
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &s(&["node", "chrome-headless-shell"]),
        &dirs,
        &[server()],
        &[root.clone()],
    );
    assert_eq!(p.revoke_write, Some(root.clone()));
    assert_eq!(p.revoke_dir, Some(root.clone()));
    let cmds = commands("mur", &p);
    assert_eq!(
        cmds[..2],
        [
            format!("mur agent perm remove-path mur write {root}"),
            format!("mur agent perm deny-spawn-dir mur {root}"),
        ],
        "{cmds:?}"
    );
    assert!(cmds.contains(&format!(
        "mur agent perm allow-write mur {}",
        st(&registry())
    )));
}

/// `PLAYWRIGHT_BROWSERS_PATH=0` (or no `HOME`) leaves the cache
/// unlocatable: nothing about it is planned rather than a path guessed.
#[test]
fn unlocatable_cache_is_skipped_not_guessed() {
    let p = plan(
        &home(),
        "mur",
        &Browsers::default(),
        &s(&["node", "chrome-headless-shell"]),
        &[probe("mur")],
        &[server()],
        &[],
    );
    assert!(p.is_empty(), "{p:?}");
}

/// `locate` picks the headless shell replay launches and the full build
/// `auth` needs, and ignores interrupted downloads.
#[test]
fn locate_finds_the_launched_builds_only() {
    let d = tempfile::tempdir().unwrap();
    let mk = |name: &str, exe: Option<&str>, done: bool| {
        let b = d.path().join(name);
        std::fs::create_dir_all(&b).unwrap();
        if let Some(rel) = exe {
            let e = b.join(rel);
            std::fs::create_dir_all(e.parent().unwrap()).unwrap();
            std::fs::write(e, "").unwrap();
        }
        if done {
            std::fs::write(b.join("INSTALLATION_COMPLETE"), "").unwrap();
        }
    };
    let shell = "chrome-headless-shell-linux64/chrome-headless-shell";
    mk("chromium_headless_shell-1246", Some(shell), true);
    mk("chromium_headless_shell-1300", Some(shell), false);
    mk("chromium-1246", None, true);
    let reg = d.path().join("b");
    let b = Browsers::locate(Some(d.path()), Some(&reg));
    assert_eq!(
        b.builds,
        vec![
            d.path().join("chromium_headless_shell-1246"),
            d.path().join("chromium-1246"),
        ]
    );
    assert_eq!(b.registry, Some(reg));
    assert_eq!(b.cache.as_deref(), Some(d.path()));
}

/// The launch chain is `node <install>/…`; the npx-era bare name
/// `playwright-mcp` never resolves to an executable and must not count.
#[test]
fn grants_match_the_vendored_launch_chain() {
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &s(&["playwright-mcp"]),
        &[],
        &[],
        &[],
    );
    assert!(p.binaries.contains(&"node".to_string()), "{p:?}");
    let cmds = commands("mur", &p);
    assert!(
        cmds.iter()
            .any(|c| c == &format!("mur agent perm allow-read mur {}", server())),
        "{cmds:?}"
    );
}

fn run(answer: &str, p: &Plan) -> (bool, String, Vec<String>) {
    let mut out = Vec::new();
    let mut applied = Vec::new();
    let mut grant = |g: &Grant| {
        applied.push(command_for("mur", g));
        Ok(())
    };
    let ok = confirm_and_apply(
        "mur",
        p,
        false,
        &mut answer.as_bytes(),
        &mut out,
        &mut grant,
    )
    .unwrap();
    (ok, String::from_utf8(out).unwrap(), applied)
}

#[test]
fn nothing_is_granted_without_a_literal_yes() {
    let p = plan(&home(), "mur", &browsers(), &[], &[], &[], &[]);
    for a in ["y\n", "YES\n", "\n", "", "no\n"] {
        let (ok, out, applied) = run(a, &p);
        assert!(!ok, "{a:?} must not consent");
        assert!(applied.is_empty(), "{a:?} granted {applied:?}");
        assert!(out.contains("run the commands above"), "{out}");
    }
}

#[test]
fn a_literal_yes_applies_every_missing_grant() {
    let p = plan(&home(), "mur", &browsers(), &[], &[], &[], &[]);
    let (ok, out, applied) = run("yes\n", &p);
    assert!(ok);
    assert_eq!(applied, commands("mur", &p));
    assert!(out.contains("exec lane"), "{out}");
    assert!(out.contains("✓ granted"), "{out}");
}

#[test]
fn legacy_revocation_is_explained_before_asking() {
    let root = st(&cache());
    let p = plan(
        &home(),
        "mur",
        &browsers(),
        &[],
        &[root.clone()],
        &[],
        &[root],
    );
    let (_, out, _) = run("no\n", &p);
    assert!(out.contains("take back old grants"), "{out}");
}

#[test]
fn an_already_granted_profile_asks_nothing() {
    let (ok, out, applied) = run("no\n", &Plan::default());
    assert!(ok, "a no-op must not read as refusal");
    assert!(applied.is_empty());
    assert!(out.contains("already granted"), "{out}");
}
