//! The apply flow against a fake [`Ops`]: shared installs once, the merged
//! lane question, per-target failure isolation, exit codes, and restart
//! eligibility. No network, no profile writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::exec::apply;
use super::names::{Existing, Naming};
use super::ops::{Ops, merged};
use super::prompt::{Answer, Script};
use super::*;
use crate::cmd::code_nav::plan::{Detected, InstallRow};

#[derive(Default)]
struct Fake {
    fail_shared: BTreeSet<&'static str>,
    fail_install: BTreeSet<usize>,
    fail_commit: BTreeSet<usize>,
    fail_smoke: BTreeSet<usize>,
    lanes: BTreeMap<usize, Vec<PathBuf>>,
    running: BTreeSet<String>,
    restart_fails: BTreeSet<String>,
    log: Vec<String>,
}

impl Ops for Fake {
    type Staged = ();
    fn install_shared(&mut self, row: &InstallRow, _: &mut dyn Write) -> Result<()> {
        self.log.push(format!("shared {}", row.name));
        if self.fail_shared.contains(row.name) {
            bail!("download failed");
        }
        Ok(())
    }
    fn install(&mut self, i: usize, _: &mut dyn Write) -> Result<((), Vec<PathBuf>)> {
        self.log.push(format!("install {i}"));
        if self.fail_install.contains(&i) {
            bail!("boom");
        }
        Ok(((), self.lanes.get(&i).cloned().unwrap_or_default()))
    }
    fn commit(&mut self, i: usize, _: (), _: &mut dyn Write) -> Result<Manifest> {
        self.log.push(format!("commit {i}"));
        if self.fail_commit.contains(&i) {
            bail!("profile locked");
        }
        Ok(Manifest::default())
    }
    fn smoke(&mut self, i: usize, _: &Manifest, _: &mut dyn Write) -> Result<()> {
        if self.fail_smoke.contains(&i) {
            bail!("rust: failed to start");
        }
        Ok(())
    }
    fn running(&self, a: &str) -> bool {
        self.running.contains(a)
    }
    fn restart(&mut self, a: &str) -> Result<(bool, String)> {
        self.log.push(format!("restart {a}"));
        Ok((!self.restart_fails.contains(a), "did not come back".into()))
    }
}

fn ctx(name: &str) -> AgentCtx {
    AgentCtx {
        name: name.into(),
        home: PathBuf::new(),
        existing: Existing::default(),
        manifest_path: PathBuf::new(),
        prior: Manifest::default(),
    }
}

/// `agents.len()` agents, each with one serena target on its own project,
/// planned with uv/node/npm on PATH, nothing applied before.
pub(super) fn decided(agents: &[&str]) -> Decided {
    let detected = Detected {
        on_path: ["uv", "node", "npm"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let mut d = Decided {
        agents: agents.iter().map(|a| ctx(a)).collect(),
        projects: Vec::new(),
        specs: Vec::new(),
        kept: Vec::new(),
        duplicates: Vec::new(),
    };
    for (ai, _) in agents.iter().enumerate() {
        let path = PathBuf::from(format!("/src/p{ai}"));
        d.projects.push(Project {
            name: format!("p{ai}"),
            path: path.clone(),
            indexed: true,
            detected: [Lang::Python].into(),
            base: format!("p{ai}"),
        });
        let flags = Flags {
            with_serena: true,
            lsp: vec!["python".into()],
            ..Default::default()
        };
        let plan = plan::plan(Path::new("/mh"), &flags, &detected).unwrap();
        let entry = format!("p{ai}");
        let diff = consent::diff(&plan, &entry, Some(&path), &Manifest::default());
        d.specs.push(Spec {
            agent: ai,
            project: Some(ai),
            naming: Some(Naming::New {
                entry,
                wanted: None,
            }),
            flags,
            plan,
            diff,
            hold: None,
        });
    }
    d
}

fn go(
    d: &Decided,
    consent: Consent,
    restart: bool,
    s: &mut Script,
    f: &mut Fake,
) -> (Result<()>, String) {
    let mut out = Vec::new();
    let r = apply(d, consent, restart, s, f, &mut out);
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn shared_tools_install_once_for_every_target() {
    let d = decided(&["pm", "qa"]);
    let mut f = Fake::default();
    let (r, out) = go(&d, Consent::Given, false, &mut Script::default(), &mut f);
    r.unwrap();
    let shared: Vec<_> = f.log.iter().filter(|l| l.starts_with("shared")).collect();
    let uniq: BTreeSet<_> = shared.iter().collect();
    assert_eq!(shared.len(), uniq.len(), "{:?}", f.log);
    assert!(f.log.contains(&"shared serena".to_owned()));
    assert!(
        out.contains("--agent pm --with-serena --project /src/p0 --entry p0 --lsp python"),
        "{out}"
    );
}

#[test]
fn one_confirmation_and_no_lane_question_without_new_lanes() {
    let d = decided(&["pm", "qa"]);
    let mut s = Script::new([Answer::Bool(true)]);
    let (r, _) = go(&d, Consent::Ask, false, &mut s, &mut Fake::default());
    r.unwrap();
    assert_eq!(s.asked, ["yes?"]);
}

#[test]
fn declining_the_plan_writes_nothing_and_fails() {
    let d = decided(&["pm"]);
    let mut f = Fake::default();
    let (r, _) = go(
        &d,
        Consent::Ask,
        false,
        &mut Script::new([Answer::Bool(false)]),
        &mut f,
    );
    assert!(r.is_err());
    assert!(f.log.is_empty(), "{:?}", f.log);
}

#[test]
fn lanes_are_asked_once_merged_and_declining_skips_only_those_targets() {
    let d = decided(&["pm", "qa", "dev"]);
    let lane = PathBuf::from("/mh/tools/serena/venv/bin");
    let mut f = Fake {
        lanes: [(0, vec![lane.clone()]), (1, vec![lane.clone()])].into(),
        ..Default::default()
    };
    let mut s = Script::new([Answer::Bool(true), Answer::Bool(false)]);
    let (r, out) = go(&d, Consent::Ask, false, &mut s, &mut f);
    assert!(r.is_err(), "declined lanes fail the run");
    assert_eq!(s.asked.len(), 2, "one plan yes, one merged lane yes");
    assert_eq!(
        out.matches("venv/bin").count(),
        1,
        "lane listed once: {out}"
    );
    assert!(out.contains("→ pm, qa"), "{out}");
    assert!(!f.log.contains(&"commit 0".to_owned()));
    assert!(!f.log.contains(&"commit 1".to_owned()));
    assert!(
        f.log.contains(&"commit 2".to_owned()),
        "dev had no new lane"
    );
}

#[test]
fn a_failed_target_leaves_the_others_and_fails_the_run() {
    let d = decided(&["pm", "qa"]);
    let mut f = Fake {
        fail_install: [0].into(),
        ..Default::default()
    };
    let (r, out) = go(&d, Consent::Given, false, &mut Script::default(), &mut f);
    assert!(r.unwrap_err().to_string().contains("1 of 2"));
    assert!(!f.log.contains(&"commit 0".to_owned()));
    assert!(f.log.contains(&"commit 1".to_owned()));
    assert!(out.contains("✗ pm → /src/p0 (p0): install failed"), "{out}");
}

#[test]
fn a_failed_shared_install_is_tried_once_and_fails_every_dependent() {
    let d = decided(&["pm", "qa"]);
    let mut f = Fake {
        fail_shared: [plan::SERENA].into(),
        ..Default::default()
    };
    let (r, out) = go(&d, Consent::Given, false, &mut Script::default(), &mut f);
    assert!(r.unwrap_err().to_string().contains("2 of 2"));
    assert_eq!(f.log.iter().filter(|l| *l == "shared serena").count(), 1);
    assert!(
        !f.log.iter().any(|l| l.starts_with("install ")),
        "{:?}",
        f.log
    );
    assert_eq!(out.matches("download failed").count(), 1, "{out}");
}

#[test]
fn nothing_new_asks_nothing() {
    let mut d = decided(&["pm"]);
    d.specs[0].diff = Default::default();
    let mut s = Script::default();
    let (r, out) = go(&d, Consent::Ask, false, &mut s, &mut Fake::default());
    r.unwrap();
    assert!(out.contains("Nothing to do"), "{out}");
    assert!(s.asked.is_empty());
}

#[test]
fn skipped_is_not_a_failure_but_blocked_is() {
    let mut d = decided(&["pm"]);
    d.specs[0].hold = Some(Hold::Skipped("user".into()));
    go(
        &d,
        Consent::Given,
        false,
        &mut Script::default(),
        &mut Fake::default(),
    )
    .0
    .unwrap();
    d.specs[0].hold = Some(Hold::Blocked("no record".into()));
    assert!(
        go(
            &d,
            Consent::Given,
            false,
            &mut Script::default(),
            &mut Fake::default()
        )
        .0
        .is_err()
    );
}

#[test]
fn restart_needs_its_own_flag_under_yes() {
    let d = decided(&["pm"]);
    let mut f = Fake {
        running: ["pm".into()].into(),
        ..Default::default()
    };
    let (r, out) = go(&d, Consent::Given, false, &mut Script::default(), &mut f);
    r.unwrap();
    assert!(!f.log.iter().any(|l| l.starts_with("restart")));
    assert!(
        out.contains("Restart to apply: mur agent restart pm"),
        "{out}"
    );

    let (r, _) = go(&d, Consent::Given, true, &mut Script::default(), &mut f);
    r.unwrap();
    assert!(f.log.contains(&"restart pm".to_owned()));
}

#[test]
fn restart_eligibility() {
    // pm: smoke failed (still restarts); qa: commit failed (blocked);
    // dev: stopped; ops: restart fails.
    let d = decided(&["pm", "qa", "dev", "ops"]);
    let mut f = Fake {
        fail_smoke: [0].into(),
        fail_commit: [1].into(),
        running: ["pm", "qa", "ops"].iter().map(|s| s.to_string()).collect(),
        restart_fails: ["ops".into()].into(),
        ..Default::default()
    };
    let mut s = Script::new([Answer::Bool(true), Answer::Bool(true)]);
    let (r, out) = go(&d, Consent::Ask, false, &mut s, &mut f);
    assert!(r.is_err());
    assert!(f.log.contains(&"restart pm".to_owned()));
    assert!(!f.log.contains(&"restart qa".to_owned()));
    assert!(!f.log.contains(&"restart dev".to_owned()));
    assert!(
        out.contains("qa not restarted: a target failed to commit"),
        "{out}"
    );
    assert!(out.contains("dev is stopped"), "{out}");
    assert!(
        out.contains("✗ ops: did not come back — retry: mur agent restart ops"),
        "{out}"
    );
    assert!(
        s.asked.iter().any(|q| q.contains("Restart pm, ops")),
        "{:?}",
        s.asked
    );
}

#[test]
fn merged_keeps_earlier_commits_of_the_same_agent() {
    let mut first = Manifest::default();
    first.set_project("a", Path::new("/a"));
    first.set_languages("a", ["python".to_owned()]);
    first.granted.insert("read /a".into());
    let mut staged = Manifest::default();
    staged.granted.insert("spawn x".into());
    let m = merged(first, staged);
    assert_eq!(m.project_of("a"), Some(Path::new("/a")));
    assert!(m.granted.contains("read /a") && m.granted.contains("spawn x"));
}

#[test]
fn unknown_agent_fails_before_any_output() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = mur_common::test_env::EnvGuard::hold();
    env.set_var("MUR_HOME", tmp.path());
    std::fs::create_dir_all(tmp.path().join("agents/pm")).unwrap();
    let mut profile = mur_common::AgentProfile::default_for_tests();
    profile.name = "pm".into();
    std::fs::write(
        tmp.path().join("agents/pm/profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    let mut out = Vec::new();
    let args = Args {
        agents: vec!["PM".into(), "ghost".into()],
        paths: vec![],
        restart: false,
    };
    let e = run(args, Consent::Given, &mut std::io::empty(), &mut out).unwrap_err();
    assert!(e.to_string().contains("agent 'ghost' not found"), "{e:#}");
    assert!(out.is_empty(), "printed before failing");
}

#[test]
fn no_terminal_and_no_yes_refuses() {
    let args = Args {
        agents: vec!["pm".into()],
        paths: vec![],
        restart: false,
    };
    let mut out = Vec::new();
    let e = run(args, Consent::Impossible, &mut std::io::empty(), &mut out).unwrap_err();
    assert!(e.to_string().contains("no terminal"), "{e:#}");
}
