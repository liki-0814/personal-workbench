use crate::{
    settings::{MineruSettings, Settings},
    tools::{ToolCall, ToolResult},
    PwError, Result,
};
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use zip::ZipArchive;

const DEFAULT_MODEL_VERSION: &str = "vlm";
const DEFAULT_LANGUAGE: &str = "ch";
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 3;

#[derive(Debug, Clone)]
pub struct MineruExecutor;

impl MineruExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MineruExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for MineruExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let settings = Settings::load()?.mineru;
        let args = MineruParseArgs::from_value(&call.arguments)?;
        let output = execute_mineru_parse(&settings, &args)?;
        let mut result = ToolResult::ok(output.to_string());
        result.metadata = output;
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct MineruParseArgs {
    url: Option<String>,
    path: Option<PathBuf>,
    model_version: String,
    language: String,
    is_ocr: Option<bool>,
    enable_formula: Option<bool>,
    enable_table: Option<bool>,
    page_ranges: Option<String>,
    extra_formats: Vec<String>,
    no_cache: Option<bool>,
    cache_tolerance: Option<u64>,
    wait: bool,
    timeout_seconds: u64,
    poll_interval_seconds: u64,
}

impl MineruParseArgs {
    fn from_value(value: &Value) -> Result<Self> {
        let url = optional_string(value, "url");
        let path = optional_string(value, "path").map(PathBuf::from);
        if url.is_none() == path.is_none() {
            return Err(PwError::ToolExecution(
                "mineru_parse_document requires exactly one of 'url' or 'path'".to_string(),
            ));
        }

        Ok(Self {
            url,
            path,
            model_version: optional_string(value, "model_version")
                .unwrap_or_else(|| DEFAULT_MODEL_VERSION.to_string()),
            language: optional_string(value, "language")
                .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string()),
            is_ocr: optional_bool(value, "is_ocr"),
            enable_formula: optional_bool(value, "enable_formula"),
            enable_table: optional_bool(value, "enable_table"),
            page_ranges: optional_string(value, "page_ranges")
                .or_else(|| optional_string(value, "page_range")),
            extra_formats: value
                .get("extra_formats")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            no_cache: optional_bool(value, "no_cache"),
            cache_tolerance: optional_u64(value, "cache_tolerance"),
            wait: optional_bool(value, "wait").unwrap_or(true),
            timeout_seconds: optional_u64(value, "timeout_seconds")
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS),
            poll_interval_seconds: optional_u64(value, "poll_interval_seconds")
                .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS)
                .max(1),
        })
    }
}

pub fn mineru_parse_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "Remote document URL. Use either url or path, not both."
            },
            "path": {
                "type": "string",
                "description": "Local document path. pwcli uploads it through MinerU signed upload."
            },
            "model_version": {
                "type": "string",
                "enum": ["pipeline", "vlm", "MinerU-HTML"],
                "description": "MinerU model version. Defaults to vlm."
            },
            "language": {
                "type": "string",
                "description": "OCR language. Defaults to ch."
            },
            "is_ocr": {
                "type": "boolean",
                "description": "Whether to enable OCR."
            },
            "enable_formula": {
                "type": "boolean",
                "description": "Whether to enable formula recognition."
            },
            "enable_table": {
                "type": "boolean",
                "description": "Whether to enable table recognition."
            },
            "page_ranges": {
                "type": "string",
                "description": "Page range, for example 1-10 or 2,4-6."
            },
            "extra_formats": {
                "type": "array",
                "items": { "type": "string", "enum": ["docx", "html", "latex"] },
                "description": "Additional precise API export formats. Markdown and JSON are included by default."
            },
            "no_cache": {
                "type": "boolean",
                "description": "Bypass MinerU URL cache."
            },
            "cache_tolerance": {
                "type": "integer",
                "description": "URL cache tolerance seconds when no_cache is false."
            },
            "wait": {
                "type": "boolean",
                "description": "Poll until done or failed. Defaults to true."
            },
            "timeout_seconds": {
                "type": "integer",
                "description": "Polling timeout. Defaults to 300 seconds."
            },
            "poll_interval_seconds": {
                "type": "integer",
                "description": "Polling interval. Defaults to 3 seconds."
            }
        }
    })
}

fn execute_mineru_parse(settings: &MineruSettings, args: &MineruParseArgs) -> Result<Value> {
    let token = settings.token.as_deref().ok_or_else(|| {
        PwError::ToolExecution(
            "MinerU token is not configured. Add {\"mineru\":{\"token\":\"...\"}} to ~/.pwcli/config.json or set PWCLI_MINERU_TOKEN.".to_string(),
        )
    })?;
    let client = Client::builder()
        .timeout(Duration::from_secs(settings.request_timeout_seconds.max(1)))
        .build()
        .map_err(|err| PwError::ToolExecution(format!("failed to build MinerU client: {err}")))?;

    if let Some(url) = &args.url {
        submit_url_task(&client, settings, token, args, url)
    } else if let Some(path) = &args.path {
        submit_local_file_task(&client, settings, token, args, path)
    } else {
        unreachable!("validated by MineruParseArgs::from_value")
    }
}

fn submit_url_task(
    client: &Client,
    settings: &MineruSettings,
    token: &str,
    args: &MineruParseArgs,
    url: &str,
) -> Result<Value> {
    let mut body = precise_base_body(args);
    body.insert("url".to_string(), Value::String(url.to_string()));
    insert_optional_bool(&mut body, "no_cache", args.no_cache);
    if let Some(cache_tolerance) = args.cache_tolerance {
        body.insert("cache_tolerance".to_string(), json!(cache_tolerance));
    }

    let response = client
        .post(endpoint(settings, "/api/v4/extract/task"))
        .bearer_auth(token)
        .json(&Value::Object(body))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| PwError::ToolExecution(format!("MinerU submit URL task failed: {err}")))?
        .json::<Value>()
        .map_err(|err| PwError::ToolExecution(format!("invalid MinerU submit response: {err}")))?;
    ensure_mineru_ok(&response)?;

    let task_id = response["data"]["task_id"].as_str().ok_or_else(|| {
        PwError::ToolExecution("MinerU response missing data.task_id".to_string())
    })?;
    if !args.wait {
        return Ok(json!({
            "mode": "url",
            "task_id": task_id,
            "state": "submitted",
            "submit_response": response
        }));
    }

    let result = poll_task(client, settings, token, task_id, args)?;
    let mut output = json!({
        "mode": "url",
        "task_id": task_id,
        "state": result["data"]["state"].clone(),
        "full_zip_url": result["data"]["full_zip_url"].clone(),
        "result": result
    });
    attach_zip_markdown(client, &mut output);
    Ok(output)
}

fn submit_local_file_task(
    client: &Client,
    settings: &MineruSettings,
    token: &str,
    args: &MineruParseArgs,
    path: &Path,
) -> Result<Value> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| PwError::ToolExecution(format!("invalid file path: {}", path.display())))?;
    let mut file_spec = Map::new();
    file_spec.insert("name".to_string(), Value::String(file_name.to_string()));
    insert_optional_bool(&mut file_spec, "is_ocr", args.is_ocr);
    if let Some(page_ranges) = &args.page_ranges {
        file_spec.insert(
            "page_ranges".to_string(),
            Value::String(page_ranges.clone()),
        );
    }

    let mut body = precise_base_body(args);
    body.insert(
        "files".to_string(),
        Value::Array(vec![Value::Object(file_spec)]),
    );

    let response = client
        .post(endpoint(settings, "/api/v4/file-urls/batch"))
        .bearer_auth(token)
        .json(&Value::Object(body))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| {
            PwError::ToolExecution(format!("MinerU signed upload request failed: {err}"))
        })?
        .json::<Value>()
        .map_err(|err| PwError::ToolExecution(format!("invalid MinerU upload response: {err}")))?;
    ensure_mineru_ok(&response)?;

    let batch_id = response["data"]["batch_id"].as_str().ok_or_else(|| {
        PwError::ToolExecution("MinerU response missing data.batch_id".to_string())
    })?;
    let file_url = response["data"]["file_urls"]
        .as_array()
        .and_then(|urls| urls.first())
        .and_then(Value::as_str)
        .ok_or_else(|| {
            PwError::ToolExecution("MinerU response missing upload file_urls[0]".to_string())
        })?;

    let bytes = fs::read(path)?;
    client
        .put(file_url)
        .body(bytes)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| PwError::ToolExecution(format!("MinerU file upload failed: {err}")))?;

    if !args.wait {
        return Ok(json!({
            "mode": "local_file",
            "batch_id": batch_id,
            "state": "uploaded",
            "submit_response": response
        }));
    }

    let result = poll_batch(client, settings, token, batch_id, args)?;
    let first = result["data"]["extract_result"]
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut output = json!({
        "mode": "local_file",
        "batch_id": batch_id,
        "state": first["state"].clone(),
        "full_zip_url": first["full_zip_url"].clone(),
        "result": result
    });
    attach_zip_markdown(client, &mut output);
    Ok(output)
}

fn poll_task(
    client: &Client,
    settings: &MineruSettings,
    token: &str,
    task_id: &str,
    args: &MineruParseArgs,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds.max(1));
    loop {
        let response = client
            .get(endpoint(
                settings,
                &format!("/api/v4/extract/task/{task_id}"),
            ))
            .bearer_auth(token)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|err| PwError::ToolExecution(format!("MinerU poll task failed: {err}")))?
            .json::<Value>()
            .map_err(|err| {
                PwError::ToolExecution(format!("invalid MinerU poll response: {err}"))
            })?;
        ensure_mineru_ok(&response)?;
        match response["data"]["state"].as_str().unwrap_or_default() {
            "done" => return Ok(response),
            "failed" => {
                return Err(PwError::ToolExecution(format!(
                    "MinerU task failed: {}",
                    response["data"]["err_msg"]
                        .as_str()
                        .unwrap_or("unknown error")
                )));
            }
            _ if Instant::now() >= deadline => {
                return Ok(json!({
                    "code": 0,
                    "msg": "timeout",
                    "data": {
                        "task_id": task_id,
                        "state": "timeout",
                        "last_response": response
                    }
                }));
            }
            _ => thread::sleep(Duration::from_secs(args.poll_interval_seconds)),
        }
    }
}

fn poll_batch(
    client: &Client,
    settings: &MineruSettings,
    token: &str,
    batch_id: &str,
    args: &MineruParseArgs,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds.max(1));
    loop {
        let response = client
            .get(endpoint(
                settings,
                &format!("/api/v4/extract-results/batch/{batch_id}"),
            ))
            .bearer_auth(token)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|err| PwError::ToolExecution(format!("MinerU poll batch failed: {err}")))?
            .json::<Value>()
            .map_err(|err| {
                PwError::ToolExecution(format!("invalid MinerU batch response: {err}"))
            })?;
        ensure_mineru_ok(&response)?;
        let first_state = response["data"]["extract_result"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["state"].as_str())
            .unwrap_or_default();
        match first_state {
            "done" => return Ok(response),
            "failed" => {
                let err_msg = response["data"]["extract_result"]
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(|item| item["err_msg"].as_str())
                    .unwrap_or("unknown error");
                return Err(PwError::ToolExecution(format!(
                    "MinerU batch failed: {err_msg}"
                )));
            }
            _ if Instant::now() >= deadline => {
                return Ok(json!({
                    "code": 0,
                    "msg": "timeout",
                    "data": {
                        "batch_id": batch_id,
                        "state": "timeout",
                        "last_response": response
                    }
                }));
            }
            _ => thread::sleep(Duration::from_secs(args.poll_interval_seconds)),
        }
    }
}

fn precise_base_body(args: &MineruParseArgs) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert(
        "model_version".to_string(),
        Value::String(args.model_version.clone()),
    );
    body.insert("language".to_string(), Value::String(args.language.clone()));
    insert_optional_bool(&mut body, "is_ocr", args.is_ocr);
    insert_optional_bool(&mut body, "enable_formula", args.enable_formula);
    insert_optional_bool(&mut body, "enable_table", args.enable_table);
    if let Some(page_ranges) = &args.page_ranges {
        body.insert(
            "page_ranges".to_string(),
            Value::String(page_ranges.clone()),
        );
    }
    if !args.extra_formats.is_empty() {
        body.insert("extra_formats".to_string(), json!(args.extra_formats));
    }
    body
}

fn ensure_mineru_ok(value: &Value) -> Result<()> {
    if value["code"].as_i64().unwrap_or(-1) == 0 {
        return Ok(());
    }
    Err(PwError::ToolExecution(format!(
        "MinerU API error: code={}, msg={}",
        value["code"],
        value["msg"].as_str().unwrap_or("unknown error")
    )))
}

fn attach_zip_markdown(client: &Client, output: &mut Value) {
    let Some(url) = output
        .get("full_zip_url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
    else {
        return;
    };
    match download_zip_markdown(client, url) {
        Ok(markdown) if !markdown.trim().is_empty() => {
            output["markdown_chars"] = json!(markdown.chars().count());
            output["markdown"] = json!(markdown);
        }
        Ok(_) => {
            output["markdown_error"] = json!("zip downloaded but no markdown/text file was found");
        }
        Err(err) => {
            output["markdown_error"] = json!(err.to_string());
        }
    }
}

fn download_zip_markdown(client: &Client, url: &str) -> Result<String> {
    let bytes = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| PwError::ToolExecution(format!("MinerU result zip download failed: {err}")))?
        .bytes()
        .map_err(|err| PwError::ToolExecution(format!("MinerU result zip read failed: {err}")))?;
    let reader = Cursor::new(bytes);
    let mut archive = ZipArchive::new(reader)
        .map_err(|err| PwError::ToolExecution(format!("MinerU result zip open failed: {err}")))?;
    let mut best_name = None;
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|err| {
            PwError::ToolExecution(format!("MinerU result zip entry failed: {err}"))
        })?;
        let name = file.name().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            best_name = Some(name);
            break;
        }
        if best_name.is_none() && lower.ends_with(".txt") {
            best_name = Some(name);
        }
    }
    let Some(best_name) = best_name else {
        return Ok(String::new());
    };
    let mut file = archive.by_name(&best_name).map_err(|err| {
        PwError::ToolExecution(format!("MinerU result zip markdown open failed: {err}"))
    })?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| PwError::ToolExecution(format!("MinerU markdown decode failed: {err}")))?;
    Ok(text)
}

fn endpoint(settings: &MineruSettings, path: &str) -> String {
    format!(
        "{}{}",
        settings.base_url.trim_end_matches('/'),
        if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        }
    )
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn insert_optional_bool(body: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        body.insert(key.to_string(), Value::Bool(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_require_exactly_one_source() {
        assert!(MineruParseArgs::from_value(&json!({})).is_err());
        assert!(MineruParseArgs::from_value(&json!({
            "url": "https://example.com/a.pdf",
            "path": "a.pdf"
        }))
        .is_err());
        assert!(MineruParseArgs::from_value(&json!({
            "url": "https://example.com/a.pdf"
        }))
        .is_ok());
    }

    #[test]
    fn precise_body_maps_common_options() {
        let args = MineruParseArgs::from_value(&json!({
            "url": "https://example.com/a.pdf",
            "model_version": "pipeline",
            "language": "en",
            "is_ocr": true,
            "enable_formula": false,
            "enable_table": true,
            "page_ranges": "1-2",
            "extra_formats": ["html"]
        }))
        .unwrap();
        let body = precise_base_body(&args);
        assert_eq!(body["model_version"], "pipeline");
        assert_eq!(body["language"], "en");
        assert_eq!(body["is_ocr"], true);
        assert_eq!(body["enable_formula"], false);
        assert_eq!(body["enable_table"], true);
        assert_eq!(body["page_ranges"], "1-2");
        assert_eq!(body["extra_formats"], json!(["html"]));
    }
}
