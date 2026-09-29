//! One-time shim tickets: how a CLI-spawn turn's MCP shim earns the right to
//! answer `allow` on its own turn's approvals without the home token.
//!
//! The shim cannot read `secrets/approval.token` — it runs under the CLI,
//! which runs under the sealed agent. The processes we must refuse (the
//! agent's own `bash` children) share its uid and can dial the same socket,
//! so "same user" proves nothing. What the shim can prove is narrower: that
//! it is the one this turn spawned. Two checks, both required:
//!
//! 1. **Ticket.** `cli_spawn` issues 32 random bytes per turn and hands them
//!    to the shim through the MCP config's `env`. The first `shim/hello` that
//!    presents it (constant-time compare) redeems it; it cannot be redeemed
//!    twice.
//! 2. **Lineage.** The redeeming connection's peer pid must be a strict
//!    descendant of the CLI process this turn spawned. The config file is
//!    readable by `bash`, so a leftover background job could hold the ticket;
//!    it cannot hold the ancestry — its parent is the runtime, not the CLI.
//!    A correct ticket from the wrong lineage is treated as stolen and burned.
//!
//! Trust is scoped to one connection and one task: a trusted connection may
//! allow only approvals the gate raised for that task, and everything is
//! revoked when the turn ends. The CLI's own children (user-installed hooks
//! for the `claude` backend, which uses the user's `~/.claude`) pass the
//! lineage check; they are the user's own software, inside the trust
//! boundary by construction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use subtle::ConstantTimeEq;

/// The env var the shim reads its ticket from. Shared with `mcp_config_json`
/// through `mur_common::cli_backend`, so the writer and reader cannot drift.
pub use mur_common::cli_backend::SHIM_TICKET_ENV;

/// Random bytes per ticket. 256 bits: not guessable over a local socket.
const TICKET_BYTES: usize = 32;

/// How far up the process tree lineage is walked before giving up. A real
/// shim is two or three levels under the CLI; the cap only bounds a cycle
/// from a pid being reused mid-walk.
pub(crate) const MAX_LINEAGE_DEPTH: usize = 64;

/// `HelloRefusal::NotReady`'s wire message; the shim retries on exactly this.
pub const NOT_READY: &str = "shim ticket not ready yet; retry";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloRefusal {
    /// No live ticket for this task (never issued, already redeemed, or the
    /// turn ended).
    Unknown,
    /// Issued, but the CLI's pid is not bound yet. Not consumed; retry.
    NotReady,
    /// The ticket did not match. Not consumed: 256 bits cannot be guessed,
    /// and burning on a miss would let any process deny the real shim.
    BadTicket,
    /// Right ticket, wrong process tree. Burned: the ticket leaked.
    NotDescendant,
    /// The transport could not say who is on the other end.
    NoPeer,
}

impl HelloRefusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::Unknown => "no shim ticket is live for this task",
            Self::NotReady => NOT_READY,
            Self::BadTicket => "shim ticket did not match",
            Self::NotDescendant => {
                "the connecting process is not a child of this turn's CLI; ticket revoked"
            }
            Self::NoPeer => "cannot identify the connecting process on this platform",
        }
    }
}

/// Per-connection identity, created by the transport and carried in
/// `RequestContext`. Dies with the connection, so trust cannot outlive it.
#[derive(Debug, Default)]
pub struct Connection {
    peer_pid: Option<u32>,
    shim_task: OnceLock<String>,
}

impl Connection {
    pub fn new(peer_pid: Option<u32>) -> Self {
        Self {
            peer_pid: peer_pid.filter(|p| *p != 0),
            shim_task: OnceLock::new(),
        }
    }
}

struct Entry {
    generation: u64,
    /// `None` once redeemed.
    ticket: Option<String>,
    cli_pid: Option<u32>,
}

#[derive(Default)]
struct State {
    next_generation: u64,
    live: HashMap<String, Entry>,
    /// hitl_id → task_id, for every approval the gate is waiting on.
    owners: HashMap<String, String>,
}

/// Shared between the task runner (issue / bind / revoke), the gate (records
/// which task raised each approval) and the dispatcher (`shim/hello`,
/// `tool/hitl_respond`). Cheap to clone.
#[derive(Clone, Default)]
pub struct ShimTrust {
    inner: Arc<Mutex<State>>,
}

/// Revokes one turn's ticket and trust when dropped — on success, error or
/// cancellation alike.
pub struct Issued {
    trust: ShimTrust,
    task_id: String,
    generation: u64,
    pub ticket: String,
}

impl Issued {
    /// Record the CLI's pid once it exists. Until then `redeem` answers
    /// `NotReady`.
    pub fn bind_cli_pid(&self, pid: u32) {
        let mut s = self.trust.lock();
        if let Some(e) = s.live.get_mut(&self.task_id)
            && e.generation == self.generation
        {
            e.cli_pid = Some(pid);
        }
    }
}

impl Drop for Issued {
    fn drop(&mut self) {
        let mut s = self.trust.lock();
        // Only our own generation: a newer turn on the same task id keeps its
        // ticket.
        if s.live
            .get(&self.task_id)
            .is_some_and(|e| e.generation == self.generation)
        {
            s.live.remove(&self.task_id);
            let task = self.task_id.clone();
            s.owners.retain(|_, t| *t != task);
        }
    }
}

impl ShimTrust {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the maps are still
        // consistent enough to deny from, and denying is the safe direction.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn issue(&self, task_id: &str) -> Issued {
        let ticket = generate();
        let mut s = self.lock();
        s.next_generation += 1;
        let generation = s.next_generation;
        s.live.insert(
            task_id.to_string(),
            Entry {
                generation,
                ticket: Some(ticket.clone()),
                cli_pid: None,
            },
        );
        Issued {
            trust: self.clone(),
            task_id: task_id.to_string(),
            generation,
            ticket,
        }
    }

    /// Redeem with the platform's real process tree.
    pub fn redeem(
        &self,
        conn: &Connection,
        task_id: &str,
        ticket: &str,
    ) -> Result<(), HelloRefusal> {
        self.redeem_with(conn, task_id, ticket, parent_of)
    }

    fn redeem_with(
        &self,
        conn: &Connection,
        task_id: &str,
        ticket: &str,
        parent: impl Fn(u32) -> Option<u32>,
    ) -> Result<(), HelloRefusal> {
        let peer = conn.peer_pid.ok_or(HelloRefusal::NoPeer)?;
        let mut s = self.lock();
        let e = s.live.get_mut(task_id).ok_or(HelloRefusal::Unknown)?;
        let expected = e.ticket.as_deref().ok_or(HelloRefusal::Unknown)?;
        if !bool::from(expected.as_bytes().ct_eq(ticket.as_bytes())) {
            return Err(HelloRefusal::BadTicket);
        }
        let cli = e.cli_pid.ok_or(HelloRefusal::NotReady)?;
        e.ticket = None;
        if !is_strict_descendant(peer, cli, parent) {
            // Burned above and not restored: the ticket leaked, so the real
            // shim loses it too and this turn's approvals fail closed.
            return Err(HelloRefusal::NotDescendant);
        }
        conn.shim_task
            .set(task_id.to_string())
            .map_err(|_| HelloRefusal::Unknown)?;
        Ok(())
    }

    pub fn record_owner(&self, hitl_id: &str, task_id: &str) {
        self.lock()
            .owners
            .insert(hitl_id.to_string(), task_id.to_string());
    }

    pub fn forget_owner(&self, hitl_id: &str) {
        self.lock().owners.remove(hitl_id);
    }

    /// True when `conn` is the redeemed shim of the live turn that raised
    /// `hitl_id`.
    pub fn authorizes(&self, conn: Option<&Connection>, hitl_id: &str) -> bool {
        let Some(task) = conn.and_then(|c| c.shim_task.get()) else {
            return false;
        };
        let s = self.lock();
        s.owners.get(hitl_id) == Some(task) && s.live.get(task).is_some_and(|e| e.ticket.is_none())
    }
}

fn generate() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; TICKET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn is_strict_descendant(pid: u32, ancestor: u32, parent: impl Fn(u32) -> Option<u32>) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_LINEAGE_DEPTH {
        match parent(cur) {
            Some(p) if p == ancestor => return true,
            Some(p) if p > 1 && p != cur => cur = p,
            _ => return false,
        }
    }
    false
}

#[cfg(target_os = "macos")]
pub(crate) fn parent_of(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a correctly sized, writable proc_bsdinfo.
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (rc == size).then_some(info.pbi_ppid)
}

#[cfg(target_os = "linux")]
pub(crate) fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` may hold spaces and parens; the fields after the LAST ')' are
    // fixed: state, then ppid.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn parent_of(_pid: u32) -> Option<u32> {
    // No lineage, no trust: the shim path fails closed here.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: u32 = 500;
    const RUNTIME: u32 = 100;

    /// runtime(100) → cli(500) → node(510) → shim(520); runtime → bash(600).
    fn tree(pid: u32) -> Option<u32> {
        match pid {
            520 => Some(510),
            510 => Some(CLI),
            CLI => Some(RUNTIME),
            600 => Some(RUNTIME),
            RUNTIME => Some(1),
            _ => None,
        }
    }

    fn ready(trust: &ShimTrust, task: &str) -> Issued {
        let t = trust.issue(task);
        t.bind_cli_pid(CLI);
        t
    }

    #[test]
    fn the_spawned_shim_redeems_once() {
        let trust = ShimTrust::default();
        let t = ready(&trust, "t1");
        let shim = Connection::new(Some(520));
        assert_eq!(trust.redeem_with(&shim, "t1", &t.ticket, tree), Ok(()));
        let again = Connection::new(Some(520));
        assert_eq!(
            trust.redeem_with(&again, "t1", &t.ticket, tree),
            Err(HelloRefusal::Unknown),
            "a ticket is single-use"
        );
    }

    #[test]
    fn a_stolen_ticket_from_bash_is_refused_and_burned() {
        let trust = ShimTrust::default();
        let t = ready(&trust, "t1");
        let bash = Connection::new(Some(600));
        assert_eq!(
            trust.redeem_with(&bash, "t1", &t.ticket, tree),
            Err(HelloRefusal::NotDescendant)
        );
        let shim = Connection::new(Some(520));
        assert_eq!(
            trust.redeem_with(&shim, "t1", &t.ticket, tree),
            Err(HelloRefusal::Unknown),
            "a leaked ticket must not stay redeemable"
        );
    }

    #[test]
    fn the_cli_itself_and_unknown_peers_are_not_descendants() {
        let trust = ShimTrust::default();
        let t = ready(&trust, "t1");
        assert_eq!(
            trust.redeem_with(&Connection::new(None), "t1", &t.ticket, tree),
            Err(HelloRefusal::NoPeer)
        );
        assert_eq!(
            trust.redeem_with(&Connection::new(Some(0)), "t1", &t.ticket, tree),
            Err(HelloRefusal::NoPeer),
            "pid 0 is the transport's 'unknown'"
        );
        assert_eq!(
            trust.redeem_with(&Connection::new(Some(CLI)), "t1", &t.ticket, tree),
            Err(HelloRefusal::NotDescendant)
        );
    }

    #[test]
    fn a_wrong_ticket_does_not_burn_the_real_one() {
        let trust = ShimTrust::default();
        let t = ready(&trust, "t1");
        let shim = Connection::new(Some(520));
        assert_eq!(
            trust.redeem_with(&shim, "t1", "00", tree),
            Err(HelloRefusal::BadTicket)
        );
        assert_eq!(trust.redeem_with(&shim, "t1", &t.ticket, tree), Ok(()));
    }

    #[test]
    fn hello_before_the_pid_is_bound_is_not_consumed() {
        let trust = ShimTrust::default();
        let t = trust.issue("t1");
        let shim = Connection::new(Some(520));
        assert_eq!(
            trust.redeem_with(&shim, "t1", &t.ticket, tree),
            Err(HelloRefusal::NotReady)
        );
        t.bind_cli_pid(CLI);
        assert_eq!(trust.redeem_with(&shim, "t1", &t.ticket, tree), Ok(()));
    }

    #[test]
    fn trust_covers_only_its_own_task_and_ends_with_the_turn() {
        let trust = ShimTrust::default();
        let t1 = ready(&trust, "t1");
        let _t2 = ready(&trust, "t2");
        let shim = Connection::new(Some(520));
        trust.redeem_with(&shim, "t1", &t1.ticket, tree).unwrap();
        trust.record_owner("h-own", "t1");
        trust.record_owner("h-other", "t2");

        assert!(trust.authorizes(Some(&shim), "h-own"));
        assert!(!trust.authorizes(Some(&shim), "h-other"), "cross-task");
        assert!(!trust.authorizes(Some(&shim), "h-unknown"));
        assert!(!trust.authorizes(None, "h-own"));
        assert!(
            !trust.authorizes(Some(&Connection::new(Some(520))), "h-own"),
            "trust belongs to the redeeming connection, not the pid"
        );

        drop(t1);
        assert!(
            !trust.authorizes(Some(&shim), "h-own"),
            "revoked at turn end"
        );
    }

    #[test]
    fn an_older_turn_ending_does_not_revoke_a_newer_one() {
        let trust = ShimTrust::default();
        let old = ready(&trust, "t1");
        let new = ready(&trust, "t1");
        drop(old);
        let shim = Connection::new(Some(520));
        assert_eq!(trust.redeem_with(&shim, "t1", &new.ticket, tree), Ok(()));
    }

    #[test]
    fn lineage_walk_is_bounded() {
        // A cycle (pid reuse mid-walk) must terminate, and as "no".
        assert!(!is_strict_descendant(7, 9, |p| Some(if p == 7 {
            8
        } else {
            7
        })));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_real_process_tree_is_readable() {
        let me = std::process::id();
        let parent = parent_of(me).expect("own parent");
        assert!(is_strict_descendant(me, parent, parent_of));
        assert!(!is_strict_descendant(parent, me, parent_of));
    }
}
