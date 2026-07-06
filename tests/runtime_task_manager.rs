use pwcli::runtime::{
    CompactScope, RuntimeTaskEvent, RuntimeTaskKind, RuntimeTaskManager, RuntimeTaskSpec,
    RuntimeTaskStatus, VerificationRecord,
};
use serde_json::json;
use std::{thread, time::Duration};

#[test]
fn runtime_task_completes_and_persists_logs_and_events() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_complete".to_string()),
            kind: RuntimeTaskKind::Shell,
            title: "echo task".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hello-runtime".to_string(),
            ],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({ "source": "test" }),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);

    let task = manager.get(&handle.task_id).unwrap();
    assert_eq!(task.status, RuntimeTaskStatus::Completed);
    assert!(manager
        .task_dir(&handle.task_id)
        .join("events.jsonl")
        .is_file());
    assert!(
        std::fs::read_to_string(manager.task_dir(&handle.task_id).join("stdout.log"))
            .unwrap()
            .contains("hello-runtime")
    );
    assert!(
        std::fs::read_to_string(manager.task_dir(&handle.task_id).join("events.jsonl"))
            .unwrap()
            .contains("Completed")
    );
    let audit = std::fs::read_to_string(temp.path().join(".pwcli/audit/events.jsonl")).unwrap();
    assert!(audit.contains("RuntimeTaskStarted"));
    assert!(audit.contains("RuntimeTaskCompleted"));
}

#[test]
fn runtime_cancel_does_not_overwrite_terminal_status() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_terminal_cancel".to_string()),
            kind: RuntimeTaskKind::Shell,
            title: "complete then cancel".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec!["sh".to_string(), "-c".to_string(), "true".to_string()],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({}),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);
    assert_eq!(
        manager.get(&handle.task_id).unwrap().status,
        RuntimeTaskStatus::Completed
    );

    let err = manager.cancel(&handle.task_id).unwrap_err().to_string();
    assert!(err.contains("already Completed"));
    assert_eq!(
        manager.get(&handle.task_id).unwrap().status,
        RuntimeTaskStatus::Completed
    );
}

#[test]
fn runtime_task_compact_writes_summary() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();
    let task = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "compact me",
            temp.path(),
            json!({ "goal": "test compact" }),
        )
        .unwrap();
    std::fs::write(
        manager.task_dir(&task.task_id).join("stdout.log"),
        "some long output",
    )
    .unwrap();

    let summary = manager.compact(&task.task_id, CompactScope::Both).unwrap();
    assert!(summary.summary_path.is_file());
    assert!(summary.content.contains("test compact"));
}

#[test]
fn runtime_task_verification_record_is_persisted_and_attached() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();
    let task = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "verify me",
            temp.path(),
            json!({ "goal": "verify task" }),
        )
        .unwrap();

    let path = manager
        .record_verification(
            &task.task_id,
            VerificationRecord {
                passed: true,
                content: "verification ok".to_string(),
                metadata: json!({ "passed": true, "commands": ["echo ok"] }),
                report: None,
            },
        )
        .unwrap();

    assert!(path.is_file());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Verification Report"));
    assert!(manager
        .task_dir(&task.task_id)
        .join("verification_report.md")
        .is_file());
    assert!(manager
        .task_dir(&task.task_id)
        .join("verification_report.json")
        .is_file());
    assert!(manager
        .task_dir(&task.task_id)
        .join("verification.json")
        .is_file());
    assert!(
        std::fs::read_to_string(manager.task_dir(&task.task_id).join("events.jsonl"))
            .unwrap()
            .contains("VerificationRecorded")
    );
    assert!(
        std::fs::read_to_string(temp.path().join(".pwcli/audit/events.jsonl"))
            .unwrap()
            .contains("RuntimeTaskVerificationRecorded")
    );
    assert!(manager.poll_events().iter().any(|event| matches!(
        event,
        RuntimeTaskEvent::VerificationRecorded {
            task_id,
            passed: true,
            ..
        } if task_id == &task.task_id
    )));
    let task = manager.get(&task.task_id).unwrap();
    assert_eq!(task.metadata["verification"]["passed"], true);
    assert_eq!(task.metadata["verification"]["gate"], "pass");
    assert_eq!(task.metadata["verification"]["status"], "passed");
    assert!(task.metadata["verification"]["path"]
        .as_str()
        .unwrap()
        .ends_with("verification.md"));
}

#[test]
fn runtime_tracks_active_task_and_resolves_selectors() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let first = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "first task",
            temp.path(),
            json!({ "goal": "first" }),
        )
        .unwrap();
    let second = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "second task",
            temp.path(),
            json!({ "goal": "second" }),
        )
        .unwrap();

    manager.set_active(&first.task_id).unwrap();
    assert_eq!(
        manager.active_task_id().unwrap(),
        Some(first.task_id.clone())
    );
    assert_eq!(
        manager.resolve_task_id(None).unwrap(),
        Some(first.task_id.clone())
    );

    manager.set_active(&second.task_id[..8]).unwrap();
    assert_eq!(
        manager.resolve_task_id(Some("active")).unwrap(),
        Some(second.task_id.clone())
    );
    assert_eq!(
        manager.resolve_task_id(Some(&second.task_id[..8])).unwrap(),
        Some(second.task_id.clone())
    );
    assert!(manager.resolve_task_id(Some("missing")).unwrap().is_none());
}

#[test]
fn runtime_deletes_task_and_clears_active() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let first = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "first task",
            temp.path(),
            json!({ "goal": "delete-me" }),
        )
        .unwrap();
    let second = manager
        .create_task(
            RuntimeTaskKind::Internal,
            "second task",
            temp.path(),
            json!({ "goal": "keep-me" }),
        )
        .unwrap();

    manager.set_active(&first.task_id).unwrap();
    assert_eq!(
        manager.active_task_id().unwrap(),
        Some(first.task_id.clone())
    );

    manager.delete(&first.task_id).unwrap();
    assert!(manager
        .list()
        .unwrap()
        .iter()
        .all(|task| task.task_id != first.task_id));
    assert!(manager.active_task_id().unwrap().is_none());
    assert!(!manager.task_dir(&first.task_id).exists());
    let remaining = manager.list().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].task_id, second.task_id);
}

#[test]
fn runtime_reuses_task_id_and_merges_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let task = manager
        .create_task(
            RuntimeTaskKind::AgentCli,
            "same session",
            temp.path(),
            json!({ "session": { "task_id_scoped": true } }),
        )
        .unwrap();
    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some(task.task_id.clone()),
            kind: RuntimeTaskKind::AgentCli,
            title: "same session step".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec!["sh".to_string(), "-c".to_string(), "echo step".to_string()],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({ "agent_cli": "codex" }),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);

    let task = manager.get(&task.task_id).unwrap();
    assert_eq!(task.task_id, handle.task_id);
    assert_eq!(task.title, "same session");
    assert_eq!(task.metadata["agent_cli"], "codex");
    assert_eq!(task.metadata["session"]["task_id_scoped"], true);
    assert_eq!(task.metadata["current_step"]["title"], "same session step");
}

#[test]
fn runtime_auto_compacts_and_records_review_recommendation() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_review".to_string()),
            kind: RuntimeTaskKind::AgentCli,
            title: "review task".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo TODO-review-required".to_string(),
            ],
            timeout_seconds: 5,
            auto_compact_threshold_chars: Some(1),
            metadata: json!({ "agent_cli": "fake", "yolo": false }),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);
    wait_until_summary(&manager, &handle.task_id);
    let task = manager.get(&handle.task_id).unwrap();
    assert_eq!(task.metadata["review_recommendation"]["required"], true);
    assert!(manager
        .task_dir(&handle.task_id)
        .join("summary.md")
        .is_file());
}

#[test]
fn runtime_streams_output_before_task_completes() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_stream".to_string()),
            kind: RuntimeTaskKind::Shell,
            title: "stream task".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo first-line; sleep 1; echo second-line".to_string(),
            ],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({}),
        })
        .unwrap();

    for _ in 0..20 {
        let events = manager.poll_events();
        if events.iter().any(|event| {
            matches!(
                event,
                RuntimeTaskEvent::Output { chunk, .. } if chunk.contains("first-line")
            )
        }) {
            let task = manager.get(&handle.task_id).unwrap();
            assert!(matches!(
                task.status,
                RuntimeTaskStatus::Pending | RuntimeTaskStatus::Running
            ));
            wait_until_terminal(&manager, &handle.task_id);
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }

    panic!("first streamed output was not observed before completion");
}

#[test]
fn runtime_emits_structured_json_output_events() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_structured".to_string()),
            kind: RuntimeTaskKind::Shell,
            title: "structured task".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s\\n' '{\"type\":\"delta\",\"text\":\"hello\"}'".to_string(),
            ],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({}),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);
    let events = manager.poll_events();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            RuntimeTaskEvent::Structured { event, .. }
                if event["type"] == "delta" && event["text"] == "hello"
        )
    }));
}

#[test]
fn runtime_reads_persisted_events_incrementally() {
    let temp = tempfile::tempdir().unwrap();
    let manager = RuntimeTaskManager::new(temp.path().join(".pwcli"));
    manager.ensure().unwrap();

    let handle = manager
        .spawn(RuntimeTaskSpec {
            task_id: Some("task_test_events_from".to_string()),
            kind: RuntimeTaskKind::Shell,
            title: "event read task".to_string(),
            cwd: temp.path().to_path_buf(),
            command: vec!["sh".to_string(), "-c".to_string(), "echo hello".to_string()],
            timeout_seconds: 5,
            auto_compact_threshold_chars: None,
            metadata: json!({}),
        })
        .unwrap();

    wait_until_terminal(&manager, &handle.task_id);
    let (events, offset) = manager.read_events_from(&handle.task_id, 0).unwrap();
    assert!(offset > 0);
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeTaskEvent::Started { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeTaskEvent::Completed { .. })));

    let (more_events, same_offset) = manager.read_events_from(&handle.task_id, offset).unwrap();
    assert!(more_events.is_empty());
    assert_eq!(same_offset, offset);
}

#[test]
fn runtime_task_spec_serializes_for_detached_worker() {
    let spec = RuntimeTaskSpec {
        task_id: Some("task_spec".to_string()),
        kind: RuntimeTaskKind::AgentCli,
        title: "detached spec".to_string(),
        cwd: std::path::PathBuf::from("/tmp"),
        command: vec!["echo".to_string(), "ok".to_string()],
        timeout_seconds: 5,
        auto_compact_threshold_chars: Some(1024),
        metadata: json!({"agent_cli": "codex", "yolo": true}),
    };

    let encoded = serde_json::to_string(&spec).unwrap();
    let decoded: RuntimeTaskSpec = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.task_id.as_deref(), Some("task_spec"));
    assert_eq!(decoded.command, vec!["echo", "ok"]);
    assert_eq!(decoded.metadata["agent_cli"], "codex");
}

fn wait_until_terminal(manager: &RuntimeTaskManager, task_id: &str) {
    for _ in 0..50 {
        let status = manager.get(task_id).unwrap().status;
        if matches!(
            status,
            RuntimeTaskStatus::Completed
                | RuntimeTaskStatus::Failed
                | RuntimeTaskStatus::Cancelled
                | RuntimeTaskStatus::TimedOut
        ) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("task did not finish");
}

fn wait_until_summary(manager: &RuntimeTaskManager, task_id: &str) {
    for _ in 0..50 {
        if manager.task_dir(task_id).join("summary.md").is_file() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("summary was not written");
}
