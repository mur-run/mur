use super::*;
use mur_common::agent::{McpNetMode, McpServerNetwork};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn live_entry() -> McpServerEntry {
    McpServerEntry {
        name: LIVE_ENTRY.into(),
        command: "mur".into(),
        ..Default::default()
    }
}

fn hosts_of(servers: &[McpServerEntry]) -> Vec<String> {
    let e = servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    let net = e.network.as_ref().expect("network set");
    assert_eq!(net.mode, McpNetMode::Restricted);
    net.allow_hosts.clone()
}

// 1. no host → refuse, with a runnable example
#[test]
fn no_host_is_refused_with_an_example() {
    let err = parse_hosts(&[]).unwrap_err();
    assert!(err.contains(USAGE_EXAMPLE), "{err}");
}

// 2. hosts → entry created, allowlist exact, restricted
#[test]
fn hosts_create_the_entry_with_that_allowlist() {
    let hosts = parse_hosts(&s(&["shop-a.example.com", "shop-b.example.com"])).unwrap();
    let mut servers = Vec::new();
    let out = apply(&mut servers, &hosts, live_entry);
    assert!(out.added);
    assert!(out.previous.is_empty());
    assert_eq!(hosts_of(&servers), hosts);
}

// 3. setup not done → refuse, naming the setup command
#[test]
fn missing_setup_is_refused_and_names_the_command() {
    let err = check_setup(false).unwrap_err();
    assert!(err.contains("mur browser setup"), "{err}");
    assert!(check_setup(true).is_ok());
}

// 4. same hosts twice → idempotent, entry not re-created
#[test]
fn repeat_with_same_hosts_is_idempotent() {
    let hosts = s(&["shop-a.example.com"]);
    let mut servers = Vec::new();
    apply(&mut servers, &hosts, live_entry);
    let out = apply(&mut servers, &hosts, || panic!("must not re-add"));
    assert!(!out.added);
    assert_eq!(servers.len(), 1);
    assert_eq!(hosts_of(&servers), hosts);
}

// 5. different hosts → overwrite, previous list reported
#[test]
fn repeat_with_other_hosts_overwrites_and_reports_old_list() {
    let mut servers = Vec::new();
    apply(&mut servers, &s(&["old.example.com"]), live_entry);
    let out = apply(&mut servers, &s(&["new.example.com"]), || {
        panic!("must not re-add")
    });
    assert_eq!(out.previous, s(&["old.example.com"]));
    assert_eq!(hosts_of(&servers), s(&["new.example.com"]));
}

// 5b. an existing entry with a foreign network mode is still overwritten
#[test]
fn existing_entry_network_is_replaced_not_merged() {
    let mut e = live_entry();
    e.network = Some(McpServerNetwork {
        allow_hosts: s(&["x.example.com"]),
        ..Default::default()
    });
    let mut servers = vec![e];
    let out = apply(&mut servers, &s(&["y.example.com"]), || {
        panic!("must not re-add")
    });
    assert_eq!(out.previous, s(&["x.example.com"]));
    assert_eq!(hosts_of(&servers), s(&["y.example.com"]));
}

// 6. --add routing unchanged; live and turns route as designed
#[test]
fn routing_keeps_add_and_splits_live() {
    assert_eq!(route(&s(&["--add"])), Route::Add);
    assert_eq!(
        route(&s(&["live", "a.example.com"])),
        Route::Live(s(&["a.example.com"]))
    );
    assert_eq!(route(&s(&["live"])), Route::Live(vec![]));
    assert_eq!(route(&s(&["check", "the", "cart"])), Route::Turn);
    assert_eq!(route(&[]), Route::Turn);
}

// 7. non-web port → refused (before any write), pointing at allow-port
#[test]
fn non_web_port_is_refused_and_points_at_allow_port() {
    let err = parse_hosts(&s(&["shop-a.example.com:9000"])).unwrap_err();
    assert!(err.contains("perm allow-port"), "{err}");
    assert!(err.contains("9000"), "{err}");
}

// 8. web-set ports are accepted (guards the RESTRICTED_GENERAL_PORTS contract)
#[test]
fn web_set_ports_are_accepted() {
    for p in mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS {
        let h = format!("shop-a.example.com:{p}");
        assert!(parse_hosts(std::slice::from_ref(&h)).is_ok(), "{h}");
    }
    assert!(parse_hosts(&s(&["a.example.com:443", "b.example.com:8080"])).is_ok());
}

// 9. ports are stripped and duplicates collapse
#[test]
fn port_is_normalized_away_and_duplicates_collapse() {
    let hosts = parse_hosts(&s(&["shop-a.example.com:443", "shop-a.example.com"])).unwrap();
    assert_eq!(hosts, s(&["shop-a.example.com"]));
}

// 9b. a trailing colon says the port is empty, not "not a number"
#[test]
fn empty_port_is_named_as_empty() {
    let err = parse_hosts(&s(&["shop-a.example.com:"])).unwrap_err();
    assert!(err.contains("empty"), "{err}");
}

// 9c. a flag-looking arg is never written into the allowlist
#[test]
fn flag_like_host_is_refused() {
    for raw in ["--add", "-x"] {
        let err = parse_hosts(&s(&[raw])).unwrap_err();
        assert!(err.contains("not a host"), "{raw}: {err}");
    }
}

/// Fake [`LiveOps`]: records every call, never touches disk or spawns.
#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    adds: Vec<Vec<String>>,
    saves: usize,
    fail_save: bool,
    fail_add: bool,
    save_notes: Vec<String>,
}

impl LiveOps for Fake {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(self.servers.clone())
    }
    fn mcp_add(&mut self, argv: &[String], network: McpServerNetwork) -> anyhow::Result<String> {
        self.adds.push(argv.to_vec());
        if self.fail_add {
            anyhow::bail!("probe failed");
        }
        let mut e = live_entry();
        e.args = argv.to_vec();
        e.network = Some(network);
        self.servers.push(e);
        Ok("added".into())
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<Vec<String>> {
        self.saves += 1;
        if self.fail_save {
            anyhow::bail!("disk full");
        }
        self.servers = servers;
        Ok(self.save_notes.clone())
    }
}

// flow: refused port → no add, no save, and no chip (it's an Err)
#[test]
fn run_refusal_writes_nothing_and_offers_no_chip() {
    let mut f = Fake::default();
    let err = run("bob", &s(&["shop-a.example.com:9000"]), true, &mut f).unwrap_err();
    assert!(err.to_string().contains("perm allow-port"), "{err}");
    assert!(f.adds.is_empty() && f.saves == 0);
    let mut f = Fake::default();
    assert!(run("bob", &s(&["a.example.com"]), false, &mut f).is_err());
    assert!(f.adds.is_empty() && f.saves == 0);
}

// flow: add once with live-<agent> argv, chip offered, idempotent on rerun
#[test]
fn run_adds_once_with_per_agent_run_and_offers_restart() {
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["old.example.com"]), true, &mut f).unwrap();
    assert_eq!(
        f.adds,
        vec![s(&[
            "browser", "record", "--run", "live-bob", "--mode", "live"
        ])]
    );
    assert!(chip.is_some_and(|p| p.is_executable()), "{text}");
    let (text, _) = run("bob", &s(&["new.example.com"]), true, &mut f).unwrap();
    assert_eq!(f.adds.len(), 1, "second call must not re-add");
    assert!(text.contains("old.example.com"), "{text}");
    assert_eq!(hosts_of(&f.servers), s(&["new.example.com"]));
}

// 11a: a new entry is written ONCE, by `mcp_add`, already restricted — no
// second save, so no window where it exists without an allowlist (#1639).
#[test]
fn new_entry_is_created_with_its_allowlist_in_one_write() {
    let mut f = Fake {
        fail_save: true, // would trip if the create path still saved twice
        ..Fake::default()
    };
    run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert_eq!((f.adds.len(), f.saves), (1, 0));
    let net = f.servers[0]
        .network
        .as_ref()
        .expect("created with a policy");
    assert_eq!(net.mode, McpNetMode::Restricted);
    assert_eq!(hosts_of(&f.servers), s(&["a.example.com"]));
}

// 11b: entry already existed + save fails → leave it alone, Err
#[test]
fn save_failure_keeps_a_preexisting_entry() {
    let mut f = Fake {
        servers: vec![live_entry()],
        fail_save: true,
        ..Fake::default()
    };
    let err = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap_err();
    assert!(f.adds.is_empty());
    assert!(f.servers.iter().any(|e| e.name == LIVE_ENTRY));
    assert!(format!("{err:#}").contains("disk full"), "{err:#}");
}

// 11c: add (probe) fails → Err, nothing persisted, no chip
#[test]
fn failed_add_leaves_no_entry() {
    let mut f = Fake {
        fail_add: true,
        ..Fake::default()
    };
    let err = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap_err();
    assert!(format!("{err:#}").contains("probe failed"), "{err:#}");
    assert!(f.servers.is_empty() && f.saves == 0);
}

// #1639 layer 2: the sealed server dies with EPERM unless all three grants
// exist — read on the install, write on the browser root and the registry.
#[test]
fn live_fs_grants_install_read_and_both_writes() {
    let home = std::path::Path::new("/murhome");
    let reg = std::path::Path::new("/cache/ms-playwright/b");
    let fs = live_fs(home, Some(reg));
    let s = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    assert_eq!(fs.read, vec![s(mur_browser::server::install_dir(home))]);
    assert_eq!(
        fs.write,
        vec![
            s(mur_browser::paths::browser_root(home)),
            "/cache/ms-playwright/b".to_string()
        ]
    );
    assert!(fs.deny.is_empty());
}

// No registry location → no guessed path, the other grants still apply.
#[test]
fn live_fs_without_registry_skips_only_that_grant() {
    let fs = live_fs(std::path::Path::new("/murhome"), None);
    assert_eq!((fs.read.len(), fs.write.len()), (1, 1));
}

// The entry must launch THIS binary by absolute path (B0 rule 6), and the
// `murmur` alias must not leak into the command.
#[test]
fn live_command_is_absolute_and_never_the_murmur_alias() {
    // The alias is swapped with `Path::join`, so the expected value must use
    // the platform separator too (`/opt/bin\mur` on Windows).
    let swapped = std::path::Path::new("/opt/bin").join("mur");
    let p = std::path::PathBuf::from("/opt/bin/murmur");
    assert_eq!(live_command(Ok(p)), swapped.to_string_lossy());
    let p = std::path::PathBuf::from("/opt/bin/mur");
    assert_eq!(live_command(Ok(p)), "/opt/bin/mur");
    assert_eq!(live_command(Err(std::io::Error::other("x"))), "mur");
}

// Grants merge exactly once and report only what was new.
#[test]
fn merge_fs_adds_missing_paths_once() {
    use mur_common::agent::FilesystemEntitlement;
    let mut dst = FilesystemEntitlement {
        read: s(&["/a"]),
        ..Default::default()
    };
    let add = FilesystemEntitlement {
        read: s(&["/a", "/b"]),
        write: s(&["/w"]),
        ..Default::default()
    };
    let notes = super::manage::merge_fs(&mut dst, &add);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!((dst.read, dst.write), (s(&["/a", "/b"]), s(&["/w"])));
    let mut again = FilesystemEntitlement {
        read: s(&["/a", "/b"]),
        write: s(&["/w"]),
        ..Default::default()
    };
    assert!(super::manage::merge_fs(&mut again, &add).is_empty());
}

// Backfill: re-running `/browser live` on an entry that predates the fs
// grants surfaces the grants `save` added, alongside the allowlist note.
#[test]
fn existing_entry_save_reports_backfilled_grants() {
    let mut ops = Fake {
        servers: vec![live_entry()],
        save_notes: vec!["granted write on /x/browser".into()],
        ..Default::default()
    };
    let (text, chip) = run("bot", &["example.com".into()], true, &mut ops).unwrap();
    assert_eq!(ops.saves, 1);
    assert!(ops.adds.is_empty(), "existing entry must not be re-added");
    assert!(text.contains("granted write on /x/browser"), "{text}");
    assert!(text.contains("example.com"), "{text}");
    assert!(chip.is_some());
}

// existing bare-`mur` entry → absolute command + fresh pin, one note
#[test]
fn repin_rewrites_bare_command_and_hash() {
    let mut e = live_entry();
    e.binary_sha256 = Some("old".into());
    e.description_hash = Some("tools".into());
    let note = repin_command(&mut e, "/opt/mur/bin/mur", "abc123".into()).expect("changed");
    assert!(note.contains("/opt/mur/bin/mur"), "{note}");
    assert_eq!(e.command, "/opt/mur/bin/mur");
    assert_eq!(e.binary_sha256.as_deref(), Some("abc123"));
    // the Playwright tools are unchanged, so their pin stays
    assert_eq!(e.description_hash.as_deref(), Some("tools"));
}

// already absolute and pinned to the same bytes → no-op, no note
#[test]
fn repin_is_a_noop_when_already_current() {
    let mut e = live_entry();
    e.command = "/opt/mur/bin/mur".into();
    e.binary_sha256 = Some("ABC123".into());
    assert!(repin_command(&mut e, "/opt/mur/bin/mur", "abc123".into()).is_none());
    assert_eq!(e.binary_sha256.as_deref(), Some("ABC123"));
}

// same path but the binary was rebuilt → re-pinned, not left drifted
#[test]
fn repin_refreshes_hash_after_rebuild() {
    let mut e = live_entry();
    e.command = "/opt/mur/bin/mur".into();
    e.binary_sha256 = Some("old".into());
    assert!(repin_command(&mut e, "/opt/mur/bin/mur", "new".into()).is_some());
    assert_eq!(e.binary_sha256.as_deref(), Some("new"));
}

#[test]
fn spawn_allowlist_gets_absolute_command_once() {
    let abs = "/opt/x/target/debug/mur";
    let mut allowed = vec!["mur".to_string()];
    let note = ensure_spawn_allowed(&mut allowed, abs);
    assert_eq!(allowed, vec!["mur".to_string(), abs.to_string()]);
    assert!(note.unwrap().contains(abs));
    assert_eq!(ensure_spawn_allowed(&mut allowed, abs), None);
    assert_eq!(allowed.len(), 2);
}

// Rerun with the same hosts and nothing for `save` to fix: no restart chip,
// and the reply says so instead of "profile updated".
#[test]
fn unchanged_rerun_offers_no_restart() {
    let mut f = Fake::default();
    run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    let (text, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none(), "{text}");
    assert!(text.contains("nothing changed"), "{text}");
    // A different allowlist is a real change again.
    let (_, chip) = run("bob", &s(&["b.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_some());
}

// #1639: the sealed live server execs Playwright's headless shell, which
// lives outside every allowlisted binary — the grant must be its build dir.
#[test]
fn live_spawn_dir_is_the_newest_complete_headless_build() {
    let d = tempfile::tempdir().unwrap();
    for (name, done) in [
        ("chromium_headless_shell-1217", true),
        ("chromium_headless_shell-1246", true),
        ("chromium_headless_shell-1300", false),
    ] {
        let exe = d
            .path()
            .join(name)
            .join("chrome-headless-shell-mac-arm64/chrome-headless-shell");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        if done {
            std::fs::write(d.path().join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
        }
    }
    let want = d.path().join("chromium_headless_shell-1246");
    assert_eq!(
        live_spawn_dir(Some(d.path())),
        Some(want.to_string_lossy().into_owned())
    );
}

#[test]
fn live_spawn_dir_is_none_without_a_shell() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(live_spawn_dir(Some(d.path())), None);
    assert_eq!(live_spawn_dir(None), None);
}

#[test]
fn spawn_dir_is_granted_once_with_a_note() {
    let mut dirs = vec!["/keep".to_string()];
    let note = ensure_spawn_dir(&mut dirs, "/b/shell-1246");
    assert_eq!(dirs, s(&["/keep", "/b/shell-1246"]));
    assert!(note.unwrap().contains("/b/shell-1246"));
    assert_eq!(ensure_spawn_dir(&mut dirs, "/b/shell-1246"), None);
}
