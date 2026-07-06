use crate::{
    settings::{GitHubSettings, Settings},
    tools::{ToolCall, ToolResult},
    PwError, Result,
};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct GitHubExecutor;

impl GitHubExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitHubExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::tools::ToolExecutor for GitHubExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let settings = Settings::load()?.github;
        let args = GitHubArgs::from_value(&call.arguments)?;
        execute_github(&settings, &args)
    }
}

#[derive(Debug, Clone)]
struct GitHubArgs {
    action: String,
    owner: String,
    repo: String,
    number: Option<u64>,
    path: Option<String>,
    branch: Option<String>,
    state: String,
    max_results: u64,
}

impl GitHubArgs {
    fn from_value(value: &Value) -> Result<Self> {
        let repo_value =
            optional_string(value, "repo").or_else(|| optional_string(value, "repository"));
        let (owner, repo) = if let Some(repo_value) = repo_value {
            parse_repo_slug(&repo_value)?
        } else {
            (
                required_string(value, "owner")?,
                required_string(value, "name").or_else(|_| required_string(value, "repo_name"))?,
            )
        };
        Ok(Self {
            action: optional_string(value, "action").unwrap_or_else(|| "repo".to_string()),
            owner,
            repo,
            number: optional_u64(value, "number"),
            path: optional_string(value, "path"),
            branch: optional_string(value, "branch").or_else(|| optional_string(value, "ref")),
            state: optional_string(value, "state").unwrap_or_else(|| "open".to_string()),
            max_results: optional_u64(value, "max_results").unwrap_or(20).min(100),
        })
    }
}

pub fn github_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["repo", "issues", "pulls", "pr", "workflow_runs", "file"],
                "description": "GitHub read-only operation."
            },
            "repo": { "type": "string", "description": "Repository slug owner/name." },
            "owner": { "type": "string" },
            "name": { "type": "string", "description": "Repository name when owner is provided separately." },
            "number": { "type": "integer", "description": "PR/issue number for action=pr." },
            "path": { "type": "string", "description": "File path for action=file." },
            "branch": { "type": "string", "description": "Branch/ref for action=file." },
            "state": { "type": "string", "enum": ["open", "closed", "all"] },
            "max_results": { "type": "integer" }
        },
        "required": ["repo"]
    })
}

fn execute_github(settings: &GitHubSettings, args: &GitHubArgs) -> Result<ToolResult> {
    let client = Client::builder()
        .timeout(Duration::from_secs(settings.request_timeout_seconds.max(1)))
        .user_agent("pwcli/0.1 github")
        .build()
        .map_err(|err| PwError::ToolExecution(format!("failed to build GitHub client: {err}")))?;
    let path = match args.action.as_str() {
        "repo" => format!("/repos/{}/{}", args.owner, args.repo),
        "issues" => format!(
            "/repos/{}/{}/issues?state={}&per_page={}",
            args.owner, args.repo, args.state, args.max_results
        ),
        "pulls" => format!(
            "/repos/{}/{}/pulls?state={}&per_page={}",
            args.owner, args.repo, args.state, args.max_results
        ),
        "pr" => {
            let number = args.number.ok_or_else(|| {
                PwError::ToolExecution("github action=pr requires number".to_string())
            })?;
            format!("/repos/{}/{}/pulls/{}", args.owner, args.repo, number)
        }
        "workflow_runs" => format!(
            "/repos/{}/{}/actions/runs?per_page={}",
            args.owner, args.repo, args.max_results
        ),
        "file" => {
            let path = args.path.as_deref().ok_or_else(|| {
                PwError::ToolExecution("github action=file requires path".to_string())
            })?;
            let mut url = format!("/repos/{}/{}/contents/{}", args.owner, args.repo, path);
            if let Some(branch) = &args.branch {
                url.push_str(&format!("?ref={branch}"));
            }
            url
        }
        other => {
            return Err(PwError::ToolExecution(format!(
                "unknown github action '{other}'"
            )))
        }
    };
    let url = format!("{}{}", settings.api_url.trim_end_matches('/'), path);
    let mut request = client
        .get(&url)
        .header("accept", "application/vnd.github+json");
    if let Some(token) = settings
        .token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|err| PwError::ToolExecution(format!("GitHub request failed: {err}")))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(PwError::ToolExecution(format!(
            "GitHub HTTP {status}: {}",
            truncate(&body, 1200)
        )));
    }
    let value = serde_json::from_str::<Value>(&body)
        .map_err(|err| PwError::ToolExecution(format!("invalid GitHub response: {err}")))?;
    let content = summarize_github_response(&args.action, &value);
    let mut result = ToolResult::ok(content).with_preview(truncate(&body, 3000));
    result.metadata = json!({
        "action": args.action,
        "repo": format!("{}/{}", args.owner, args.repo),
        "url": url,
        "response": value,
    });
    Ok(result)
}

fn summarize_github_response(action: &str, value: &Value) -> String {
    match action {
        "repo" => format!(
            "{}\n{}\nstars={} forks={} default_branch={}",
            value["full_name"].as_str().unwrap_or("repo"),
            value["description"].as_str().unwrap_or(""),
            value["stargazers_count"],
            value["forks_count"],
            value["default_branch"].as_str().unwrap_or("")
        ),
        "issues" | "pulls" => value
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|item| {
                format!(
                    "#{}\t{}\t{}",
                    item["number"],
                    item["state"],
                    item["title"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "pr" => format!(
            "PR #{} {}\nstate={} mergeable={:?}\n{}",
            value["number"],
            value["title"].as_str().unwrap_or(""),
            value["state"].as_str().unwrap_or(""),
            value["mergeable"],
            value["html_url"].as_str().unwrap_or("")
        ),
        "workflow_runs" => value["workflow_runs"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|item| {
                format!(
                    "{}\t{}\t{}\t{}",
                    item["id"],
                    item["name"].as_str().unwrap_or(""),
                    item["status"].as_str().unwrap_or(""),
                    item["conclusion"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "file" => format!(
            "{} size={} encoding={}",
            value["path"].as_str().unwrap_or("file"),
            value["size"],
            value["encoding"].as_str().unwrap_or("")
        ),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn parse_repo_slug(value: &str) -> Result<(String, String)> {
    let clean = value.trim().trim_end_matches('/');
    let slug = clean
        .strip_prefix("https://github.com/")
        .or_else(|| clean.strip_prefix("http://github.com/"))
        .unwrap_or(clean);
    let mut parts = slug.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() {
        return Err(PwError::ToolExecution(format!(
            "invalid GitHub repo '{value}', expected owner/name"
        )));
    }
    Ok((owner.to_string(), repo.trim_end_matches(".git").to_string()))
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| PwError::ToolExecution(format!("github requires '{key}'")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
