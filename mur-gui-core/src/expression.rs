use std::collections::VecDeque;
use std::time::{Duration, Instant};

use mur_common::hub::trigger::{DwellSpec, ExpressionTrigger};

use crate::event_bus::HubEvent;

// ─── Priority ────────────────────────────────────────────────────────────────

fn priority_of(expression: &str) -> u8 {
    match expression {
        "error" | "cry" => 10,
        "hidden" => 9,
        "talk_open" | "talk_close" => 8,
        "think" => 6,
        "wave" | "sparkle" | "peek" | "smile" | "wow" => 4,
        "sleep" => 2,
        _ => 0, // idle + unknown
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

    /// Returns true if this slot's timed dwell has expired.
    fn is_expired(&self, now: Instant) -> bool {
        if let DwellSpec::Seconds(secs) = self.dwell {
            now.duration_since(self.started_at) >= Duration::from_secs_f64(secs)
        } else {
            false
        }
    }
}

// ─── ExpressionChange ────────────────────────────────────────────────────────

/// Returned by the state machine whenever the visible expression changes.
#[derive(Debug, Clone)]
pub struct ExpressionChange {
    pub expression: String,
    /// Text to show in the speech bubble, if any.
    pub bubble_text: Option<String>,
}

// ─── ExpressionStateMachine ──────────────────────────────────────────────────

/// Per-pet state machine that maps `HubEvent`s to expression changes.
pub struct ExpressionStateMachine {
    agent_id: String,
    triggers: Vec<ExpressionTrigger>,
    /// Currently active expression slot (None = idle).
    active: Option<Slot>,
    /// Lower-priority reactions waiting behind the active one.
    queue: VecDeque<Slot>,
    /// Last time any trigger fired — for 100ms debounce.
    last_fired: Option<Instant>,
    /// Last time the lipsync alternation ticked.
    lipsync_last_tick: Option<Instant>,
    /// Which half of the lipsync cycle we're in (false=open, true=close).
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

    /// Process an incoming event.  Returns `Some(change)` if the visible
    /// expression should update.
    pub fn process(&mut self, event: &HubEvent) -> Option<ExpressionChange> {
        if event.agent_id != self.agent_id && event.agent_id != "*" {
            return None;
        }

        // Find the first matching trigger.
        let trigger = self.triggers.iter().find(|t| t.on == event.name)?.clone();

        let new_prio = priority_of(&trigger.expression);
        let active_prio = self.active.as_ref().map(|s| s.priority).unwrap_or(0);

        // 100ms debounce only for same-or-lower priority (prevents flicker).
        // Higher-priority events (error, cry) always preempt immediately.
        if new_prio <= active_prio {
            let now = Instant::now();
            if let Some(t) = self.last_fired
                && now.duration_since(t) < DEBOUNCE
            {
                return None;
            }
        }

        self.last_fired = Some(Instant::now());

        let bubble_text =
            if trigger.bubble {
                Some(trigger.message.clone().unwrap_or_else(|| {
                    event.payload.as_str().map(String::from).unwrap_or_default()
                }))
            } else {
                None
            };

        let slot = Slot::new(&trigger, bubble_text);

        if new_prio >= active_prio {
            // Preempt current expression.
            let change = ExpressionChange {
                expression: slot.expression.clone(),
                bubble_text: slot.bubble_text.clone(),
            };
            self.active = Some(slot);
            self.queue.clear();
            Some(change)
        } else {
            // Queue behind the active expression.
            self.queue.push_back(slot);
            None
        }
    }

    /// Advance dwell timers.  Call this regularly (e.g. every 100ms).
    /// Returns `Some(change)` if the active expression expired and a new one
    /// (or idle) takes over.
    pub fn tick(&mut self, now: Instant) -> Option<ExpressionChange> {
        // Lipsync mode: alternate talk_open / talk_close every 200ms while
        // the active slot has dwell_s: lipsync.
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

        // Pop from queue or fall back to idle.
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

    /// Resolve `until_ack` / `until_done` / `until_active` / `until_focus_off`
    /// dwells for the given resolution kind.
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::hub::trigger::default_triggers;

    fn sm() -> ExpressionStateMachine {
        ExpressionStateMachine::new("agent-1".into(), default_triggers())
    }

    fn ev(name: &str) -> HubEvent {
        HubEvent::new("agent-1", name)
    }

    #[test]
    fn click_emits_smile() {
        let mut sm = sm();
        let ch = sm.process(&ev("user.click.pet")).unwrap();
        assert_eq!(ch.expression, "smile");
    }

    #[test]
    fn error_preempts_smile() {
        let mut sm = sm();
        sm.process(&ev("user.click.pet"));
        // error has higher priority → should preempt
        let ch = sm.process(&ev("agent.error")).unwrap();
        assert_eq!(ch.expression, "error");
    }

    #[test]
    fn smile_does_not_preempt_error() {
        let mut sm = sm();
        sm.process(&ev("agent.error"));
        // smile < error priority → queued, no immediate change
        let ch = sm.process(&ev("user.click.pet"));
        assert!(ch.is_none());
    }

    #[test]
    fn tick_expires_timed_dwell() {
        let mut sm = sm();
        sm.process(&ev("user.click.pet")); // smile, dwell 2s
        // Force expire by setting started_at in the past.
        if let Some(ref mut slot) = sm.active {
            slot.started_at = Instant::now() - Duration::from_secs(5);
        }
        let ch = sm.tick(Instant::now()).unwrap();
        assert_eq!(ch.expression, "idle");
    }

    #[test]
    fn resolve_until_ack_reverts_to_idle() {
        let mut sm = sm();
        sm.process(&ev("agent.error")); // dwell=until_ack
        let ch = sm.resolve(DwellSpec::UntilAck).unwrap();
        assert_eq!(ch.expression, "idle");
    }

    #[test]
    fn unknown_agent_ignored() {
        let mut sm = sm();
        let mut e = ev("user.click.pet");
        e.agent_id = "other-agent".into();
        assert!(sm.process(&e).is_none());
    }
}
