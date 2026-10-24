//! Event envelope persistence and per-connection metadata.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::runtime::acp::events::{EventBus, EventEnvelope};

#[derive(Clone, Debug)]
pub(crate) struct EventMeta {
    conversation_id: Option<String>,
    role_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct EventSink {
    bus: EventBus,
    events: Arc<dyn crate::domain::ports::EventRepository>,
    meta: Arc<Mutex<HashMap<String, EventMeta>>>,
}

impl EventSink {
    pub(crate) fn new(events: Arc<dyn crate::domain::ports::EventRepository>) -> Self {
        Self {
            bus: EventBus::new(),
            events,
            meta: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn register(
        &self,
        connection_id: &str,
        conversation_id: Option<String>,
        role_id: Option<String>,
    ) {
        self.meta.lock().unwrap().insert(
            connection_id.to_string(),
            EventMeta {
                conversation_id,
                role_id,
            },
        );
    }

    pub(crate) fn unregister(&self, connection_id: &str) {
        self.meta.lock().unwrap().remove(connection_id);
    }

    pub fn emit(&self, mut event: EventEnvelope) {
        if let Some(meta) = self.meta.lock().unwrap().get(&event.connection_id).cloned() {
            if event.conversation_id.is_none() {
                event.conversation_id = meta.conversation_id;
            }
            if event.speaker_role_id.is_none() {
                event.speaker_role_id = meta.role_id;
            }
        }
        let event = self.bus.publish(event);
        let value = serde_json::to_value(&event).unwrap_or(Value::Null);
        let _ = self.events.append_event(
            &event.connection_id,
            event.conversation_id.as_deref(),
            &value,
        );
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.bus.subscribe()
    }
}
