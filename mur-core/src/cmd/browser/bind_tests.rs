use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;
use crate::cmd::browser::live_effect::Effect;

fn entry(
    mode: Option<McpNetMode>,
    allow: &[&str],
    deny: &[&str],
    profile: Option<&str>,
) -> McpServerEntry {
    let mut args = vec![
        "browser".to_string(),
        "record".to_string(),
        "--run".to_string(),
        "live-a".to_string(),
    ];
    if let Some(p) = profile {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let mut e: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": args }),
    )
    .unwrap();
    e.network = mode.map(|mode| McpServerNetwork {
        mode,
        allow_hosts: allow.iter().map(|s| s.to_string()).collect(),
        deny_hosts: deny.iter().map(|s| s.to_string()).collect(),
        authorization: None,
    });
    e
}

fn restricted(allow: &[&str], profile: Option<&str>) -> McpServerEntry {
    entry(Some(McpNetMode::Restricted), allow, &[], profile)
}

fn d(hosts: &[&str]) -> Vec<String> {
    hosts.iter().map(|s| s.to_string()).collect()
}

#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    repairs: Vec<String>,
    saves: Vec<Vec<McpServerEntry>>,
    bound: Vec<(String, Option<String>, String)>,
    fail_record: bool,
    /// What another writer puts on disk while the confirmation is open; the
    /// next `servers()` read sees it.
    concurrent: Rc<RefCell<Option<Vec<McpServerEntry>>>>,
}

impl LiveOps for Fake {
    fn servers(&mut self) -> Result<Vec<McpServerEntry>> {
        if let Some(newer) = self.concurrent.borrow_mut().take() {
            self.servers = newer;
        }
        Ok(self.servers.clone())
    }
    fn mcp_add(&mut self, _: &[String], _: McpServerNetwork) -> Result<String> {
        panic!("the binder never creates an entry")
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> Result<(Vec<String>, Effect)> {
        self.saves.push(servers.clone());
        self.servers = servers;
        Ok((self.repairs.clone(), Effect::NextTurn))
    }
    fn save_if_unchanged(
        &mut self,
        expected: &[McpServerEntry],
        servers: Vec<McpServerEntry>,
    ) -> Result<Option<(Vec<String>, Effect)>> {
        if self.servers()? != expected {
            return Ok(None);
        }
        self.save(servers).map(Some)
    }
    fn pending_repairs(&mut self, _: &[McpServerEntry]) -> Result<Vec<String>> {
        Ok(self.repairs.clone())
    }
    fn record_research(&mut self, _: &str) -> Result<()> {
        panic!("the binder never enables research mode")
    }
    fn record_profile_bound(&mut self, agent: &str, old: Option<&str>, new: &str) -> Result<()> {
        if self.fail_record {
            anyhow::bail!("traces dir read-only");
        }
        self.bound
            .push((agent.into(), old.map(Into::into), new.into()));
        Ok(())
    }
}

/// Bound-profile lookup for tests: whatever is already bound has no domains
/// of its own, so it neither overlaps the incoming site nor needs covering by
/// the entry's allowlist (#2263 checks the union), and adding alongside is
/// the ordinary outcome. `bind_plan_tests` covers a retained profile whose
/// hosts are no longer allowed.
fn disjoint(_: &str) -> Option<Vec<String>> {
    Some(Vec::new())
}

fn run(f: &mut Fake, site: &str, domains: &[&str], answer: Option<&str>) -> (Outcome, String) {
    run_with(f, site, domains, &disjoint, false, answer)
}

fn run_with(
    f: &mut Fake,
    site: &str,
    domains: &[&str],
    bound: &BoundDomains<'_>,
    replace: bool,
    answer: Option<&str>,
) -> (Outcome, String) {
    let mut out = Vec::new();
    let mut input = answer.map(|a| Cursor::new(a.as_bytes().to_vec()));
    let outcome = bind_agent(
        "mur",
        site,
        &d(domains),
        bound,
        replace,
        f,
        input.as_mut().map(|c| c as &mut dyn BufRead),
        &mut out,
    )
    .unwrap();
    (outcome, String::from_utf8(out).unwrap())
}

fn profiles_of(e: &McpServerEntry) -> Vec<&str> {
    attached_profiles(e)
}

#[test]
fn bind_rewrites_only_the_profile_argv_and_records_a_trace() {
    let before = restricted(&["*.deepseek.com", "gitea.example"], None);
    let mut f = Fake {
        servers: vec![before.clone()],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["chat.deepseek.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    let after = &f.saves[0][0];
    assert_eq!(
        after.network, before.network,
        "network policy is never touched"
    );
    assert_eq!(&after.args[..4], &before.args[..]);
    assert_eq!(&after.args[4..], &["--profile", "deepseek"]);
    assert_eq!(f.bound, vec![("mur".into(), None, "deepseek".into())]);
    assert!(out.contains("bound \"deepseek\" to mur"), "{out}");
    assert!(
        out.contains("/browser live *.deepseek.com gitea.example --no-profile"),
        "{out}"
    );
}

/// #2263: a second profile for other hosts joins the first instead of
/// evicting it, and the undo re-points the entry at exactly what it had.
#[test]
fn a_disjoint_profile_is_added_alongside_and_the_undo_restores_the_old_list() {
    let mut f = Fake {
        servers: vec![restricted(
            &["*.deepseek.com", "gitea.example"],
            Some("gitea"),
        )],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["deepseek.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    assert_eq!(profiles_of(&f.saves[0][0]), ["gitea", "deepseek"]);
    assert_eq!(
        f.saves[0][0].network,
        restricted(&["*.deepseek.com", "gitea.example"], None).network
    );
    // Nothing was replaced, so the trace's `old` is empty.
    assert_eq!(f.bound[0].1, None);
    assert!(
        out.contains("bound \"deepseek\" to mur alongside \"gitea\""),
        "{out}"
    );
    assert!(
        out.contains("/browser live *.deepseek.com gitea.example --profile gitea"),
        "{out}"
    );
    assert!(!out.contains("replacing"), "{out}");
}

#[test]
fn replace_drops_the_bound_profiles_and_prints_a_working_undo() {
    let mut f = Fake {
        servers: vec![restricted(
            &["*.deepseek.com", "gitea.example", "x.com"],
            Some("gitea"),
        )],
        ..Default::default()
    };
    f.servers[0]
        .args
        .extend(["--profile".to_string(), "x".to_string()]);
    let (outcome, out) = run_with(&mut f, "deepseek", &["deepseek.com"], &disjoint, true, None);
    assert_eq!(outcome, Outcome::Bound);
    assert_eq!(profiles_of(&f.saves[0][0]), ["deepseek"]);
    assert_eq!(f.bound[0].1.as_deref(), Some("gitea,x"));
    assert!(out.contains("(was \"gitea\", \"x\")"), "{out}");
    assert!(
        out.contains(
            "/browser live *.deepseek.com gitea.example x.com --profile gitea --profile x"
        ),
        "{out}"
    );
}

/// Two logins for one host never share a session: the add is refused, the
/// message names the clash and both ways to replace, and nothing is written.
#[test]
fn an_overlapping_profile_is_refused_without_replace() {
    let mut f = Fake {
        servers: vec![restricted(
            &["*.deepseek.com", "deepseek.com"],
            Some("old-ds"),
        )],
        ..Default::default()
    };
    let old = |s: &str| (s == "old-ds").then(|| d(&["deepseek.com"]));
    let (outcome, out) = run_with(&mut f, "ds", &["chat.deepseek.com"], &old, false, None);
    assert_eq!(outcome, Outcome::Refused);
    assert!(f.saves.is_empty() && f.bound.is_empty());
    assert!(
        out.contains("shares chat.deepseek.com with bound profile \"old-ds\""),
        "{out}"
    );
    assert!(out.contains("--bind mur --replace"), "{out}");
    assert!(out.contains("/browser live <hosts> --profile ds"), "{out}");

    let (outcome, _) = run_with(&mut f, "ds", &["chat.deepseek.com"], &old, true, None);
    assert_eq!(outcome, Outcome::Bound);
    assert_eq!(profiles_of(&f.saves[0][0]), ["ds"]);
}

/// `--replace` on an entry that already carries the site still drops the
/// others; without it, an already-bound site is a no-op.
#[test]
fn replace_with_the_site_already_bound_drops_only_the_others() {
    let mut f = Fake {
        servers: vec![restricted(&["x.com", "y.com"], Some("x"))],
        ..Default::default()
    };
    f.servers[0]
        .args
        .extend(["--profile".to_string(), "y".to_string()]);
    let (outcome, _) = run(&mut f, "x", &["x.com"], None);
    assert_eq!(outcome, Outcome::Unchanged);
    let (outcome, out) = run_with(&mut f, "x", &["x.com"], &disjoint, true, None);
    assert_eq!(outcome, Outcome::Bound, "{out}");
    assert_eq!(profiles_of(&f.saves[0][0]), ["x"]);
    assert_eq!(f.bound[0].1.as_deref(), Some("y"));
}

#[test]
fn research_entry_undo_reruns_research_not_an_empty_host_list() {
    let mut f = Fake {
        servers: vec![entry(Some(McpNetMode::BroadAudited), &[], &[], Some("old"))],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["deepseek.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    assert!(
        out.contains("/browser live --research --profile old"),
        "{out}"
    );
}

#[test]
fn needs_hosts_writes_nothing_and_prints_current_union_missing() {
    let mut f = Fake {
        servers: vec![restricted(&["gitea.example"], None)],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["chat.deepseek.com"], None);
    assert_eq!(outcome, Outcome::Refused);
    assert!(f.saves.is_empty() && f.bound.is_empty());
    assert!(
        out.contains("/browser live gitea.example chat.deepseek.com --profile deepseek"),
        "{out}"
    );
}

#[test]
fn denied_and_not_host_scoped_write_nothing_and_print_no_command() {
    for e in [
        entry(
            Some(McpNetMode::BroadAudited),
            &[],
            &["*.deepseek.com"],
            None,
        ),
        entry(Some(McpNetMode::Inherit), &[], &[], None),
        entry(None, &[], &[], None),
    ] {
        let mut f = Fake {
            servers: vec![e],
            ..Default::default()
        };
        let (outcome, out) = run(&mut f, "deepseek", &["chat.deepseek.com"], None);
        assert_eq!(outcome, Outcome::Refused, "{out}");
        assert!(f.saves.is_empty());
        assert!(!out.contains("/browser live"), "{out}");
    }
}

#[test]
fn no_live_entry_is_refused_with_a_create_hint() {
    let mut f = Fake::default();
    let (outcome, out) = run(&mut f, "deepseek", &["deepseek.com"], None);
    assert_eq!(outcome, Outcome::Refused);
    assert!(out.contains("no live browser entry"), "{out}");
}

#[test]
fn already_bound_is_unchanged_without_a_write() {
    let mut f = Fake {
        servers: vec![restricted(&["*.x.com"], Some("x"))],
        ..Default::default()
    };
    let (outcome, _) = run(&mut f, "x", &["a.x.com"], Some("y\n"));
    assert_eq!(outcome, Outcome::Unchanged);
    assert!(f.saves.is_empty() && f.bound.is_empty());
}

#[test]
fn confirm_shows_repairs_and_only_yes_writes() {
    for (answer, want) in [
        ("\n", Outcome::Declined),
        ("n\n", Outcome::Declined),
        ("", Outcome::Declined),
        ("y\n", Outcome::Bound),
    ] {
        let mut f = Fake {
            servers: vec![restricted(&["*.x.com"], Some("old"))],
            repairs: vec!["re-pinned the command".into()],
            ..Default::default()
        };
        let (outcome, out) = run(&mut f, "x", &["a.x.com"], Some(answer));
        assert_eq!(outcome, want, "answer {answer:?}");
        assert!(out.contains("alongside \"old\""), "{out}");
        assert!(
            out.contains("also repairs:\n  re-pinned the command"),
            "{out}"
        );
        assert_eq!(f.saves.len(), usize::from(want == Outcome::Bound));
    }
}

#[test]
fn a_failed_trace_does_not_undo_the_write() {
    let mut f = Fake {
        servers: vec![restricted(&["*.x.com"], None)],
        fail_record: true,
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "x", &["a.x.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    assert_eq!(f.saves.len(), 1);
    assert!(
        out.contains("could not record the profile-binding audit event"),
        "{out}"
    );
}

#[test]
fn candidates_list_only_agents_with_a_live_entry() {
    let other: McpServerEntry =
        serde_json::from_value(serde_json::json!({ "name": "git", "command": "mcp-git" })).unwrap();
    let cands = candidates(
        vec![
            ("a".into(), vec![restricted(&["*.x.com"], Some("old"))]),
            ("b".into(), vec![other]),
            ("c".into(), vec![restricted(&["y.com"], None)]),
        ],
        "x",
        &d(&["a.x.com"]),
        &disjoint,
    );
    let names: Vec<_> = cands.iter().map(|c| c.agent.as_str()).collect();
    assert_eq!(names, ["a", "c"]);
    assert_eq!(cands[0].current, ["old"]);
    assert_eq!(
        cands[0].plan,
        BindPlan::Bind {
            alongside: vec!["old".into()],
            replaces: Vec::new(),
        }
    );
    assert!(matches!(cands[1].plan, BindPlan::NeedsHosts { .. }));
}

fn pick(cands: &[Candidate], input: &str) -> (Option<usize>, String) {
    let mut out = Vec::new();
    let got = menu(
        "x",
        cands,
        &mut Cursor::new(input.as_bytes().to_vec()),
        &mut out,
    )
    .unwrap();
    (got, String::from_utf8(out).unwrap())
}

#[test]
fn menu_enter_or_eof_skips_and_bad_input_reasks() {
    let cands = candidates(
        vec![("mur".into(), vec![restricted(&["y.com"], None)])],
        "x",
        &d(&["a.x.com"]),
        &disjoint,
    );
    assert_eq!(pick(&cands, "\n").0, None);
    assert_eq!(pick(&cands, "").0, None);
    let (got, out) = pick(&cands, "7\nabc\n1\n");
    assert_eq!(got, Some(0));
    assert_eq!(out.matches("not a choice").count(), 2, "{out}");
    assert!(
        out.contains("[1] mur  → none  ⚠ allow_hosts lacks a.x.com"),
        "{out}"
    );
}

#[test]
fn menu_with_no_candidates_prints_the_hint_and_reads_nothing() {
    let (got, out) = pick(&[], "1\n");
    assert_eq!(got, None);
    assert!(out.contains("no agent has a live browser entry"), "{out}");
}

#[test]
fn set_profiles_writes_exactly_the_given_list_in_place() {
    let mut e = restricted(&[], None);
    set_profiles(&mut e, &d(&["a"]));
    set_profiles(&mut e, &d(&["a", "b"]));
    assert_eq!(e.args.iter().filter(|x| *x == "--profile").count(), 2);
    assert_eq!(profiles_of(&e), ["a", "b"]);
    set_profiles(&mut e, &d(&["b"]));
    assert_eq!(profiles_of(&e), ["b"]);
    assert_eq!(&e.args[4..], &["--profile", "b"]);
    set_profiles(&mut e, &[]);
    assert_eq!(e.args.len(), 4);
}

#[test]
fn attached_profiles_reads_both_spellings_in_argv_order() {
    let mut e = restricted(&[], None);
    e.args.extend([
        "--profile=one".to_string(),
        "--headless".to_string(),
        "--profile".to_string(),
        "two".to_string(),
    ]);
    assert_eq!(profiles_of(&e), ["one", "two"]);
}

/// A hand-edited entry may spell the flag `--profile=old` (clap accepts both).
fn restricted_eq(allow: &[&str], site: &str) -> McpServerEntry {
    let mut e = restricted(allow, None);
    e.args.push(format!("--profile={site}"));
    e
}

fn profile_flags(e: &McpServerEntry) -> usize {
    e.args
        .iter()
        .filter(|a| *a == "--profile" || a.starts_with("--profile="))
        .count()
}

#[test]
fn equals_style_profile_is_read_as_the_attached_profile() {
    let e = restricted_eq(&["*.x.com"], "old");
    assert_eq!(profiles_of(&e), ["old"]);
}

#[test]
fn set_profiles_rewrites_an_equals_style_flag_in_place() {
    let mut e = restricted_eq(&[], "old");
    set_profiles(&mut e, &d(&["new"]));
    assert_eq!(profile_flags(&e), 1, "{:?}", e.args);
    assert_eq!(&e.args[e.args.len() - 2..], &["--profile", "new"]);
    assert_eq!(profiles_of(&e), ["new"]);
}

#[test]
fn replacing_an_equals_style_entry_names_the_old_profile_and_keeps_one_flag() {
    let mut f = Fake {
        servers: vec![restricted_eq(&["*.deepseek.com", "gitea.example"], "gitea")],
        ..Default::default()
    };
    let (outcome, out) = run_with(&mut f, "deepseek", &["deepseek.com"], &disjoint, true, None);
    assert_eq!(outcome, Outcome::Bound);
    let after = &f.saves[0][0];
    assert_eq!(profile_flags(after), 1, "{:?}", after.args);
    assert_eq!(profiles_of(after), ["deepseek"]);
    assert_eq!(f.bound[0].1.as_deref(), Some("gitea"));
    assert!(out.contains("(was \"gitea\")"), "{out}");
    assert!(
        out.contains("/browser live *.deepseek.com gitea.example --profile gitea"),
        "{out}"
    );
}

#[test]
fn equals_style_entry_already_bound_is_unchanged_without_a_write() {
    let mut f = Fake {
        servers: vec![restricted_eq(&["*.x.com"], "x")],
        ..Default::default()
    };
    let (outcome, _) = run(&mut f, "x", &["a.x.com"], Some("y\n"));
    assert_eq!(outcome, Outcome::Unchanged);
    assert!(f.saves.is_empty() && f.bound.is_empty());
}

#[test]
fn set_profiles_never_writes_past_a_double_dash() {
    let mut e = restricted(&[], None);
    e.args
        .extend(["--".to_string(), "--profile=pos".to_string()]);
    set_profiles(&mut e, &d(&["new"]));
    let dd = e.args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&e.args[dd - 2..dd], &["--profile", "new"]);
    assert_eq!(e.args.last().unwrap(), "--profile=pos");
    assert_eq!(profiles_of(&e), ["new"]);
}

#[test]
fn flag_succeeds_only_on_bound_or_unchanged() {
    assert!(flag_result(&Outcome::Bound, "mur", "x").is_ok());
    assert!(flag_result(&Outcome::Unchanged, "mur", "x").is_ok());
    for o in [Outcome::Refused, Outcome::Declined] {
        let e = flag_result(&o, "mur", "deepseek").unwrap_err().to_string();
        assert!(e.contains("saved but not bound to mur"), "{e}");
    }
}

#[test]
fn flag_path_writes_without_asking_and_refusals_stay_unwritten() {
    let mut f = Fake {
        servers: vec![restricted(&["chat.deepseek.com"], None)],
        ..Default::default()
    };
    let (o, out) = run(&mut f, "deepseek", &["chat.deepseek.com"], None);
    assert_eq!(o, Outcome::Bound);
    assert!(!out.contains("proceed?"), "{out}");
    assert_eq!(f.saves.len(), 1);

    let mut f = Fake {
        servers: vec![restricted(&["gitea.example.com"], None)],
        ..Default::default()
    };
    let (o, _) = run(&mut f, "deepseek", &["chat.deepseek.com"], None);
    assert_eq!(o, Outcome::Refused);
    assert!(f.saves.is_empty());
    assert!(flag_result(&o, "mur", "deepseek").is_err());
}

#[test]
fn resolve_agent_is_case_insensitive_and_rejects_unknown_names() {
    let home = tempfile::TempDir::new().unwrap();
    let dir = home.path().join("agents").join("reviewer");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), "name: reviewer\n").unwrap();
    assert_eq!(resolve_agent(home.path(), "Reviewer").unwrap(), "reviewer");
    let e = resolve_agent(home.path(), "nobody")
        .unwrap_err()
        .to_string();
    assert!(e.contains("no such agent"), "{e}");
}

/// #2205 review: two case variants (case-sensitive filesystems only) with no
/// exact match must fail before login, not bind whichever `read_dir` lists
/// first. The selection itself is covered on every filesystem by
/// `a2a_dial::tests::match_agent_name_several_folds_is_ambiguous_in_any_order`.
#[test]
fn resolve_agent_rejects_ambiguous_case_variants() {
    let home = tempfile::TempDir::new().unwrap();
    for n in ["reviewer", "Reviewer"] {
        let dir = home.path().join("agents").join(n);
        if n == "Reviewer" && dir.exists() {
            return; // case-insensitive filesystem: the scenario cannot exist
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), format!("name: {n}\n")).unwrap();
    }
    assert_eq!(resolve_agent(home.path(), "Reviewer").unwrap(), "Reviewer");
    assert_eq!(resolve_agent(home.path(), "reviewer").unwrap(), "reviewer");
    let e = resolve_agent(home.path(), "REVIEWER")
        .unwrap_err()
        .to_string();
    assert!(e.contains("ambiguous"), "{e}");
    assert!(e.contains("Reviewer") && e.contains("reviewer"), "{e}");
}

/// Answers the confirmation, but first lets "another terminal" write `newer`
/// to disk — the window the user spends reading the prompt.
struct EditWhileConfirming {
    answer: Cursor<Vec<u8>>,
    disk: Rc<RefCell<Option<Vec<McpServerEntry>>>>,
    newer: Option<Vec<McpServerEntry>>,
}

impl std::io::Read for EditWhileConfirming {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.answer.read(buf)
    }
}

impl BufRead for EditWhileConfirming {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if let Some(newer) = self.newer.take() {
            *self.disk.borrow_mut() = Some(newer);
        }
        self.answer.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.answer.consume(n)
    }
}

fn confirm_while_edited(f: &mut Fake, newer: Vec<McpServerEntry>) -> (Outcome, String) {
    let mut input = EditWhileConfirming {
        answer: Cursor::new(b"y\n".to_vec()),
        disk: f.concurrent.clone(),
        newer: Some(newer),
    };
    let mut out = Vec::new();
    let outcome = bind_agent(
        "mur",
        "x",
        &d(&["a.x.com"]),
        &disjoint,
        false,
        f,
        Some(&mut input),
        &mut out,
    )
    .unwrap();
    (outcome, String::from_utf8(out).unwrap())
}

#[test]
fn a_host_removed_during_confirmation_is_not_restored() {
    let mut f = Fake {
        servers: vec![restricted(&["*.x.com", "gitea.example"], None)],
        ..Default::default()
    };
    let narrowed = vec![restricted(&["gitea.example"], None)];
    let (outcome, out) = confirm_while_edited(&mut f, narrowed.clone());
    assert_eq!(outcome, Outcome::Refused, "{out}");
    assert!(f.saves.is_empty(), "the stale snapshot was written");
    assert_eq!(f.servers, narrowed, "the newer policy survives");
    assert!(f.bound.is_empty());
    assert!(out.contains("changed while this was waiting"), "{out}");
}

#[test]
fn an_entry_deleted_during_confirmation_is_not_recreated() {
    let mut f = Fake {
        servers: vec![restricted(&["*.x.com"], None)],
        ..Default::default()
    };
    let (outcome, _) = confirm_while_edited(&mut f, Vec::new());
    assert_eq!(outcome, Outcome::Refused);
    assert!(f.saves.is_empty() && f.servers.is_empty());
}

#[test]
fn another_mcp_entry_added_during_confirmation_is_not_lost() {
    let mut f = Fake {
        servers: vec![restricted(&["*.x.com"], None)],
        ..Default::default()
    };
    let git: McpServerEntry =
        serde_json::from_value(serde_json::json!({ "name": "git", "command": "mcp-git" })).unwrap();
    let newer = vec![restricted(&["*.x.com"], None), git];
    let (outcome, _) = confirm_while_edited(&mut f, newer.clone());
    assert_eq!(outcome, Outcome::Refused);
    assert_eq!(f.servers, newer);
}
