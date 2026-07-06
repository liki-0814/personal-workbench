use pwcli::{
    context::ContextBuilder,
    memory::{
        FactVector, MemoryCandidateAction, MemoryCandidateSignal, MemoryDownloadPolicy,
        MemoryEmbeddingSettings, MemoryLifecycleEventKind, MemoryStore, SemanticFactDraft,
        SemanticHypothesisDraft, SemanticInferenceDraft, SemanticLogicChainDraft,
        SemanticMemoryExtraction,
    },
    tools::ToolRegistry,
};
use std::fs;

fn sparse_store(temp: &tempfile::TempDir) -> MemoryStore {
    MemoryStore::new(
        temp.path().join(".pwcli"),
        MemoryEmbeddingSettings {
            enabled: false,
            model: "BAAI/bge-small-zh-v1.5".to_string(),
            download: MemoryDownloadPolicy::Never,
            mirror: None,
        },
    )
}

#[test]
fn memory_fact_write_and_sparse_recall() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let fact = store
        .add_fact(
            "用户认可 pwcli memory 使用多层近邻事实图进行召回。",
            "2026-06-29 与用户讨论 memory 图结构时明确确认。",
        )
        .unwrap();

    assert!(fact.id.starts_with("fact_"));
    assert!(temp.path().join(".pwcli/memory/facts").is_dir());
    assert!(temp
        .path()
        .join(".pwcli/memory/index/signatures.jsonl")
        .is_file());

    let recalled = store.search("memory 多层 事实图", 5).unwrap();
    assert_eq!(recalled[0].fact.id, fact.id);
    assert!(recalled[0].score > 0.0);
}

#[test]
fn memory_lists_fact_inference_and_hypothesis_layers() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let fact = store
        .add_fact("用户希望 memory 页面区分事实、推论和猜想。", "测试事实。")
        .unwrap();
    let logic = store
        .add_logic_chain(
            vec![fact.id.clone()],
            "事实层已经存在，因此可以生成推论层。",
        )
        .unwrap();
    let inference = store
        .add_inference("memory 页面需要一个推论层。", logic.id.clone())
        .unwrap();
    let hypothesis = store
        .add_hypothesis("用户会更容易信任可分层的 memory 图。", vec![fact.id], 0.72)
        .unwrap();

    assert_eq!(store.list_facts().unwrap().len(), 1);
    assert_eq!(store.list_inferences().unwrap()[0].id, inference.id);
    assert_eq!(store.list_hypotheses().unwrap()[0].id, hypothesis.id);
}

#[test]
fn memory_candidate_requires_accept_before_fact_write() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let candidate = store
        .generate_candidate_from_text(
            "用户明确要求 memory 自动考虑写入，但写入前必须询问用户确认。",
            "测试对话中用户明确提出。",
        )
        .unwrap()
        .unwrap();
    let candidate_id = candidate.id.clone();
    store.add_candidate(&candidate).unwrap();

    assert!(store.list_facts().unwrap().is_empty());
    assert_eq!(store.list_candidates().unwrap().len(), 1);
    assert_eq!(
        store.get_candidate(&candidate_id).unwrap().unwrap().id,
        candidate_id
    );

    let facts = store.accept_candidate(&candidate_id).unwrap();
    assert!(!facts.is_empty());
    assert!(store.list_candidates().unwrap().is_empty());
    assert!(!store.list_facts().unwrap().is_empty());
}

#[test]
fn memory_candidate_write_deduplicates_pending_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let first = store
        .generate_candidate_from_text(
            "用户明确要求 memory 自动考虑写入，但写入前必须询问用户确认。",
            "测试对话中用户明确提出。",
        )
        .unwrap()
        .unwrap();
    let second = store
        .generate_candidate_from_text(
            "用户明确要求 memory 自动考虑写入，但写入前必须询问用户确认。",
            "测试对话中用户明确提出。",
        )
        .unwrap()
        .unwrap();

    assert!(!first.facts[0].statement.contains("该事实来自"));
    assert_eq!(first.facts[0].source, "测试对话中用户明确提出。");

    store.add_candidate(&first).unwrap();
    store.add_candidate(&second).unwrap();

    assert_eq!(store.list_candidates().unwrap().len(), 1);
}

#[test]
fn memory_candidate_write_deduplicates_existing_fact() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    store
        .add_fact(
            "用户决定 pwcli 配置不暴露 home_dir 等派生字段。",
            "历史测试事实。",
        )
        .unwrap();
    let candidate = store
        .generate_candidate_from_text(
            "用户决定 pwcli 配置不暴露 home_dir 等派生字段。",
            "后续测试对话重复提到。",
        )
        .unwrap();

    assert!(candidate.is_none());
    assert!(store.list_candidates().unwrap().is_empty());
}

#[test]
fn memory_candidate_reject_hides_inbox_item() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let candidate = store
        .generate_candidate_from_text(
            "用户决定 pwcli 配置不暴露 home_dir 等派生字段。",
            "测试对话中用户明确提出。",
        )
        .unwrap()
        .unwrap();
    let candidate_id = candidate.id.clone();
    store.add_candidate(&candidate).unwrap();
    store.reject_candidate(&candidate_id).unwrap();

    assert!(store.list_candidates().unwrap().is_empty());
    assert!(store.list_facts().unwrap().is_empty());
}

#[test]
fn memory_candidate_reject_suppresses_same_candidate_regeneration() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let first = store
        .generate_candidate_from_text(
            "用户决定 pwcli 配置不暴露 home_dir 等派生字段。",
            "测试对话中用户明确提出。",
        )
        .unwrap()
        .unwrap();
    let first_id = first.id.clone();
    store.add_candidate(&first).unwrap();
    store.reject_candidate(&first_id).unwrap();

    let regenerated = store
        .generate_candidate_from_text(
            "用户决定 pwcli 配置不暴露 home_dir 等派生字段。",
            "后续测试对话再次提到。",
        )
        .unwrap()
        .unwrap();
    assert_eq!(regenerated.id, first_id);
    store.add_candidate(&regenerated).unwrap();

    assert!(store.list_candidates().unwrap().is_empty());
    assert!(store.list_facts().unwrap().is_empty());
}

#[test]
fn memory_candidate_review_marks_corrections_and_writes_evolution_events() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let original = store
        .add_fact("用户要求 memory 每天总结事实。", "早期测试事实。")
        .unwrap();

    let candidate = store
        .generate_candidate_from_semantic_extraction(
            SemanticMemoryExtraction {
                facts: vec![SemanticFactDraft {
                    ref_id: Some("f1".to_string()),
                    statement:
                        "用户更正：memory 不是每天总结事实，而是每5轮会话判断是否需要写入事实。"
                            .to_string(),
                    source_note: Some("测试更正事实。".to_string()),
                }],
                logic_chains: Vec::new(),
                inferences: Vec::new(),
                hypotheses: Vec::new(),
                reason: Some("测试候选审查。".to_string()),
            },
            "测试来源",
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        candidate.review.action,
        MemoryCandidateAction::NeedsClarification
    );
    assert!(candidate
        .review
        .signals
        .contains(&MemoryCandidateSignal::CorrectionOrContradiction));
    assert!(candidate.review.related_fact_count > 0);

    let candidate_id = candidate.id.clone();
    store.add_candidate(&candidate).unwrap();
    let inbox = store.list_candidates().unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].review.action,
        MemoryCandidateAction::NeedsClarification
    );

    let facts = store.accept_candidate(&candidate_id).unwrap();
    assert_eq!(facts.len(), 1);
    let events = store.lifecycle_events().unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == MemoryLifecycleEventKind::CandidateCreated));
    assert!(events
        .iter()
        .any(|event| event.kind == MemoryLifecycleEventKind::CandidateAccepted));
    assert!(events
        .iter()
        .any(|event| event.kind == MemoryLifecycleEventKind::EdgeAdded));
    assert!(events
        .iter()
        .any(|event| event.kind == MemoryLifecycleEventKind::StatusChanged));
    let facts = store.list_facts().unwrap();
    let original = facts
        .iter()
        .find(|fact| fact.id == original.id)
        .expect("original fact should remain readable");
    assert_ne!(format!("{:?}", original.status), "Active");

    let graph_events = read_memory_graph_events_text(temp.path());
    assert!(graph_events.contains("updates") || graph_events.contains("contradicts"));
}

#[test]
fn memory_derives_candidates_from_fact_graph() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    store
        .add_fact(
            "pwcli workflow 可以组合 code agent、verification 和 approval 节点。",
            "测试事实一。",
        )
        .unwrap();
    store
        .add_fact(
            "pwcli runtime task manager 可以记录后台任务状态和审计事件。",
            "测试事实二。",
        )
        .unwrap();

    let candidate = store
        .derive_candidate_from_graph(Some("workflow runtime"))
        .unwrap()
        .unwrap();

    assert!(candidate.facts.is_empty());
    assert!(!candidate.logic_chains.is_empty());
    assert!(!candidate.inferences.is_empty());
    assert!(candidate.review.score > 0.0);
    store.add_candidate(&candidate).unwrap();
    assert_eq!(store.list_candidates().unwrap().len(), 1);
}

#[test]
fn context_builder_recalls_memory_store_facts() {
    let temp = tempfile::tempdir().unwrap();
    let pwcli_home = temp.path().join(".pwcli");
    let store = MemoryStore::new(
        &pwcli_home,
        MemoryEmbeddingSettings {
            enabled: false,
            model: "BAAI/bge-small-zh-v1.5".to_string(),
            download: MemoryDownloadPolicy::Never,
            mirror: None,
        },
    );
    store
        .add_fact(
            "用户要求配置只暴露 provider、protocol、base_url、api_key 和 models。",
            "2026-06-29 与用户讨论配置结构时明确提出。",
        )
        .unwrap();
    fs::create_dir_all(pwcli_home.join("rules")).unwrap();
    fs::write(
        pwcli_home.join("rules/safety.md"),
        "rule: ask before delete",
    )
    .unwrap();

    let registry = ToolRegistry::new();
    let pack = ContextBuilder::new().build_with_sources(
        "配置 base_url 怎么设计",
        &registry.snapshot(),
        Some(pwcli_home),
        Vec::new(),
    );

    assert!(pack
        .memory_items
        .iter()
        .any(|item| item.contains("provider、protocol")));
    assert!(pack.summary.contains("Relevant memory"));
    assert!(pack.summary.contains("Rules"));
}

#[test]
fn memory_hnsw_graph_index_is_incremental_and_rebuildable() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    store
        .add_fact(
            "pwcli memory 使用本地 semantic hash 向量插入多层近邻图。",
            "测试写入第一条 memory graph 事实。",
        )
        .unwrap();
    store
        .add_fact(
            "pwcli memory 的 HNSW graph 会为相关事实建立近邻边。",
            "测试写入第二条 memory graph 事实。",
        )
        .unwrap();
    store
        .add_fact(
            "AnySearch 工具用于外部网页和学术搜索。",
            "测试写入不相关事实。",
        )
        .unwrap();

    let stats = store.graph_stats().unwrap();
    assert_eq!(stats.facts, 3);
    assert_eq!(stats.hnsw_nodes, 3);
    assert!(stats.entry_point.is_some());
    assert!(temp
        .path()
        .join(".pwcli/memory/index/hnsw_nodes.jsonl")
        .is_file());

    let recalled = store.search("memory HNSW 近邻图", 2).unwrap();
    assert!(!recalled.is_empty());
    assert!(recalled
        .iter()
        .any(|scored| scored.fact.statement.contains("memory")));

    let rebuilt = store.rebuild_graph_index().unwrap();
    assert_eq!(rebuilt.facts, 3);
    assert_eq!(rebuilt.hnsw_nodes, 3);
}

#[test]
fn memory_hnsw_uses_hybrid_embedding_vectors_when_available() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let first = store
        .add_fact(
            "pwcli memory 使用 bge embedding 辅助事实召回。",
            "测试写入第一条 hybrid embedding 事实。",
        )
        .unwrap();
    let second = store
        .add_fact(
            "事实图插入应该在有 dense vector 时使用 hybrid embedding。",
            "测试写入第二条 hybrid embedding 事实。",
        )
        .unwrap();
    let vectors_path = temp.path().join(".pwcli/memory/index/vectors.jsonl");
    let vectors = [
        FactVector {
            fact_id: first.id.clone(),
            model: "BAAI/bge-small-zh-v1.5".to_string(),
            dim: 4,
            vector: vec![0.9, 0.1, 0.0, 0.0],
            indexed_at: "2026-07-02T00:00:00Z".to_string(),
        },
        FactVector {
            fact_id: second.id.clone(),
            model: "BAAI/bge-small-zh-v1.5".to_string(),
            dim: 4,
            vector: vec![0.8, 0.2, 0.0, 0.0],
            indexed_at: "2026-07-02T00:00:00Z".to_string(),
        },
    ];
    let vector_lines = vectors
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    fs::write(&vectors_path, format!("{vector_lines}\n")).unwrap();

    let rebuilt = store.rebuild_graph_index().unwrap();
    assert_eq!(rebuilt.hnsw_nodes, 2);

    let nodes_text =
        fs::read_to_string(temp.path().join(".pwcli/memory/index/hnsw_nodes.jsonl")).unwrap();
    assert!(nodes_text.contains("hybrid_embedding:BAAI/bge-small-zh-v1.5"));

    let recalled = store.search("embedding 事实图召回", 2).unwrap();
    assert!(!recalled.is_empty());
}

#[test]
fn semantic_extraction_candidate_accepts_fact_logic_inference_and_hypothesis() {
    let temp = tempfile::tempdir().unwrap();
    let store = sparse_store(&temp);
    let candidate = store
        .generate_candidate_from_semantic_extraction(
            SemanticMemoryExtraction {
                facts: vec![
                    SemanticFactDraft {
                        ref_id: Some("f1".to_string()),
                        statement: "用户要求 memory 写入前必须经过确认。".to_string(),
                        source_note: Some("测试语义抽取。".to_string()),
                    },
                    SemanticFactDraft {
                        ref_id: Some("f2".to_string()),
                        statement: "pwcli memory 会把事实插入本地近邻图。".to_string(),
                        source_note: Some("测试语义抽取。".to_string()),
                    },
                ],
                logic_chains: vec![SemanticLogicChainDraft {
                    ref_id: Some("l1".to_string()),
                    premises: vec!["f1".to_string(), "f2".to_string()],
                    explanation:
                        "因为事实写入需要确认，且写入后进入图索引，所以候选接受是事实入图的边界。"
                            .to_string(),
                }],
                inferences: vec![SemanticInferenceDraft {
                    statement: "memory inbox 是事实层和对话层之间的审核边界。".to_string(),
                    logic_chain: "l1".to_string(),
                }],
                hypotheses: vec![SemanticHypothesisDraft {
                    statement: "用户会偏好可审计的 memory 写入流程。".to_string(),
                    supporting_facts: vec!["f1".to_string()],
                    confidence: 0.72,
                }],
                reason: Some("测试语义候选。".to_string()),
            },
            "测试来源",
        )
        .unwrap()
        .unwrap();
    let candidate_id = candidate.id.clone();
    store.add_candidate(&candidate).unwrap();
    let facts = store.accept_candidate(&candidate_id).unwrap();
    assert_eq!(facts.len(), 2);

    let recall = store.recall("memory inbox 审核边界", 5).unwrap();
    assert!(!recall.facts.is_empty());
    assert!(recall
        .inferences
        .iter()
        .any(|inference| inference.statement.contains("审核边界")));
    assert!(recall
        .hypotheses
        .iter()
        .any(|hypothesis| hypothesis.statement.contains("可审计")));
}

fn read_memory_graph_events_text(root: &std::path::Path) -> String {
    let mut out = String::new();
    read_text_tree(&root.join(".pwcli/memory/graph/events"), &mut out);
    out
}

fn read_text_tree(path: &std::path::Path, out: &mut String) {
    if path.is_file() {
        if let Ok(text) = fs::read_to_string(path) {
            out.push_str(&text);
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        read_text_tree(&entry.path(), out);
    }
}
