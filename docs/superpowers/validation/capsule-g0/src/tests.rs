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

/// I09-b1 — a live key identity cannot be re-bound by a different operation.
#[test]
fn capture_onto_existing_key_is_key_identity_reused() {
    for profile in [Profile::Strict, Profile::Managed] {
        let mut model = Model::new(profile);
        capture(&mut model, "first", "A");
        assert_eq!(
            model.prepare("second", Action::Capture, "A", &[], false),
            Err(Refusal::KeyIdentityReused),
            "{profile:?}"
        );
    }
}

/// I10(a) — with committed objects intact, recovery converges to the same
/// root: the committed one if the anchor moved, the prior one if it did not.
#[test]
fn recovery_converges_to_identical_root_on_each_side_of_anchor() {
    for profile in [Profile::Strict, Profile::Managed] {
        for cut in 0..4 {
            let mut model = Model::new(profile);
            capture(&mut model, "create-a", "A");
            let before = model.anchor();
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
            let expected = if cut >= 2 { prepared.root } else { before };
            assert_eq!(model.anchor(), expected, "{profile:?} cut={cut}");
            assert_eq!(model.pointer, expected, "{profile:?} cut={cut}");
            assert_eq!(model.current().map(|s| snapshot_digest(&s)), Ok(expected));
        }
    }
}

/// Disk swapped under an in-flight preparation (no restart in between).
/// Strict: every step refuses — this is a safety property.
#[test]
fn strict_refuses_inflight_preparation_after_disk_rollback() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "a", "A");
    let old = model.export_disk();
    let prepared = model
        .prepare("b", Action::Capture, "B", &[], false)
        .expect("prepare");
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    model.restore_disk(&old);
    model.flush(&prepared);
    // Quarantined, not CommitConflict/NotCommitted: those imply "retry",
    // which t1_quarantined_never_implies_try_again_later forbids (§6).
    assert_eq!(model.advance(&prepared), Err(Refusal::Quarantined));
    assert_eq!(model.publish(&prepared), Err(Refusal::Quarantined));
    assert_eq!(model.readable("A"), Err(Refusal::Quarantined));
}

/// Same scenario under Managed: it proceeds, and the outcome is exactly the
/// declared rollback limit — the erased key is back. Records a boundary,
/// proves nothing about safety.
#[test]
fn managed_inflight_preparation_after_disk_rollback_stays_within_declared_limit() {
    let mut model = Model::new(Profile::Managed);
    capture(&mut model, "a", "A");
    let old = model.export_disk();
    let prepared = model
        .prepare("b", Action::Capture, "B", &[], false)
        .expect("prepare");
    model
        .commit("erase", Action::Erase, "A", &[], false)
        .expect("erase");
    model.restore_disk(&old);
    model.flush(&prepared);
    assert_eq!(model.advance(&prepared), Ok(()));
    assert_eq!(model.publish(&prepared), Ok(()));
    model.restart().expect("restart");
    assert_eq!(model.anchor(), prepared.root);
    assert_eq!(model.readable("A"), Ok(true));
    assert_eq!(model.readable("B"), Ok(true));
}

/// §6 regression: in Strict the hardware anchor does not roll back with the
/// disk, so a root-equality check alone let a pre-swap prepared op advance
/// and silently lift quarantine (violates state_space.rs:250, N5, I10).
#[test]
fn strict_advance_refuses_while_quarantined() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "c1", "A");
    let old = model.export_disk();
    capture(&mut model, "c2", "B");
    let prepared = model
        .prepare("c3", Action::Capture, "C", &[], false)
        .expect("prepare before swap");
    model.restore_disk(&old);
    assert_eq!(model.current().map(|_| ()), Err(Refusal::Quarantined));

    model.flush(&prepared);
    assert_eq!(model.advance(&prepared), Err(Refusal::Quarantined));
    assert_eq!(model.current().map(|_| ()), Err(Refusal::Quarantined));
    assert_eq!(model.readable("C"), Err(Refusal::Quarantined));
}

/// §6 regression: publish must not move the pointer onto a root whose
/// manifest is gone from disk (violates I02).
#[test]
fn strict_publish_refuses_while_quarantined() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "c1", "A");
    let old = model.export_disk();
    let prepared = model
        .prepare("c2", Action::Capture, "B", &[], false)
        .expect("prepare");
    model.flush(&prepared);
    model.advance(&prepared).expect("advance before swap");
    model.restore_disk(&old);
    let pointer_after_swap = model.pointer;
    assert_eq!(model.current().map(|_| ()), Err(Refusal::Quarantined));

    assert_eq!(model.publish(&prepared), Err(Refusal::Quarantined));
    assert_eq!(model.pointer, pointer_after_swap);
    assert_ne!(model.pointer, prepared.root, "pointer must not name a missing manifest");
}

/// §7 / N5 caveat: flush does not move the anchor, and current() digest-verifies
/// the anchored manifest, so writing that exact manifest back is
/// content-addressed recovery (I10), not a quarantine bypass.
#[test]
fn strict_flush_of_anchored_manifest_recovers_quarantine() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "c1", "A");
    let old = model.export_disk();
    let prepared = model
        .prepare("c2", Action::Capture, "B", &[], false)
        .expect("prepare");
    model.flush(&prepared);
    model.advance(&prepared).expect("advance");
    model.restore_disk(&old);
    assert_eq!(model.readable("B"), Err(Refusal::Quarantined));

    model.flush(&prepared);
    assert_eq!(model.anchor(), prepared.root, "flush must not move the anchor");
    assert_eq!(model.readable("B"), Ok(true));
}

/// §7 / N5 caveat, other half: flushing a manifest for any root other than
/// the anchored one must leave quarantine in place.
#[test]
fn strict_flush_of_other_root_does_not_lift_quarantine() {
    let mut model = Model::new(Profile::Strict);
    capture(&mut model, "c1", "A");
    let old = model.export_disk();
    capture(&mut model, "c2", "B");
    let anchored = model.anchor();
    let other = model
        .prepare("c3", Action::Capture, "C", &[], false)
        .expect("prepare");
    model.restore_disk(&old);
    assert_eq!(model.readable("A"), Err(Refusal::Quarantined));

    model.flush(&other);
    assert_ne!(other.root, anchored);
    assert_eq!(model.anchor(), anchored);
    assert_eq!(model.readable("A"), Err(Refusal::Quarantined));
    assert_eq!(model.readable("C"), Err(Refusal::Quarantined));
}
