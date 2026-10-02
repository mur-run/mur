use std::path::{Path, PathBuf};

use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::hitl::Unanswered;
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use super::*;
use crate::executor::delegation::grant::tests::profile_yaml;

fn step(description: &str, delegate_to: Option<&str>, intent: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        description: description.into(),
        delegate_to: delegate_to.map(Into::into),
        intent: intent.map(Into::into),
        ..Default::default()
    }
}

fn procedure(steps: Vec<ProcedureStep>) -> Procedure {
    Procedure {
        variables: vec![],
        steps,
    }
}

/// A home with one channel and `member` on disk, its profile granting
/// `write`. Returns `(home, channel_id, project)`.
fn setup(member: &str, grant: bool) -> (tempfile::TempDir, String, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let project = std::fs::canonicalize(home).unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let agent = home.join("agents").join(member);
    std::fs::create_dir_all(&agent).unwrap();
    let write: Vec<&Path> = if grant { vec![&project] } else { vec![] };
    std::fs::write(agent.join("profile.yaml"), profile_yaml(member, &write)).unwrap();
    let channel_id = ChannelService::open(home)
        .unwrap()
        .create_for_workflow("wf-test")
        .unwrap()
        .id;
    (tmp, channel_id, project)
}

fn defer() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

fn hitl_requests(home: &Path, channel_id: &str) -> usize {
    ChannelService::open(home)
        .unwrap()
        .load_events(channel_id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .count()
}

/// One entry per member profile, however many steps name it and however it
/// is cased. (Which spelling survives depends on the filesystem: a
/// case-insensitive one keeps the first as typed — see `canonicalize_agent_name`.)
#[test]
fn delegate_members_are_unique_per_profile() {
    let (tmp, _, _) = setup("coder", true);
    let p = procedure(vec![
        step("plan", None, None),
        step("build", Some("Coder"), None),
        step("fix", Some("coder"), None),
    ]);
    let m = delegate_members(tmp.path(), &p);
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(m[0].eq_ignore_ascii_case("coder"), "{m:?}");
}

/// The routing note lands on the prompt the member receives (intent, else
/// description) and only on delegate steps; labels stay clean.
#[test]
fn with_routing_appends_to_delegate_prompts_only() {
    let p = procedure(vec![
        step("local", None, None),
        step("build it", Some("coder"), None),
        step("label", Some("coder"), Some("do the thing")),
    ]);
    let r = with_routing(&p, "\n\nNOTE");
    assert_eq!(r.steps[0].intent, None);
    assert_eq!(r.steps[0].description, "local");
    assert_eq!(r.steps[1].intent.as_deref(), Some("build it\n\nNOTE"));
    assert_eq!(r.steps[1].description, "build it");
    assert_eq!(r.steps[2].intent.as_deref(), Some("do the thing\n\nNOTE"));
    assert_eq!(r.steps[2].description, "label");
}

/// No delegate steps, no warning; with them, the warning names the members
/// and the flag that would make them run.
#[test]
fn no_channel_warning_names_members_and_the_fix() {
    assert_eq!(no_channel_warning(&[]), None);
    let w = no_channel_warning(&["coder".into(), "qa".into()]).unwrap();
    assert!(w.contains("coder") && w.contains("qa"), "{w}");
    assert!(
        w.contains("--channel-new") && w.contains("--channel"),
        "{w}"
    );
}

/// A workflow with no delegate steps never touches the gate.
#[tokio::test]
async fn workflow_without_delegates_passes_without_asking() {
    let (tmp, cid, project) = setup("coder", false);
    let p = procedure(vec![step("local only", None, None)]);
    let out = gate_workflow(tmp.path(), &p, &cid, &project, defer())
        .await
        .unwrap();
    assert_eq!(out, None);
    assert_eq!(hitl_requests(tmp.path(), &cid), 0);
}

/// #1607 rollout 3: an ungranted delegate member is blocked before the first
/// step runs; the approval is parked on the run's channel, once per member.
#[tokio::test]
async fn workflow_blocks_an_ungranted_delegate_member() {
    let (tmp, cid, project) = setup("coder", false);
    let p = procedure(vec![
        step("build", Some("coder"), None),
        step("test", Some("Coder"), None),
    ]);
    let msg = gate_workflow(tmp.path(), &p, &cid, &project, defer())
        .await
        .unwrap()
        .expect("an ungranted member must block");
    assert!(
        msg.contains("coder") && msg.contains("needs approval"),
        "{msg}"
    );
    assert_eq!(
        hitl_requests(tmp.path(), &cid),
        1,
        "one approval per member"
    );
}

/// The workflow target is always inferred (`mur workflow run` has no
/// `--cwd`), so even a member that already may write there is asked once —
/// and an unanswered ask blocks rather than waving it through.
#[tokio::test]
async fn workflow_asks_even_when_already_granted() {
    let (tmp, cid, project) = setup("coder", true);
    let p = procedure(vec![step("build", Some("coder"), None)]);
    let msg = gate_workflow(tmp.path(), &p, &cid, &project, defer())
        .await
        .unwrap()
        .expect("an inferred target must be confirmed");
    assert!(msg.contains("needs approval"), "{msg}");
    assert_eq!(hitl_requests(tmp.path(), &cid), 1);
}

/// The `mur workflow run` seam: a channel run with an ungranted delegate
/// fails before the DAG, and nothing is delegated on the channel.
#[tokio::test]
async fn prepare_blocks_a_channel_run_before_any_step() {
    let (tmp, cid, project) = setup("coder", false);
    let p = procedure(vec![step("build", Some("coder"), None)]);
    let err = prepare_procedure(tmp.path(), &p, Some(&cid), &project, defer())
        .await
        .expect_err("ungranted member must block the run");
    assert!(format!("{err:#}").contains("delegation blocked"), "{err:#}");
    let events = ChannelService::open(tmp.path())
        .unwrap()
        .load_events(&cid)
        .unwrap();
    assert!(
        !events.iter().any(|e| e.kind == EventKind::Delegation),
        "{events:?}"
    );
}

/// `--yes` (interactive) approves the inferred directory for a granted member,
/// and the member's prompt then names that directory.
#[tokio::test]
async fn prepare_routes_delegates_once_approved() {
    let (tmp, cid, project) = setup("coder", true);
    let p = procedure(vec![
        step("local", None, None),
        step("build", Some("coder"), None),
    ]);
    let out = prepare_procedure(
        tmp.path(),
        &p,
        Some(&cid),
        &project,
        GatePolicy {
            yes: true,
            unanswered: Unanswered::Wait,
            auto_approve_tiers: vec![],
        },
    )
    .await
    .unwrap();
    let prompt = out.steps[1].intent.as_deref().unwrap();
    assert!(prompt.starts_with("build"), "{prompt}");
    assert!(prompt.contains(&project.display().to_string()), "{prompt}");
    assert_eq!(out.steps[0].intent, None, "non-delegate steps untouched");
}

/// No channel: delegate steps cannot dial anyone, so nothing is gated and
/// the procedure passes through unchanged (the caller only warns).
#[tokio::test]
async fn prepare_passes_channel_less_runs_through() {
    let (tmp, cid, project) = setup("coder", false);
    let p = procedure(vec![step("build", Some("coder"), None)]);
    let out = prepare_procedure(tmp.path(), &p, None, &project, defer())
        .await
        .unwrap();
    assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    assert_eq!(hitl_requests(tmp.path(), &cid), 0);
}
