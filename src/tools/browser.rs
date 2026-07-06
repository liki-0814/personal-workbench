use crate::{
    tools::{ToolArtifact, ToolArtifactKind, ToolArtifactProvenance, ToolCall, ToolResult},
    PwError, Result,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
pub struct BrowserAutomationExecutor;

impl BrowserAutomationExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BrowserAutomationExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for BrowserAutomationExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let args = BrowserArgs::from_value(&call.arguments)?;
        execute_browser(&args, call)
    }
}

#[derive(Debug, Clone)]
struct BrowserArgs {
    action: String,
    url: String,
    output_path: Option<PathBuf>,
    wait_ms: u64,
    timeout_ms: u64,
}

impl BrowserArgs {
    fn from_value(value: &Value) -> Result<Self> {
        let url = required_string(value, "url")?;
        if !(url.starts_with("http://")
            || url.starts_with("https://")
            || url.starts_with("file://"))
        {
            return Err(PwError::ToolExecution(
                "browser_automation only supports http://, https://, and file:// URLs".to_string(),
            ));
        }
        Ok(Self {
            action: optional_string(value, "action").unwrap_or_else(|| "extract_text".to_string()),
            url,
            output_path: optional_string(value, "output_path").map(PathBuf::from),
            wait_ms: optional_u64(value, "wait_ms").unwrap_or(1000).min(30_000),
            timeout_ms: optional_u64(value, "timeout_ms")
                .unwrap_or(30_000)
                .min(120_000),
        })
    }
}

pub fn browser_automation_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["extract_text", "screenshot"],
                "description": "Browser action using local Playwright."
            },
            "url": { "type": "string" },
            "output_path": { "type": "string", "description": "PNG path for action=screenshot." },
            "wait_ms": { "type": "integer" },
            "timeout_ms": { "type": "integer" }
        },
        "required": ["url"]
    })
}

fn execute_browser(args: &BrowserArgs, call: &ToolCall) -> Result<ToolResult> {
    let payload = serde_json::to_string(&json!({
        "action": args.action,
        "url": args.url,
        "output_path": args.output_path,
        "wait_ms": args.wait_ms,
        "timeout_ms": args.timeout_ms,
    }))?;
    let output = Command::new("node")
        .arg("-e")
        .arg(BROWSER_SCRIPT)
        .env("PWCLI_BROWSER_ARGS", payload)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                PwError::ToolExecution(
                    "node binary not found; browser_automation requires Node.js and Playwright"
                        .to_string(),
                )
            } else {
                PwError::ToolExecution(format!("browser_automation failed to start node: {err}"))
            }
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(PwError::ToolExecution(format!(
            "browser_automation failed: {}\n{}",
            output.status, stderr
        )));
    }
    let value = serde_json::from_str::<Value>(&stdout).map_err(|err| {
        PwError::ToolExecution(format!("invalid browser output: {err}: {stdout}"))
    })?;
    let mut result = match args.action.as_str() {
        "screenshot" => ToolResult::ok(format!(
            "screenshot saved to {}",
            value["output_path"].as_str().unwrap_or("")
        )),
        _ => ToolResult::ok(value["text"].as_str().unwrap_or("").to_string()).with_preview(
            value["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(3000)
                .collect::<String>(),
        ),
    };
    if args.action == "screenshot" {
        let path = value["output_path"].as_str().unwrap_or_default();
        if !path.is_empty() && fs::metadata(path).is_ok() {
            result = result.add_artifact(ToolArtifact {
                path: PathBuf::from(path),
                kind: ToolArtifactKind::Image,
                title: Some("Browser screenshot".to_string()),
                media_type: Some("image/png".to_string()),
                preview: Some(args.url.clone()),
                full_content_ref: Some(path.to_string()),
                provenance: Some(ToolArtifactProvenance {
                    source: "builtin.browser_automation".to_string(),
                    uri: Some(args.url.clone()),
                    tool_call_id: Some(call.id.clone()),
                    metadata: json!({}),
                }),
            });
        }
    }
    result.metadata = value;
    Ok(result)
}

const BROWSER_SCRIPT: &str = r#"
const args = JSON.parse(process.env.PWCLI_BROWSER_ARGS || "{}");
let pw;
try {
  pw = require("playwright");
} catch (err) {
  try { pw = require("playwright-core"); } catch (err2) {
    console.error("Playwright is not installed. Install with: npm install -g playwright && playwright install chromium");
    process.exit(2);
  }
}
(async () => {
  const browser = await pw.chromium.launch({ headless: true });
  const page = await browser.newPage();
  await page.goto(args.url, { waitUntil: "domcontentloaded", timeout: args.timeout_ms || 30000 });
  if (args.wait_ms) await page.waitForTimeout(args.wait_ms);
  if (args.action === "screenshot") {
    const output = args.output_path || `pwcli-screenshot-${Date.now()}.png`;
    await page.screenshot({ path: output, fullPage: true });
    console.log(JSON.stringify({ action: args.action, url: page.url(), title: await page.title(), output_path: output }));
  } else {
    const text = await page.locator("body").innerText({ timeout: args.timeout_ms || 30000 }).catch(async () => await page.content());
    console.log(JSON.stringify({ action: args.action || "extract_text", url: page.url(), title: await page.title(), text }));
  }
  await browser.close();
})().catch(async (err) => {
  console.error(err && err.stack ? err.stack : String(err));
  process.exit(1);
});
"#;

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| PwError::ToolExecution(format!("browser_automation requires '{key}'")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}
