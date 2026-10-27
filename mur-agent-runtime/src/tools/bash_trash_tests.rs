//! D1 / D9 / D10 through `BashTool::execute`: the real tool, a real shell.

use std::fs;
use std::path::PathBuf;

use mur_common::action::{ActionEvent, DeletionConfig};
use mur_common::ledger::Ledger;
use serde_json::json;

use super::BashTrash;
use crate::tools::bash::BashTool;
use crate::tools::fs_policy::SessionCwd;
use crate::tools::{ToolExecutor, ToolStatus};

const AGENT: &str = "probe";

struct Fixture {
    _t: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn fixture() -> Fixture {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let (home, work) = (base.join("agent"), base.join("work"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();
    Fixture { _t: t, home, work }
}

/// `trash_guard` on (`Some`) or off (`None`), as the supervisor builds it.
fn tool(f: &Fixture, on: bool) -> BashTool {
    BashTool::new(f.home.clone(), SessionCwd::new(f.home.clone())).with_trash(on.then(|| {
        BashTrash {
            agent_home: f.home.clone(),
            agent_name: AGENT.into(),
            deletion: DeletionConfig::default(),
        }
    }))
}

fn events(f: &Fixture) -> Vec<ActionEvent> {
    Ledger::<ActionEvent>::scan_days(&mur_trash::ledger_dir(&f.home), 1)
        .into_iter()
        .filter_map(Result::ok)
        .collect()
}

/// D1 + D9: a relative `rm` resolves against the call's `cwd` (not the agent
/// home), the file lands in trash instead of vanishing, and the reply names
/// the restore command.
#[tokio::test]
async fn rm_moves_into_trash_relative_to_the_call_cwd() {
    let f = fixture();
    fs::write(f.work.join("a.txt"), b"keep me").unwrap();
    // Same name in the agent home: a wrong base would trash this one instead.
    fs::write(f.home.join("a.txt"), b"decoy").unwrap();

    let out = tool(&f, true)
        .execute(json!({"command": "rm a.txt", "cwd": f.work}))
        .await
        .unwrap();

    assert_eq!(out.status, ToolStatus::Ok, "{}", out.text);
    assert!(!f.work.join("a.txt").exists());
    assert!(f.home.join("a.txt").exists(), "decoy must be untouched");
    let id = match events(&f).as_slice() {
        [ActionEvent::TrashCreated { entry }] => {
            let tp = entry.trash_path.as_ref().unwrap();
            assert_eq!(fs::read(tp).unwrap(), b"keep me");
            entry.id
        }
        other => panic!("expected one TrashCreated, got {other:?}"),
    };
    let restore = format!("mur agent trash {AGENT} restore {id}");
    assert!(out.text.contains("moved to trash: "), "{}", out.text);
    assert!(out.text.contains(&restore), "{}", out.text);
}

/// D9: like `rm`, one missing target fails the call (exit 1) but does not
/// stop the others.
#[tokio::test]
async fn a_failed_target_reports_exit_1_and_the_rest_still_move() {
    let f = fixture();
    fs::write(f.work.join("there"), b"x").unwrap();

    let out = tool(&f, true)
        .execute(json!({"command": "rm missing there", "cwd": f.work}))
        .await
        .unwrap();

    assert_eq!(
        out.status,
        ToolStatus::Failed { exit_code: 1 },
        "{}",
        out.text
    );
    assert!(out.text.contains("rm: missing: "), "{}", out.text);
    assert!(out.text.contains("[exit code: 1]"), "{}", out.text);
    assert!(!f.work.join("there").exists());
}

/// D10: `trash_guard: false` runs `rm` for real and records nothing.
#[tokio::test]
async fn switched_off_runs_rm_as_is() {
    let f = fixture();
    fs::write(f.work.join("a.txt"), b"x").unwrap();

    let out = tool(&f, false)
        .execute(json!({"command": "rm a.txt", "cwd": f.work}))
        .await
        .unwrap();

    assert_eq!(out.status, ToolStatus::Ok, "{}", out.text);
    assert!(!f.work.join("a.txt").exists());
    assert!(events(&f).is_empty());
    assert!(!out.text.contains("moved to trash"), "{}", out.text);
}

/// D7 via the tool: a delete MUR cannot rewrite still runs, and leaves an
/// audit line naming the command.
#[tokio::test]
async fn unrewritable_delete_runs_and_is_recorded() {
    let f = fixture();
    fs::write(f.work.join("a.txt"), b"x").unwrap();
    let cmd = "echo hi && rm a.txt";

    let out = tool(&f, true)
        .execute(json!({"command": cmd, "cwd": f.work}))
        .await
        .unwrap();

    assert_eq!(out.status, ToolStatus::Ok, "{}", out.text);
    assert!(
        out.text.contains("hi"),
        "the shell must have run: {}",
        out.text
    );
    assert!(!f.work.join("a.txt").exists());
    match events(&f).as_slice() {
        [ActionEvent::UnprotectedDelete { command, .. }] => assert_eq!(command, cmd),
        other => panic!("expected one UnprotectedDelete, got {other:?}"),
    }
}

/// Not a delete: no interception, no ledger.
#[tokio::test]
async fn ordinary_commands_are_untouched() {
    let f = fixture();
    let out = tool(&f, true)
        .execute(json!({"command": "echo rmdir-is-just-text", "cwd": f.work}))
        .await
        .unwrap();
    assert_eq!(out.status, ToolStatus::Ok, "{}", out.text);
    assert!(out.text.contains("rmdir-is-just-text"), "{}", out.text);
    assert!(events(&f).is_empty());
}
