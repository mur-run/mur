//! `mur agent turn {list,undo}` end to end: the snapshot is written through
//! the same public `mur_track::UndoStore::snapshot` the runtime calls before
//! promote, the promote is simulated by writing the track's bytes into the
//! project, and every assertion goes through the `mur` binary.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use mur_track::UndoStore;
use tempfile::TempDir;

const AGENT: &str = "undo_agent";
const TURN: &str = "turn-0001";

struct Fixture {
    mur_home: TempDir,
    project: TempDir,
    _track: TempDir,
}

impl Fixture {
    /// Project before the turn: `a.txt`=old, `c.txt`=doomed.
    /// The turn: modifies `a.txt`, adds `b.txt`, deletes `c.txt`.
    fn promoted() -> Self {
        let mur_home = TempDir::new().unwrap();
        let agent_home = mur_home.path().join("agents").join(AGENT);
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::write(agent_home.join("profile.yaml"), format!("name: {AGENT}\n")).unwrap();

        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("a.txt"), "old\n").unwrap();
        std::fs::write(project.path().join("c.txt"), "doomed\n").unwrap();

        let track = TempDir::new().unwrap();
        std::fs::write(track.path().join("a.txt"), "new\n").unwrap();
        std::fs::write(track.path().join("b.txt"), "added\n").unwrap();

        let files: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"].map(PathBuf::from).into();
        UndoStore::new(&agent_home)
            .snapshot(TURN, project.path(), track.path(), &files)
            .unwrap();

        // The promote itself.
        std::fs::write(project.path().join("a.txt"), "new\n").unwrap();
        std::fs::write(project.path().join("b.txt"), "added\n").unwrap();
        std::fs::remove_file(project.path().join("c.txt")).unwrap();

        Self {
            mur_home,
            project,
            _track: track,
        }
    }

    fn file(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.project.path().join(rel)).ok()
    }

    fn mur(&self, args: &[&str], stdin: Option<&str>) -> Output {
        run(self.mur_home.path(), args, stdin)
    }
}

fn run(mur_home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", mur_home)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mur");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("wait mur")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn assert_ok(o: &Output) {
    assert!(
        o.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(o),
        stderr(o)
    );
}

fn assert_untouched(f: &Fixture) {
    assert_eq!(f.file("a.txt").as_deref(), Some("new\n"));
    assert_eq!(f.file("b.txt").as_deref(), Some("added\n"));
    assert_eq!(f.file("c.txt"), None);
}

#[test]
fn list_json_shows_the_promoted_turn() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "list", AGENT, "--json"], None);
    assert_ok(&out);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let turns = v.as_array().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["turn"], TURN);
    assert_eq!(turns[0]["entries"].as_array().unwrap().len(), 3);
}

#[test]
fn agent_name_lookup_is_case_insensitive() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "list", "UNDO_AGENT", "--json"], None);
    assert_ok(&out);
    assert!(stdout(&out).contains(TURN));
}

#[test]
fn dry_run_lists_the_plan_and_writes_nothing() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN, "--dry-run"], None);
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("restore a.txt"), "{s}");
    assert!(s.contains("remove  b.txt"), "{s}");
    assert!(s.contains("restore c.txt"), "{s}");
    assert!(s.contains("dry run — nothing written"), "{s}");
    assert_untouched(&f);
}

#[test]
fn undo_yes_restores_the_before_state() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN, "--yes"], None);
    assert_ok(&out);
    assert!(stdout(&out).contains("1 removed"), "{}", stdout(&out));
    assert_eq!(f.file("a.txt").as_deref(), Some("old\n"));
    assert_eq!(f.file("b.txt"), None);
    assert_eq!(f.file("c.txt").as_deref(), Some("doomed\n"));
}

#[test]
fn second_undo_of_the_same_turn_is_refused() {
    let f = Fixture::promoted();
    assert_ok(&f.mur(&["agent", "turn", "undo", AGENT, TURN, "--yes"], None));
    // Something lands after the undo; a second undo must not clobber it.
    std::fs::write(f.project.path().join("a.txt"), "later\n").unwrap();

    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN, "--yes"], None);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("already undone"), "{}", stderr(&out));
    assert_eq!(f.file("a.txt").as_deref(), Some("later\n"));
}

#[test]
fn prompt_declined_writes_nothing() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN], Some("n\n"));
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("undo this turn? [y/N]"), "{s}");
    assert!(s.contains("aborted"), "{s}");
    assert_untouched(&f);
}

#[test]
fn prompt_with_no_stdin_defaults_to_no() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN], None);
    assert_ok(&out);
    assert!(stdout(&out).contains("aborted"), "{}", stdout(&out));
    assert_untouched(&f);
}

#[test]
fn prompt_accepted_undoes() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN], Some("y\n"));
    assert_ok(&out);
    assert_eq!(f.file("a.txt").as_deref(), Some("old\n"));
    assert_eq!(f.file("b.txt"), None);
}

#[test]
fn drift_after_promote_is_flagged_before_overwriting() {
    let f = Fixture::promoted();
    std::fs::write(f.project.path().join("a.txt"), "user edit\n").unwrap();

    let out = f.mur(&["agent", "turn", "undo", AGENT, TURN], Some("n\n"));
    assert_ok(&out);
    let s = stdout(&out);
    assert!(s.contains("CHANGED SINCE PROMOTE"), "{s}");
    assert!(s.contains("warning: 1 path(s) changed"), "{s}");
    assert!(s.contains("overwrite the changed paths"), "{s}");
    assert_eq!(f.file("a.txt").as_deref(), Some("user edit\n"));
}

#[test]
fn unknown_turn_and_unknown_agent_fail_cleanly() {
    let f = Fixture::promoted();
    let out = f.mur(&["agent", "turn", "undo", AGENT, "nope", "--yes"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no undo snapshot"),
        "{}",
        stderr(&out)
    );

    let out = f.mur(
        &["agent", "turn", "undo", AGENT, "../escape", "--yes"],
        None,
    );
    assert!(!out.status.success());

    let out = f.mur(&["agent", "turn", "list", "ghost"], None);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not found"), "{}", stderr(&out));
    assert_untouched(&f);
}
