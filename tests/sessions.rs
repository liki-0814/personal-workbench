use pwcli::{
    graph::{GraphMessage, GraphRunSummary, GraphState, GraphStatus},
    session::{format_session_list, format_session_record, SessionStore},
};

fn summary(user: &str, assistant: &str) -> GraphRunSummary {
    let state = GraphState {
        round_count: 2,
        messages: vec![
            GraphMessage::User(user.to_string()),
            GraphMessage::Assistant(assistant.to_string()),
        ],
        last_content: assistant.to_string(),
        status: GraphStatus::Completed,
        ..GraphState::default()
    };
    GraphRunSummary {
        registry_version: 7,
        state,
    }
}

#[test]
fn session_store_saves_lists_reads_and_deletes_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(temp.path().join(".pwcli"));
    store
        .save("100", &summary("first question", "first answer"))
        .unwrap();
    store
        .save("200", &summary("second question", "second answer"))
        .unwrap();

    let sessions = store.list().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|session| session.id == "100"));
    assert!(sessions.iter().any(|session| session.id == "200"));

    let record = store.get("100").unwrap().unwrap();
    assert_eq!(record.entry.user_preview, "first question");
    assert_eq!(record.entry.assistant_preview, "first answer");

    let list_text = format_session_list(&sessions);
    assert!(list_text.contains("first question") || list_text.contains("second question"));

    let record_text = format_session_record(&record);
    assert!(record_text.contains("session 100"));
    assert!(record_text.contains("user:"));
    assert!(record_text.contains("assistant:"));

    let deleted = store.delete("100").unwrap().unwrap();
    assert_eq!(deleted.id, "100");
    assert!(store.get("100").unwrap().is_none());
}

#[test]
fn session_store_supports_last_selector() {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(temp.path().join(".pwcli"));
    store
        .save("100", &summary("older", "older answer"))
        .unwrap();
    store
        .save("200", &summary("newer", "newer answer"))
        .unwrap();

    let last = store.get("last").unwrap().unwrap();
    assert!(last.entry.id == "100" || last.entry.id == "200");
}

#[test]
fn session_store_persists_folders_and_assignments() {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionStore::new(temp.path().join(".pwcli"));
    store
        .save("100", &summary("folder question", "folder answer"))
        .unwrap();

    let initial = store.folder_state().unwrap();
    assert!(initial.folders.iter().any(|folder| folder.id == "product"));

    let created = store.create_folder("Design Notes").unwrap();
    let folder = created
        .folders
        .iter()
        .find(|folder| folder.name == "Design Notes")
        .unwrap();
    assert_eq!(folder.id, "design-notes");

    let assigned = store.assign_folder("100", "design-notes").unwrap();
    assert_eq!(
        assigned.assignments.get("100").map(String::as_str),
        Some("design-notes")
    );

    let reloaded = SessionStore::new(temp.path().join(".pwcli"))
        .folder_state()
        .unwrap();
    assert_eq!(
        reloaded.assignments.get("100").map(String::as_str),
        Some("design-notes")
    );
}
