use pwcli::audit::AuditEvent;

#[test]
fn flowchart_nodes_have_auditable_events() {
    let events = vec![
        AuditEvent::RuntimeInitialized,
        AuditEvent::ConfigLoaded {
            provider: "Nvidia".to_string(),
            model: "minimaxai/minimax-m3".to_string(),
        },
        AuditEvent::ToolDiscoveryStarted,
        AuditEvent::SkillsScanned {
            roots: vec!["~/.agents/skills".to_string()],
            loaded: 0,
        },
        AuditEvent::ToolRegistryBuilt {
            registry_version: 0,
            tool_count: 0,
        },
        AuditEvent::ContextPackBuilt {
            context_id: "ctx".to_string(),
            selected_tool_ids: vec![],
        },
        AuditEvent::RegistrySnapshotCreated {
            registry_version: 0,
            tool_count: 0,
        },
        AuditEvent::ModelNodeStarted {
            provider: "Nvidia".to_string(),
            model: "minimaxai/minimax-m3".to_string(),
        },
        AuditEvent::GraphRunStarted {
            registry_version: 0,
            user_input: "hello".to_string(),
        },
        AuditEvent::ToolsSelected { tool_ids: vec![] },
        AuditEvent::GraphRunCompleted,
        AuditEvent::ModelNodeCompleted { output_chars: 2 },
        AuditEvent::TokenUsageRecorded {
            input_tokens: 1,
            output_tokens: 2,
        },
        AuditEvent::ModelNodeFailed {
            error: "timeout".to_string(),
        },
        AuditEvent::GraphRunFailed {
            error: "timeout".to_string(),
        },
        AuditEvent::FinalOutputProduced,
        AuditEvent::SessionSaved {
            path: "/tmp/session.json".to_string(),
        },
    ];

    for event in events {
        serde_json::to_string(&event).expect("audit event should serialize");
    }
}
