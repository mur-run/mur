//! Executable encoding of contracts §2.4 — the four state spaces and their
//! interaction legality (item B1).
//!
//! The document states the tables as necessary conditions (T1, T2, T3) plus one
//! sufficient condition (R-READ), and adds the nested-InDoubt boundary rules
//! N1–N6. This module is the oracle for those statements: the tables below are
//! transcribed cell by cell from the spec, and `effective_read` implements
//! R-READ's fixed reporting order. A test that disagrees with the spec is a
//! test failure, not a spec revision.
//!
//! Scope differs per space (§2.4): `VaultHealth` is global, `SlotState` is per
//! slot, `EffectiveRead` is per read attempt, `OperationState` is per operation
//! ID. Nothing here models cryptography or I/O.

use std::collections::{BTreeMap, BTreeSet};

/// §2.1 global vault health.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VaultHealth {
    Ready,
    Recovering,
    Quarantined,
    Unsupported,
}

/// §2.1 per-slot lifecycle state.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SlotState {
    Absent,
    Live,
    DestroyedStrict,
    ErasedManaged,
}

/// §2.1 per-read-attempt outcome. `VaultQuarantined` was added by the B1-Q1
/// ruling so that "you lack a grant" and "this vault is no longer trustworthy"
/// stop sharing one externally visible value.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EffectiveRead {
    Readable,
    DependencyUnavailable,
    Denied,
    BackendUnavailable,
    VaultQuarantined,
}

/// §2.1 per-operation-ID state.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OperationState {
    Accepted,
    Prepared,
    DurablePrepared,
    Anchored,
    Published,
    Replied,
    Aborted,
    InDoubt,
}

/// T3 groups the eight `OperationState` values into five columns.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OperationColumn {
    /// Newly creating an Accepted/Prepared entry.
    CanCreateAccepted,
    DurablePreparedOrAnchored,
    PublishedOrReplied,
    Aborted,
    InDoubt,
}

/// T3 cells are not all binary: some are "legal only for pre-existing entries"
/// or "legal only as a historical record" or "legal but frozen".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Legality {
    /// 合法 — permitted without qualification.
    Legal,
    /// 僅既有者續存 — existing entries persist; no new ones.
    ExistingOnly,
    /// 僅為歷史記錄 — readable as history, not as a live claim.
    HistoricalOnly,
    /// 合法但凍結（N5）— the state persists and is not resolved.
    Frozen,
    /// 僅 Managed — permitted only under the Managed profile.
    ManagedOnly,
    /// **禁止** — forbidden.
    Forbidden,
}

impl Legality {
    /// Whether the combination may occur at all in a conforming implementation.
    pub const fn is_permitted(self) -> bool {
        !matches!(self, Self::Forbidden)
    }
}

pub const ALL_VAULT_HEALTH: [VaultHealth; 4] = [
    VaultHealth::Ready,
    VaultHealth::Recovering,
    VaultHealth::Quarantined,
    VaultHealth::Unsupported,
];

pub const ALL_SLOT_STATES: [SlotState; 4] = [
    SlotState::Absent,
    SlotState::Live,
    SlotState::DestroyedStrict,
    SlotState::ErasedManaged,
];

pub const ALL_EFFECTIVE_READS: [EffectiveRead; 5] = [
    EffectiveRead::Readable,
    EffectiveRead::DependencyUnavailable,
    EffectiveRead::Denied,
    EffectiveRead::BackendUnavailable,
    EffectiveRead::VaultQuarantined,
];

pub const ALL_OPERATION_STATES: [OperationState; 8] = [
    OperationState::Accepted,
    OperationState::Prepared,
    OperationState::DurablePrepared,
    OperationState::Anchored,
    OperationState::Published,
    OperationState::Replied,
    OperationState::Aborted,
    OperationState::InDoubt,
];

pub const ALL_OPERATION_COLUMNS: [OperationColumn; 5] = [
    OperationColumn::CanCreateAccepted,
    OperationColumn::DurablePreparedOrAnchored,
    OperationColumn::PublishedOrReplied,
    OperationColumn::Aborted,
    OperationColumn::InDoubt,
];

/// The full Cartesian product the spec declines to enumerate: 4×4×8×4 = 512.
pub const CARTESIAN_PRODUCT_SIZE: usize =
    ALL_VAULT_HEALTH.len() * ALL_SLOT_STATES.len() * ALL_OPERATION_STATES.len() * 4;

/// **T1: VaultHealth × EffectiveRead** — for reads that already passed discover
/// authorization. Necessary condition only.
pub const fn t1(health: VaultHealth, read: EffectiveRead) -> Legality {
    use EffectiveRead as E;
    use Legality::{Forbidden, Legal};
    use VaultHealth as V;

    match (health, read) {
        // Ready: everything except VaultQuarantined.
        (V::Ready, E::VaultQuarantined) => Forbidden,
        (V::Ready, _) => Legal,

        // Recovering: I01 forbids publishing new plaintext while state is
        // unconfirmable; judging an ancestor dead needs a confirmed manifest.
        // BackendUnavailable is the only non-authorization answer available.
        (V::Recovering, E::Readable) => Forbidden,
        (V::Recovering, E::DependencyUnavailable) => Forbidden,
        (V::Recovering, E::Denied) => Legal,
        (V::Recovering, E::BackendUnavailable) => Legal,
        (V::Recovering, E::VaultQuarantined) => Forbidden,

        // Quarantined: BackendUnavailable is forbidden because it would imply
        // "try again later". Denied stays legal — authorization precedes
        // VaultHealth, so an ungranted caller still sees only Denied.
        (V::Quarantined, E::Denied) => Legal,
        (V::Quarantined, E::VaultQuarantined) => Legal,
        (V::Quarantined, _) => Forbidden,

        // Unsupported: Managed reads are unaffected.
        (V::Unsupported, E::VaultQuarantined) => Forbidden,
        (V::Unsupported, _) => Legal,
    }
}

/// **T2: SlotState × EffectiveRead** — premise `VaultHealth = Ready` and
/// authorization passed.
///
/// The spec omits the `VaultQuarantined` column here and says why: under the
/// premise `VaultHealth = Ready`, T1's Ready row already forbids it in every
/// cell, so the column would carry no information. This function honours that
/// by forbidding it outright rather than silently accepting it.
pub const fn t2(slot: SlotState, read: EffectiveRead) -> Legality {
    use EffectiveRead as E;
    use Legality::{Forbidden, Legal};
    use SlotState as S;

    match (slot, read) {
        (_, E::VaultQuarantined) => Forbidden,

        (S::Absent, E::Denied) => Legal,
        (S::Absent, _) => Forbidden,

        (S::Live, _) => Legal,

        // I04: a Strict-destroyed slot is terminal. Its own terminal state
        // outranks ancestor state — an ancestor reason must not mask it.
        (S::DestroyedStrict, E::Denied) => Legal,
        (S::DestroyedStrict, _) => Forbidden,

        (S::ErasedManaged, E::Denied) => Legal,
        (S::ErasedManaged, _) => Forbidden,
    }
}

/// **T3: VaultHealth × OperationState**.
pub const fn t3(health: VaultHealth, column: OperationColumn) -> Legality {
    use Legality::{ExistingOnly, Forbidden, Frozen, HistoricalOnly, Legal, ManagedOnly};
    use OperationColumn as C;
    use VaultHealth as V;

    match (health, column) {
        (V::Ready, _) => Legal,

        (V::Recovering, C::CanCreateAccepted) => Forbidden,
        (V::Recovering, C::DurablePreparedOrAnchored) => ExistingOnly,
        (V::Recovering, C::PublishedOrReplied) => ExistingOnly,
        (V::Recovering, C::Aborted) => Legal,
        // InDoubt's normal home.
        (V::Recovering, C::InDoubt) => Legal,

        (V::Quarantined, C::CanCreateAccepted) => Forbidden,
        (V::Quarantined, C::DurablePreparedOrAnchored) => HistoricalOnly,
        (V::Quarantined, C::PublishedOrReplied) => HistoricalOnly,
        (V::Quarantined, C::Aborted) => Legal,
        // N5: Quarantined freezes InDoubt, it does not resolve it.
        (V::Quarantined, C::InDoubt) => Frozen,

        (V::Unsupported, C::Aborted) => Legal,
        (V::Unsupported, C::InDoubt) => Legal,
        (V::Unsupported, _) => ManagedOnly,
    }
}

/// The five inputs R-READ conjoins, plus the graph needed to check ancestors.
#[derive(Clone, Debug)]
pub struct ReadAttempt {
    pub health: VaultHealth,
    pub slot: String,
    pub slot_states: BTreeMap<String, SlotState>,
    /// Required ancestors per slot (§2.1: any required ancestor failing kills
    /// the derivation).
    pub ancestors: BTreeMap<String, Vec<String>>,
    pub grant_valid: bool,
    /// §2.2: the manifest at `H.manifest_root` is complete and its commitment
    /// matches.
    pub manifest_intact: bool,
    pub backend_available: bool,
}

impl ReadAttempt {
    /// Minimal readable attempt for a single slot with no ancestors.
    pub fn readable(slot: &str) -> Self {
        let mut slot_states = BTreeMap::new();
        slot_states.insert(slot.to_string(), SlotState::Live);
        Self {
            health: VaultHealth::Ready,
            slot: slot.to_string(),
            slot_states,
            ancestors: BTreeMap::new(),
            grant_valid: true,
            manifest_intact: true,
            backend_available: true,
        }
    }

    fn state_of(&self, slot: &str) -> SlotState {
        self.slot_states
            .get(slot)
            .copied()
            .unwrap_or(SlotState::Absent)
    }

    /// Transitive required ancestors, cycle-safe.
    fn required_ancestors(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<String> = self.ancestors.get(&self.slot).cloned().unwrap_or_default();
        let mut out = Vec::new();
        while let Some(next) = stack.pop() {
            if !seen.insert(next.clone()) {
                continue;
            }
            out.push(next.clone());
            if let Some(parents) = self.ancestors.get(&next) {
                stack.extend(parents.iter().cloned());
            }
        }
        out
    }
}

/// **R-READ** — the sufficient condition, evaluated in the spec's fixed
/// reporting order:
///
/// authorization → VaultHealth → own SlotState → ancestors → backend.
///
/// The order is fixed so that an error code never leaks information of higher
/// sensitivity than the caller has already earned.
pub fn effective_read(attempt: &ReadAttempt) -> EffectiveRead {
    // 1. Authorization first (§2.1 final clause): an ungranted caller learns
    //    nothing about vault health.
    if !attempt.grant_valid {
        return EffectiveRead::Denied;
    }

    // 2. VaultHealth. Quarantined answers VaultQuarantined and stops here.
    match attempt.health {
        VaultHealth::Quarantined => return EffectiveRead::VaultQuarantined,
        VaultHealth::Recovering => return EffectiveRead::BackendUnavailable,
        VaultHealth::Ready | VaultHealth::Unsupported => {}
    }

    // 3. Own SlotState — terminal states outrank ancestor reasons (T2).
    match attempt.state_of(&attempt.slot) {
        SlotState::Live => {}
        SlotState::Absent | SlotState::DestroyedStrict | SlotState::ErasedManaged => {
            return EffectiveRead::Denied;
        }
    }

    // 4. Ancestors.
    for ancestor in attempt.required_ancestors() {
        if attempt.state_of(&ancestor) != SlotState::Live {
            return EffectiveRead::DependencyUnavailable;
        }
    }

    // 5. Backend availability, including manifest integrity (§2.2).
    if !attempt.backend_available || !attempt.manifest_intact {
        return EffectiveRead::BackendUnavailable;
    }

    EffectiveRead::Readable
}

// ---------------------------------------------------------------------------
// §2.4.1 — nested InDoubt recovery (N1–N6)
// ---------------------------------------------------------------------------

/// §3.1: `read_anchor() -> Anchor | BackendUnavailable`. The return domain has
/// no `InDoubt` variant — that is N1's type-level argument, and it is why
/// recovery is a bounded loop rather than a recursion. Only `advance_epoch`
/// yields `InDoubt`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnchorRead {
    Anchor(u64),
    BackendUnavailable,
}

/// N3: the triple that must be persisted alongside `InDoubt`, without which a
/// restart cannot tell committed from uncommitted and I07's idempotent reply
/// becomes unimplementable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InDoubtRecord {
    pub operation_id: String,
    pub expected_anchor: u64,
    pub candidate_manifest_root: String,
}

/// Outcome of one bounded recovery run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryOutcome {
    pub health: VaultHealth,
    pub operation: OperationState,
    pub read: EffectiveRead,
    /// N3: the triple is retained whenever the operation stays undecided.
    pub retained: Option<InDoubtRecord>,
    /// How many times `read_anchor` was called — N1's bound, in evidence form.
    pub anchor_reads: usize,
}

/// §2.3 deadlines quoted by N2.
pub const SINGLE_CALL_DEADLINE_SECS: u64 = 5;
pub const TOTAL_RECOVERY_DEADLINE_SECS: u64 = 30;

/// Bounded recovery loop for an `InDoubt` operation.
///
/// `attempts` is the sequence of `read_anchor` results the backend will give,
/// each costing `SINGLE_CALL_DEADLINE_SECS`. `vault_birth_matches` false models
/// N5's unknown-root / birth-mismatch condition.
///
/// N4: every attempt is read-only and idempotent, so no attempt produces a
/// second side effect.
pub fn recover_in_doubt(
    record: &InDoubtRecord,
    attempts: &[AnchorRead],
    vault_birth_matches: bool,
) -> RecoveryOutcome {
    // N5: an unknown root or a birth mismatch means Quarantined, which FREEZES
    // the operation rather than resolving it. Reporting Aborted here would
    // violate both I02 and I07 if the operation had in fact committed.
    if !vault_birth_matches {
        return RecoveryOutcome {
            health: VaultHealth::Quarantined,
            operation: OperationState::InDoubt,
            read: EffectiveRead::VaultQuarantined,
            retained: Some(record.clone()),
            anchor_reads: 0,
        };
    }

    let budget = (TOTAL_RECOVERY_DEADLINE_SECS / SINGLE_CALL_DEADLINE_SECS) as usize;
    let mut anchor_reads = 0;

    for attempt in attempts.iter().take(budget) {
        anchor_reads += 1;
        match attempt {
            // N1: the return domain contains no InDoubt, so no second layer of
            // undecidedness is created here.
            AnchorRead::Anchor(anchor) => {
                let operation = if *anchor >= record.expected_anchor {
                    OperationState::Published
                } else {
                    OperationState::Aborted
                };
                return RecoveryOutcome {
                    health: VaultHealth::Ready,
                    operation,
                    read: EffectiveRead::Readable,
                    retained: None,
                    anchor_reads,
                };
            }
            // N1: unconfirmed always surfaces as BackendUnavailable.
            AnchorRead::BackendUnavailable => continue,
        }
    }

    // N2: on timeout the vault is Recovering and the operation STAYS InDoubt.
    // Timeout must never be read as Aborted.
    RecoveryOutcome {
        health: VaultHealth::Recovering,
        operation: OperationState::InDoubt,
        read: EffectiveRead::BackendUnavailable,
        retained: Some(record.clone()),
        anchor_reads,
    }
}

/// N6: `InDoubt` is not a key state. Its existence changes no `SlotState`; T2
/// judges by the slot's own state, so a pending operation can never make a
/// `DestroyedStrict` slot readable again.
pub const fn slot_state_after_pending_operation(
    slot: SlotState,
    _pending: Option<OperationState>,
) -> SlotState {
    slot
}

#[cfg(test)]
mod tests;
