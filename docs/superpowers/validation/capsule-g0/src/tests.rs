use super::*;

fn capture(model: &mut Model, id: &str, key: &str) {
    model
        .commit(id, Action::Capture, key, &[], false)
        .expect("capture should commit");
}

#[test]
fn erase_recovery_on_each_side_of_commit_anchor() {
    for profile in [Profile::Strict, Profile::Managed] {
        for cut in 0..4 {
            let mut model = Model::new(profile);
            capture(&mut model, "create-a", "A");
            let prepared = model
                .prepare("erase-a", Action::Erase, "A", &[], false)
                .expect("erase should prepare");
            if cut >= 1 {
                model.flush(&prepared);
            }
            if cut >= 2 {
                model.advance(&prepared).expect("advance should commit");
            }
            if cut == 3 {
                model.publish(&prepared).expect("publish should succeed");
            }
            model.restart().expect("restart should recover");
            assert_eq!(model.readable("A"), Ok(cut < 2));
        }
    }
}

#[test]
fn old_disk_image_cannot_roll_back_strict_anchor() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let old = model.export_disk();
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase should commit");
    model.restore_disk(&old);
    assert_eq!(model.restart(), Err(Refusal::Quarantined));
    assert_eq!(model.readable("A"), Err(Refusal::Quarantined));
}

#[test]
fn source_erasure_blocks_transitive_derivations() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    model
        .commit("b", Action::Derive, "B", &["A"], false)
        .expect("B");
    model
        .commit("c", Action::Derive, "C", &["B"], false)
        .expect("C");
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    assert_eq!(model.readable("B"), Ok(false));
    assert_eq!(model.readable("C"), Ok(false));
    assert_eq!(
        model.commit("d", Action::Derive, "D", &["C"], false),
        Err(Refusal::SourceUnavailable)
    );
}

#[test]
fn independent_save_requires_approval_and_live_sources_at_commit() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    assert_eq!(
        model.prepare("d", Action::Materialize, "D", &["A"], false),
        Err(Refusal::ApprovalRequired)
    );
    let prepared = model
        .prepare("d", Action::Materialize, "D", &["A"], true)
        .expect("prepare");
    model.flush(&prepared);
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    assert_eq!(model.advance(&prepared), Err(Refusal::CommitConflict));
    assert_eq!(
        model.prepare("d", Action::Materialize, "D", &["A"], true),
        Err(Refusal::SourceUnavailable)
    );
}

#[test]
fn lost_reply_retry_returns_original_receipt_without_resurrection() {
    let mut model = Model::new(Profile::Strict);
    let receipt = model
        .commit("a", Action::Capture, "A", &[], false)
        .expect("capture");
    assert_eq!(
        model.commit("a", Action::Capture, "A", &[], false),
        Ok(receipt)
    );
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    assert!(model.commit("a", Action::Capture, "A", &[], false).is_ok());
    assert_eq!(model.readable("A"), Ok(false));
    assert_eq!(
        model.commit("a", Action::Capture, "DIFFERENT", &[], false),
        Err(Refusal::IdempotencyConflict)
    );
}

#[test]
fn restart_cannot_redeem_uncommitted_preparation() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let prepared = model
        .prepare("d", Action::Materialize, "D", &["A"], true)
        .expect("prepare");
    model.flush(&prepared);
    model.restart().expect("restart");
    assert_eq!(model.advance(&prepared), Err(Refusal::ExpiredPreparation));
    assert_eq!(model.readable("D"), Ok(false));
}

#[test]
fn managed_old_image_demonstrates_declared_rollback_limit() {
    let mut model = Model::new(Profile::Managed);
    capture(&mut model, "a", "A");
    let old = model.export_disk();
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    model.restore_disk(&old);
    model.restart().expect("managed image should restore");
    assert_eq!(model.readable("A"), Ok(true));
}

#[test]
fn stale_pointer_does_not_override_strict_anchor() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let old_pointer = model.export_disk().pointer;
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    let mut image = model.export_disk();
    image.pointer = old_pointer;
    model.restore_disk(&image);
    model.restart().expect("restart");
    assert_eq!(model.readable("A"), Ok(false));
}

#[test]
fn unflushed_snapshot_cannot_advance_anchor() {
    let mut model = Model::new(Profile::Strict);
    let prepared = model
        .prepare("a", Action::Capture, "A", &[], false)
        .expect("prepare");
    assert_eq!(model.advance(&prepared), Err(Refusal::NotDurable));
    assert_eq!(model.readable("A"), Ok(false));
}

#[test]
fn approved_save_before_erasure_survives() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    model
        .commit("d", Action::Materialize, "D", &["A"], true)
        .expect("materialize");
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    assert_eq!(model.readable("D"), Ok(true));
}

#[test]
fn negative_control_detects_rollbackable_strict_anchor() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let old = model.export_disk();
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    model.restore_disk(&old);
    model.hardware_root = old.pointer;
    model
        .restart()
        .expect("deliberately broken anchor restores");
    assert_eq!(
        model.readable("A"),
        Ok(true),
        "negative control must expose resurrection"
    );
}

#[test]
fn corrupt_committed_manifest_is_quarantined() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let mut image = model.export_disk();
    image
        .manifests
        .get_mut(&image.pointer)
        .expect("manifest")
        .epoch = 999;
    model.restore_disk(&image);
    assert_eq!(model.restart(), Err(Refusal::Quarantined));
}

#[test]
fn new_capture_visibility_at_every_crash_cut() {
    for cut in 0..4 {
        let mut model = Model::new(Profile::Strict);
        let prepared = model
            .prepare("a", Action::Capture, "A", &[], false)
            .expect("prepare");
        if cut >= 1 {
            model.flush(&prepared);
        }
        if cut >= 2 {
            model.advance(&prepared).expect("advance");
        }
        if cut == 3 {
            model.publish(&prepared).expect("publish");
        }
        model.restart().expect("restart");
        assert_eq!(model.readable("A"), Ok(cut >= 2));
    }
}

#[test]
fn both_derive_erase_serializations_have_no_post_erase_read() {
    for derive_first in [true, false] {
        let mut model = Model::new(Profile::Strict);
        capture(&mut model, "a", "A");
        let derive = model
            .prepare("b", Action::Derive, "B", &["A"], false)
            .expect("derive");
        let erase = model
            .prepare("erase", Action::Erase, "A", &[], false)
            .expect("erase");
        model.flush(&derive);
        model.flush(&erase);
        let (winner, loser) = if derive_first {
            (&derive, &erase)
        } else {
            (&erase, &derive)
        };
        model.advance(winner).expect("winner");
        assert_eq!(model.advance(loser), Err(Refusal::CommitConflict));
        if derive_first {
            model
                .commit("erase", Action::Erase, "A", &[], false)
                .expect("erase retry");
        }
        assert_eq!(model.readable("B"), Ok(false));
    }
}

#[test]
fn destroyed_key_identity_cannot_be_reused() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    assert_eq!(
        model.commit("new-a", Action::Capture, "A", &[], false),
        Err(Refusal::KeyIdentityReused)
    );
}
