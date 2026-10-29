use std::io::Cursor;

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
}

impl LiveOps for Fake {
    fn servers(&mut self) -> Result<Vec<McpServerEntry>> {
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

fn run(f: &mut Fake, site: &str, domains: &[&str], answer: Option<&str>) -> (Outcome, String) {
    let mut out = Vec::new();
    let mut input = answer.map(|a| Cursor::new(a.as_bytes().to_vec()));
    let outcome = bind_agent(
        "mur",
        site,
        &d(domains),
        f,
        input.as_mut().map(|c| c as &mut dyn BufRead),
        &mut out,
    )
    .unwrap();
    (outcome, String::from_utf8(out).unwrap())
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
    assert!(out.contains("no one-command unbind"), "{out}");
}

#[test]
fn replacing_a_profile_prints_a_working_undo_with_the_full_host_list() {
    let mut f = Fake {
        servers: vec![restricted(
            &["*.deepseek.com", "gitea.example"],
            Some("gitea"),
        )],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["deepseek.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    assert_eq!(
        f.saves[0][0]
            .args
            .iter()
            .filter(|a| *a == "--profile")
            .count(),
        1
    );
    assert_eq!(f.saves[0][0].args.last().unwrap(), "deepseek");
    assert_eq!(f.bound[0].1.as_deref(), Some("gitea"));
    assert!(out.contains("(was \"gitea\")"), "{out}");
    assert!(
        out.contains("/browser live *.deepseek.com gitea.example --profile gitea"),
        "{out}"
    );
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
        assert!(out.contains("replacing \"old\""), "{out}");
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
    );
    let names: Vec<_> = cands.iter().map(|c| c.agent.as_str()).collect();
    assert_eq!(names, ["a", "c"]);
    assert_eq!(cands[0].current.as_deref(), Some("old"));
    assert_eq!(
        cands[0].plan,
        BindPlan::Bind {
            replaces: Some("old".into())
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
fn set_profile_appends_or_replaces_in_place() {
    let mut e = restricted(&[], None);
    set_profile(&mut e, "a");
    set_profile(&mut e, "b");
    assert_eq!(e.args.iter().filter(|x| *x == "--profile").count(), 1);
    assert_eq!(e.args.last().unwrap(), "b");
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
    assert_eq!(attached_profile(&e), Some("old"));
}

#[test]
fn set_profile_rewrites_an_equals_style_flag_in_place() {
    let mut e = restricted_eq(&[], "old");
    set_profile(&mut e, "new");
    assert_eq!(profile_flags(&e), 1, "{:?}", e.args);
    assert_eq!(e.args.last().unwrap(), "--profile=new");
    assert_eq!(attached_profile(&e), Some("new"));
}

#[test]
fn rebinding_an_equals_style_entry_names_the_old_profile_and_keeps_one_flag() {
    let mut f = Fake {
        servers: vec![restricted_eq(&["*.deepseek.com", "gitea.example"], "gitea")],
        ..Default::default()
    };
    let (outcome, out) = run(&mut f, "deepseek", &["deepseek.com"], None);
    assert_eq!(outcome, Outcome::Bound);
    let after = &f.saves[0][0];
    assert_eq!(profile_flags(after), 1, "{:?}", after.args);
    assert_eq!(attached_profile(after), Some("deepseek"));
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
fn set_profile_never_writes_past_a_double_dash() {
    let mut e = restricted(&[], None);
    e.args
        .extend(["--".to_string(), "--profile=pos".to_string()]);
    set_profile(&mut e, "new");
    let dd = e.args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&e.args[dd - 2..dd], &["--profile", "new"]);
    assert_eq!(e.args.last().unwrap(), "--profile=pos");
    assert_eq!(attached_profile(&e), Some("new"));
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
