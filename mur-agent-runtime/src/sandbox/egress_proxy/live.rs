//! Runtime additions to the egress proxy's policy, and the `egress/denied`
//! signal (#1907, spec Part 3).
//!
//! The proxy starts before the sandbox seals, long before the socket notifier
//! exists, so the event sink is attached later ([`EgressProxyHandle::attach_events`]).
//! Until then denials are only logged — the headless path.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mur_common::egress_event::{
    DENIED_EVENT_WINDOW_SECS, DENIED_METHOD, DenyReason, EgressDenied, port_fix,
};
use tokio::sync::mpsc;

use super::{EgressProxyHandle, Registry};
use crate::tools::mcp::MCP_TOOL_TIMEOUT;

/// How long a turn stays a candidate owner of a server's denials after its
/// call ended (answered or abandoned). A browser page keeps loading after
/// `page.goto` returns — challenge iframes, lazy assets — so denials trail
/// the call; and an abandoned call may still be running server-side for up
/// to another tool timeout. Two timeouts covers both without keeping a turn
/// on the hook for the runtime's life.
const ATTRIBUTION_GRACE: Duration = Duration::from_secs(MCP_TOOL_TIMEOUT.as_secs() * 2);

/// `server → task → when`, for turns whose call ended within the grace.
type Recent = HashMap<String, HashMap<String, Instant>>;

/// Where denials go once a client-facing notifier exists.
#[derive(Clone, Default)]
pub(super) struct Events {
    inner: Arc<Mutex<EventsInner>>,
}

#[derive(Default)]
struct EventsInner {
    agent: String,
    sink: Option<mpsc::Sender<serde_json::Value>>,
    /// Throttle, keyed by `(server, host, audience)`: a notice routed to one
    /// turn says nothing to another, so each audience gets its own window.
    last: HashMap<(String, String, Option<String>), Instant>,
    /// `server → task → open tool calls`. A denial names the task only when
    /// exactly one task is using the server; the pool is per agent, so two
    /// turns on the same server cannot be told apart.
    in_flight: HashMap<String, HashMap<String, usize>>,
    /// Turns with a call we stopped waiting on (timeout, cancel, panic).
    /// MCP has no cancel, so that work may still be running and may be the
    /// source of a later denial: until [`ATTRIBUTION_GRACE`] passes the turn
    /// stays a candidate owner, even while another turn uses the server.
    abandoned: Recent,
    /// Turns whose call the server answered. Their page may still be loading,
    /// and another turn starting a call does not stop that traffic, so they
    /// remain candidate owners until [`ATTRIBUTION_GRACE`] passes.
    finished: Recent,
    /// `server → (when, host, port)` for upstream dials the seal refused on a
    /// closed port (#2418), kept for [`ATTRIBUTION_GRACE`] so the tool call
    /// that caused one can name the port in its own result.
    closed_ports: HashMap<String, Vec<(Instant, String, u16)>>,
}

/// Forget entries older than the grace, and servers left with none.
fn sweep(map: &mut Recent, now: Instant) {
    map.retain(|_, tasks| {
        tasks.retain(|_, t| now.saturating_duration_since(*t) < ATTRIBUTION_GRACE);
        !tasks.is_empty()
    });
}

impl EventsInner {
    /// The turns that could have caused a denial on `server` at `now`:
    /// everyone using it, and every turn that ended a call on it (abandoned
    /// or answered) within the grace. A finished turn's page keeps loading
    /// while another turn uses the server, so it stays a suspect then too;
    /// the pool is per agent and requests carry no page owner, so overlap is
    /// ambiguous and broadcasts. Sorted, for a stable log line.
    fn candidates(&mut self, server: &str, now: Instant) -> BTreeSet<String> {
        sweep(&mut self.abandoned, now);
        sweep(&mut self.finished, now);
        // A server that hit one closed port and never another would otherwise
        // keep that entry for the life of the process.
        self.closed_ports.retain(|_, log| {
            log.retain(|(t, _, _)| now.saturating_duration_since(*t) < ATTRIBUTION_GRACE);
            !log.is_empty()
        });
        let mut s: BTreeSet<String> = BTreeSet::new();
        let active = self.in_flight.get(server);
        s.extend(active.into_iter().flat_map(|t| t.keys().cloned()));
        for ended in [&self.abandoned, &self.finished] {
            s.extend(
                ended
                    .get(server)
                    .into_iter()
                    .flat_map(|t| t.keys().cloned()),
            );
        }
        s
    }
}

/// How a tool call ended, as far as its trailing traffic is concerned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Dropped unresolved (timeout, cancel, panic): it may still be running.
    Abandoned,
    /// The server answered (a result or a JSON-RPC error); what the tool
    /// started before answering (a page) may still load.
    Finished,
    /// The request never reached the server: no traffic can follow.
    Settled,
}

/// Marks one tool call on `server` by `task` as in flight. [`InFlight::finish`]
/// when the server answered, [`InFlight::settle`] when it was never asked; a
/// plain drop (timeout, cancel, panic) means the call may still be running
/// server-side. Finished and abandoned calls leave the task a candidate owner
/// of that server's denials for [`ATTRIBUTION_GRACE`].
pub struct InFlight {
    events: Events,
    server: String,
    task: String,
    ending: Ending,
    started: Instant,
}

impl InFlight {
    /// A note naming every port the seal refused for this server since the
    /// call started, with the fix — or `None`. The browser only sees a bare
    /// tunnel failure (`ERR_TUNNEL_CONNECTION_FAILED`), so without this the
    /// agent and the user never learn the host was fine and the port was not.
    ///
    /// Keyed by server, not by turn: two turns calling the same server at once
    /// both hear each other's ports. Requests carry no page owner, so this is
    /// the same ambiguity under which [`Events::emit`] broadcasts.
    pub fn closed_port_note(&self) -> Option<String> {
        let g = self.events.inner.lock().unwrap();
        let mut seen: BTreeSet<(String, u16)> = BTreeSet::new();
        for (at, host, port) in g.closed_ports.get(&self.server)? {
            if *at >= self.started {
                seen.insert((host.clone(), *port));
            }
        }
        if seen.is_empty() {
            return None;
        }
        let lines: Vec<String> = seen
            .into_iter()
            .map(|(host, port)| closed_port_line(&g.agent, &host, port))
            .collect();
        Some(lines.join("\n"))
    }

    /// The server answered — a result (an `isError` one included) or a
    /// JSON-RPC error. The call is over, but work the tool started before
    /// answering — a page still loading — may not be: an error does not
    /// prove the tool never ran.
    pub fn finish(mut self) {
        self.ending = Ending::Finished;
    }

    /// The request never reached the server (no client could be obtained),
    /// so nothing it started can be behind a later denial. Only for failures
    /// known to precede execution; any answer from the server is `finish`.
    pub fn settle(mut self) {
        self.ending = Ending::Settled;
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut g = self.events.inner.lock().unwrap();
        let now = Instant::now();
        let ended = match self.ending {
            Ending::Abandoned => Some(&mut g.abandoned),
            Ending::Finished => Some(&mut g.finished),
            Ending::Settled => None,
        };
        if let Some(ended) = ended {
            ended
                .entry(self.server.clone())
                .or_default()
                .insert(self.task.clone(), now);
        }
        if let Some(tasks) = g.in_flight.get_mut(&self.server) {
            if let Some(n) = tasks.get_mut(&self.task) {
                *n -= 1;
                if *n == 0 {
                    tasks.remove(&self.task);
                }
            }
            if tasks.is_empty() {
                g.in_flight.remove(&self.server);
            }
        }
    }
}

impl Events {
    pub(super) fn new(agent: &str) -> Self {
        let e = Self::default();
        e.inner.lock().unwrap().agent = agent.to_owned();
        e
    }

    pub(super) fn in_flight(&self, server: &str, task: &str) -> InFlight {
        *self
            .inner
            .lock()
            .unwrap()
            .in_flight
            .entry(server.to_owned())
            .or_default()
            .entry(task.to_owned())
            .or_default() += 1;
        InFlight {
            events: self.clone(),
            server: server.to_owned(),
            task: task.to_owned(),
            ending: Ending::Abandoned,
            started: Instant::now(),
        }
    }

    /// The seal refused an upstream dial to an allowed `host` on `port`
    /// (#2418). Remembered for in-flight tool calls, and raised as an
    /// `egress/denied` with [`DenyReason::PortNotOpen`].
    pub(super) fn port_closed(
        &self,
        server: &str,
        host: &str,
        port: u16,
        now: Instant,
    ) -> Option<EgressDenied> {
        {
            let mut g = self.inner.lock().unwrap();
            let log = g.closed_ports.entry(server.to_owned()).or_default();
            log.retain(|(t, _, _)| now.saturating_duration_since(*t) < ATTRIBUTION_GRACE);
            log.push((now, host.to_owned(), port));
        }
        self.emit(
            server,
            host,
            Some(port),
            "restricted",
            DenyReason::PortNotOpen,
            now,
        )
    }

    /// Emit at most one event per `(server, host, audience)` per window. Returns the
    /// event when one was sent, for tests and the log line.
    pub(super) fn denied(
        &self,
        server: &str,
        host: &str,
        port: Option<u16>,
        broad: bool,
        now: Instant,
    ) -> Option<EgressDenied> {
        let (mode, reason) = if broad {
            ("broad_audited", DenyReason::DenyListed)
        } else {
            ("restricted", DenyReason::NotAllowed)
        };
        self.emit(server, host, port, mode, reason, now)
    }

    fn emit(
        &self,
        server: &str,
        host: &str,
        port: Option<u16>,
        mode: &str,
        reason: DenyReason,
        now: Instant,
    ) -> Option<EgressDenied> {
        let mut g = self.inner.lock().unwrap();
        let candidates = g.candidates(server, now);
        let task_id = match candidates.len() {
            1 => candidates.into_iter().next(),
            _ => {
                // Broadcast. Say why, so the next leak is a log grep away.
                tracing::debug!(
                    server,
                    host,
                    ?candidates,
                    active = g.in_flight.get(server).map_or(0, HashMap::len),
                    abandoned = g.abandoned.get(server).map_or(0, HashMap::len),
                    "egress denial unattributed: broadcasting to every connection"
                );
                None
            }
        };
        // A closed port on an allowed host is a different problem from the
        // host itself being refused; neither may throttle the other.
        let throttled_host = match (reason, port) {
            (DenyReason::PortNotOpen, Some(p)) => format!("{host}:{p}"),
            _ => host.to_owned(),
        };
        let key = (server.to_owned(), throttled_host, task_id.clone());
        let window = Duration::from_secs(DENIED_EVENT_WINDOW_SECS);
        // An entry outside its window suppresses nothing; drop it so per-turn
        // keys do not pile up for the runtime's life. The map only ever holds
        // the last window's emits, so the sweep stays small.
        g.last.retain(|_, t| now.duration_since(*t) < window);
        if g.last.contains_key(&key) {
            return None;
        }
        g.last.insert(key, now);
        let ev = EgressDenied {
            agent: g.agent.clone(),
            server: server.to_owned(),
            host: host.to_owned(),
            port,
            mode: mode.to_owned(),
            reason,
            at: chrono::Utc::now().to_rfc3339(),
            task_id,
        };
        if let Some(sink) = &g.sink {
            let n = serde_json::json!({
                "jsonrpc": "2.0",
                "method": DENIED_METHOD,
                "params": ev,
            });
            // Never block the proxy on a slow client: drop when full.
            let _ = sink.try_send(n);
        }
        Some(ev)
    }
}

/// The one line naming a closed port and its fix, for the runtime log and
/// the tool result alike.
pub(super) fn closed_port_line(agent: &str, host: &str, port: u16) -> String {
    format!(
        "`{host}` is allowed, but port {port} is not open for this agent, so the \
         connection to {host}:{port} was refused (a browser reports this as \
         ERR_TUNNEL_CONNECTION_FAILED). Fix: {}",
        port_fix(agent, port)
    )
}

/// Why a runtime grant was refused. Text is user-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// No running server by that name is behind the proxy.
    UnknownServer(String),
    /// The server is `BroadAudited`: every host but the deny list is already
    /// reachable, and a deny-listed host is never grantable (D3).
    DenyListed { server: String, host: String },
    /// The host failed the shared validator.
    InvalidHost(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownServer(s) => write!(
                f,
                "no running MCP server `{s}` is routed through this agent's egress proxy"
            ),
            Self::DenyListed { server, host } => write!(
                f,
                "`{host}` is on `{server}`'s deny list; a deny-listed host cannot be granted. \
                 Remove the entry with `mur agent mcp set-network` (previewed and audited)"
            ),
            Self::InvalidHost(e) => write!(f, "{e}"),
        }
    }
}

impl EgressProxyHandle {
    /// Give the proxy somewhere to send `egress/denied`. Called once the
    /// supervisor's notifier exists.
    pub fn attach_events(&self, sink: mpsc::Sender<serde_json::Value>) {
        self.events.inner.lock().unwrap().sink = Some(sink);
    }

    /// Mark `task`'s tool call on `server` as in flight until the guard drops,
    /// so a denial raised meanwhile can name the task.
    pub fn in_flight(&self, server: &str, task: &str) -> InFlight {
        self.events.in_flight(server, task)
    }

    /// Let `host` through for `server` from the next connection on. Applies to
    /// every token registered for that server (a respawned child gets a new
    /// one). Returns the normalized host that was added.
    pub fn grant(&self, server: &str, host: &str) -> Result<String, GrantError> {
        let n = mur_common::host_pattern::normalize_host_pattern(host)
            .map_err(|e| GrantError::InvalidHost(format!("`{host}` is not a host: {e}")))?;
        grant_in(&self.registry, server, &n.value)?;
        Ok(n.value)
    }
}

fn grant_in(registry: &Registry, server: &str, host: &str) -> Result<(), GrantError> {
    let mut reg = registry.lock().unwrap();
    let mut found = false;
    for e in reg.values_mut().filter(|e| e.server == server) {
        found = true;
        if e.broad {
            return Err(GrantError::DenyListed {
                server: server.to_owned(),
                host: host.to_owned(),
            });
        }
        if !e.allow.iter().any(|h| h == host) {
            e.allow.push(host.to_owned());
        }
    }
    if found {
        Ok(())
    } else {
        Err(GrantError::UnknownServer(server.to_owned()))
    }
}

#[cfg(test)]
mod tests;
