use crate::{
    tools::{ToolCall, ToolResult},
    PwError, Result,
};
use serde_json::{json, Value};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct SqlDryRunExecutor;

impl SqlDryRunExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SqlDryRunExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for SqlDryRunExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let args = SqlDryRunArgs::from_value(&call.arguments)?;
        execute_sql_dry_run(&args)
    }
}

#[derive(Debug, Clone)]
struct SqlDryRunArgs {
    dialect: String,
    query: String,
    database_url: Option<String>,
    path: Option<String>,
    explain: bool,
    run_sample: bool,
    max_rows: u64,
}

impl SqlDryRunArgs {
    fn from_value(value: &Value) -> Result<Self> {
        let query = required_string(value, "query")?;
        ensure_read_only_sql(&query)?;
        Ok(Self {
            dialect: optional_string(value, "dialect").unwrap_or_else(|| "sqlite".to_string()),
            query,
            database_url: optional_string(value, "database_url"),
            path: optional_string(value, "path"),
            explain: optional_bool(value, "explain").unwrap_or(true),
            run_sample: optional_bool(value, "run_sample").unwrap_or(false),
            max_rows: optional_u64(value, "max_rows").unwrap_or(20).min(200),
        })
    }
}

pub fn sql_dry_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dialect": { "type": "string", "enum": ["sqlite", "postgres", "mysql"] },
            "query": { "type": "string", "description": "Read-only SELECT/WITH/EXPLAIN query." },
            "path": { "type": "string", "description": "SQLite database file path." },
            "database_url": { "type": "string", "description": "Postgres/MySQL connection string or SQLite path." },
            "explain": { "type": "boolean", "description": "Run EXPLAIN first. Defaults to true." },
            "run_sample": { "type": "boolean", "description": "Also run a limited sample query. Defaults to false." },
            "max_rows": { "type": "integer" }
        },
        "required": ["query"]
    })
}

fn execute_sql_dry_run(args: &SqlDryRunArgs) -> Result<ToolResult> {
    let mut sections = Vec::new();
    let mut metadata = json!({
        "dialect": args.dialect,
        "read_only": true,
        "explain": args.explain,
        "run_sample": args.run_sample,
        "max_rows": args.max_rows,
    });
    if args.explain {
        let explain = match args.dialect.as_str() {
            "sqlite" => run_sqlite(
                args,
                &format!("EXPLAIN QUERY PLAN {}", trim_semicolon(&args.query)),
            )?,
            "postgres" => run_postgres(args, &format!("EXPLAIN {}", trim_semicolon(&args.query)))?,
            "mysql" => run_mysql(args, &format!("EXPLAIN {}", trim_semicolon(&args.query)))?,
            other => {
                return Err(PwError::ToolExecution(format!(
                    "unsupported SQL dialect '{other}'"
                )))
            }
        };
        metadata["explain_output"] = json!(explain);
        sections.push(format!(
            "EXPLAIN:\n{}",
            metadata["explain_output"].as_str().unwrap_or("")
        ));
    }
    if args.run_sample {
        let sample_query = limit_query(&args.query, args.max_rows);
        let sample = match args.dialect.as_str() {
            "sqlite" => run_sqlite(args, &sample_query)?,
            "postgres" => run_postgres(args, &sample_query)?,
            "mysql" => run_mysql(args, &sample_query)?,
            other => {
                return Err(PwError::ToolExecution(format!(
                    "unsupported SQL dialect '{other}'"
                )))
            }
        };
        metadata["sample_output"] = json!(sample);
        sections.push(format!(
            "SAMPLE:\n{}",
            metadata["sample_output"].as_str().unwrap_or("")
        ));
    }
    if sections.is_empty() {
        sections.push("SQL query passed read-only validation; no execution requested.".to_string());
    }
    let mut result = ToolResult::ok(sections.join("\n\n"));
    result.metadata = metadata;
    Ok(result)
}

fn run_sqlite(args: &SqlDryRunArgs, query: &str) -> Result<String> {
    let database = args
        .path
        .as_deref()
        .or(args.database_url.as_deref())
        .ok_or_else(|| PwError::ToolExecution("sqlite dry run requires path".to_string()))?;
    run_command(
        "sqlite3",
        &["-readonly", "-json", database, query],
        "sqlite3",
    )
}

fn run_postgres(args: &SqlDryRunArgs, query: &str) -> Result<String> {
    let database_url = args.database_url.as_deref().ok_or_else(|| {
        PwError::ToolExecution("postgres dry run requires database_url".to_string())
    })?;
    run_command(
        "psql",
        &[
            database_url,
            "-X",
            "-A",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            query,
        ],
        "psql",
    )
}

fn run_mysql(args: &SqlDryRunArgs, query: &str) -> Result<String> {
    let database_url = args
        .database_url
        .as_deref()
        .ok_or_else(|| PwError::ToolExecution("mysql dry run requires database_url".to_string()))?;
    run_command(
        "mysql",
        &["--batch", "--raw", database_url, "-e", query],
        "mysql",
    )
}

fn run_command(binary: &str, args: &[&str], label: &str) -> Result<String> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                PwError::ToolExecution(format!("{label} binary not found on PATH"))
            } else {
                PwError::ToolExecution(format!("{label} failed to start: {err}"))
            }
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(PwError::ToolExecution(format!(
            "{label} failed: {}\n{}",
            output.status, stderr
        )));
    }
    Ok(if stderr.trim().is_empty() {
        stdout
    } else {
        format!("{stdout}\n[stderr]\n{stderr}")
    })
}

fn ensure_read_only_sql(query: &str) -> Result<()> {
    let normalized = query
        .trim()
        .trim_start_matches('(')
        .trim()
        .to_ascii_lowercase();
    let allowed = ["select", "with", "explain", "pragma"]
        .iter()
        .any(|prefix| normalized.starts_with(prefix));
    if !allowed {
        return Err(PwError::ToolExecution(
            "SQL dry run only accepts SELECT/WITH/EXPLAIN/PRAGMA statements".to_string(),
        ));
    }
    let dangerous = [
        " insert ",
        " update ",
        " delete ",
        " drop ",
        " alter ",
        " create ",
        " truncate ",
        " grant ",
        " revoke ",
        " vacuum ",
        " attach ",
        " detach ",
        " replace ",
    ];
    let padded = format!(" {normalized} ");
    if dangerous.iter().any(|needle| padded.contains(needle)) {
        return Err(PwError::ToolExecution(
            "SQL dry run rejected mutating or administrative statement".to_string(),
        ));
    }
    Ok(())
}

fn limit_query(query: &str, max_rows: u64) -> String {
    let trimmed = trim_semicolon(query);
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains(" limit ") {
        trimmed.to_string()
    } else {
        format!("SELECT * FROM ({trimmed}) pwcli_limited_query LIMIT {max_rows}")
    }
}

fn trim_semicolon(query: &str) -> &str {
    query.trim().trim_end_matches(';').trim()
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| PwError::ToolExecution(format!("sql_dry_run requires '{key}'")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn optional_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}
