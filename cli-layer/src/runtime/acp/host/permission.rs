//! Pending ACP permission requests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub enum PermissionDecision {
    Selected(String),
    Cancelled,
}

#[derive(Clone, Default)]
pub struct PermissionBroker {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>>,
}

impl PermissionBroker {
    pub(crate) fn open(&self) -> (String, oneshot::Receiver<PermissionDecision>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        (request_id, rx)
    }

    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        let sender = self.pending.lock().unwrap().remove(request_id);
        match sender {
            Some(sender) => sender.send(decision).is_ok(),
            None => false,
        }
    }
}
