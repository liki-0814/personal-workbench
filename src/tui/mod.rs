pub trait UserPrompter {
    fn confirm(&self, prompt: &str) -> bool;
}

mod terminal;

pub use terminal::TerminalUi;

pub struct StdinPrompter;

impl UserPrompter for StdinPrompter {
    fn confirm(&self, prompt: &str) -> bool {
        use std::io::{self, Write};

        print!("{prompt} [y/N] ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim(), "y" | "Y" | "yes" | "YES")
    }
}

impl crate::policy::UserApproval for StdinPrompter {
    fn ask_user(&self, prompt: &str, _call: &crate::tools::ToolCall) -> bool {
        self.confirm(prompt)
    }
}
