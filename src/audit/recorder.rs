use crate::{storage::append_jsonl, Result};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::AuditEvent;

pub trait AuditRecorder: Send + Sync {
    fn record(&self, event: AuditEvent);
}

#[derive(Clone, Default)]
pub struct InMemoryAuditRecorder {
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl InMemoryAuditRecorder {
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl AuditRecorder for InMemoryAuditRecorder {
    fn record(&self, event: AuditEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

#[derive(Clone)]
pub struct JsonlAuditRecorder {
    path: PathBuf,
    fallback: InMemoryAuditRecorder,
}

impl JsonlAuditRecorder {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            fallback: InMemoryAuditRecorder::default(),
        }
    }

    pub fn try_record(&self, event: &AuditEvent) -> Result<()> {
        append_jsonl(&self.path, event)
    }

    pub fn fallback_events(&self) -> Vec<AuditEvent> {
        self.fallback.events()
    }
}

impl AuditRecorder for JsonlAuditRecorder {
    fn record(&self, event: AuditEvent) {
        if self.try_record(&event).is_err() {
            self.fallback.record(event);
        }
    }
}
