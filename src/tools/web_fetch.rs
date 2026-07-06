use crate::{
    tools::{ToolCall, ToolResult},
    PwError, Result,
};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone)]
pub struct WebFetchExecutor;

impl WebFetchExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for WebFetchExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let args = WebFetchArgs::from_value(&call.arguments)?;
        execute_web_fetch(&args)
    }
}

#[derive(Debug, Clone)]
struct WebFetchArgs {
    url: String,
    extract_text: bool,
    max_bytes: usize,
    timeout_seconds: u64,
}

impl WebFetchArgs {
    fn from_value(value: &Value) -> Result<Self> {
        let url = required_string(value, "url")?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(PwError::ToolExecution(
                "web_fetch only supports http:// and https:// URLs".to_string(),
            ));
        }
        Ok(Self {
            url,
            extract_text: optional_bool(value, "extract_text").unwrap_or(true),
            max_bytes: optional_u64(value, "max_bytes")
                .unwrap_or(DEFAULT_MAX_BYTES as u64)
                .min(4 * 1024 * 1024) as usize,
            timeout_seconds: optional_u64(value, "timeout_seconds")
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
                .max(1),
        })
    }
}

pub fn web_fetch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": { "type": "string", "description": "HTTP/HTTPS URL to fetch." },
            "extract_text": { "type": "boolean", "description": "Extract readable text from HTML. Defaults to true." },
            "max_bytes": { "type": "integer", "description": "Maximum response bytes to keep. Defaults to 512 KiB." },
            "timeout_seconds": { "type": "integer" }
        },
        "required": ["url"]
    })
}

fn execute_web_fetch(args: &WebFetchArgs) -> Result<ToolResult> {
    let client = Client::builder()
        .timeout(Duration::from_secs(args.timeout_seconds))
        .user_agent("pwcli/0.1 web_fetch")
        .build()
        .map_err(|err| PwError::ToolExecution(format!("failed to build web client: {err}")))?;
    let response = client
        .get(&args.url)
        .send()
        .map_err(|err| PwError::ToolExecution(format!("web_fetch request failed: {err}")))?;
    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(PwError::ToolExecution(format!(
            "web_fetch HTTP {status}: {}",
            truncate(&body, 1200)
        )));
    }
    let bytes = response
        .bytes()
        .map_err(|err| PwError::ToolExecution(format!("failed to read web response: {err}")))?;
    let truncated = bytes.len() > args.max_bytes;
    let slice = &bytes[..bytes.len().min(args.max_bytes)];
    let body = String::from_utf8_lossy(slice).to_string();
    let title = extract_title(&body);
    let text = if args.extract_text && content_type.to_ascii_lowercase().contains("html") {
        html_to_text(&body)
    } else {
        body.clone()
    };
    let preview = truncate(&text, 3000);
    let mut result = ToolResult::ok(text).with_preview(preview);
    result.metadata = json!({
        "url": args.url,
        "final_url": final_url,
        "status": status.as_u16(),
        "content_type": content_type,
        "title": title,
        "truncated": truncated,
        "bytes_read": slice.len(),
    });
    Ok(result)
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = &html[start..];
    let close_open = after.find('>')?;
    let after_open = &after[close_open + 1..];
    let end = after_open.to_ascii_lowercase().find("</title>")?;
    Some(decode_entities(after_open[..end].trim()))
}

fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut skip_until: Option<&'static str> = None;
    let mut tag_buf = String::new();
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(end_tag) = skip_until {
            if ch == '<' {
                let mut probe = String::from("<");
                while let Some(next) = chars.peek().copied() {
                    probe.push(next);
                    chars.next();
                    if next == '>' {
                        break;
                    }
                }
                if probe.to_ascii_lowercase().starts_with(end_tag) {
                    skip_until = None;
                }
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if in_tag {
            if ch == '>' {
                let tag = tag_buf.trim().to_ascii_lowercase();
                if tag.starts_with("script") {
                    skip_until = Some("</script");
                } else if tag.starts_with("style") {
                    skip_until = Some("</style");
                } else if block_tag(&tag) {
                    out.push('\n');
                }
                in_tag = false;
            } else {
                tag_buf.push(ch);
            }
            continue;
        }
        out.push(ch);
    }
    normalize_ws(&decode_entities(&out))
}

fn block_tag(tag: &str) -> bool {
    [
        "p", "br", "div", "section", "article", "li", "h1", "h2", "h3", "tr",
    ]
    .iter()
    .any(|prefix| tag.starts_with(prefix) || tag.starts_with(&format!("/{prefix}")))
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn normalize_ws(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| PwError::ToolExecution(format!("web_fetch requires '{key}'")))
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
