use crate::{
    settings::{McpServerSettings, McpSettings, McpTransportKind},
    PwError, Result,
};

use super::{
    InvocationMode, LoadedTool, RiskLevel, ToolCall, ToolDescriptor, ToolExecutor, ToolLoader,
    ToolResult, ToolSource,
};
use reqwest::blocking::Client;
use reqwest::Url;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct McpToolLoader {
    settings: McpSettings,
    pwcli_home: PathBuf,
}

impl McpToolLoader {
    pub fn new(settings: McpSettings, pwcli_home: impl Into<PathBuf>) -> Self {
        Self {
            settings,
            pwcli_home: pwcli_home.into(),
        }
    }
}

impl Default for McpToolLoader {
    fn default() -> Self {
        Self {
            settings: McpSettings::default(),
            pwcli_home: PathBuf::from(".pwcli"),
        }
    }
}

impl ToolLoader for McpToolLoader {
    fn load(&self) -> Result<Vec<LoadedTool>> {
        let mut loaded = Vec::new();
        for server in &self.settings.servers {
            if !server.enabled {
                continue;
            }
            if let Err(err) = validate_server_config(server) {
                log_mcp(
                    &self.pwcli_home,
                    server,
                    &format!("configuration error: {err}"),
                );
                return Err(err);
            }
            match list_server_tools(server, &self.pwcli_home) {
                Ok(tools) => loaded.extend(tools),
                Err(err) => {
                    log_mcp(
                        &self.pwcli_home,
                        server,
                        &format!("failed to load tools: {err}"),
                    );
                }
            }
        }
        Ok(loaded)
    }
}

pub fn probe_mcp_server(server: &McpServerSettings, pwcli_home: impl AsRef<Path>) -> Result<usize> {
    validate_server_config(server)?;
    let tools = list_server_tools(server, pwcli_home.as_ref())?;
    Ok(tools.len())
}

fn validate_server_config(server: &McpServerSettings) -> Result<()> {
    if server.name.trim().is_empty() {
        return Err(PwError::Message("MCP server name is required".to_string()));
    }
    match server.transport {
        McpTransportKind::Stdio => {
            if server
                .command
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(PwError::Message(format!(
                    "MCP stdio server '{}' requires command",
                    server.name
                )));
            }
        }
        McpTransportKind::Http | McpTransportKind::Sse => {
            if server.url.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(PwError::Message(format!(
                    "MCP {:?} server '{}' requires url",
                    server.transport, server.name
                )));
            }
        }
    }
    Ok(())
}

fn list_server_tools(server: &McpServerSettings, pwcli_home: &Path) -> Result<Vec<LoadedTool>> {
    let mut client = McpClient::connect(server, pwcli_home)?;
    client.initialize()?;
    let tools = client.list_tools()?;
    Ok(tools
        .into_iter()
        .map(|tool| loaded_mcp_tool(server, pwcli_home, tool))
        .collect())
}

fn loaded_mcp_tool(server: &McpServerSettings, pwcli_home: &Path, tool: McpTool) -> LoadedTool {
    let tool_id = format!(
        "mcp.{}.{}",
        sanitize_id(&server.name),
        sanitize_id(&tool.name)
    );
    let description = tool
        .description
        .clone()
        .unwrap_or_else(|| format!("MCP tool '{}' from server '{}'", tool.name, server.name));
    let risk_level = parse_risk_level(server.risk_level.as_deref()).unwrap_or(RiskLevel::Medium);
    LoadedTool {
        descriptor: ToolDescriptor {
            id: tool_id,
            name: tool.name.clone(),
            description,
            input_schema: tool.input_schema.clone(),
            source: ToolSource::Mcp {
                server: server.name.clone(),
            },
            risk_level,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec![
                "mcp".to_string(),
                format!("mcp.server.{}", server.name),
                format!("mcp.tool.{}", tool.name),
            ],
            metadata: json!({
                "server": server.name,
                "transport": format!("{:?}", server.transport).to_lowercase(),
                "raw_tool": tool.raw,
                "log_path": mcp_log_path(pwcli_home, server).display().to_string(),
            }),
            enabled: true,
        },
        executor: Some(Arc::new(McpToolExecutor {
            server: server.clone(),
            pwcli_home: pwcli_home.to_path_buf(),
            tool_name: tool.name,
        })),
    }
}

#[derive(Clone)]
struct McpToolExecutor {
    server: McpServerSettings,
    pwcli_home: PathBuf,
    tool_name: String,
}

impl ToolExecutor for McpToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut client = McpClient::connect(&self.server, &self.pwcli_home)?;
        client.initialize()?;
        client.call_tool(&self.tool_name, call.arguments.clone())
    }
}

struct McpClient {
    transport: McpTransport,
    server: McpServerSettings,
    initialized: bool,
}

impl McpClient {
    fn connect(server: &McpServerSettings, pwcli_home: &Path) -> Result<Self> {
        let transport = match server.transport {
            McpTransportKind::Stdio => {
                McpTransport::Stdio(StdioSession::start(server, pwcli_home)?)
            }
            McpTransportKind::Http => McpTransport::Http(HttpSession::new(server)?),
            McpTransportKind::Sse => McpTransport::Sse(SseSession::connect(server)?),
        };
        Ok(Self {
            transport,
            server: server.clone(),
            initialized: false,
        })
    }

    fn initialize(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        let response = self.request(
            "initialize",
            Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "pwcli",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        )?;
        if response.get("serverInfo").is_none() && response.get("capabilities").is_none() {
            log_mcp(
                Path::new(".pwcli"),
                &self.server,
                &format!("initialize returned unexpected result: {response}"),
            );
        }
        self.notify("notifications/initialized", None)?;
        self.initialized = true;
        Ok(())
    }

    fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let mut cursor = None::<String>;
        let mut out = Vec::new();
        loop {
            let params = cursor
                .as_ref()
                .map(|cursor| json!({ "cursor": cursor }))
                .unwrap_or(Value::Null);
            let result = self.request(
                "tools/list",
                if params.is_null() { None } else { Some(params) },
            )?;
            let tools = result
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for raw in tools {
                if let Some(tool) = McpTool::from_value(raw) {
                    out.push(tool);
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolResult> {
        let result = self.request(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": if arguments.is_null() { json!({}) } else { arguments },
            })),
        )?;
        Ok(tool_result_from_mcp_result(result))
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = REQUEST_ID.fetch_add(1, Ordering::SeqCst);
        let request = json_rpc_request(id, method, params);
        let response = self.transport.request(&request, id)?;
        parse_json_rpc_response(response)
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = json_rpc_notification(method, params);
        self.transport.notify(&notification)
    }
}

enum McpTransport {
    Stdio(StdioSession),
    Http(HttpSession),
    Sse(SseSession),
}

impl McpTransport {
    fn request(&mut self, request: &Value, id: u64) -> Result<Value> {
        match self {
            Self::Stdio(session) => session.request(request, id),
            Self::Http(session) => session.request(request, id),
            Self::Sse(session) => session.request(request, id),
        }
    }

    fn notify(&mut self, notification: &Value) -> Result<()> {
        match self {
            Self::Stdio(session) => session.notify(notification),
            Self::Http(session) => session.notify(notification),
            Self::Sse(session) => session.notify(notification),
        }
    }
}

struct StdioSession {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    timeout: Duration,
}

impl StdioSession {
    fn start(server: &McpServerSettings, pwcli_home: &Path) -> Result<Self> {
        let command = server.command.as_deref().ok_or_else(|| {
            PwError::Message(format!(
                "MCP stdio server '{}' requires command",
                server.name
            ))
        })?;
        let mut cmd = Command::new(command);
        cmd.args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &server.cwd {
            cmd.current_dir(cwd);
        }
        for (key, value) in &server.env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|err| {
            PwError::ToolExecution(format!(
                "failed to start MCP server '{}': {err}",
                server.name
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            PwError::ToolExecution(format!("MCP server '{}' stdin unavailable", server.name))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PwError::ToolExecution(format!("MCP server '{}' stdout unavailable", server.name))
        })?;
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_logger(stderr, pwcli_home.to_path_buf(), server.clone());
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || read_mcp_json_stream(stdout, tx));
        Ok(Self {
            child,
            stdin,
            rx,
            timeout: Duration::from_secs(server.timeout_seconds),
        })
    }

    fn request(&mut self, request: &Value, id: u64) -> Result<Value> {
        write_json_line(&mut self.stdin, request)?;
        loop {
            let response = self
                .rx
                .recv_timeout(self.timeout)
                .map_err(|_| PwError::ToolExecution(format!("MCP stdio request {id} timed out")))?;
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(response);
            }
        }
    }

    fn notify(&mut self, notification: &Value) -> Result<()> {
        write_json_line(&mut self.stdin, notification)
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpSession {
    client: Client,
    url: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
}

impl HttpSession {
    fn new(server: &McpServerSettings) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(server.timeout_seconds))
                .build()
                .map_err(|err| PwError::ToolExecution(format!("MCP HTTP client error: {err}")))?,
            url: server.url.clone().ok_or_else(|| {
                PwError::Message(format!("MCP server '{}' requires url", server.name))
            })?,
            headers: server.headers.clone(),
            session_id: None,
        })
    }

    fn request(&mut self, request: &Value, id: u64) -> Result<Value> {
        let response = self.post_json(request)?;
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .or_else(|| response.headers().get("Mcp-Session-Id"))
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        if session_id.is_some() {
            self.session_id = session_id;
        }
        parse_http_json_or_sse_response(response, id)
    }

    fn notify(&mut self, notification: &Value) -> Result<()> {
        let _ = self.post_json(notification)?;
        Ok(())
    }

    fn post_json(&self, payload: &Value) -> Result<reqwest::blocking::Response> {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .json(payload);
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }
        if let Some(session_id) = &self.session_id {
            request = request.header("mcp-session-id", session_id);
        }
        request
            .send()
            .map_err(|err| PwError::ToolExecution(format!("MCP HTTP request failed: {err}")))
    }
}

struct SseSession {
    client: Client,
    reader: BufReader<reqwest::blocking::Response>,
    endpoint: String,
    headers: BTreeMap<String, String>,
}

impl SseSession {
    fn connect(server: &McpServerSettings) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(server.timeout_seconds))
            .build()
            .map_err(|err| PwError::ToolExecution(format!("MCP SSE client error: {err}")))?;
        let sse_url = server.url.clone().ok_or_else(|| {
            PwError::Message(format!("MCP server '{}' requires url", server.name))
        })?;
        let mut request = client.get(&sse_url).header("accept", "text/event-stream");
        for (key, value) in &server.headers {
            request = request.header(key, value);
        }
        let response = request
            .send()
            .map_err(|err| PwError::ToolExecution(format!("MCP SSE connect failed: {err}")))?;
        let mut session = Self {
            client,
            reader: BufReader::new(response),
            endpoint: String::new(),
            headers: server.headers.clone(),
        };
        session.endpoint = session.read_endpoint(&sse_url)?;
        Ok(session)
    }

    fn read_endpoint(&mut self, sse_url: &str) -> Result<String> {
        loop {
            let event = read_sse_event(&mut self.reader)?;
            if event.event.as_deref() == Some("endpoint") {
                let endpoint = event.data.trim();
                if endpoint.is_empty() {
                    return Err(PwError::ToolExecution(
                        "MCP SSE endpoint event was empty".to_string(),
                    ));
                }
                return absolutize_sse_endpoint(sse_url, endpoint);
            }
        }
    }

    fn request(&mut self, request: &Value, id: u64) -> Result<Value> {
        self.post_json(request)?;
        loop {
            let event = read_sse_event(&mut self.reader)?;
            if event.data.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(event.data.trim())?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(value);
            }
        }
    }

    fn notify(&mut self, notification: &Value) -> Result<()> {
        self.post_json(notification)
    }

    fn post_json(&self, payload: &Value) -> Result<()> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(payload);
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }
        let response = request
            .send()
            .map_err(|err| PwError::ToolExecution(format!("MCP SSE post failed: {err}")))?;
        if !response.status().is_success() {
            return Err(PwError::ToolExecution(format!(
                "MCP SSE post failed with HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SseEvent {
    event: Option<String>,
    data: String,
}

fn read_sse_event<R: BufRead>(reader: &mut R) -> Result<SseEvent> {
    let mut event = None::<String>;
    let mut data_lines = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(PwError::ToolExecution(
                "MCP SSE stream closed before response".to_string(),
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data_lines.is_empty() || event.is_some() {
                return Ok(SseEvent {
                    event,
                    data: data_lines.join("\n"),
                });
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
}

fn absolutize_sse_endpoint(sse_url: &str, endpoint: &str) -> Result<String> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return Ok(endpoint.to_string());
    }
    let base = Url::parse(sse_url)
        .map_err(|err| PwError::ToolExecution(format!("invalid MCP SSE url: {err}")))?;
    base.join(endpoint)
        .map(|url| url.to_string())
        .map_err(|err| PwError::ToolExecution(format!("invalid MCP SSE endpoint: {err}")))
}

#[derive(Debug, Clone)]
struct McpTool {
    name: String,
    description: Option<String>,
    input_schema: Value,
    raw: Value,
}

impl McpTool {
    fn from_value(raw: Value) -> Option<Self> {
        let name = raw.get("name")?.as_str()?.to_string();
        let description = raw
            .get("description")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let input_schema = raw
            .get("inputSchema")
            .or_else(|| raw.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        Some(Self {
            name,
            description,
            input_schema,
            raw,
        })
    }
}

fn json_rpc_request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    if let Some(params) = params {
        request["params"] = params;
    }
    request
}

fn json_rpc_notification(method: &str, params: Option<Value>) -> Value {
    let mut request = json!({
        "jsonrpc": "2.0",
        "method": method,
    });
    if let Some(params) = params {
        request["params"] = params;
    }
    request
}

fn parse_json_rpc_response(response: Value) -> Result<Value> {
    if let Some(error) = response.get("error") {
        return Err(PwError::ToolExecution(format!("MCP error: {error}")));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn parse_http_json_or_sse_response(
    response: reqwest::blocking::Response,
    id: u64,
) -> Result<Value> {
    if !response.status().is_success() {
        return Err(PwError::ToolExecution(format!(
            "MCP HTTP request failed with HTTP {}",
            response.status()
        )));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.contains("text/event-stream") {
        let mut reader = BufReader::new(response);
        loop {
            let event = read_sse_event(&mut reader)?;
            if event.data.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(event.data.trim())?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(value);
            }
        }
    }
    let text = response
        .text()
        .map_err(|err| PwError::ToolExecution(format!("MCP HTTP response read failed: {err}")))?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(PwError::from)
}

fn tool_result_from_mcp_result(result: Value) -> ToolResult {
    let is_error = result
        .get("isError")
        .or_else(|| result.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut content_parts = Vec::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "text" => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        content_parts.push(text.to_string());
                    }
                }
                "image" => {
                    let mime = item
                        .get("mimeType")
                        .or_else(|| item.get("mime_type"))
                        .and_then(Value::as_str)
                        .unwrap_or("image/*");
                    content_parts.push(format!("[MCP image content: {mime}]"));
                }
                "resource" => {
                    if let Some(resource) = item.get("resource") {
                        content_parts.push(format!("[MCP resource: {resource}]"));
                    } else {
                        content_parts.push(format!("[MCP resource: {item}]"));
                    }
                }
                _ => content_parts.push(item.to_string()),
            }
        }
    }
    if content_parts.is_empty() && !result.is_null() {
        content_parts.push(result.to_string());
    }
    ToolResult {
        content: content_parts.join("\n"),
        is_error,
        preview: None,
        full_content_ref: None,
        metadata: json!({ "mcp_result": result }),
        artifacts: Vec::new(),
        audit_hints: json!({ "source": "mcp" }),
    }
}

fn read_mcp_json_stream<R: Read>(reader: R, tx: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = String::new();
        let Ok(read) = reader.read_line(&mut line) else {
            break;
        };
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.to_ascii_lowercase().starts_with("content-length:") {
            let Some(length) = trimmed
                .split_once(':')
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            else {
                continue;
            };
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).ok().unwrap_or(0) == 0 {
                    return;
                }
                if header.trim().is_empty() {
                    break;
                }
            }
            let mut bytes = vec![0_u8; length];
            if reader.read_exact(&mut bytes).is_err() {
                return;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let _ = tx.send(value);
            }
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            let _ = tx.send(value);
        }
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

fn spawn_stderr_logger<R: Read + Send + 'static>(
    stderr: R,
    pwcli_home: PathBuf,
    server: McpServerSettings,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(std::result::Result::ok) {
            log_mcp(&pwcli_home, &server, &format!("stderr: {line}"));
        }
    });
}

fn log_mcp(pwcli_home: &Path, server: &McpServerSettings, message: &str) {
    let path = mcp_log_path(pwcli_home, server);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {}", chrono::Utc::now().to_rfc3339(), message);
    }
}

fn mcp_log_path(pwcli_home: &Path, server: &McpServerSettings) -> PathBuf {
    pwcli_home
        .join("logs")
        .join("mcp")
        .join(format!("{}.log", sanitize_id(&server.name)))
}

fn sanitize_id(value: &str) -> String {
    let out: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

fn parse_risk_level(value: Option<&str>) -> Option<RiskLevel> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "read_only" | "readonly" | "read-only" => Some(RiskLevel::ReadOnly),
        "low" => Some(RiskLevel::Low),
        "medium" => Some(RiskLevel::Medium),
        "high" => Some(RiskLevel::High),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolRegistry, ToolSource};
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
    };

    #[test]
    fn parses_tool_result_text_and_error_flag() {
        let result = tool_result_from_mcp_result(json!({
            "isError": true,
            "content": [
                { "type": "text", "text": "boom" },
                { "type": "image", "mimeType": "image/png", "data": "abc" }
            ]
        }));
        assert!(result.is_error);
        assert!(result.content.contains("boom"));
        assert!(result.content.contains("image/png"));
    }

    #[test]
    fn http_mcp_loader_registers_and_executes_tool() {
        let server = start_test_http_mcp_server();
        let temp = tempfile::tempdir().unwrap();
        let settings = McpSettings {
            servers: vec![McpServerSettings {
                name: "mock".to_string(),
                transport: McpTransportKind::Http,
                url: Some(server.url),
                timeout_seconds: 5,
                ..McpServerSettings::default()
            }],
        };
        let tools = McpToolLoader::new(settings, temp.path().join(".pwcli"))
            .load()
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].descriptor.source,
            ToolSource::Mcp {
                server: "mock".to_string()
            }
        );
        assert_eq!(tools[0].descriptor.id, "mcp.mock.echo");

        let mut registry = ToolRegistry::new();
        registry.register_many(tools);
        let snapshot = registry.snapshot();
        let result = snapshot
            .execute(&ToolCall {
                id: "call_1".to_string(),
                tool_id: "mcp.mock.echo".to_string(),
                name: "echo".to_string(),
                arguments: json!({ "message": "hello" }),
            })
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    struct TestHttpServer {
        url: String,
    }

    fn start_test_http_mcp_server() -> TestHttpServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming().flatten().take(8) {
                handle_test_http_connection(stream, Arc::clone(&seen_thread));
            }
        });
        TestHttpServer {
            url: format!("http://{addr}/mcp"),
        }
    }

    fn handle_test_http_connection(mut stream: TcpStream, seen: Arc<Mutex<Vec<String>>>) {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let read = stream.read(&mut tmp).unwrap();
            if read == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..read]);
            if let Some(header_end) = find_header_end(&buf) {
                let headers = String::from_utf8_lossy(&buf[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let body_start = header_end + 4;
                while buf.len() < body_start + content_length {
                    let read = stream.read(&mut tmp).unwrap();
                    if read == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..read]);
                }
                let body = &buf[body_start..body_start + content_length];
                let request: Value = serde_json::from_slice(body).unwrap();
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push(method.clone());
                let response = match method.as_str() {
                    "initialize" => json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": {
                            "protocolVersion": MCP_PROTOCOL_VERSION,
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "mock", "version": "1.0.0" }
                        }
                    }),
                    "notifications/initialized" => {
                        write_http_response(&mut stream, 202, "");
                        return;
                    }
                    "tools/list" => json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": {
                            "tools": [{
                                "name": "echo",
                                "description": "Echoes a message",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "message": { "type": "string" }
                                    }
                                }
                            }]
                        }
                    }),
                    "tools/call" => {
                        let message = request
                            .pointer("/params/arguments/message")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": {
                                "content": [{ "type": "text", "text": message }],
                                "isError": false
                            }
                        })
                    }
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "error": { "code": -32601, "message": "method not found" }
                    }),
                };
                write_http_response(&mut stream, 200, &response.to_string());
                return;
            }
        }
    }

    fn find_header_end(bytes: &[u8]) -> Option<usize> {
        bytes.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) {
        let status_text = match status {
            200 => "OK",
            202 => "Accepted",
            _ => "Error",
        };
        let response = format!(
            "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    }
}
