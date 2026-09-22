//! Executable abstract model for Capsule's G0 feasibility gate.
//!
//! This crate models state transitions only. It does not implement encryption,
//! durable I/O, hardware anchors, process isolation, or the MUR adapter.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    Strict,
    Managed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Capture,
    Derive,
    Materialize,
    Erase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotStatus {
    Live,
    Destroyed,
    ManagedErased,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Slot {
    status: SlotStatus,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiptRecord {
    request_digest: u64,
    epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    epoch: u64,
    slots: BTreeMap<String, Slot>,
    receipts: BTreeMap<String, ReceiptRecord>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Root(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub operation_id: String,
    pub epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prepared {
    base: Root,
    root: Root,
    snapshot: Snapshot,
    receipt: Receipt,
    session: u64,
}

#[derive(Clone, Debug)]
pub struct DiskImage {
    manifests: BTreeMap<Root, Snapshot>,
    pointer: Root,
    disk_root: Root,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    ApprovalRequired,
    CommitConflict,
    ExpiredPreparation,
    IdempotencyConflict,
    InvalidDependencyGraph,
    InvalidSources,
    KeyIdentityReused,
    NotCommitted,
    NotDurable,
    Quarantined,
    SourceUnavailable,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct Model {
    profile: Profile,
    manifests: BTreeMap<Root, Snapshot>,
    hardware_root: Root,
    disk_root: Root,
    pointer: Root,
    session: u64,
}

impl Model {
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        let initial = Snapshot {
            epoch: 0,
            slots: BTreeMap::new(),
            receipts: BTreeMap::new(),
        };
        let root = snapshot_digest(&initial);
        Self {
            profile,
            manifests: BTreeMap::from([(root, initial)]),
            hardware_root: root,
            disk_root: root,
            pointer: root,
            session: 0,
        }
    }

    fn anchor(&self) -> Root {
        match self.profile {
            Profile::Strict => self.hardware_root,
            Profile::Managed => self.disk_root,
        }
    }

    fn current(&self) -> Result<Snapshot, Refusal> {
        let root = self.anchor();
        let snapshot = self.manifests.get(&root).ok_or(Refusal::Quarantined)?;
        if snapshot_digest(snapshot) != root {
            return Err(Refusal::Quarantined);
        }
        Ok(snapshot.clone())
    }

    pub fn readable(&self, key: &str) -> Result<bool, Refusal> {
        fn visit(
            slots: &BTreeMap<String, Slot>,
            key: &str,
            path: &mut BTreeSet<String>,
        ) -> Result<bool, Refusal> {
            if path.len() > 64 || !path.insert(key.to_owned()) {
                return Err(Refusal::InvalidDependencyGraph);
            }
            let result = match slots.get(key) {
                Some(slot) if slot.status == SlotStatus::Live => slot
                    .dependencies
                    .iter()
                    .try_fold(true, |readable, dependency| {
                        Ok(readable && visit(slots, dependency, path)?)
                    })?,
                _ => false,
            };
            path.remove(key);
            Ok(result)
        }

        visit(&self.current()?.slots, key, &mut BTreeSet::new())
    }

    pub fn prepare(
        &self,
        operation_id: &str,
        action: Action,
        target: &str,
        sources: &[&str],
        approved: bool,
    ) -> Result<Prepared, Refusal> {
        let before = self.current()?;
        let request_digest = request_digest(action, target, sources, approved);
        if let Some(old) = before.receipts.get(operation_id) {
            if old.request_digest != request_digest {
                return Err(Refusal::IdempotencyConflict);
            }
            let epoch = old.epoch;
            return Ok(Prepared {
                base: self.anchor(),
                root: self.anchor(),
                snapshot: before,
                receipt: Receipt {
                    operation_id: operation_id.to_owned(),
                    epoch,
                },
                session: self.session,
            });
        }

        let mut after = before;
        if action == Action::Materialize && !approved {
            return Err(Refusal::ApprovalRequired);
        }
        match action {
            Action::Capture | Action::Derive | Action::Materialize => {
                if after.slots.contains_key(target) {
                    return Err(Refusal::KeyIdentityReused);
                }
                if (action == Action::Capture && !sources.is_empty())
                    || (action == Action::Derive && sources.is_empty())
                {
                    return Err(Refusal::InvalidSources);
                }
                for source in sources {
                    if !self.readable(source)? {
                        return Err(Refusal::SourceUnavailable);
                    }
                }
                after.slots.insert(
                    target.to_owned(),
                    Slot {
                        status: SlotStatus::Live,
                        dependencies: if action == Action::Derive {
                            sources.iter().map(|source| (*source).to_owned()).collect()
                        } else {
                            Vec::new()
                        },
                    },
                );
            }
            Action::Erase => {
                let slot = after.slots.get_mut(target).ok_or(Refusal::Unavailable)?;
                slot.status = match self.profile {
                    Profile::Strict => SlotStatus::Destroyed,
                    Profile::Managed => SlotStatus::ManagedErased,
                };
            }
        }
        after.epoch += 1;
        after.receipts.insert(
            operation_id.to_owned(),
            ReceiptRecord {
                request_digest,
                epoch: after.epoch,
            },
        );
        Ok(Prepared {
            base: self.anchor(),
            root: snapshot_digest(&after),
            receipt: Receipt {
                operation_id: operation_id.to_owned(),
                epoch: after.epoch,
            },
            snapshot: after,
            session: self.session,
        })
    }

    pub fn flush(&mut self, prepared: &Prepared) {
        self.manifests
            .insert(prepared.root, prepared.snapshot.clone());
    }

    pub fn advance(&mut self, prepared: &Prepared) -> Result<(), Refusal> {
        if prepared.session != self.session {
            return Err(Refusal::ExpiredPreparation);
        }
        if self.anchor() != prepared.base {
            return Err(Refusal::CommitConflict);
        }
        let durable = self
            .manifests
            .get(&prepared.root)
            .is_some_and(|snapshot| snapshot_digest(snapshot) == prepared.root);
        if !durable {
            return Err(Refusal::NotDurable);
        }
        match self.profile {
            Profile::Strict => self.hardware_root = prepared.root,
            Profile::Managed => self.disk_root = prepared.root,
        }
        Ok(())
    }

    pub fn publish(&mut self, prepared: &Prepared) -> Result<(), Refusal> {
        if self.anchor() != prepared.root {
            return Err(Refusal::NotCommitted);
        }
        self.pointer = prepared.root;
        Ok(())
    }

    pub fn restart(&mut self) -> Result<(), Refusal> {
        self.session += 1;
        self.current()?;
        self.pointer = self.anchor();
        Ok(())
    }

    #[must_use]
    pub fn export_disk(&self) -> DiskImage {
        DiskImage {
            manifests: self.manifests.clone(),
            pointer: self.pointer,
            disk_root: self.disk_root,
        }
    }

    pub fn restore_disk(&mut self, image: &DiskImage) {
        self.manifests.clone_from(&image.manifests);
        self.pointer = image.pointer;
        self.disk_root = image.disk_root;
    }

    pub fn commit(
        &mut self,
        operation_id: &str,
        action: Action,
        target: &str,
        sources: &[&str],
        approved: bool,
    ) -> Result<Receipt, Refusal> {
        let prepared = self.prepare(operation_id, action, target, sources, approved)?;
        self.flush(&prepared);
        self.advance(&prepared)?;
        self.publish(&prepared)?;
        Ok(prepared.receipt)
    }
}

fn request_digest(action: Action, target: &str, sources: &[&str], approved: bool) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_u8(action as u8);
    hash.write_str(target);
    hash.write_u8(u8::from(approved));
    for source in sources {
        hash.write_str(source);
    }
    hash.finish()
}

fn snapshot_digest(snapshot: &Snapshot) -> Root {
    let mut hash = Fnv1a::new();
    hash.write_u64(snapshot.epoch);
    for (name, slot) in &snapshot.slots {
        hash.write_str(name);
        hash.write_u8(slot.status as u8);
        for dependency in &slot.dependencies {
            hash.write_str(dependency);
        }
    }
    for (operation_id, receipt) in &snapshot.receipts {
        hash.write_str(operation_id);
        hash.write_u64(receipt.request_digest);
        hash.write_u64(receipt.epoch);
    }
    Root(hash.finish())
}

struct Fnv1a(u64);

impl Fnv1a {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.write(&[value]);
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn write_str(&mut self, value: &str) {
        self.write_u64(value.len() as u64);
        self.write(value.as_bytes());
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests;
