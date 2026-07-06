use crate::tools::ToolCall;

pub trait UserApproval {
    fn ask_user(&self, prompt: &str, call: &ToolCall) -> bool;
}
