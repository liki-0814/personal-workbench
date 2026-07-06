use std::sync::Arc;

use super::{executor::GraphExecutor, hooks::GraphHook};

#[derive(Clone)]
pub struct GraphBuilder {
    max_rounds: u32,
    hooks: Vec<Arc<dyn GraphHook>>,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self {
            max_rounds: 100,
            hooks: Vec::new(),
        }
    }
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn max_rounds(mut self, max_rounds: u32) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    pub fn hook(mut self, hook: impl GraphHook + 'static) -> Self {
        self.hooks.push(Arc::new(hook));
        self
    }

    pub fn build(self) -> GraphExecutor {
        GraphExecutor {
            max_rounds: self.max_rounds,
            hooks: self.hooks,
        }
    }
}
