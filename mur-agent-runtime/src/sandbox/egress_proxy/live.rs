//! Runtime additions to the egress proxy's policy, and the `egress/denied`
//! signal (#1907, spec Part 3).
//!
//! The proxy starts before the sandbox seals, long before the socket notifier
//! exists, so the event sink is attached later ([`EgressProxyHandle::attach_events`]).
//! Until then denials are only logged — the headless path.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mur_common::egress_event::{DENIED_EVENT_WINDOW_SECS, DENIED_METHOD, DenyReason, EgressDenied};
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
    /// The tool ran and returned; what it started (a page) may still load.
    Finished,
    /// The tool never ran (unreachable server, JSON-RPC error): no traffic.
    Settled,
}

/// Marks one tool call on `server` by `task` as in flight. [`InFlight::finish`]
/// when the tool returned a result, [`InFlight::settle`] when it never ran; a
/// plain drop (timeout, cancel, panic) means the call may still be running
/// server-side. Finished and abandoned calls leave the task a candidate owner
/// of that server's denials for [`ATTRIBUTION_GRACE`].
pub struct InFlight {
    events: Events,
    server: String,
    task: String,
    ending: Ending,
}

impl InFlight {
    /// The tool returned a result (an `isError` one included): the call is
    /// over, but the work it started — a page still loading — may not be.
    pub fn finish(mut self) {
        self.ending = Ending::Finished;
    }

    /// The tool never ran — the request did not reach the server, or the
    /// server refused it at the protocol level — so nothing it started can
    /// be behind a later denial.
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
        }
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
        let key = (server.to_owned(), host.to_owned(), task_id.clone());
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
            mode: if broad { "broad_audited" } else { "restricted" }.to_owned(),
            reason: if broad {
                DenyReason::DenyListed
            } else {
                DenyReason::NotAllowed
            },
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
mod tests {
    use super::*;

    #[test]
    fn one_event_per_server_host_per_window() {
        let ev = Events::new("mur");
        let (tx, mut rx) = mpsc::channel(8);
        ev.inner.lock().unwrap().sink = Some(tx);
        let t0 = Instant::now();
        let first = ev
            .denied("browser", "img.x.com", Some(443), false, t0)
            .unwrap();
        assert_eq!(first.reason, DenyReason::NotAllowed);
        assert_eq!(first.agent, "mur");
        assert!(
            ev.denied(
                "browser",
                "img.x.com",
                Some(443),
                false,
                t0 + Duration::from_secs(59)
            )
            .is_none()
        );
        // Different host or server: its own window.
        assert!(ev.denied("browser", "cdn.x.com", None, false, t0).is_some());
        assert!(ev.denied("search", "img.x.com", None, true, t0).is_some());
        assert!(
            ev.denied(
                "browser",
                "img.x.com",
                None,
                false,
                t0 + Duration::from_secs(DENIED_EVENT_WINDOW_SECS)
            )
            .is_some()
        );
        let n = rx.try_recv().unwrap();
        assert_eq!(n["method"], DENIED_METHOD);
        assert_eq!(n["params"]["host"], "img.x.com");
    }

    /// A denial raised while exactly one turn's tool call is using the server
    /// is that turn's business: it names the task so the runtime can route it
    /// to that turn's connection instead of every connection on the socket.
    #[test]
    fn a_denial_names_the_one_task_using_the_server() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let _using = h.in_flight("browser", "task-a");
        let d = h
            .events
            .denied("browser", "img.x.com", Some(443), false, Instant::now())
            .unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-a"));
    }

    /// Two turns on one shared server cannot be told apart — while both use
    /// it, and after both finished, while either page may still be loading —
    /// and a child that no turn ever called belongs to no turn: all `None`,
    /// which the runtime reads as "broadcast, as before".
    #[test]
    fn a_denial_is_unattributed_when_zero_or_two_tasks_use_the_server() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t = Instant::now();
        let a = h.in_flight("browser", "task-a");
        let b = h.in_flight("browser", "task-b");
        let both = h.events.denied("browser", "one.x", None, false, t).unwrap();
        assert_eq!(both.task_id, None);
        b.finish();
        let a_and_trailing_b = h.events.denied("browser", "two.x", None, false, t).unwrap();
        assert_eq!(
            a_and_trailing_b.task_id, None,
            "b's page may still be loading"
        );
        a.finish();
        let nobody = h
            .events
            .denied("browser", "three.x", None, false, t)
            .unwrap();
        assert_eq!(nobody.task_id, None, "a and b both finished recently");
        let never = h.events.denied("fetch", "four.x", None, false, t).unwrap();
        assert_eq!(never.task_id, None, "no turn ever called fetch");
    }

    /// The leak in the screenshot: `page.goto` returns, the call finishes,
    /// and the page keeps loading (a Cloudflare challenge iframe). With no
    /// call in flight the denial used to broadcast to every session; it
    /// belongs to the turn that just used the server, until the grace ends.
    #[test]
    fn a_denial_trailing_a_finished_call_goes_to_that_turn() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t0 = Instant::now();
        h.in_flight("browser", "task-a").finish();
        let d = h
            .events
            .denied("browser", "challenges.x", None, false, t0)
            .unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-a"));
        let late = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
        let d = h
            .events
            .denied("browser", "stale.x", None, false, late)
            .unwrap();
        assert_eq!(d.task_id, None, "past the grace nobody owns it");
        assert!(h.events.inner.lock().unwrap().finished.is_empty());
    }

    /// One timeout used to switch attribution off for the server until the
    /// runtime restarted, so every later denial broadcast. The abandoned
    /// turn stays a suspect only for the grace; after that the turn using
    /// the server owns its denials again.
    #[test]
    fn an_abandoned_call_stops_blocking_attribution_after_the_grace() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t0 = Instant::now();
        drop(h.in_flight("browser", "task-a")); // timed out
        let alone = h
            .events
            .denied("browser", "one.x", None, false, t0)
            .unwrap();
        assert_eq!(
            alone.task_id.as_deref(),
            Some("task-a"),
            "its leftover work is the only suspect"
        );
        let _b = h.in_flight("browser", "task-b");
        let late = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
        let d = h
            .events
            .denied("browser", "two.x", None, false, late)
            .unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-b"));
        assert!(h.events.inner.lock().unwrap().abandoned.is_empty());
    }

    /// Review on #2106: turn A's page keeps loading after its call returns,
    /// and turn B starting a call does not stop it. A denial then could be
    /// either turn's, so it must broadcast, never be stamped with B's id
    /// (which would route A's own denial away from A). Once A's grace has
    /// passed, B is the only suspect again.
    #[test]
    fn a_finished_turns_trailing_traffic_is_not_stamped_with_another_turn() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t0 = Instant::now();
        h.in_flight("browser", "task-a").finish();
        let _b = h.in_flight("browser", "task-b");
        let d = h
            .events
            .denied("browser", "challenge.x", None, false, t0)
            .expect("an ambiguous denial still notifies");
        assert_eq!(d.task_id, None, "A or B: ambiguous, so broadcast");
        let later = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
        let d = h
            .events
            .denied("browser", "img.x", None, false, later)
            .unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-b"));
    }

    /// A call the server refused (or never received) started nothing, so it
    /// leaves no suspect behind: the next turn's denials stay its own.
    #[test]
    fn a_settled_call_leaves_no_trailing_suspect() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        h.in_flight("browser", "task-a").settle();
        let _b = h.in_flight("browser", "task-b");
        let d = h
            .events
            .denied("browser", "img.x", None, false, Instant::now())
            .unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-b"));
    }

    /// The throttle is per audience: once denials are routed to one turn, a
    /// later turn hitting the same host never saw the first notice, so it
    /// must get one instead of being swallowed by the other turn's window.
    /// While task-a's page may still be loading that notice is a broadcast.
    #[test]
    fn a_second_turn_gets_its_own_notice_inside_the_window() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t0 = Instant::now();
        let a = h.in_flight("browser", "task-a");
        assert!(
            h.events
                .denied("browser", "img.x", None, false, t0)
                .is_some()
        );
        assert!(
            h.events
                .denied("browser", "img.x", None, false, t0 + Duration::from_secs(1))
                .is_none(),
            "the same turn stays throttled"
        );
        a.finish();
        let _b = h.in_flight("browser", "task-b");
        let d = h
            .events
            .denied("browser", "img.x", None, false, t0 + Duration::from_secs(2))
            .expect("task-b never saw task-a's notice");
        assert_eq!(
            d.task_id, None,
            "task-a's trailing traffic is still a suspect"
        );
    }

    /// A call that stopped being waited on (timeout, cancel, panic) may still
    /// be running in the server. A later denial could be that leftover work,
    /// so it must not be pinned on whichever turn happens to be using the
    /// server now: it is broadcast, as before #1907.
    #[test]
    fn a_call_abandoned_mid_flight_makes_later_denials_unattributed() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let t = Instant::now();
        drop(h.in_flight("browser", "task-a")); // timed out: never finished
        let _b = h.in_flight("browser", "task-b");
        let d = h
            .events
            .denied("browser", "late.x", None, false, t)
            .unwrap();
        assert_eq!(d.task_id, None);
        // Other servers are unaffected.
        let _c = h.in_flight("fetch", "task-b");
        let d = h.events.denied("fetch", "ok.x", None, false, t).unwrap();
        assert_eq!(d.task_id.as_deref(), Some("task-b"));
    }

    /// Keying the throttle by audience adds an entry per turn that hits a
    /// host. In a runtime that lives for weeks those must not pile up: an
    /// entry whose window has passed is gone after the next denial, while an
    /// entry still inside its window keeps suppressing duplicates.
    #[test]
    fn expired_throttle_entries_do_not_outlive_their_window() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let window = Duration::from_secs(DENIED_EVENT_WINDOW_SECS);
        let t0 = Instant::now();
        for i in 0..100 {
            // One server each, so every turn is the sole suspect of its own.
            let server = format!("srv-{i}");
            let turn = h.in_flight(&server, &format!("task-{i}"));
            assert!(h.events.denied(&server, "img.x", None, false, t0).is_some());
            turn.finish();
        }
        let live = h.in_flight("browser", "task-live");
        let mid = t0 + window / 2;
        assert!(
            h.events
                .denied("browser", "img.x", None, false, mid)
                .is_some()
        );

        let later = t0 + window;
        assert!(
            h.events
                .denied("browser", "img.x", None, false, later)
                .is_none(),
            "task-live's own window is still open"
        );
        assert_eq!(
            h.events.inner.lock().unwrap().last.len(),
            1,
            "only task-live's entry is still inside its window"
        );
        live.finish();
    }

    #[test]
    fn deny_list_hits_are_tagged_ungrantable() {
        let ev = Events::new("mur");
        let d = ev
            .denied("search", "169.254.169.254", None, true, Instant::now())
            .unwrap();
        assert_eq!(d.reason, DenyReason::DenyListed);
        assert!(!d.grantable());
    }

    #[test]
    fn grant_adds_to_every_token_of_that_server_only() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let a = h.register_server("browser", vec!["shop.example.com".into()], vec![], false);
        let b = h.register_server("browser", vec![], vec![], false);
        let other = h.register_server("fetch", vec![], vec![], false);
        assert_eq!(
            h.grant("browser", "IMG.Shop.Example.com").unwrap(),
            "img.shop.example.com"
        );
        let reg = h.registry.lock().unwrap();
        assert!(reg[&a].allow.contains(&"img.shop.example.com".to_owned()));
        assert!(reg[&b].allow.contains(&"img.shop.example.com".to_owned()));
        assert!(reg[&other].allow.is_empty());
    }

    #[test]
    fn grant_refuses_broad_unknown_and_garbage() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        h.register_server("search", vec![], vec!["evil.example".into()], true);
        assert!(matches!(
            h.grant("search", "evil.example"),
            Err(GrantError::DenyListed { .. })
        ));
        assert!(matches!(
            h.grant("nope", "a.com"),
            Err(GrantError::UnknownServer(_))
        ));
        assert!(matches!(
            h.grant("search", "評分，"),
            Err(GrantError::InvalidHost(_))
        ));
    }
}
