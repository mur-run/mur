use super::*;

fn dispatch() -> JobsDispatch {
    JobsDispatch {
        requested_by: "pm".into(),
        jobs: vec![RequestedJob {
            description: "fix it".into(),
            agent: "coder".into(),
        }],
        max_concurrency: 4,
        // Absolute on every platform: `/work` is not on Windows.
        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        cwd_inferred: false,
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        channel_id: uuid::Uuid::now_v7().to_string(),
    }
}

#[test]
fn sign_verify_roundtrip() {
    let id = AgentIdentity::generate();
    let req = JobsDispatchRequest::create(dispatch(), &id, 0, Utc::now());
    assert!(req.verify(&id.verifying_key_bytes()));
}

#[test]
fn every_field_is_covered_by_the_signature() {
    let id = AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    let base = JobsDispatchRequest::create(dispatch(), &id, 0, Utc::now());
    let tamper: [fn(&mut JobsDispatchRequest); 10] = [
        |r| r.dispatch.requested_by = "mur".into(),
        |r| r.dispatch.jobs[0].agent = "root".into(),
        |r| r.dispatch.jobs[0].description = "rm -rf".into(),
        |r| r.dispatch.jobs.push(r.dispatch.jobs[0].clone()),
        |r| r.dispatch.max_concurrency = 32,
        |r| r.dispatch.cwd = Some("/elsewhere".into()),
        |r| r.dispatch.cwd_inferred = true,
        |r| r.dispatch.run_id = "run-other".into(),
        |r| r.dispatch.channel_id = uuid::Uuid::now_v7().to_string(),
        |r| r.key_version = 7,
    ];
    for t in tamper {
        let mut r = base.clone();
        t(&mut r);
        assert!(!r.verify(&pk), "tampered field still verified: {r:?}");
    }
}

#[test]
fn a_fleet_signature_never_verifies_as_a_jobs_request() {
    // Same body bytes under the other domain tag must not verify.
    let id = AgentIdentity::generate();
    let req = JobsDispatchRequest::create(dispatch(), &id, 0, Utc::now());
    let other = crate::signed_request::sign_input(
        "mur-fleet-dispatch-request-v1\n",
        &req.dispatch,
        &req.requested_at,
        0,
    );
    let forged = JobsDispatchRequest {
        sig: id.sign_multibase(&other),
        ..req
    };
    assert!(!forged.verify(&id.verifying_key_bytes()));
}

#[test]
fn freshness_window_rejects_old_and_future() {
    let now = Utc::now();
    let req = JobsDispatchRequest::create(dispatch(), &AgentIdentity::generate(), 0, now);
    assert!(req.is_fresh(now, 60));
    assert!(!req.is_fresh(now + chrono::Duration::seconds(61), 60));
    assert!(!req.is_fresh(now - chrono::Duration::seconds(1), 60));
}

#[test]
fn yaml_roundtrip_preserves_signature() {
    let id = AgentIdentity::generate();
    let req = JobsDispatchRequest::create(dispatch(), &id, 3, Utc::now());
    let yaml = serde_yaml_ng::to_string(&req).unwrap();
    let back: JobsDispatchRequest = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(back.dispatch, req.dispatch);
    assert!(back.verify(&id.verifying_key_bytes()));
}

#[test]
fn shape_rejects_what_would_become_a_bad_path_or_count() {
    assert!(dispatch().check_shape().is_ok());
    let bad: [fn(&mut JobsDispatch); 9] = [
        |d| d.requested_by = "../mur".into(),
        |d| d.run_id = "run-../x".into(),
        |d| d.run_id = "fleet-x-1".into(),
        |d| d.channel_id = "not-a-uuid".into(),
        |d| d.jobs.clear(),
        |d| d.jobs[0].agent = "a/b".into(),
        |d| d.jobs[0].description = "  ".into(),
        |d| d.max_concurrency = 0,
        |d| d.cwd = Some("relative/dir".into()),
    ];
    for b in bad {
        let mut d = dispatch();
        b(&mut d);
        assert!(d.check_shape().is_err(), "accepted: {d:?}");
    }
    let mut many = dispatch();
    many.jobs = vec![many.jobs[0].clone(); MAX_JOBS + 1];
    assert!(many.check_shape().is_err());
}
