use pwcli::{
    runtime::{RuntimeTaskKind, RuntimeTaskManager, RuntimeTaskSpec},
    tools::{
        builtin::BuiltinToolLoader, verification::VerificationToolLoader, InvocationMode,
        LoadedTool, RiskLevel, ToolArtifact, ToolArtifactKind, ToolArtifactProvenance, ToolCall,
        ToolDescriptor, ToolExecutionContext, ToolExecutionMode, ToolExecutionRuntime,
        ToolExecutor, ToolLoader, ToolRegistry, ToolResult, ToolRuntimeEvent, ToolSource,
    },
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

struct EchoExecutor;

impl ToolExecutor for EchoExecutor {
    fn execute(&self, call: &ToolCall) -> pwcli::Result<ToolResult> {
        Ok(ToolResult::ok(call.arguments.to_string()))
    }
}

struct StreamingExecutor;

impl ToolExecutor for StreamingExecutor {
    fn execute(&self, _call: &ToolCall) -> pwcli::Result<ToolResult> {
        Ok(ToolResult::ok("streamed"))
    }

    fn execute_with_runtime(
        &self,
        _call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> pwcli::Result<ToolResult> {
        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Streaming,
        });
        runtime.emit(ToolRuntimeEvent::Progress {
            message: "halfway".to_string(),
        });
        runtime.emit(ToolRuntimeEvent::Output {
            stream: "stdout".to_string(),
            chunk: "hello\n".to_string(),
        });
        runtime.emit(ToolRuntimeEvent::Completed { is_error: false });
        Ok(ToolResult::ok("streamed"))
    }
}

struct BackgroundExecutor;

impl ToolExecutor for BackgroundExecutor {
    fn execute(&self, _call: &ToolCall) -> pwcli::Result<ToolResult> {
        Ok(ToolResult::error("runtime context required"))
    }

    fn execute_with_runtime(
        &self,
        _call: &ToolCall,
        runtime: &mut ToolExecutionRuntime<'_>,
    ) -> pwcli::Result<ToolResult> {
        runtime.emit(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Background,
        });
        let manager = runtime
            .runtime_tasks()
            .cloned()
            .expect("test supplies runtime manager");
        let handle = manager.spawn(RuntimeTaskSpec {
            task_id: None,
            kind: RuntimeTaskKind::Internal,
            title: "background fake tool".to_string(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo background".to_string(),
            ],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({ "source": "test" }),
        })?;
        runtime.emit(ToolRuntimeEvent::BackgroundTaskStarted {
            task_id: handle.task_id.clone(),
            task_dir: handle.task_dir.clone(),
        });
        runtime.emit(ToolRuntimeEvent::Completed { is_error: false });
        let mut result = ToolResult::ok(handle.task_id.clone());
        result.metadata = json!({ "task_id": handle.task_id });
        Ok(result)
    }
}

fn echo_tool(id: &str) -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: id.to_string(),
            name: "echo".to_string(),
            description: "returns input".to_string(),
            input_schema: json!({"type": "object"}),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec!["test.echo".to_string()],
            metadata: json!({}),
            enabled: true,
        },
        executor: Some(Arc::new(EchoExecutor)),
    }
}

fn loaded_tool(id: &str, executor: Arc<dyn ToolExecutor>) -> LoadedTool {
    LoadedTool {
        descriptor: ToolDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: "test tool".to_string(),
            input_schema: json!({"type": "object"}),
            source: ToolSource::Builtin,
            risk_level: RiskLevel::ReadOnly,
            invocation_mode: InvocationMode::Internal,
            capabilities: vec!["test".to_string()],
            metadata: json!({}),
            enabled: true,
        },
        executor: Some(executor),
    }
}

#[test]
fn snapshot_is_stable_after_registry_changes() {
    let mut registry = ToolRegistry::new();
    registry.register(echo_tool("builtin.echo"));
    let snapshot = registry.snapshot();
    let original_version = snapshot.version();

    registry.unregister("builtin.echo");

    let call = ToolCall {
        id: "call-1".to_string(),
        tool_id: "builtin.echo".to_string(),
        name: "echo".to_string(),
        arguments: json!({"hello": "world"}),
    };

    let result = snapshot
        .execute(&call)
        .expect("snapshot should retain executor");
    assert!(!result.is_error);
    assert_eq!(snapshot.version(), original_version);
    assert!(registry.snapshot().get("builtin.echo").is_none());
}

#[test]
fn builtin_loader_registers_mineru_parse_tool() {
    let tools = BuiltinToolLoader.load().unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.descriptor.id == "builtin.mineru_parse_document"));
}

#[test]
fn registry_default_runtime_wraps_sync_executor_events() {
    let mut registry = ToolRegistry::new();
    registry.register(echo_tool("builtin.echo"));
    let snapshot = registry.snapshot();
    let call = ToolCall {
        id: "call-sync".to_string(),
        tool_id: "builtin.echo".to_string(),
        name: "echo".to_string(),
        arguments: json!({"x": 1}),
    };
    let mut events = Vec::new();
    let mut runtime = ToolExecutionRuntime::new(ToolExecutionContext::default(), |event| {
        events.push(event);
    });
    let result = snapshot.execute_with_runtime(&call, &mut runtime).unwrap();
    drop(runtime);
    assert!(!result.is_error);
    assert!(matches!(
        events.first(),
        Some(ToolRuntimeEvent::Started {
            mode: ToolExecutionMode::Sync
        })
    ));
    assert!(matches!(
        events.last(),
        Some(ToolRuntimeEvent::Completed { is_error: false })
    ));
}

#[test]
fn registry_supports_streaming_tool_runtime_events() {
    let mut registry = ToolRegistry::new();
    registry.register(loaded_tool(
        "builtin.streaming",
        Arc::new(StreamingExecutor),
    ));
    let snapshot = registry.snapshot();
    let call = ToolCall {
        id: "call-stream".to_string(),
        tool_id: "builtin.streaming".to_string(),
        name: "streaming".to_string(),
        arguments: json!({}),
    };
    let mut events = Vec::new();
    let mut runtime = ToolExecutionRuntime::new(ToolExecutionContext::default(), |event| {
        events.push(event);
    });
    let result = snapshot.execute_with_runtime(&call, &mut runtime).unwrap();
    drop(runtime);
    assert_eq!(result.content, "streamed");
    assert!(events.iter().any(|event| matches!(
        event,
        ToolRuntimeEvent::Output { stream, chunk }
            if stream == "stdout" && chunk.contains("hello")
    )));
}

#[test]
fn registry_supports_background_tool_runtime_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    let mut registry = ToolRegistry::new();
    registry.register(loaded_tool(
        "builtin.background",
        Arc::new(BackgroundExecutor),
    ));
    let snapshot = registry.snapshot();
    let call = ToolCall {
        id: "call-bg".to_string(),
        tool_id: "builtin.background".to_string(),
        name: "background".to_string(),
        arguments: json!({}),
    };
    let mut events = Vec::new();
    let mut runtime = ToolExecutionRuntime::new(
        ToolExecutionContext {
            runtime_tasks: Some(manager.clone()),
            ..ToolExecutionContext::default()
        },
        |event| events.push(event),
    );
    let result = snapshot.execute_with_runtime(&call, &mut runtime).unwrap();
    drop(runtime);
    let task_id = result.metadata["task_id"].as_str().unwrap();
    assert!(manager.get(task_id).is_ok());
    assert!(events.iter().any(|event| matches!(
        event,
        ToolRuntimeEvent::BackgroundTaskStarted { task_id: id, .. } if id == task_id
    )));
}

#[test]
fn builtin_loader_registers_anysearch_tool() {
    let tools = BuiltinToolLoader.load().unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.descriptor.id == "builtin.anysearch"));
}

#[test]
fn builtin_loader_registers_external_capability_matrix_tools() {
    let tools = BuiltinToolLoader.load().unwrap();
    for id in [
        "builtin.local_file_index",
        "builtin.web_fetch",
        "builtin.browser_automation",
        "builtin.github",
        "builtin.sql_dry_run",
        "builtin.ssh_exec",
    ] {
        assert!(tools.iter().any(|tool| tool.descriptor.id == id), "{id}");
    }
}

#[test]
fn local_file_index_reads_local_file_preview() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("note.md");
    std::fs::write(&file, "pwcli local index test").unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_many(BuiltinToolLoader.load().unwrap());
    let snapshot = registry.snapshot();
    let result = snapshot
        .execute(&ToolCall {
            id: "local-read".to_string(),
            tool_id: "builtin.local_file_index".to_string(),
            name: "local_file_index".to_string(),
            arguments: json!({
                "action": "read",
                "path": file,
            }),
        })
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("pwcli local index test"));
}

#[test]
fn sql_dry_run_rejects_mutating_statement_before_execution() {
    let mut registry = ToolRegistry::new();
    registry.register_many(BuiltinToolLoader.load().unwrap());
    let snapshot = registry.snapshot();
    let err = snapshot
        .execute(&ToolCall {
            id: "sql-delete".to_string(),
            tool_id: "builtin.sql_dry_run".to_string(),
            name: "sql_dry_run".to_string(),
            arguments: json!({
                "dialect": "sqlite",
                "path": "/tmp/missing.db",
                "query": "delete from users"
            }),
        })
        .unwrap_err();
    assert!(err.to_string().contains("only accepts"));
}

#[test]
fn verification_loader_registers_project_check_tool() {
    let tools = VerificationToolLoader.load().unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.descriptor.id == "verification.project_check"));
}

#[test]
fn registry_executes_verification_tool() {
    let mut registry = ToolRegistry::new();
    registry.register_many(VerificationToolLoader.load().unwrap());
    let snapshot = registry.snapshot();
    let call = ToolCall {
        id: "verify-test".to_string(),
        tool_id: "verification.project_check".to_string(),
        name: "project_check".to_string(),
        arguments: json!({
            "commands": ["echo REGISTRY_VERIFY_OK"],
            "timeout_seconds": 5
        }),
    };

    let result = snapshot.execute(&call).unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("REGISTRY_VERIFY_OK"));
}

#[test]
fn tool_result_supports_standard_artifact_metadata() {
    let result = ToolResult::ok("report ready")
        .with_preview("report preview")
        .with_full_content_ref("file:///tmp/report.md")
        .add_artifact(ToolArtifact {
            path: PathBuf::from("/tmp/report.md"),
            kind: ToolArtifactKind::Report,
            title: Some("Report".to_string()),
            media_type: Some("text/markdown".to_string()),
            preview: Some("preview".to_string()),
            full_content_ref: Some("file:///tmp/report.md".to_string()),
            provenance: Some(ToolArtifactProvenance {
                source: "test".to_string(),
                uri: None,
                tool_call_id: Some("call-1".to_string()),
                metadata: json!({}),
            }),
        });
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["preview"], "report preview");
    assert_eq!(value["artifacts"][0]["kind"], "report");
    assert_eq!(value["artifacts"][0]["provenance"]["source"], "test");
}
