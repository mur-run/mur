use std::sync::Arc;
use tokio::sync::broadcast;

use serde::{Deserialize, Serialize};

// ─── HubEvent ────────────────────────────────────────────────────────────────

/// A single event on the Hub event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubEvent {
    /// Agent this event targets (or "*" for broadcast).
    pub agent_id: String,
    /// Dot-namespaced event name, e.g. "companion.message.new".
    pub name: String,
    /// Optional freeform payload (message text, error string, etc.).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,
}

impl HubEvent {
    pub fn new(agent_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            name: name.into(),
            payload: serde_json::Value::Null,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

// ─── EventBus ────────────────────────────────────────────────────────────────

/// Application-wide broadcast bus.  Clone to get a sender handle.
#[derive(Clone)]
pub struct EventBus {
    tx: Arc<broadcast::Sender<HubEvent>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx: Arc::new(tx) }
    }

    /// Publish an event; silently drops if no subscribers.
    pub fn publish(&self, event: HubEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to all future events.
    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.tx.subscribe()
    }
}
