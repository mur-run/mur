//! Expression state machine for background pet rendering + IPC push to Hub.
//!
//! The runtime runs one `ExpressionStateMachine` per agent. A 100ms tick
//! timer advances dwell timers. On every expression change the runtime
//! attempts to push a `SidecarMessage::ExpressionUpdate` to Hub's sidecar
//! stream socket, reconnecting with 1s backoff if Hub restarts.
//!
//! This mirrors `mur-gui-core/src/expression.rs` but runs inside the OS-managed
//! sidecar so C6/C7/D1 expression features work without Hub open.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use mur_common::expression::ExpressionChange;
use mur_common::hub::trigger::{DwellSpec, ExpressionTrigger, load_triggers};

// ─── Priority ─────────────────────────────────────────────────────────────────

fn priority_of(expression: &str) -> u8 {
    match expression {
        "error" | "cry" => 10,
        "hidden" => 9,
        "talk_open" | "talk_close" => 8,
        "think" => 6,
        "wave" | "sparkle" | "peek" | "smile" | "wow" => 4,
        "sleep" => 2,
        _ => 0,
    }
}

// ─── Active expression slot ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Slot {
    expression: String,
    dwell: DwellSpec,
    bubble_text: Option<String>,
    started_at: Instant,
    priority: u8,
}

impl Slot {
    fn new(trigger: &ExpressionTrigger, bubble_text: Option<String>) -> Self {
        let priority = priority_of(&trigger.expression);
        Self {
            expression: trigger.expression.clone(),
            dwell: trigger.dwell_s.clone(),
            bubble_text,
            started_at: Instant::now(),
            priority,
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        if let DwellSpec::Seconds(secs) = self.dwell {
            now.duration_since(self.started_at) >= Duration::from_secs_f64(secs)
        } else {
            false
        }
    }
}

// ─── ExpressionStateMachine ───────────────────────────────────────────────────

pub struct ExpressionStateMachine {
    agent_id: String,
    triggers: Vec<ExpressionTrigger>,
    active: Option<Slot>,
    queue: VecDeque<Slot>,
    last_fired: Option<Instant>,
    lipsync_last_tick: Option<Instant>,
    lipsync_phase: bool,
}

const DEBOUNCE: Duration = Duration::from_millis(100);

impl ExpressionStateMachine {
    pub fn new(agent_id: String, triggers: Vec<ExpressionTrigger>) -> Self {
        Self {
            agent_id,
            triggers,
            active: None,
            queue: VecDeque::new(),
            last_fired: None,
            lipsync_last_tick: None,
            lipsync_phase: false,
        }
    }

    /// Load triggers from the installed agent dir.
    pub fn from_agent_dir(agent_id: String, agent_dir: &std::path::Path) -> Self {
        let triggers = load_triggers(agent_dir);
        Self::new(agent_id, triggers)
    }

    /// Process an event by name. Returns `Some(change)` if the expression changed.
    pub fn process_event(
        &mut self,
        event_name: &str,
        payload_str: Option<&str>,
    ) -> Option<ExpressionChange> {
        let trigger = self.triggers.iter().find(|t| t.on == event_name)?.clone();
        let new_prio = priority_of(&trigger.expression);
        let active_prio = self.active.as_ref().map(|s| s.priority).unwrap_or(0);

        if new_prio <= active_prio {
            let now = Instant::now();
            if let Some(t) = self.last_fired
                && now.duration_since(t) < DEBOUNCE
            {
                return None;
            }
        }

        self.last_fired = Some(Instant::now());

        let bubble_text = if trigger.bubble {
            Some(
                trigger
                    .message
                    .clone()
                    .unwrap_or_else(|| payload_str.map(String::from).unwrap_or_default()),
            )
        } else {
            None
        };

        let slot = Slot::new(&trigger, bubble_text);

        if new_prio >= active_prio {
            let change = ExpressionChange {
                expression: slot.expression.clone(),
                bubble_text: slot.bubble_text.clone(),
            };
            self.active = Some(slot);
            self.queue.clear();
            Some(change)
        } else {
            self.queue.push_back(slot);
            None
        }
    }

    /// Advance dwell timers. Call every 100ms.
    pub fn tick(&mut self, now: Instant) -> Option<ExpressionChange> {
        if let Some(ref slot) = self.active
            && slot.dwell == DwellSpec::Lipsync
        {
            let elapsed = self
                .lipsync_last_tick
                .map(|t| now.duration_since(t))
                .unwrap_or(Duration::MAX);
            if elapsed >= Duration::from_millis(200) {
                self.lipsync_phase = !self.lipsync_phase;
                self.lipsync_last_tick = Some(now);
                let expression = if self.lipsync_phase {
                    "talk_close"
                } else {
                    "talk_open"
                };
                return Some(ExpressionChange {
                    expression: expression.into(),
                    bubble_text: slot.bubble_text.clone(),
                });
            }
            return None;
        }

        let expired = self
            .active
            .as_ref()
            .map(|s| s.is_expired(now))
            .unwrap_or(false);
        if !expired {
            return None;
        }

        if let Some(next) = self.queue.pop_front() {
            let change = ExpressionChange {
                expression: next.expression.clone(),
                bubble_text: next.bubble_text.clone(),
            };
            self.active = Some(next);
            Some(change)
        } else {
            self.active = None;
            Some(ExpressionChange {
                expression: "idle".into(),
                bubble_text: None,
            })
        }
    }

    /// Resolve a dwell that waits for an external signal (ack, done, etc.).
    pub fn resolve(&mut self, kind: DwellSpec) -> Option<ExpressionChange> {
        let matches = self
            .active
            .as_ref()
            .map(|s| s.dwell == kind)
            .unwrap_or(false);
        if !matches {
            return None;
        }
        if let Some(next) = self.queue.pop_front() {
            let change = ExpressionChange {
                expression: next.expression.clone(),
                bubble_text: next.bubble_text.clone(),
            };
            self.active = Some(next);
            Some(change)
        } else {
            self.active = None;
            Some(ExpressionChange {
                expression: "idle".into(),
                bubble_text: None,
            })
        }
    }
}

// ─── Hub push task ────────────────────────────────────────────────────────────

/// Spawn a background task that connects to Hub's sidecar stream socket and
/// pushes expression updates. Reconnects every 1s if Hub isn't ready yet.
///
/// Returns a channel sender — send `ExpressionChange` values to push them.
/// Only available on Unix (uses Unix domain sockets).
#[cfg(unix)]
pub fn spawn_hub_push(
    agent_id: String,
    slug: String,
    mur_home: std::path::PathBuf,
) -> tokio::sync::mpsc::Sender<ExpressionChange> {
    spawn_hub_push_impl(agent_id, slug, mur_home)
}

/// No-op stub on non-Unix platforms — the sidecar socket protocol is Unix-only.
#[cfg(not(unix))]
pub fn spawn_hub_push(
    _agent_id: String,
    _slug: String,
    _mur_home: std::path::PathBuf,
) -> tokio::sync::mpsc::Sender<ExpressionChange> {
    let (tx, _rx) = tokio::sync::mpsc::channel::<ExpressionChange>(32);
    tx
}

#[cfg(unix)]
fn spawn_hub_push_impl(
    agent_id: String,
    slug: String,
    mur_home: std::path::PathBuf,
) -> tokio::sync::mpsc::Sender<ExpressionChange> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ExpressionChange>(32);

    tokio::spawn(async move {
        loop {
            // Try to connect to Hub's sidecar stream socket.
            let socket_path = mur_home.join("agents").join(&slug).join("sidecar.sock");
            let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await else {
                // Hub not ready yet — wait and retry.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            };

            // Send heartbeat to signal presence.
            let hb = serde_json::json!({
                "kind": "heartbeat",
                "ts": chrono::Utc::now().timestamp()
            });
            let mut hb_bytes = serde_json::to_vec(&hb).unwrap_or_default();
            hb_bytes.push(b'\n');
            use tokio::io::AsyncWriteExt;
            if stream.write_all(&hb_bytes).await.is_err() {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }

            // Forward ExpressionChange messages until the connection drops.
            while let Some(change) = rx.recv().await {
                let msg = serde_json::json!({
                    "kind": "expression_update",
                    "change": {
                        "expression": change.expression,
                        "bubble_text": change.bubble_text
                    },
                    "agent_id": agent_id,
                    "ts": chrono::Utc::now().timestamp()
                });
                let mut bytes = serde_json::to_vec(&msg).unwrap_or_default();
                bytes.push(b'\n');
                if stream.write_all(&bytes).await.is_err() {
                    break; // reconnect
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    tx
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::hub::trigger::default_triggers;

    fn sm() -> ExpressionStateMachine {
        ExpressionStateMachine::new("agent-1".into(), default_triggers())
    }

    #[test]
    fn click_emits_smile() {
        let mut sm = sm();
        let ch = sm.process_event("user.click.pet", None).unwrap();
        assert_eq!(ch.expression, "smile");
    }

    #[test]
    fn error_preempts_smile() {
        let mut sm = sm();
        sm.process_event("user.click.pet", None);
        let ch = sm.process_event("agent.error", None).unwrap();
        assert_eq!(ch.expression, "error");
    }

    #[test]
    fn smile_does_not_preempt_error() {
        let mut sm = sm();
        sm.process_event("agent.error", None);
        let ch = sm.process_event("user.click.pet", None);
        assert!(ch.is_none());
    }

    #[test]
    fn tick_expires_timed_dwell() {
        let mut sm = sm();
        sm.process_event("user.click.pet", None); // smile, dwell 2s
        if let Some(ref mut slot) = sm.active {
            slot.started_at = Instant::now() - Duration::from_secs(5);
        }
        let ch = sm.tick(Instant::now()).unwrap();
        assert_eq!(ch.expression, "idle");
    }

    #[test]
    fn resolve_until_ack_reverts_to_idle() {
        let mut sm = sm();
        sm.process_event("agent.error", None);
        let ch = sm.resolve(DwellSpec::UntilAck).unwrap();
        assert_eq!(ch.expression, "idle");
    }
}
