//! Tests for contracts §2.4 (B1) — the interaction tables, R-READ's fixed
//! reporting order, and the N1–N6 nested-InDoubt boundary rules.
//!
//! These are oracle tests: each asserts what the spec says, so a divergence is
//! reported against the implementation, not negotiated away.

use super::*;

fn attempt_with(health: VaultHealth) -> ReadAttempt {
    let mut attempt = ReadAttempt::readable("A");
    attempt.health = health;
    attempt
}

// ---------------------------------------------------------------------------
// T1 / T2 / T3 — the tables as written
// ---------------------------------------------------------------------------

#[test]
fn t1_forbids_vault_quarantined_everywhere_except_quarantined() {
    for health in ALL_VAULT_HEALTH {
        let legality = t1(health, EffectiveRead::VaultQuarantined);
        if health == VaultHealth::Quarantined {
            assert_eq!(
                legality,
                Legality::Legal,
                "VaultQuarantined is the only non-authorization answer under Quarantined"
            );
        } else {
            assert_eq!(
                legality,
                Legality::Forbidden,
                "{health:?} must not answer VaultQuarantined"
            );
        }
    }
}

#[test]
fn t1_denied_is_legal_under_every_health_because_authorization_precedes_health() {
    for health in ALL_VAULT_HEALTH {
        assert_eq!(
            t1(health, EffectiveRead::Denied),
            Legality::Legal,
            "an ungranted caller must see Denied under {health:?}"
        );
    }
}

#[test]
fn t1_recovering_publishes_no_new_plaintext_and_judges_no_ancestor() {
    // I01: state that cannot be confirmed must not yield new plaintext.
    assert_eq!(
        t1(VaultHealth::Recovering, EffectiveRead::Readable),
        Legality::Forbidden
    );
    // Judging an ancestor dead requires a confirmed manifest.
    assert_eq!(
        t1(
            VaultHealth::Recovering,
            EffectiveRead::DependencyUnavailable
        ),
        Legality::Forbidden
    );
    assert_eq!(
        t1(VaultHealth::Recovering, EffectiveRead::BackendUnavailable),
        Legality::Legal
    );
}

#[test]
fn t1_quarantined_never_implies_try_again_later() {
    assert_eq!(
        t1(VaultHealth::Quarantined, EffectiveRead::BackendUnavailable),
        Legality::Forbidden,
        "BackendUnavailable would falsely imply the vault may recover"
    );
    for read in [
        EffectiveRead::Readable,
        EffectiveRead::DependencyUnavailable,
    ] {
        assert_eq!(t1(VaultHealth::Quarantined, read), Legality::Forbidden);
    }
}

#[test]
fn t1_unsupported_leaves_managed_reads_untouched() {
    for read in [
        EffectiveRead::Readable,
        EffectiveRead::DependencyUnavailable,
        EffectiveRead::Denied,
        EffectiveRead::BackendUnavailable,
    ] {
        assert_eq!(t1(VaultHealth::Unsupported, read), Legality::Legal);
    }
}

#[test]
fn t2_omitted_vault_quarantined_column_is_forbidden_in_every_cell() {
    // The spec omits the column because its premise is VaultHealth = Ready,
    // where T1 already forbids it. Omission must mean forbidden, not accepted.
    for slot in ALL_SLOT_STATES {
        assert_eq!(
            t2(slot, EffectiveRead::VaultQuarantined),
            Legality::Forbidden,
            "{slot:?} under a Ready vault cannot answer VaultQuarantined"
        );
    }
    assert_eq!(
        t1(VaultHealth::Ready, EffectiveRead::VaultQuarantined),
        Legality::Forbidden,
        "and T1's Ready row is the reason why"
    );
}

#[test]
fn t2_destroyed_strict_is_terminal_and_not_masked_by_ancestor_reasons() {
    // I04, plus: a slot's own terminal state outranks any ancestor cause.
    assert_eq!(
        t2(SlotState::DestroyedStrict, EffectiveRead::Readable),
        Legality::Forbidden
    );
    assert_eq!(
        t2(
            SlotState::DestroyedStrict,
            EffectiveRead::DependencyUnavailable
        ),
        Legality::Forbidden,
        "an ancestor reason must not be used to explain a DestroyedStrict slot"
    );
    assert_eq!(
        t2(SlotState::DestroyedStrict, EffectiveRead::Denied),
        Legality::Legal
    );
}

#[test]
fn t2_only_live_slots_admit_anything_beyond_denied() {
    for slot in ALL_SLOT_STATES {
        for read in [
            EffectiveRead::Readable,
            EffectiveRead::DependencyUnavailable,
            EffectiveRead::BackendUnavailable,
        ] {
            let expected = if slot == SlotState::Live {
                Legality::Legal
            } else {
                Legality::Forbidden
            };
            assert_eq!(t2(slot, read), expected, "T2[{slot:?}][{read:?}]");
        }
        assert_eq!(t2(slot, EffectiveRead::Denied), Legality::Legal);
    }
}

#[test]
fn t3_no_new_operations_outside_ready() {
    assert_eq!(
        t3(VaultHealth::Ready, OperationColumn::CanCreateAccepted),
        Legality::Legal
    );
    for health in [VaultHealth::Recovering, VaultHealth::Quarantined] {
        assert_eq!(
            t3(health, OperationColumn::CanCreateAccepted),
            Legality::Forbidden,
            "{health:?} must not admit new operations"
        );
    }
    assert_eq!(
        t3(VaultHealth::Unsupported, OperationColumn::CanCreateAccepted),
        Legality::ManagedOnly
    );
}

#[test]
fn t3_aborted_and_in_doubt_are_legal_under_every_health() {
    for health in ALL_VAULT_HEALTH {
        assert!(
            t3(health, OperationColumn::Aborted).is_permitted(),
            "Aborted must remain expressible under {health:?}"
        );
        assert!(
            t3(health, OperationColumn::InDoubt).is_permitted(),
            "InDoubt must remain expressible under {health:?}"
        );
    }
}

#[test]
fn t3_quarantined_freezes_in_doubt_rather_than_resolving_it() {
    // N5 in table form: frozen, not aborted, not published.
    assert_eq!(
        t3(VaultHealth::Quarantined, OperationColumn::InDoubt),
        Legality::Frozen
    );
    assert_eq!(
        t3(VaultHealth::Recovering, OperationColumn::InDoubt),
        Legality::Legal,
        "Recovering is InDoubt's normal home"
    );
    assert_eq!(
        t3(
            VaultHealth::Quarantined,
            OperationColumn::PublishedOrReplied
        ),
        Legality::HistoricalOnly
    );
    assert_eq!(
        t3(
            VaultHealth::Recovering,
            OperationColumn::DurablePreparedOrAnchored
        ),
        Legality::ExistingOnly
    );
}

// ---------------------------------------------------------------------------
// R-READ — the composition rule and its fixed reporting order
// ---------------------------------------------------------------------------

#[test]
fn r_read_requires_all_five_conjuncts() {
    let base = ReadAttempt::readable("A");
    assert_eq!(effective_read(&base), EffectiveRead::Readable);

    // Each conjunct removed in turn must stop being Readable.
    let mut no_grant = base.clone();
    no_grant.grant_valid = false;
    assert_ne!(effective_read(&no_grant), EffectiveRead::Readable);

    let mut not_ready = base.clone();
    not_ready.health = VaultHealth::Recovering;
    assert_ne!(effective_read(&not_ready), EffectiveRead::Readable);

    let mut dead_slot = base.clone();
    dead_slot
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);
    assert_ne!(effective_read(&dead_slot), EffectiveRead::Readable);

    let mut dead_ancestor = base.clone();
    dead_ancestor
        .ancestors
        .insert("A".to_string(), vec!["P".to_string()]);
    dead_ancestor
        .slot_states
        .insert("P".to_string(), SlotState::ErasedManaged);
    assert_ne!(effective_read(&dead_ancestor), EffectiveRead::Readable);

    let mut broken_manifest = base;
    broken_manifest.manifest_intact = false;
    assert_ne!(effective_read(&broken_manifest), EffectiveRead::Readable);
}

#[test]
fn r_read_authorization_outranks_vault_health() {
    // An ungranted caller sees Denied even against a Quarantined vault — the
    // B1-Q1 ruling's "no new leak" claim, in executable form.
    let mut attempt = attempt_with(VaultHealth::Quarantined);
    attempt.grant_valid = false;
    assert_eq!(effective_read(&attempt), EffectiveRead::Denied);

    attempt.grant_valid = true;
    assert_eq!(effective_read(&attempt), EffectiveRead::VaultQuarantined);
}

#[test]
fn r_read_quarantined_stops_before_slot_state_is_consulted() {
    // Two vaults differing only in slot state must be indistinguishable once
    // Quarantined: the order stops at stage 2.
    let mut live = attempt_with(VaultHealth::Quarantined);
    live.slot_states.insert("A".to_string(), SlotState::Live);

    let mut destroyed = attempt_with(VaultHealth::Quarantined);
    destroyed
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);

    assert_eq!(effective_read(&live), EffectiveRead::VaultQuarantined);
    assert_eq!(effective_read(&destroyed), EffectiveRead::VaultQuarantined);
}

#[test]
fn r_read_own_slot_state_outranks_ancestor_state() {
    // T2's DestroyedStrict row: the ancestor cause must not mask the slot's own
    // terminal state.
    let mut attempt = ReadAttempt::readable("A");
    attempt
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);
    attempt
        .ancestors
        .insert("A".to_string(), vec!["P".to_string()]);
    attempt
        .slot_states
        .insert("P".to_string(), SlotState::ErasedManaged);

    assert_eq!(
        effective_read(&attempt),
        EffectiveRead::Denied,
        "must report the slot's own state, not DependencyUnavailable"
    );
}

#[test]
fn r_read_transitive_ancestor_failure_is_caught() {
    let mut attempt = ReadAttempt::readable("C");
    attempt.slot_states.insert("B".to_string(), SlotState::Live);
    attempt
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);
    attempt
        .ancestors
        .insert("C".to_string(), vec!["B".to_string()]);
    attempt
        .ancestors
        .insert("B".to_string(), vec!["A".to_string()]);

    // I05: any required source failing blocks every derivation depending on it.
    assert_eq!(
        effective_read(&attempt),
        EffectiveRead::DependencyUnavailable
    );
}

#[test]
fn r_read_ancestor_walk_terminates_on_a_cycle() {
    let mut attempt = ReadAttempt::readable("A");
    attempt.slot_states.insert("B".to_string(), SlotState::Live);
    attempt
        .ancestors
        .insert("A".to_string(), vec!["B".to_string()]);
    attempt
        .ancestors
        .insert("B".to_string(), vec!["A".to_string()]);

    // A malformed graph must not hang the reader.
    assert_eq!(effective_read(&attempt), EffectiveRead::Readable);
}

#[test]
fn r_read_never_returns_a_value_t1_forbids() {
    // The composition rule must stay inside the pairwise necessary conditions.
    for health in ALL_VAULT_HEALTH {
        for slot in ALL_SLOT_STATES {
            for grant in [true, false] {
                for backend in [true, false] {
                    let mut attempt = attempt_with(health);
                    attempt.slot_states.insert("A".to_string(), slot);
                    attempt.grant_valid = grant;
                    attempt.backend_available = backend;

                    let read = effective_read(&attempt);
                    assert!(
                        t1(health, read).is_permitted(),
                        "R-READ produced {read:?} which T1 forbids under {health:?}"
                    );
                    if health == VaultHealth::Ready && grant {
                        assert!(
                            t2(slot, read).is_permitted(),
                            "R-READ produced {read:?} which T2 forbids for {slot:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cartesian_product_is_the_512_the_spec_declines_to_enumerate() {
    assert_eq!(CARTESIAN_PRODUCT_SIZE, 512);
    // And the pairwise tables that replace it are small enough to audit.
    let pairwise = ALL_VAULT_HEALTH.len() * ALL_EFFECTIVE_READS.len()
        + ALL_SLOT_STATES.len() * (ALL_EFFECTIVE_READS.len() - 1)
        + ALL_VAULT_HEALTH.len() * ALL_OPERATION_COLUMNS.len();
    assert_eq!(pairwise, 56);
}

// ---------------------------------------------------------------------------
// §2.4.1 — N1 through N6
// ---------------------------------------------------------------------------

fn record() -> InDoubtRecord {
    InDoubtRecord {
        operation_id: "op-1".to_string(),
        expected_anchor: 7,
        candidate_manifest_root: "root-7".to_string(),
    }
}

#[test]
fn n1_anchor_read_domain_cannot_express_in_doubt() {
    // The type-level argument: recovery reads the branch that cannot itself be
    // undecided, so the loop is bounded rather than recursive. This match is
    // exhaustive over AnchorRead and compiles only while that stays true.
    for outcome in [AnchorRead::Anchor(9), AnchorRead::BackendUnavailable] {
        match outcome {
            AnchorRead::Anchor(_) | AnchorRead::BackendUnavailable => {}
        }
    }
    let out = recover_in_doubt(&record(), &[AnchorRead::BackendUnavailable], true);
    assert_eq!(
        out.read,
        EffectiveRead::BackendUnavailable,
        "unconfirmed must surface as BackendUnavailable, never a second InDoubt"
    );
}

#[test]
fn n2_timeout_keeps_in_doubt_and_never_presumes_aborted() {
    let attempts = vec![AnchorRead::BackendUnavailable; 20];
    let out = recover_in_doubt(&record(), &attempts, true);

    assert_eq!(out.health, VaultHealth::Recovering);
    assert_eq!(out.operation, OperationState::InDoubt);
    assert_ne!(
        out.operation,
        OperationState::Aborted,
        "a deadline must never be read as an abort"
    );
    assert_eq!(out.read, EffectiveRead::BackendUnavailable);
    assert_eq!(
        out.anchor_reads,
        (TOTAL_RECOVERY_DEADLINE_SECS / SINGLE_CALL_DEADLINE_SECS) as usize,
        "the loop is bounded by the §2.3 budget"
    );
}

#[test]
fn n3_triple_is_retained_whenever_the_operation_stays_undecided() {
    let rec = record();

    let timed_out = recover_in_doubt(&rec, &[AnchorRead::BackendUnavailable; 20], true);
    assert_eq!(timed_out.retained.as_ref(), Some(&rec));

    let frozen = recover_in_doubt(&rec, &[], false);
    assert_eq!(
        frozen.retained.as_ref(),
        Some(&rec),
        "Quarantined must keep the triple so I07's idempotent reply stays possible"
    );

    let decided = recover_in_doubt(&rec, &[AnchorRead::Anchor(7)], true);
    assert_eq!(decided.retained, None);
}

#[test]
fn n4_every_attempt_is_read_only_and_idempotent() {
    let rec = record();
    let attempts = vec![AnchorRead::BackendUnavailable, AnchorRead::Anchor(7)];
    let first = recover_in_doubt(&rec, &attempts, true);
    let second = recover_in_doubt(&rec, &attempts, true);
    assert_eq!(first, second, "repeating recovery changes nothing");
    assert_eq!(rec, record(), "recovery does not mutate the record");
}

#[test]
fn n5_quarantine_freezes_rather_than_aborting_a_possibly_committed_operation() {
    // Reporting Aborted here would violate I02 and I07 together if the
    // operation had in fact committed.
    let out = recover_in_doubt(&record(), &[AnchorRead::Anchor(7)], false);

    assert_eq!(out.health, VaultHealth::Quarantined);
    assert_eq!(out.operation, OperationState::InDoubt);
    assert_ne!(out.operation, OperationState::Aborted);
    assert_ne!(out.operation, OperationState::Published);
    assert_eq!(
        out.read,
        EffectiveRead::VaultQuarantined,
        "the correct answer to the caller is 'unconfirmable'"
    );
    assert_eq!(
        out.anchor_reads, 0,
        "a birth mismatch is decided before any anchor read"
    );
    assert_eq!(
        t3(VaultHealth::Quarantined, OperationColumn::InDoubt),
        Legality::Frozen
    );
}

#[test]
fn n6_pending_operation_never_changes_a_slot_state() {
    for slot in ALL_SLOT_STATES {
        for op in ALL_OPERATION_STATES {
            assert_eq!(
                slot_state_after_pending_operation(slot, Some(op)),
                slot,
                "{op:?} must not move {slot:?}"
            );
        }
    }

    // The concrete danger the rule names: a pending operation must not make a
    // Strict-destroyed slot readable again.
    let mut attempt = ReadAttempt::readable("A");
    attempt
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);
    assert_eq!(effective_read(&attempt), EffectiveRead::Denied);
}

#[test]
fn recovery_decides_published_or_aborted_when_the_anchor_answers() {
    let rec = record();
    let published = recover_in_doubt(&rec, &[AnchorRead::Anchor(7)], true);
    assert_eq!(published.operation, OperationState::Published);

    let aborted = recover_in_doubt(&rec, &[AnchorRead::Anchor(6)], true);
    assert_eq!(aborted.operation, OperationState::Aborted);
    assert_eq!(aborted.health, VaultHealth::Ready);
}

// ---------------------------------------------------------------------------
// Negative controls — contracts §2.4 requires the negative model to FAIL on any
// violated cell. Each control below is a deliberately wrong implementation; the
// test asserts that the table catches it. If a control ever passes the table,
// the oracle has stopped discriminating and G0-SM's evidence is void.
// ---------------------------------------------------------------------------

/// Deliberately broken R-READ that checks VaultHealth before authorization,
/// leaking vault state to an ungranted caller.
fn broken_order_health_before_auth(attempt: &ReadAttempt) -> EffectiveRead {
    if attempt.health == VaultHealth::Quarantined {
        return EffectiveRead::VaultQuarantined;
    }
    if !attempt.grant_valid {
        return EffectiveRead::Denied;
    }
    effective_read(attempt)
}

#[test]
fn negative_control_detects_health_reported_before_authorization() {
    let mut attempt = attempt_with(VaultHealth::Quarantined);
    attempt.grant_valid = false;

    assert_eq!(
        broken_order_health_before_auth(&attempt),
        EffectiveRead::VaultQuarantined,
        "the broken implementation leaks vault health"
    );
    assert_ne!(
        effective_read(&attempt),
        broken_order_health_before_auth(&attempt),
        "the conforming implementation must differ from the leaking one"
    );
    assert_eq!(effective_read(&attempt), EffectiveRead::Denied);
}

/// Deliberately broken quarantine handling that answers BackendUnavailable,
/// inviting the caller to retry a vault that will never recover.
fn broken_quarantine_says_retry(attempt: &ReadAttempt) -> EffectiveRead {
    if attempt.grant_valid && attempt.health == VaultHealth::Quarantined {
        return EffectiveRead::BackendUnavailable;
    }
    effective_read(attempt)
}

#[test]
fn negative_control_detects_quarantine_disguised_as_backend_failure() {
    let attempt = attempt_with(VaultHealth::Quarantined);
    let produced = broken_quarantine_says_retry(&attempt);

    assert_eq!(produced, EffectiveRead::BackendUnavailable);
    assert_eq!(
        t1(VaultHealth::Quarantined, produced),
        Legality::Forbidden,
        "T1 must reject the disguise"
    );
    assert!(
        t1(VaultHealth::Quarantined, effective_read(&attempt)).is_permitted(),
        "and must accept the conforming answer"
    );
}

/// Deliberately broken slot handling that explains a Strict-destroyed slot by
/// its ancestor, masking I04's terminal state.
fn broken_ancestor_masks_destroyed(attempt: &ReadAttempt) -> EffectiveRead {
    let slot = attempt
        .slot_states
        .get(&attempt.slot)
        .copied()
        .unwrap_or(SlotState::Absent);
    if slot == SlotState::DestroyedStrict && attempt.ancestors.contains_key(&attempt.slot) {
        return EffectiveRead::DependencyUnavailable;
    }
    effective_read(attempt)
}

#[test]
fn negative_control_detects_ancestor_reason_masking_a_destroyed_slot() {
    let mut attempt = ReadAttempt::readable("A");
    attempt
        .slot_states
        .insert("A".to_string(), SlotState::DestroyedStrict);
    attempt
        .ancestors
        .insert("A".to_string(), vec!["P".to_string()]);
    attempt
        .slot_states
        .insert("P".to_string(), SlotState::ErasedManaged);

    let produced = broken_ancestor_masks_destroyed(&attempt);
    assert_eq!(produced, EffectiveRead::DependencyUnavailable);
    assert_eq!(
        t2(SlotState::DestroyedStrict, produced),
        Legality::Forbidden,
        "T2 must reject an ancestor reason for a DestroyedStrict slot"
    );
}

/// Deliberately broken recovery that presumes Aborted on timeout — the exact
/// I02/I07 double violation N2 and N5 forbid.
fn broken_recovery_presumes_aborted(
    record: &InDoubtRecord,
    attempts: &[AnchorRead],
    vault_birth_matches: bool,
) -> OperationState {
    let out = recover_in_doubt(record, attempts, vault_birth_matches);
    if out.operation == OperationState::InDoubt {
        OperationState::Aborted
    } else {
        out.operation
    }
}

#[test]
fn negative_control_detects_timeout_presumed_as_abort() {
    let rec = record();
    let attempts = vec![AnchorRead::BackendUnavailable; 20];

    assert_eq!(
        broken_recovery_presumes_aborted(&rec, &attempts, true),
        OperationState::Aborted,
        "the broken implementation decides what it cannot know"
    );
    assert_eq!(
        recover_in_doubt(&rec, &attempts, true).operation,
        OperationState::InDoubt,
        "the conforming implementation stays undecided"
    );
}

#[test]
fn negative_control_detects_quarantine_resolving_in_doubt() {
    let rec = record();

    assert_eq!(
        broken_recovery_presumes_aborted(&rec, &[], false),
        OperationState::Aborted
    );
    let conforming = recover_in_doubt(&rec, &[], false);
    assert_eq!(conforming.operation, OperationState::InDoubt);
    assert_eq!(
        t3(VaultHealth::Quarantined, OperationColumn::InDoubt),
        Legality::Frozen,
        "N5: frozen, not resolved"
    );
    assert!(
        conforming.retained.is_some(),
        "and the N3 triple survives for the eventual idempotent reply"
    );
}

#[test]
fn negative_control_sweep_every_forbidden_cell_is_rejected() {
    // The blanket requirement: for every forbidden cell in T1 and T2, no
    // conforming read may ever produce it. This sweeps the reachable inputs.
    let mut produced_t1: BTreeSet<(VaultHealth, EffectiveRead)> = BTreeSet::new();
    let mut produced_t2: BTreeSet<(SlotState, EffectiveRead)> = BTreeSet::new();

    for health in ALL_VAULT_HEALTH {
        for slot in ALL_SLOT_STATES {
            for grant in [true, false] {
                for backend in [true, false] {
                    for manifest in [true, false] {
                        for with_dead_ancestor in [true, false] {
                            let mut attempt = attempt_with(health);
                            attempt.slot_states.insert("A".to_string(), slot);
                            attempt.grant_valid = grant;
                            attempt.backend_available = backend;
                            attempt.manifest_intact = manifest;
                            if with_dead_ancestor {
                                attempt
                                    .ancestors
                                    .insert("A".to_string(), vec!["P".to_string()]);
                                attempt
                                    .slot_states
                                    .insert("P".to_string(), SlotState::DestroyedStrict);
                            }

                            let read = effective_read(&attempt);
                            produced_t1.insert((health, read));
                            if health == VaultHealth::Ready && grant {
                                produced_t2.insert((slot, read));
                            }
                        }
                    }
                }
            }
        }
    }

    for (health, read) in produced_t1 {
        assert!(
            t1(health, read).is_permitted(),
            "produced forbidden T1 cell [{health:?}][{read:?}]"
        );
    }
    for (slot, read) in produced_t2 {
        assert!(
            t2(slot, read).is_permitted(),
            "produced forbidden T2 cell [{slot:?}][{read:?}]"
        );
    }
}
