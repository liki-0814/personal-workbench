use crate::{PwError, Result};
use std::{
    io::Write,
    process::{Command, Stdio},
};

use super::manifest::ExecutableSkill;
use crate::tools::{registry::ToolExecutor, ToolCall, ToolResult};

pub struct JsonExecutableSkillExecutor {
    executable: ExecutableSkill,
}

impl JsonExecutableSkillExecutor {
    pub fn new(executable: ExecutableSkill) -> Self {
        Self { executable }
    }
}

impl ToolExecutor for JsonExecutableSkillExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let (program, args) = self
            .executable
            .command
            .split_first()
            .ok_or_else(|| PwError::ToolExecution("empty executable command".to_string()))?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let input = serde_json::to_vec(call)?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| PwError::ToolExecution("failed to open executable stdin".to_string()))?
            .write_all(&input)?;

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Ok(ToolResult::error(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let result: ToolResult = serde_json::from_slice(&output.stdout)?;
        Ok(result)
    }
}
