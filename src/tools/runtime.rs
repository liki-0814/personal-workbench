use crate::runtime::RuntimeTaskManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolExecutionMode {
    Sync,
    Streaming,
    Background,
    Resumable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolRuntimeEvent {
    Started {
        mode: ToolExecutionMode,
    },
    Progress {
        message: String,
    },
    Output {
        stream: String,
        chunk: String,
    },
    Artifact {
        path: PathBuf,
        media_type: Option<String>,
    },
    BackgroundTaskStarted {
        task_id: String,
        task_dir: PathBuf,
    },
    Poll {
        message: String,
        metadata: Value,
    },
    CancelRequested,
    Cancelled,
    TimedOut {
        timeout_seconds: u64,
    },
    Completed {
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ToolCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl ToolCancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for ToolCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Default)]
pub struct ToolExecutionContext {
    pub cancellation: ToolCancellationToken,
    pub runtime_tasks: Option<RuntimeTaskManager>,
}

pub struct ToolExecutionRuntime<'a> {
    context: ToolExecutionContext,
    sink: Box<dyn FnMut(ToolRuntimeEvent) + 'a>,
}

impl<'a> ToolExecutionRuntime<'a> {
    pub fn new(context: ToolExecutionContext, sink: impl FnMut(ToolRuntimeEvent) + 'a) -> Self {
        Self {
            context,
            sink: Box::new(sink),
        }
    }

    pub fn noop() -> Self {
        Self::new(ToolExecutionContext::default(), |_| {})
    }

    pub fn context(&self) -> &ToolExecutionContext {
        &self.context
    }

    pub fn cancellation(&self) -> &ToolCancellationToken {
        &self.context.cancellation
    }

    pub fn runtime_tasks(&self) -> Option<&RuntimeTaskManager> {
        self.context.runtime_tasks.as_ref()
    }

    pub fn emit(&mut self, event: ToolRuntimeEvent) {
        (self.sink)(event);
    }
}
