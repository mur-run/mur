//! #2379 end to end, the regression inverted: a sandboxed agent's request,
//! hosted by the daemon's child, parks a write-grant approval that is
//! router-signed, so `hitl::authority` can accept the human's answer.

use super::*;
use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::identity::AgentIdentity;
use mur_track::jobs_dispatch_request::{JobsDispatch, JobsDispatchRequest, RequestedJob};

use crate::executor::delegation::grant::tests::profile_yaml;

#[tokio::test]
async fn the_childs_write_grant_request_is_router_signed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // Outside the sandbox the child reads the router key from disk.
    crate::channel_writer::plant_writer_identity(home);
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    // `coder` may not write the project yet: the gate has to ask.
    for (name, write) in [("pm", vec![]), ("coder", vec![])] {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), profile_yaml(name, &write)).unwrap();
    }
    let pm = AgentIdentity::generate();
    pm.save(&home.join("agents/pm")).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        "parallel_jobs:\n  targets: [coder]\n",
    )
    .unwrap();

    let channel_id = uuid::Uuid::now_v7().to_string();
    let req = JobsDispatchRequest::create(
        JobsDispatch {
            requested_by: "pm".into(),
            jobs: vec![RequestedJob {
                description: "build it".into(),
                agent: "coder".into(),
            }],
            max_concurrency: 1,
            cwd: Some(project.display().to_string()),
            cwd_inferred: false,
            run_id: format!("run-{}", uuid::Uuid::now_v7()),
            channel_id: channel_id.clone(),
        },
        &pm,
        0,
        chrono::Utc::now(),
    );
    let path = home.join("request.yaml");
    std::fs::write(&path, serde_yaml_ng::to_string(&req).unwrap()).unwrap();

    // The child waits for the human (`Unanswered::Wait`); nobody answers here,
    // so stop once the request has had time to land.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), run(home, &path)).await;

    let svc = ChannelService::open(home).unwrap();
    let asks: Vec<_> = svc
        .load_events(&channel_id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .collect();
    assert_eq!(asks.len(), 1, "one write-grant approval for coder");
    assert!(
        crate::hitl::authority::is_router_signed(home, &channel_id, &asks[0]),
        "the approval card must be answerable: {:?}",
        asks[0]
    );
}
