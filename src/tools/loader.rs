use crate::Result;

use super::{registry::ToolExecutor, ToolDescriptor};
use std::sync::Arc;

#[derive(Clone)]
pub struct LoadedTool {
    pub descriptor: ToolDescriptor,
    pub executor: Option<Arc<dyn ToolExecutor>>,
}

pub trait ToolLoader {
    fn load(&self) -> Result<Vec<LoadedTool>>;
}
