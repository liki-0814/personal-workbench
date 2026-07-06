use pwcli::{
    diagnostics::{build_doctor_report, build_status_report},
    runtime::{RuntimeTaskKind, RuntimeTaskManager},
    settings::{ModelDefinition, ProviderProtocol, ProviderSettings, Settings},
};

fn configured_settings(home: &std::path::Path) -> Settings {
    let mut settings = Settings::from_home(home);
    settings.provider = "local".to_string();
    settings.model = "local-model".to_string();
    settings.providers = vec![ProviderSettings {
        name: "local".to_string(),
        protocol: ProviderProtocol::OpenAi,
        base_url: "http://127.0.0.1:8046/v1".to_string(),
        api_key: Some("test-key".to_string()),
        api_key_env: None,
        api: Default::default(),
        request_timeout_seconds: 0,
        stream: true,
        extra_body: serde_json::json!({}),
        models: vec![ModelDefinition {
            name: "local-model".to_string(),
            supports_image_input: true,
            supports_thinking: true,
            is_image_generation: false,
            max_input_tokens: 128000,
            max_output_tokens: 8192,
            extra_body: serde_json::json!({}),
        }],
    }];
    settings
}

#[test]
fn status_report_summarizes_config_tools_memory_and_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let settings = configured_settings(temp.path());
    std::fs::create_dir_all(settings.pwcli_home.join("rules")).unwrap();
    std::fs::write(
        settings.pwcli_home.join("rules/safety.md"),
        "ask before risky tools",
    )
    .unwrap();
    let runtime = RuntimeTaskManager::new(settings.pwcli_home.clone());
    let task = runtime
        .create_task(
            RuntimeTaskKind::Internal,
            "diagnostic active task",
            temp.path(),
            serde_json::json!({"goal": "diagnostic active task"}),
        )
        .unwrap();
    runtime.set_active(&task.task_id).unwrap();

    let report = build_status_report(&settings);

    assert!(report.contains("pwcli status"));
    assert!(report.contains("provider: local (openai)"));
    assert!(report.contains("model: local-model"));
    assert!(report.contains("tools: total="));
    assert!(report.contains("memory: enabled="));
    assert!(report.contains("rules: count=1"));
    assert!(report.contains("tasks: total="));
    assert!(report.contains("active_task:"));
    assert!(report.contains("next: pwcli plan --wait"));
}

#[test]
fn doctor_report_warns_when_provider_is_not_configured() {
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings::from_home(temp.path());

    let report = build_doctor_report(&settings);

    assert!(report.contains("pwcli doctor"));
    assert!(report.contains("[warn] provider:"));
    assert!(report.contains("[warn] model:"));
    assert!(report.contains("summary:"));
}

#[test]
fn doctor_report_accepts_configured_provider_and_model() {
    let temp = tempfile::tempdir().unwrap();
    let settings = configured_settings(temp.path());

    let report = build_doctor_report(&settings);

    assert!(report.contains("[ok] provider: local (openai)"));
    assert!(report.contains("[ok] provider api key: configured in ~/.pwcli/config.json"));
    assert!(report.contains("[ok] model: local-model"));
    assert!(report.contains("[ok] tools:"));
}
