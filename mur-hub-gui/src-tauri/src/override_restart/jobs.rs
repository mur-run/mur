//! Which agents have an override restart in flight, and the quit-aware gate
//! the orchestrator consults before it publishes and before it cleans up
//! (Hub spec §Concurrency, §Quit). One table for the whole Hub: at most one
//! confirmation dialog anywhere, at most one job per agent.
//!
//! Every lock site goes through [`OverrideJobs::lock`], which recovers a
//! poisoned mutex; nothing does I/O or waits while holding it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mur_core::cmd::agent::{JobGate, PublishPermit};

/// Where one agent's override restart is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// The native confirmation is on screen.
    Dialog,
    /// Confirmed; preparing, sweeping, stopping.
    Preparing,
    /// Under the writer lock, writing the override file.
    Publishing,
    /// The file is published; the job is waiting for the runtime's ack.
    Published { nonce: String },
    /// The job owns cleanup of what it published.
    Cleaning,
    /// The quit path claimed this job's published file and will withdraw it.
    QuitWithdrawing,
}

/// Why a new job was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    DialogOpen,
    InProgress,
}

impl Busy {
    /// The `reason` recorded in the Refused audit event.
    pub fn audit_reason(self) -> &'static str {
        match self {
            Busy::DialogOpen => "dialog open",
            Busy::InProgress => "job in progress",
        }
    }
}

/// A published override the quit path must withdraw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuitClaim {
    pub slug: String,
    pub nonce: String,
    /// The window that started the job, for the quit dialog's wording.
    pub window: String,
}

struct Entry {
    state: JobState,
    window: String,
}

#[derive(Default)]
struct Jobs {
    entries: HashMap<String, Entry>,
    quitting: bool,
    quit_prompt_open: bool,
}

/// The Hub's override job table; cheap to clone, shared through `.manage`.
#[derive(Clone, Default)]
pub struct OverrideJobs(Arc<Mutex<Jobs>>);

impl OverrideJobs {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a job for `slug`. Refused while any dialog is open, while
    /// `slug` already has a job, or once the Hub is quitting.
    pub fn admit(&self, slug: &str, window: &str, initial: JobState) -> Result<JobEntry, Busy> {
        let mut g = self.lock();
        if g.entries.values().any(|e| e.state == JobState::Dialog) {
            return Err(Busy::DialogOpen);
        }
        if g.quitting || g.entries.contains_key(slug) {
            return Err(Busy::InProgress);
        }
        g.entries.insert(
            slug.to_owned(),
            Entry {
                state: initial,
                window: window.to_owned(),
            },
        );
        Ok(JobEntry {
            jobs: self.clone(),
            slug: slug.to_owned(),
        })
    }

    /// For Start / Stop / Restart / model switch: refuse while `slug` has an
    /// override restart in flight.
    pub fn refuse_if_busy(&self, slug: &str) -> Result<(), String> {
        if self.lock().entries.contains_key(slug) {
            Err(format!("an override restart of {slug} is in progress"))
        } else {
            Ok(())
        }
    }

    pub fn set_state(&self, slug: &str, state: JobState) {
        if let Some(e) = self.lock().entries.get_mut(slug) {
            e.state = state;
        }
    }

    pub fn is_quitting(&self) -> bool {
        self.lock().quitting
    }

    /// Agents with a job, sorted, for the quit dialog.
    pub fn active_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lock().entries.keys().cloned().collect();
        v.sort();
        v
    }

    /// The orchestrator's gate for `slug`'s job.
    pub fn gate(&self, slug: &str) -> HubGate {
        HubGate {
            jobs: self.clone(),
            slug: slug.to_owned(),
        }
    }

    /// `false` when a "Quit anyway?" prompt is already open.
    pub fn begin_quit_prompt(&self) -> bool {
        let mut g = self.lock();
        !std::mem::replace(&mut g.quit_prompt_open, true)
    }

    pub fn end_quit_prompt(&self) {
        self.lock().quit_prompt_open = false;
    }

    /// Mark the Hub as quitting and claim every published file: those jobs
    /// move to `QuitWithdrawing` and their own cleanup stands down. Jobs
    /// still publishing or cleaning finish themselves.
    pub fn begin_quit(&self) -> Vec<QuitClaim> {
        let mut g = self.lock();
        g.quitting = true;
        let mut claims = Vec::new();
        for (slug, e) in g.entries.iter_mut() {
            if let JobState::Published { nonce } = &e.state {
                claims.push(QuitClaim {
                    slug: slug.clone(),
                    nonce: nonce.clone(),
                    window: e.window.clone(),
                });
                e.state = JobState::QuitWithdrawing;
            }
        }
        claims.sort_by(|a, b| a.slug.cmp(&b.slug));
        claims
    }

    /// The quit path is done with `slug`'s claim.
    pub fn finish_claim(&self, slug: &str) {
        let mut g = self.lock();
        if g.entries
            .get(slug)
            .is_some_and(|e| e.state == JobState::QuitWithdrawing)
        {
            g.entries.remove(slug);
        }
    }

    #[cfg(test)]
    fn state_of(&self, slug: &str) -> Option<JobState> {
        self.lock().entries.get(slug).map(|e| e.state.clone())
    }

    #[cfg(test)]
    fn poison(&self) {
        let me = self.clone();
        let _ = std::thread::spawn(move || {
            let _g = me.0.lock().unwrap();
            panic!("poison the job table");
        })
        .join();
        assert!(self.0.is_poisoned());
    }
}

/// A job's slot in the table. Dropping it (normal return or unwind) frees
/// the slot, unless the quit path has claimed it.
pub struct JobEntry {
    jobs: OverrideJobs,
    slug: String,
}

impl Drop for JobEntry {
    fn drop(&mut self) {
        let mut g = self.jobs.lock();
        if g.entries
            .get(&self.slug)
            .is_some_and(|e| e.state != JobState::QuitWithdrawing)
        {
            g.entries.remove(&self.slug);
        }
    }
}

/// The orchestrator's view of the table for one job.
pub struct HubGate {
    jobs: OverrideJobs,
    slug: String,
}

impl JobGate for HubGate {
    fn begin_publish(&mut self) -> Option<Box<dyn PublishPermit>> {
        let mut g = self.jobs.lock();
        if g.quitting {
            return None;
        }
        let e = g.entries.get_mut(&self.slug)?;
        e.state = JobState::Publishing;
        Some(Box::new(HubPermit {
            jobs: self.jobs.clone(),
            slug: self.slug.clone(),
            committed: false,
        }))
    }

    fn begin_cleanup(&mut self) -> bool {
        let mut g = self.jobs.lock();
        match g.entries.get_mut(&self.slug) {
            Some(e) if e.state == JobState::QuitWithdrawing => false,
            Some(e) => {
                e.state = JobState::Cleaning;
                true
            }
            // No slot means nobody else can own the claim.
            None => true,
        }
    }
}

struct HubPermit {
    jobs: OverrideJobs,
    slug: String,
    committed: bool,
}

impl PublishPermit for HubPermit {
    fn commit(mut self: Box<Self>, nonce: &str) {
        self.committed = true;
        self.jobs.set_state(
            &self.slug,
            JobState::Published {
                nonce: nonce.to_owned(),
            },
        );
    }
}

impl Drop for HubPermit {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut g = self.jobs.lock();
        if let Some(e) = g.entries.get_mut(&self.slug)
            && e.state == JobState::Publishing
        {
            e.state = JobState::Preparing;
        }
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
