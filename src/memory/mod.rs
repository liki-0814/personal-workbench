use crate::{
    storage::{append_jsonl, write_json},
    PwError, Result,
};
use chrono::{DateTime, Datelike, Local, Utc};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

const HNSW_MAX_LAYER: usize = 6;
const HNSW_MAX_NEIGHBORS: usize = 12;
const HNSW_EF_CONSTRUCTION: usize = 48;
const HNSW_EF_SEARCH: usize = 64;
const SPARSE_VECTOR_DIMS: usize = 384;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Superseded,
    Contradicted,
    Retracted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub statement: String,
    pub source: String,
    pub observed_at: String,
    #[serde(default)]
    pub status: MemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicChain {
    pub id: String,
    pub premises: Vec<String>,
    pub explanation: String,
    pub created_at: String,
    #[serde(default)]
    pub status: MemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inference {
    pub id: String,
    pub statement: String,
    pub logic_chain: String,
    pub created_at: String,
    #[serde(default)]
    pub status: MemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub statement: String,
    pub supporting_facts: Vec<String>,
    pub confidence: f32,
    pub created_at: String,
    #[serde(default)]
    pub status: MemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStatusEvent {
    pub target_id: String,
    pub status: MemoryStatus,
    pub reason: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEdgeRelation {
    SimilarTo,
    SameTopic,
    TemporalNeighbor,
    Supports,
    Contradicts,
    DerivedBy,
    Updates,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEdge {
    pub from: String,
    pub to: String,
    pub relation: MemoryEdgeRelation,
    pub score: f32,
    pub method: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSignature {
    pub fact_id: String,
    pub index_text: String,
    pub tokens: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactVector {
    pub fact_id: String,
    pub model: String,
    pub dim: usize,
    pub vector: Vec<f32>,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswNode {
    pub fact_id: String,
    pub max_layer: usize,
    pub vector_method: String,
    pub inserted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswEdge {
    pub from: String,
    pub to: String,
    pub layer: usize,
    pub score: f32,
    pub method: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HnswMeta {
    pub entry_point: Option<String>,
    pub max_layer: usize,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryGraphStats {
    pub facts: usize,
    pub hnsw_nodes: usize,
    pub hnsw_edges: usize,
    pub max_layer: usize,
    pub entry_point: Option<String>,
    pub vectors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub id: String,
    pub facts: Vec<CandidateFact>,
    #[serde(default)]
    pub logic_chains: Vec<CandidateLogicChain>,
    #[serde(default)]
    pub inferences: Vec<CandidateInference>,
    #[serde(default)]
    pub hypotheses: Vec<CandidateHypothesis>,
    pub reason: String,
    pub source: String,
    pub created_at: String,
    #[serde(default)]
    pub review: MemoryCandidateReview,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateAction {
    Ignore,
    #[default]
    AskUser,
    NeedsClarification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCandidateSignal {
    DurableUserDecision,
    ProjectArchitecture,
    CorrectionOrContradiction,
    RelatedFactsFound,
    MostlyDuplicate,
    HasLogicChain,
    HasInference,
    HasHypothesis,
    LowInformation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidateReview {
    pub action: MemoryCandidateAction,
    pub score: f32,
    #[serde(default)]
    pub signals: Vec<MemoryCandidateSignal>,
    pub strongest_related_score: f32,
    pub related_fact_count: usize,
    pub rationale: String,
}

impl Default for MemoryCandidateReview {
    fn default() -> Self {
        Self {
            action: MemoryCandidateAction::AskUser,
            score: 0.5,
            signals: Vec::new(),
            strongest_related_score: 0.0,
            related_fact_count: 0,
            rationale: "legacy candidate; review was not recorded when it was created".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLifecycleEventKind {
    CandidateCreated,
    CandidateAccepted,
    CandidateRejected,
    FactAdded,
    EdgeAdded,
    StatusChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLifecycleEvent {
    pub kind: MemoryLifecycleEventKind,
    pub subject_id: String,
    #[serde(default)]
    pub related_ids: Vec<String>,
    pub note: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDecisionKind {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateDecision {
    pub candidate_id: String,
    pub decision: CandidateDecisionKind,
    pub decided_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateFact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub statement: String,
    pub source: String,
    #[serde(default)]
    pub related_facts: Vec<RelatedFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateLogicChain {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(default)]
    pub premises: Vec<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateInference {
    pub statement: String,
    pub logic_chain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateHypothesis {
    pub statement: String,
    #[serde(default)]
    pub supporting_facts: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticMemoryExtraction {
    #[serde(default)]
    pub facts: Vec<SemanticFactDraft>,
    #[serde(default)]
    pub logic_chains: Vec<SemanticLogicChainDraft>,
    #[serde(default)]
    pub inferences: Vec<SemanticInferenceDraft>,
    #[serde(default)]
    pub hypotheses: Vec<SemanticHypothesisDraft>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticFactDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticLogicChainDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(default)]
    pub premises: Vec<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticInferenceDraft {
    pub statement: String,
    pub logic_chain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticHypothesisDraft {
    pub statement: String,
    #[serde(default)]
    pub supporting_facts: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedFact {
    pub fact_id: String,
    pub statement: String,
    pub score: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDownloadPolicy {
    #[default]
    Ask,
    Auto,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_consider_write: bool,
    #[serde(default)]
    pub semantic_extraction: MemorySemanticExtractionSettings,
    #[serde(default)]
    pub embedding: MemoryEmbeddingSettings,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_consider_write: true,
            semantic_extraction: MemorySemanticExtractionSettings::default(),
            embedding: MemoryEmbeddingSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySemanticExtractionSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_semantic_extraction_max_input_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_semantic_extraction_max_facts")]
    pub max_facts: usize,
    #[serde(default = "default_semantic_extraction_max_logic_chains")]
    pub max_logic_chains: usize,
    #[serde(default = "default_semantic_extraction_max_inferences")]
    pub max_inferences: usize,
    #[serde(default = "default_semantic_extraction_max_hypotheses")]
    pub max_hypotheses: usize,
}

impl Default for MemorySemanticExtractionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_input_chars: default_semantic_extraction_max_input_chars(),
            max_facts: default_semantic_extraction_max_facts(),
            max_logic_chains: default_semantic_extraction_max_logic_chains(),
            max_inferences: default_semantic_extraction_max_inferences(),
            max_hypotheses: default_semantic_extraction_max_hypotheses(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEmbeddingSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default)]
    pub download: MemoryDownloadPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
}

impl Default for MemoryEmbeddingSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_embedding_model(),
            download: MemoryDownloadPolicy::Ask,
            mirror: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
    embedding_settings: MemoryEmbeddingSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResult {
    pub facts: Vec<ScoredFact>,
    pub inferences: Vec<Inference>,
    pub hypotheses: Vec<Hypothesis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredFact {
    pub fact: Fact,
    pub score: f32,
}

impl MemoryStore {
    pub fn new(
        pwcli_home: impl Into<PathBuf>,
        embedding_settings: MemoryEmbeddingSettings,
    ) -> Self {
        Self {
            root: pwcli_home.into().join("memory"),
            embedding_settings,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for relative in [
            "facts",
            "logic",
            "inferences",
            "hypotheses",
            "inbox",
            "graph/events",
            "graph/hnsw",
            "index",
        ] {
            fs::create_dir_all(self.root.join(relative))?;
        }
        Ok(())
    }

    pub fn add_fact(
        &self,
        statement: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Fact> {
        self.ensure()?;
        let now = Utc::now();
        let statement = statement.into().trim().to_string();
        let source = source.into().trim().to_string();
        if statement.is_empty() {
            return Err(PwError::Message(
                "fact statement cannot be empty".to_string(),
            ));
        }
        if source.is_empty() {
            return Err(PwError::Message("fact source cannot be empty".to_string()));
        }
        let fact = Fact {
            id: stable_id(
                "fact",
                &format!("{statement}\n{source}\n{}", now.to_rfc3339()),
            ),
            statement,
            source,
            observed_at: now.to_rfc3339(),
            status: MemoryStatus::Active,
        };
        self.write_fact(&fact)?;
        self.index_fact(&fact)?;
        self.append_lifecycle_event(MemoryLifecycleEvent {
            kind: MemoryLifecycleEventKind::FactAdded,
            subject_id: fact.id.clone(),
            related_ids: Vec::new(),
            note: "fact written to memory store and indexed".to_string(),
            created_at: Utc::now().to_rfc3339(),
        })?;
        Ok(fact)
    }

    pub fn add_candidate(&self, candidate: &MemoryCandidate) -> Result<()> {
        self.ensure()?;
        if self
            .candidate_decisions()?
            .iter()
            .any(|decision| decision.candidate_id == candidate.id)
        {
            return Ok(());
        }
        if self.is_redundant_candidate(candidate)? {
            return Ok(());
        }
        if candidate.review.action == MemoryCandidateAction::Ignore {
            return Ok(());
        }
        append_jsonl(&self.weekly_path("inbox", Utc::now()), candidate)?;
        self.append_lifecycle_event(MemoryLifecycleEvent {
            kind: MemoryLifecycleEventKind::CandidateCreated,
            subject_id: candidate.id.clone(),
            related_ids: candidate
                .facts
                .iter()
                .flat_map(|fact| {
                    fact.related_facts
                        .iter()
                        .map(|related| related.fact_id.clone())
                })
                .collect(),
            note: format!(
                "candidate created action={:?} score={:.2}: {}",
                candidate.review.action, candidate.review.score, candidate.review.rationale
            ),
            created_at: Utc::now().to_rfc3339(),
        })?;
        Ok(())
    }

    pub fn list_candidates(&self) -> Result<Vec<MemoryCandidate>> {
        let decided = self
            .candidate_decisions()?
            .into_iter()
            .map(|decision| decision.candidate_id)
            .collect::<BTreeSet<_>>();
        let mut candidates = read_jsonl_tree::<MemoryCandidate>(&self.root.join("inbox"))?;
        candidates.retain(|candidate| !decided.contains(&candidate.id));
        candidates.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(candidates)
    }

    pub fn get_candidate(&self, candidate_id: &str) -> Result<Option<MemoryCandidate>> {
        Ok(self
            .list_candidates()?
            .into_iter()
            .find(|candidate| candidate.id == candidate_id))
    }

    pub fn accept_candidate(&self, candidate_id: &str) -> Result<Vec<Fact>> {
        let candidates = self.list_candidates()?;
        let candidate = candidates
            .into_iter()
            .find(|candidate| candidate.id == candidate_id)
            .ok_or_else(|| {
                PwError::Message(format!("unknown memory candidate '{candidate_id}'"))
            })?;
        let review = candidate.review.clone();
        let mut facts = Vec::new();
        let mut fact_ref_map = HashMap::new();
        for (idx, fact) in candidate.facts.into_iter().enumerate() {
            let ref_id = fact
                .ref_id
                .clone()
                .unwrap_or_else(|| format!("f{}", idx + 1));
            let added = self.add_fact(fact.statement.clone(), fact.source)?;
            self.insert_candidate_relation_edges(
                &added.id,
                &fact.statement,
                &fact.related_facts,
                &review,
            )?;
            fact_ref_map.insert(ref_id, added.id.clone());
            facts.push(added);
        }

        let mut logic_ref_map = HashMap::new();
        for (idx, chain) in candidate.logic_chains.into_iter().enumerate() {
            let premises = resolve_fact_refs(&chain.premises, &fact_ref_map);
            if premises.is_empty() || chain.explanation.trim().is_empty() {
                continue;
            }
            let ref_id = chain
                .ref_id
                .clone()
                .unwrap_or_else(|| format!("l{}", idx + 1));
            let added = self.add_logic_chain(premises, chain.explanation)?;
            logic_ref_map.insert(ref_id, added.id);
        }

        for inference in candidate.inferences {
            let logic_chain = logic_ref_map
                .get(&inference.logic_chain)
                .cloned()
                .unwrap_or(inference.logic_chain);
            if logic_chain.trim().is_empty() || inference.statement.trim().is_empty() {
                continue;
            }
            self.add_inference(inference.statement, logic_chain)?;
        }

        for hypothesis in candidate.hypotheses {
            let supporting_facts = resolve_fact_refs(&hypothesis.supporting_facts, &fact_ref_map);
            if supporting_facts.is_empty() || hypothesis.statement.trim().is_empty() {
                continue;
            }
            self.add_hypothesis(
                hypothesis.statement,
                supporting_facts,
                hypothesis.confidence.clamp(0.0, 1.0),
            )?;
        }
        self.decide_candidate(candidate_id, CandidateDecisionKind::Accepted)?;
        self.append_lifecycle_event(MemoryLifecycleEvent {
            kind: MemoryLifecycleEventKind::CandidateAccepted,
            subject_id: candidate_id.to_string(),
            related_ids: facts.iter().map(|fact| fact.id.clone()).collect(),
            note: format!("candidate accepted; wrote {} facts", facts.len()),
            created_at: Utc::now().to_rfc3339(),
        })?;
        Ok(facts)
    }

    pub fn reject_candidate(&self, candidate_id: &str) -> Result<()> {
        if !self
            .list_candidates()?
            .iter()
            .any(|candidate| candidate.id == candidate_id)
        {
            return Err(PwError::Message(format!(
                "unknown memory candidate '{candidate_id}'"
            )));
        }
        self.decide_candidate(candidate_id, CandidateDecisionKind::Rejected)?;
        self.append_lifecycle_event(MemoryLifecycleEvent {
            kind: MemoryLifecycleEventKind::CandidateRejected,
            subject_id: candidate_id.to_string(),
            related_ids: Vec::new(),
            note: "candidate rejected by user".to_string(),
            created_at: Utc::now().to_rfc3339(),
        })?;
        Ok(())
    }

    pub fn list_facts(&self) -> Result<Vec<Fact>> {
        let mut facts = read_jsonl_tree::<Fact>(&self.root.join("facts"))?;
        self.apply_fact_status_events(&mut facts)?;
        facts.sort_by(|a, b| b.observed_at.cmp(&a.observed_at));
        Ok(facts)
    }

    pub fn list_inferences(&self) -> Result<Vec<Inference>> {
        let mut inferences = read_jsonl_tree::<Inference>(&self.root.join("inferences"))?
            .into_iter()
            .filter(|inference| inference.status == MemoryStatus::Active)
            .collect::<Vec<_>>();
        inferences.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(inferences)
    }

    pub fn list_hypotheses(&self) -> Result<Vec<Hypothesis>> {
        let mut hypotheses = read_jsonl_tree::<Hypothesis>(&self.root.join("hypotheses"))?
            .into_iter()
            .filter(|hypothesis| hypothesis.status == MemoryStatus::Active)
            .collect::<Vec<_>>();
        hypotheses.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(hypotheses)
    }

    pub fn add_logic_chain(
        &self,
        premises: Vec<String>,
        explanation: impl Into<String>,
    ) -> Result<LogicChain> {
        self.ensure()?;
        let now = Utc::now();
        let explanation = explanation.into().trim().to_string();
        if premises.is_empty() {
            return Err(PwError::Message(
                "logic chain premises cannot be empty".to_string(),
            ));
        }
        if explanation.is_empty() {
            return Err(PwError::Message(
                "logic chain explanation cannot be empty".to_string(),
            ));
        }
        let chain = LogicChain {
            id: stable_id(
                "logic",
                &format!(
                    "{}\n{}\n{}",
                    premises.join("\n"),
                    explanation,
                    now.to_rfc3339()
                ),
            ),
            premises,
            explanation,
            created_at: now.to_rfc3339(),
            status: MemoryStatus::Active,
        };
        append_jsonl(&self.weekly_path("logic", now), &chain)?;
        Ok(chain)
    }

    pub fn add_inference(
        &self,
        statement: impl Into<String>,
        logic_chain: impl Into<String>,
    ) -> Result<Inference> {
        self.ensure()?;
        let now = Utc::now();
        let statement = statement.into().trim().to_string();
        let logic_chain = logic_chain.into().trim().to_string();
        if statement.is_empty() || logic_chain.is_empty() {
            return Err(PwError::Message(
                "inference statement and logic chain cannot be empty".to_string(),
            ));
        }
        let inference = Inference {
            id: stable_id(
                "infer",
                &format!("{statement}\n{logic_chain}\n{}", now.to_rfc3339()),
            ),
            statement,
            logic_chain,
            created_at: now.to_rfc3339(),
            status: MemoryStatus::Active,
        };
        append_jsonl(&self.weekly_path("inferences", now), &inference)?;
        Ok(inference)
    }

    pub fn add_hypothesis(
        &self,
        statement: impl Into<String>,
        supporting_facts: Vec<String>,
        confidence: f32,
    ) -> Result<Hypothesis> {
        self.ensure()?;
        let now = Utc::now();
        let statement = statement.into().trim().to_string();
        if statement.is_empty() || supporting_facts.is_empty() {
            return Err(PwError::Message(
                "hypothesis statement and supporting facts cannot be empty".to_string(),
            ));
        }
        let hypothesis = Hypothesis {
            id: stable_id(
                "hyp",
                &format!(
                    "{statement}\n{}\n{}\n{}",
                    supporting_facts.join("\n"),
                    confidence,
                    now.to_rfc3339()
                ),
            ),
            statement,
            supporting_facts,
            confidence: confidence.clamp(0.0, 1.0),
            created_at: now.to_rfc3339(),
            status: MemoryStatus::Active,
        };
        append_jsonl(&self.weekly_path("hypotheses", now), &hypothesis)?;
        Ok(hypothesis)
    }

    pub fn recall(&self, query: &str, limit: usize) -> Result<RecallResult> {
        if limit == 0 || query.trim().is_empty() {
            return Ok(RecallResult {
                facts: Vec::new(),
                inferences: Vec::new(),
                hypotheses: Vec::new(),
            });
        }

        let facts = self.active_facts()?;
        if facts.is_empty() {
            return Ok(RecallResult {
                facts: Vec::new(),
                inferences: Vec::new(),
                hypotheses: Vec::new(),
            });
        }

        let signatures = self.signatures_by_fact()?;
        let query_tokens = tokenize(query);
        let mut scored = self.hnsw_recall(&facts, &signatures, query, &query_tokens, limit)?;
        if scored.len() < limit {
            let already = scored
                .iter()
                .map(|scored| scored.fact.id.clone())
                .collect::<HashSet<_>>();
            let edges = self.edges()?;
            scored.extend(
                nann_style_recall(&facts, &signatures, &edges, &query_tokens, limit)
                    .into_iter()
                    .filter(|candidate| !already.contains(&candidate.fact.id)),
            );
        }
        for scored_fact in &mut scored {
            scored_fact.score =
                rerank_memory_score(scored_fact.score, &scored_fact.fact, &query_tokens);
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| b.fact.observed_at.cmp(&a.fact.observed_at))
        });
        scored.truncate(limit);

        let fact_ids = scored
            .iter()
            .map(|fact| fact.fact.id.clone())
            .collect::<BTreeSet<_>>();
        let related_logic_chain_ids = read_jsonl_tree::<LogicChain>(&self.root.join("logic"))?
            .into_iter()
            .filter(|chain| chain.status == MemoryStatus::Active)
            .filter(|chain| {
                chain
                    .premises
                    .iter()
                    .any(|fact_id| fact_ids.contains(fact_id))
            })
            .map(|chain| chain.id)
            .collect::<BTreeSet<_>>();
        let inferences = read_jsonl_tree::<Inference>(&self.root.join("inferences"))?
            .into_iter()
            .filter(|inference| inference.status == MemoryStatus::Active)
            .filter(|inference| related_logic_chain_ids.contains(&inference.logic_chain))
            .take(5)
            .collect();
        let hypotheses = read_jsonl_tree::<Hypothesis>(&self.root.join("hypotheses"))?
            .into_iter()
            .filter(|hypothesis| hypothesis.status == MemoryStatus::Active)
            .filter(|hypothesis| {
                hypothesis
                    .supporting_facts
                    .iter()
                    .any(|fact_id| fact_ids.contains(fact_id))
            })
            .take(5)
            .collect();

        Ok(RecallResult {
            facts: scored,
            inferences,
            hypotheses,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ScoredFact>> {
        Ok(self.recall(query, limit)?.facts)
    }

    pub fn generate_candidate_from_text(
        &self,
        text: &str,
        source: impl Into<String>,
    ) -> Result<Option<MemoryCandidate>> {
        let text = text.trim();
        if !is_high_value_memory_text(text) {
            return Ok(None);
        }
        let source = source.into();
        let now = Utc::now();
        let statements = extract_candidate_statements(text, &source);
        if statements.is_empty() {
            return Ok(None);
        }
        let facts = statements
            .into_iter()
            .map(|statement| {
                let related_facts = self
                    .search(&statement, 3)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|scored| RelatedFact {
                        fact_id: scored.fact.id,
                        statement: scored.fact.statement,
                        score: scored.score,
                        reason: "本地事实图召回的相近事实".to_string(),
                    })
                    .collect();
                CandidateFact {
                    ref_id: None,
                    statement,
                    source: source.clone(),
                    related_facts,
                }
            })
            .collect::<Vec<_>>();
        let mut candidate = MemoryCandidate {
            id: stable_id("memcand", &canonical_candidate_facts_fingerprint(&facts)),
            facts,
            logic_chains: Vec::new(),
            inferences: Vec::new(),
            hypotheses: Vec::new(),
            reason: "内容包含明确决定、要求、否定、项目背景或重要总结，值得询问是否写入事实层。"
                .to_string(),
            source,
            created_at: now.to_rfc3339(),
            review: MemoryCandidateReview::default(),
        };
        self.review_memory_candidate(&mut candidate);
        if candidate.review.action == MemoryCandidateAction::Ignore {
            return Ok(None);
        }
        Ok(Some(candidate))
    }

    pub fn generate_candidate_from_semantic_extraction(
        &self,
        extraction: SemanticMemoryExtraction,
        source: impl Into<String>,
    ) -> Result<Option<MemoryCandidate>> {
        let source = source.into();
        let now = Utc::now();
        let mut facts = Vec::new();
        for (idx, draft) in extraction.facts.into_iter().enumerate() {
            let statement = normalize_space(&draft.statement);
            if statement.chars().count() < 8 {
                continue;
            }
            let source_note = draft
                .source_note
                .map(|note| normalize_space(&note))
                .filter(|note| !note.is_empty())
                .unwrap_or_else(|| source.clone());
            let related_facts = self
                .search(&statement, 3)
                .unwrap_or_default()
                .into_iter()
                .map(|scored| RelatedFact {
                    fact_id: scored.fact.id,
                    statement: scored.fact.statement,
                    score: scored.score,
                    reason: "语义抽取候选写入前召回的相近事实".to_string(),
                })
                .collect();
            facts.push(CandidateFact {
                ref_id: Some(draft.ref_id.unwrap_or_else(|| format!("f{}", idx + 1))),
                statement,
                source: source_note,
                related_facts,
            });
            if facts.len() >= 5 {
                break;
            }
        }

        let logic_chains = extraction
            .logic_chains
            .into_iter()
            .filter_map(|chain| {
                let explanation = normalize_space(&chain.explanation);
                if explanation.is_empty() || chain.premises.is_empty() {
                    return None;
                }
                Some(CandidateLogicChain {
                    ref_id: chain.ref_id,
                    premises: chain.premises,
                    explanation,
                })
            })
            .take(5)
            .collect::<Vec<_>>();
        let inferences = extraction
            .inferences
            .into_iter()
            .filter_map(|inference| {
                let statement = normalize_space(&inference.statement);
                if statement.is_empty() || inference.logic_chain.trim().is_empty() {
                    return None;
                }
                Some(CandidateInference {
                    statement,
                    logic_chain: inference.logic_chain,
                })
            })
            .take(5)
            .collect::<Vec<_>>();
        let hypotheses = extraction
            .hypotheses
            .into_iter()
            .filter_map(|hypothesis| {
                let statement = normalize_space(&hypothesis.statement);
                if statement.is_empty() || hypothesis.supporting_facts.is_empty() {
                    return None;
                }
                Some(CandidateHypothesis {
                    statement,
                    supporting_facts: hypothesis.supporting_facts,
                    confidence: hypothesis.confidence.clamp(0.0, 1.0),
                })
            })
            .take(5)
            .collect::<Vec<_>>();

        if facts.is_empty()
            && logic_chains.is_empty()
            && inferences.is_empty()
            && hypotheses.is_empty()
        {
            return Ok(None);
        }

        let reason = extraction.reason.unwrap_or_else(|| {
            "LLM 语义抽取认为该内容包含可复用事实、推论或猜想，写入前需要用户确认。".to_string()
        });
        let id_input = canonical_candidate_fingerprint(&MemoryCandidate {
            id: String::new(),
            facts: facts.clone(),
            logic_chains: logic_chains.clone(),
            inferences: inferences.clone(),
            hypotheses: hypotheses.clone(),
            reason: String::new(),
            source: String::new(),
            created_at: String::new(),
            review: MemoryCandidateReview::default(),
        });
        let mut candidate = MemoryCandidate {
            id: stable_id("memcand", &id_input),
            facts,
            logic_chains,
            inferences,
            hypotheses,
            reason,
            source,
            created_at: now.to_rfc3339(),
            review: MemoryCandidateReview::default(),
        };
        self.review_memory_candidate(&mut candidate);
        if candidate.review.action == MemoryCandidateAction::Ignore {
            return Ok(None);
        }
        Ok(Some(candidate))
    }

    pub fn derive_candidate_from_graph(
        &self,
        query: Option<&str>,
    ) -> Result<Option<MemoryCandidate>> {
        let facts = self.active_facts()?;
        if facts.len() < 2 {
            return Ok(None);
        }
        let fact_map = facts
            .iter()
            .map(|fact| (fact.id.clone(), fact.clone()))
            .collect::<HashMap<_, _>>();
        let query_tokens = query
            .map(tokenize)
            .filter(|tokens| !tokens.is_empty())
            .unwrap_or_default();
        let mut edges = self
            .edges()?
            .into_iter()
            .filter(|edge| fact_map.contains_key(&edge.from) && fact_map.contains_key(&edge.to))
            .filter(|edge| {
                if query_tokens.is_empty() {
                    true
                } else {
                    let from = fact_map.get(&edge.from).unwrap();
                    let to = fact_map.get(&edge.to).unwrap();
                    let text = format!("{} {}", from.statement, to.statement);
                    let tokens = tokenize(&text).into_iter().collect::<Vec<_>>();
                    token_overlap_score(&query_tokens, &tokens) > 0.0
                }
            })
            .collect::<Vec<_>>();
        edges.sort_by(|a, b| {
            edge_relation_priority(b.relation)
                .cmp(&edge_relation_priority(a.relation))
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
        });
        let Some(edge) = edges.first().cloned() else {
            return Ok(None);
        };
        let from = fact_map.get(&edge.from).unwrap();
        let to = fact_map.get(&edge.to).unwrap();
        let now = Utc::now();
        let (logic_explanation, inference_statement, hypotheses) = match edge.relation {
            MemoryEdgeRelation::Updates => (
                format!(
                    "事实 {} 与事实 {} 之间存在更新关系：较新的事实应作为后续判断的优先依据。",
                    from.id, to.id
                ),
                format!(
                    "关于该主题，{} 更新了此前事实 {}。",
                    trim_sentence(&from.statement, 120),
                    trim_sentence(&to.statement, 120)
                ),
                Vec::new(),
            ),
            MemoryEdgeRelation::Contradicts => (
                format!(
                    "事实 {} 与事实 {} 存在冲突，需要后续以用户确认或更新事实为准。",
                    from.id, to.id
                ),
                format!(
                    "关于该主题，当前事实图存在冲突：{} 与 {} 不能同时作为稳定结论。",
                    trim_sentence(&from.statement, 120),
                    trim_sentence(&to.statement, 120)
                ),
                Vec::new(),
            ),
            _ => (
                format!(
                    "事实 {} 与事实 {} 在事实图中高度相关，因此可以作为同一主题下的联合依据。",
                    from.id, to.id
                ),
                format!(
                    "这些事实共同说明该主题值得在后续任务中优先召回：{} / {}。",
                    trim_sentence(&from.statement, 100),
                    trim_sentence(&to.statement, 100)
                ),
                vec![CandidateHypothesis {
                    statement: format!(
                        "用户可能会继续围绕该主题推进工作：{}。",
                        trim_sentence(&from.statement, 120)
                    ),
                    supporting_facts: vec![from.id.clone(), to.id.clone()],
                    confidence: edge.score.clamp(0.35, 0.85),
                }],
            ),
        };
        let mut candidate = MemoryCandidate {
            id: String::new(),
            facts: Vec::new(),
            logic_chains: vec![CandidateLogicChain {
                ref_id: Some("l1".to_string()),
                premises: vec![from.id.clone(), to.id.clone()],
                explanation: logic_explanation,
            }],
            inferences: vec![CandidateInference {
                statement: inference_statement,
                logic_chain: "l1".to_string(),
            }],
            hypotheses,
            reason: format!(
                "根据 memory graph 中的 {:?} 边自动派生，写入前需要用户确认。",
                edge.relation
            ),
            source: format!(
                "{} memory graph derive{}",
                now.to_rfc3339(),
                query.map(|q| format!(" query={q}")).unwrap_or_default()
            ),
            created_at: now.to_rfc3339(),
            review: MemoryCandidateReview::default(),
        };
        let id_input = canonical_candidate_fingerprint(&candidate);
        candidate.id = stable_id("memcand", &id_input);
        self.review_memory_candidate(&mut candidate);
        if candidate.review.action == MemoryCandidateAction::Ignore {
            return Ok(None);
        }
        Ok(Some(candidate))
    }

    pub fn graph_stats(&self) -> Result<MemoryGraphStats> {
        let facts = self.active_facts()?;
        let nodes = self.hnsw_nodes()?;
        let edges = self.hnsw_edges()?;
        let meta = self.hnsw_meta()?;
        let vectors = self.fact_vectors()?.len();
        Ok(MemoryGraphStats {
            facts: facts.len(),
            hnsw_nodes: nodes.len(),
            hnsw_edges: edges.len(),
            max_layer: meta.max_layer,
            entry_point: meta.entry_point,
            vectors,
        })
    }

    pub fn rebuild_graph_index(&self) -> Result<MemoryGraphStats> {
        self.ensure()?;
        let nodes_path = self.root.join("index/hnsw_nodes.jsonl");
        let meta_path = self.root.join("index/hnsw_meta.json");
        if nodes_path.exists() {
            fs::remove_file(nodes_path)?;
        }
        if meta_path.exists() {
            fs::remove_file(meta_path)?;
        }
        fs::create_dir_all(self.root.join("graph/hnsw/archive"))?;
        let hnsw_dir = self.root.join("graph/hnsw");
        if hnsw_dir.exists() {
            for entry in fs::read_dir(&hnsw_dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    fs::remove_dir_all(path)?;
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                    fs::remove_file(path)?;
                }
            }
        }

        let mut facts = self.active_facts()?;
        facts.sort_by(|a, b| a.observed_at.cmp(&b.observed_at));
        for fact in &facts {
            if let Some(signature) = self.signatures_by_fact()?.get(&fact.id).cloned() {
                self.insert_hnsw_node(fact, &signature)?;
            }
        }
        self.graph_stats()
    }

    pub fn ensure_embedding_model(&self) -> Result<PathBuf> {
        self.ensure()?;
        let signature = SemanticSignature {
            fact_id: "embedding_probe".to_string(),
            index_text: "pwcli memory embedding probe".to_string(),
            tokens: tokenize("pwcli memory embedding probe")
                .into_iter()
                .collect::<Vec<_>>(),
            created_at: Utc::now().to_rfc3339(),
        };
        embed_with_fastembed(
            &self.embedding_settings,
            &self.model_cache_dir(),
            &signature,
        )?;
        Ok(self.model_cache_dir())
    }

    pub fn model_cache_dir(&self) -> PathBuf {
        self.root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("models/embeddings")
    }

    fn write_fact(&self, fact: &Fact) -> Result<()> {
        append_jsonl(
            &self.weekly_path("facts", parse_time_or_now(&fact.observed_at)),
            fact,
        )
    }

    fn index_fact(&self, fact: &Fact) -> Result<()> {
        let signature = SemanticSignature::from_fact(fact);
        append_jsonl(&self.root.join("index/signatures.jsonl"), &signature)?;
        if self.embedding_settings.enabled {
            if let Some(vector) = self.try_embed_signature(&signature)? {
                append_jsonl(&self.root.join("index/vectors.jsonl"), &vector)?;
            }
        }
        self.insert_weak_edges(fact, &signature)?;
        self.insert_hnsw_node(fact, &signature)?;
        Ok(())
    }

    fn try_embed_signature(&self, signature: &SemanticSignature) -> Result<Option<FactVector>> {
        if matches!(
            self.embedding_settings.download,
            MemoryDownloadPolicy::Never
        ) {
            return Ok(None);
        }

        let cache_dir = self.model_cache_dir();
        if !has_cached_embedding_model(&cache_dir, &self.embedding_settings.model)
            && matches!(self.embedding_settings.download, MemoryDownloadPolicy::Ask)
        {
            return Ok(None);
        }

        embed_with_fastembed(&self.embedding_settings, &cache_dir, signature)
    }

    fn insert_weak_edges(&self, fact: &Fact, signature: &SemanticSignature) -> Result<()> {
        let facts = self.active_facts()?;
        let signatures = self.signatures_by_fact()?;
        let mut neighbors = facts
            .into_iter()
            .filter(|candidate| candidate.id != fact.id)
            .filter_map(|candidate| {
                let candidate_signature = signatures.get(&candidate.id)?;
                let score = token_jaccard(&signature.tokens, &candidate_signature.tokens);
                if score < 0.08 {
                    return None;
                }
                Some((candidate, score))
            })
            .collect::<Vec<_>>();
        neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

        let now = Utc::now().to_rfc3339();
        for (neighbor, score) in neighbors.into_iter().take(8) {
            append_jsonl(
                &self.weekly_path("graph/events", Utc::now()),
                &MemoryEdge {
                    from: fact.id.clone(),
                    to: neighbor.id,
                    relation: if score > 0.25 {
                        MemoryEdgeRelation::SameTopic
                    } else {
                        MemoryEdgeRelation::SimilarTo
                    },
                    score,
                    method: "sparse_token_overlap".to_string(),
                    created_at: now.clone(),
                },
            )?;
        }
        Ok(())
    }

    fn insert_hnsw_node(&self, fact: &Fact, signature: &SemanticSignature) -> Result<()> {
        let existing_nodes = self.hnsw_nodes()?;
        if existing_nodes.iter().any(|node| node.fact_id == fact.id) {
            return Ok(());
        }

        let dense_vectors = self.fact_vectors()?;
        let level = deterministic_hnsw_level(&fact.id);
        let node = HnswNode {
            fact_id: fact.id.clone(),
            max_layer: level,
            vector_method: hnsw_vector_method(&fact.id, &dense_vectors),
            inserted_at: Utc::now().to_rfc3339(),
        };

        let mut meta = self.hnsw_meta()?;
        append_jsonl(&self.root.join("index/hnsw_nodes.jsonl"), &node)?;

        if existing_nodes.is_empty() || meta.entry_point.is_none() {
            meta.entry_point = Some(fact.id.clone());
            meta.max_layer = level;
            meta.updated_at = Some(Utc::now().to_rfc3339());
            self.write_hnsw_meta(&meta)?;
            return Ok(());
        }

        let facts = self.active_facts()?;
        let signatures = self.signatures_by_fact()?;
        let new_vector = graph_vector_for_fact(&fact.id, signature, &dense_vectors);
        let vectors = graph_vectors_for_facts(&facts, &signatures, &dense_vectors);
        let existing_edges = self.hnsw_edges()?;
        let mut adjacency = hnsw_adjacency(&existing_edges);
        let now = Utc::now().to_rfc3339();

        let mut entry = meta
            .entry_point
            .clone()
            .unwrap_or_else(|| existing_nodes[0].fact_id.clone());
        for layer in (0..=meta.max_layer).rev() {
            entry = greedy_hnsw_search_layer(&new_vector, &entry, layer, &adjacency, &vectors)
                .unwrap_or(entry);
            if layer <= level {
                let candidates = ef_hnsw_search_layer(
                    &new_vector,
                    &entry,
                    layer,
                    HNSW_EF_CONSTRUCTION,
                    &adjacency,
                    &vectors,
                );
                for neighbor in
                    select_hnsw_neighbors(&new_vector, candidates, &vectors, HNSW_MAX_NEIGHBORS)
                {
                    let score = vector_similarity(
                        &new_vector,
                        vectors.get(&neighbor).unwrap_or(&new_vector),
                    );
                    let edge = HnswEdge {
                        from: fact.id.clone(),
                        to: neighbor.clone(),
                        layer,
                        score,
                        method: "hnsw_semantic_vector".to_string(),
                        created_at: now.clone(),
                    };
                    append_jsonl(&self.weekly_path("graph/hnsw", Utc::now()), &edge)?;
                    append_jsonl(
                        &self.weekly_path("graph/hnsw", Utc::now()),
                        &HnswEdge {
                            from: neighbor.clone(),
                            to: fact.id.clone(),
                            layer,
                            score,
                            method: "hnsw_semantic_vector".to_string(),
                            created_at: now.clone(),
                        },
                    )?;
                    adjacency
                        .entry((edge.from.clone(), layer))
                        .or_default()
                        .push(edge.to.clone());
                    adjacency
                        .entry((neighbor, layer))
                        .or_default()
                        .push(fact.id.clone());
                }
            }
        }

        if level > meta.max_layer {
            meta.entry_point = Some(fact.id.clone());
            meta.max_layer = level;
        }
        meta.updated_at = Some(Utc::now().to_rfc3339());
        self.write_hnsw_meta(&meta)?;
        Ok(())
    }

    fn hnsw_recall(
        &self,
        facts: &[Fact],
        signatures: &HashMap<String, SemanticSignature>,
        query: &str,
        query_tokens: &BTreeSet<String>,
        limit: usize,
    ) -> Result<Vec<ScoredFact>> {
        let nodes = self.hnsw_nodes()?;
        if nodes.is_empty() {
            return Ok(Vec::new());
        }
        let meta = self.hnsw_meta()?;
        let Some(entry_point) = meta.entry_point else {
            return Ok(Vec::new());
        };

        let dense_vectors = self.fact_vectors()?;
        let vectors = graph_vectors_for_facts(facts, signatures, &dense_vectors);
        let dense_query = self.try_embed_query(query)?.map(|vector| vector.vector);
        let query_vector = graph_vector_for_query(query, dense_query.as_deref());
        let edges = self.hnsw_edges()?;
        let adjacency = hnsw_adjacency(&edges);
        let fact_map = facts
            .iter()
            .map(|fact| (fact.id.clone(), fact.clone()))
            .collect::<HashMap<_, _>>();
        let mut entry = entry_point;
        for layer in (1..=meta.max_layer).rev() {
            entry = greedy_hnsw_search_layer(&query_vector, &entry, layer, &adjacency, &vectors)
                .unwrap_or(entry);
        }
        let candidates = ef_hnsw_search_layer(
            &query_vector,
            &entry,
            0,
            HNSW_EF_SEARCH.max(limit * 4),
            &adjacency,
            &vectors,
        );
        let mut scored = candidates
            .into_iter()
            .filter_map(|fact_id| {
                let fact = fact_map.get(&fact_id)?.clone();
                let vector_score = vectors
                    .get(&fact_id)
                    .map(|vector| vector_similarity(&query_vector, vector))
                    .unwrap_or_default()
                    .max(0.0);
                let token_score = signatures
                    .get(&fact_id)
                    .map(|signature| token_overlap_score(query_tokens, &signature.tokens))
                    .unwrap_or_default();
                let dense_score = dense_query
                    .as_ref()
                    .zip(dense_vectors.get(&fact_id))
                    .map(|(query, vector)| vector_similarity(query, &vector.vector).max(0.0))
                    .unwrap_or_default();
                let score = if dense_score > 0.0 {
                    (vector_score * 0.45) + (dense_score * 0.35) + (token_score.min(1.0) * 0.20)
                } else {
                    (vector_score * 0.75) + (token_score.min(1.0) * 0.25)
                };
                Some(ScoredFact { fact, score })
            })
            .filter(|scored| scored.score > 0.0)
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    fn try_embed_query(&self, query: &str) -> Result<Option<FactVector>> {
        if !self.embedding_settings.enabled
            || matches!(
                self.embedding_settings.download,
                MemoryDownloadPolicy::Never
            )
        {
            return Ok(None);
        }
        let cache_dir = self.model_cache_dir();
        if !has_cached_embedding_model(&cache_dir, &self.embedding_settings.model)
            && matches!(self.embedding_settings.download, MemoryDownloadPolicy::Ask)
        {
            return Ok(None);
        }
        let signature = SemanticSignature {
            fact_id: "query".to_string(),
            index_text: query.to_string(),
            tokens: tokenize(query).into_iter().collect(),
            created_at: Utc::now().to_rfc3339(),
        };
        embed_with_fastembed(&self.embedding_settings, &cache_dir, &signature)
    }

    fn active_facts(&self) -> Result<Vec<Fact>> {
        Ok(self
            .list_facts()?
            .into_iter()
            .filter(|fact| fact.status == MemoryStatus::Active)
            .collect())
    }

    fn signatures_by_fact(&self) -> Result<HashMap<String, SemanticSignature>> {
        Ok(
            read_jsonl_tree::<SemanticSignature>(&self.root.join("index/signatures.jsonl"))?
                .into_iter()
                .map(|signature| (signature.fact_id.clone(), signature))
                .collect(),
        )
    }

    fn fact_vectors(&self) -> Result<HashMap<String, FactVector>> {
        Ok(
            read_jsonl_tree::<FactVector>(&self.root.join("index/vectors.jsonl"))?
                .into_iter()
                .map(|vector| (vector.fact_id.clone(), vector))
                .collect(),
        )
    }

    fn edges(&self) -> Result<Vec<MemoryEdge>> {
        read_jsonl_tree::<MemoryEdge>(&self.root.join("graph/events"))
    }

    fn hnsw_nodes(&self) -> Result<Vec<HnswNode>> {
        read_jsonl_tree::<HnswNode>(&self.root.join("index/hnsw_nodes.jsonl"))
    }

    fn hnsw_edges(&self) -> Result<Vec<HnswEdge>> {
        read_jsonl_tree::<HnswEdge>(&self.root.join("graph/hnsw"))
    }

    fn hnsw_meta(&self) -> Result<HnswMeta> {
        let path = self.root.join("index/hnsw_meta.json");
        if !path.exists() {
            return Ok(HnswMeta::default());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn write_hnsw_meta(&self, meta: &HnswMeta) -> Result<()> {
        write_json(&self.root.join("index/hnsw_meta.json"), meta)
    }

    fn candidate_decisions(&self) -> Result<Vec<CandidateDecision>> {
        read_jsonl_tree::<CandidateDecision>(&self.root.join("index/candidate_decisions.jsonl"))
    }

    fn decide_candidate(&self, candidate_id: &str, decision: CandidateDecisionKind) -> Result<()> {
        append_jsonl(
            &self.root.join("index/candidate_decisions.jsonl"),
            &CandidateDecision {
                candidate_id: candidate_id.to_string(),
                decision,
                decided_at: Utc::now().to_rfc3339(),
            },
        )
    }

    pub fn lifecycle_events(&self) -> Result<Vec<MemoryLifecycleEvent>> {
        read_jsonl_tree::<MemoryLifecycleEvent>(&self.root.join("index/lifecycle.jsonl"))
    }

    pub fn status_events(&self) -> Result<Vec<MemoryStatusEvent>> {
        read_jsonl_tree::<MemoryStatusEvent>(&self.root.join("index/status_events.jsonl"))
    }

    fn append_lifecycle_event(&self, event: MemoryLifecycleEvent) -> Result<()> {
        self.ensure()?;
        append_jsonl(&self.root.join("index/lifecycle.jsonl"), &event)
    }

    fn append_status_event(&self, event: MemoryStatusEvent) -> Result<()> {
        self.ensure()?;
        append_jsonl(&self.root.join("index/status_events.jsonl"), &event)?;
        self.append_lifecycle_event(MemoryLifecycleEvent {
            kind: MemoryLifecycleEventKind::StatusChanged,
            subject_id: event.target_id,
            related_ids: Vec::new(),
            note: format!("status changed to {:?}: {}", event.status, event.reason),
            created_at: event.created_at,
        })
    }

    fn apply_fact_status_events(&self, facts: &mut [Fact]) -> Result<()> {
        let mut latest = HashMap::new();
        for event in self.status_events()? {
            latest.insert(event.target_id, event.status);
        }
        for fact in facts {
            if let Some(status) = latest.get(&fact.id) {
                fact.status = *status;
            }
        }
        Ok(())
    }

    fn review_memory_candidate(&self, candidate: &mut MemoryCandidate) {
        let mut signals = Vec::new();
        let text = candidate_text(candidate);
        if has_durable_decision_signal(&text) {
            signals.push(MemoryCandidateSignal::DurableUserDecision);
        }
        if has_architecture_signal(&text) {
            signals.push(MemoryCandidateSignal::ProjectArchitecture);
        }
        if has_correction_signal(&text) {
            signals.push(MemoryCandidateSignal::CorrectionOrContradiction);
        }
        if !candidate.logic_chains.is_empty() {
            signals.push(MemoryCandidateSignal::HasLogicChain);
        }
        if !candidate.inferences.is_empty() {
            signals.push(MemoryCandidateSignal::HasInference);
        }
        if !candidate.hypotheses.is_empty() {
            signals.push(MemoryCandidateSignal::HasHypothesis);
        }

        let related_fact_count = candidate
            .facts
            .iter()
            .map(|fact| fact.related_facts.len())
            .sum::<usize>();
        let strongest_related_score = candidate
            .facts
            .iter()
            .flat_map(|fact| fact.related_facts.iter().map(|related| related.score))
            .fold(0.0_f32, f32::max);
        if related_fact_count > 0 {
            signals.push(MemoryCandidateSignal::RelatedFactsFound);
        }
        if strongest_related_score >= 0.92 && candidate.logic_chains.is_empty() {
            signals.push(MemoryCandidateSignal::MostlyDuplicate);
        }
        let ontology_items = candidate.facts.len()
            + candidate.logic_chains.len()
            + candidate.inferences.len()
            + candidate.hypotheses.len();
        if text.chars().count() < 24 || ontology_items == 0 {
            signals.push(MemoryCandidateSignal::LowInformation);
        }
        signals.sort_by_key(|signal| format!("{signal:?}"));
        signals.dedup();

        let mut score = 0.20_f32;
        score += (candidate.facts.len() as f32 * 0.14).min(0.35);
        score += (candidate.logic_chains.len() as f32 * 0.16).min(0.20);
        score += (candidate.inferences.len() as f32 * 0.10).min(0.14);
        score += (candidate.hypotheses.len() as f32 * 0.06).min(0.10);
        if signals.contains(&MemoryCandidateSignal::DurableUserDecision) {
            score += 0.18;
        }
        if signals.contains(&MemoryCandidateSignal::ProjectArchitecture) {
            score += 0.10;
        }
        if signals.contains(&MemoryCandidateSignal::CorrectionOrContradiction) {
            score += 0.12;
        }
        if strongest_related_score >= 0.92 {
            score -= 0.30;
        } else if strongest_related_score >= 0.62 {
            score += 0.06;
        }
        if signals.contains(&MemoryCandidateSignal::LowInformation) {
            score -= 0.22;
        }
        score = score.clamp(0.0, 1.0);

        let action = if signals.contains(&MemoryCandidateSignal::CorrectionOrContradiction)
            && strongest_related_score >= 0.18
        {
            MemoryCandidateAction::NeedsClarification
        } else if signals.contains(&MemoryCandidateSignal::MostlyDuplicate) || score < 0.36 {
            MemoryCandidateAction::Ignore
        } else {
            MemoryCandidateAction::AskUser
        };
        let rationale = candidate_review_rationale(action, score, &signals);
        candidate.review = MemoryCandidateReview {
            action,
            score,
            signals,
            strongest_related_score,
            related_fact_count,
            rationale,
        };
    }

    fn insert_candidate_relation_edges(
        &self,
        fact_id: &str,
        fact_statement: &str,
        related_facts: &[RelatedFact],
        review: &MemoryCandidateReview,
    ) -> Result<()> {
        if related_facts.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut written = Vec::new();
        for related in related_facts.iter().take(8) {
            if related.fact_id == fact_id || related.score <= 0.0 {
                continue;
            }
            let relation = relation_from_candidate_review(fact_statement, related.score, review);
            append_jsonl(
                &self.weekly_path("graph/events", Utc::now()),
                &MemoryEdge {
                    from: fact_id.to_string(),
                    to: related.fact_id.clone(),
                    relation,
                    score: related.score,
                    method: format!("candidate_review:{:?}", review.action),
                    created_at: now.clone(),
                },
            )?;
            if matches!(
                relation,
                MemoryEdgeRelation::Updates | MemoryEdgeRelation::Contradicts
            ) {
                self.append_status_event(MemoryStatusEvent {
                    target_id: related.fact_id.clone(),
                    status: if relation == MemoryEdgeRelation::Contradicts {
                        MemoryStatus::Contradicted
                    } else {
                        MemoryStatus::Superseded
                    },
                    reason: format!(
                        "new fact {fact_id} created a {:?} edge during candidate acceptance",
                        relation
                    ),
                    created_at: Utc::now().to_rfc3339(),
                })?;
            }
            written.push(related.fact_id.clone());
        }
        if !written.is_empty() {
            self.append_lifecycle_event(MemoryLifecycleEvent {
                kind: MemoryLifecycleEventKind::EdgeAdded,
                subject_id: fact_id.to_string(),
                related_ids: written,
                note: "candidate acceptance wrote relation edges from reviewed related facts"
                    .to_string(),
                created_at: Utc::now().to_rfc3339(),
            })?;
        }
        Ok(())
    }

    fn is_redundant_candidate(&self, candidate: &MemoryCandidate) -> Result<bool> {
        let fingerprint = canonical_candidate_fingerprint(candidate);
        if fingerprint.is_empty() {
            return Ok(true);
        }

        if self
            .list_candidates()?
            .iter()
            .any(|existing| canonical_candidate_fingerprint(existing) == fingerprint)
        {
            return Ok(true);
        }

        if candidate.facts.is_empty()
            || !candidate.logic_chains.is_empty()
            || !candidate.inferences.is_empty()
            || !candidate.hypotheses.is_empty()
        {
            return Ok(false);
        }

        let known_facts = self
            .active_facts()?
            .into_iter()
            .map(|fact| canonical_memory_statement(&fact.statement))
            .filter(|statement| !statement.is_empty())
            .collect::<HashSet<_>>();

        Ok(candidate
            .facts
            .iter()
            .map(|fact| canonical_memory_statement(&fact.statement))
            .filter(|statement| !statement.is_empty())
            .all(|statement| known_facts.contains(&statement)))
    }

    fn weekly_path(&self, prefix: &str, timestamp: DateTime<Utc>) -> PathBuf {
        let local = timestamp.with_timezone(&Local);
        let iso_week = local.iso_week().week();
        self.root
            .join(prefix)
            .join(format!("{:04}", local.year()))
            .join(format!("{:02}", local.month()))
            .join(format!("w{iso_week:02}.jsonl"))
    }
}

impl SemanticSignature {
    pub fn from_fact(fact: &Fact) -> Self {
        let index_text = format!("{}\n来源：{}", fact.statement, fact.source);
        let tokens = tokenize(&index_text).into_iter().collect::<Vec<_>>();
        Self {
            fact_id: fact.id.clone(),
            index_text,
            tokens,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

fn candidate_text(candidate: &MemoryCandidate) -> String {
    let mut parts = Vec::new();
    parts.extend(candidate.facts.iter().map(|fact| fact.statement.as_str()));
    parts.extend(
        candidate
            .logic_chains
            .iter()
            .map(|chain| chain.explanation.as_str()),
    );
    parts.extend(
        candidate
            .inferences
            .iter()
            .map(|inference| inference.statement.as_str()),
    );
    parts.extend(
        candidate
            .hypotheses
            .iter()
            .map(|hypothesis| hypothesis.statement.as_str()),
    );
    parts.join("\n")
}

fn has_durable_decision_signal(text: &str) -> bool {
    contains_any(
        text,
        &[
            "用户要求",
            "用户决定",
            "用户认可",
            "必须",
            "不要",
            "不需要",
            "固定",
            "保留",
            "默认",
            "写入前",
        ],
    )
}

fn has_architecture_signal(text: &str) -> bool {
    contains_any(
        text,
        &[
            "架构", "graph", "workflow", "memory", "tools", "runtime", "policy", "audit", "agent",
            "MCP",
        ],
    )
}

fn has_correction_signal(text: &str) -> bool {
    contains_any(
        text,
        &[
            "更正", "推翻", "纠正", "不是", "不再", "改为", "替换", "废弃", "错了", "不对",
        ],
    )
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn candidate_review_rationale(
    action: MemoryCandidateAction,
    score: f32,
    signals: &[MemoryCandidateSignal],
) -> String {
    let signal_text = if signals.is_empty() {
        "none".to_string()
    } else {
        signals
            .iter()
            .map(|signal| format!("{signal:?}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    match action {
        MemoryCandidateAction::Ignore => {
            format!("score={score:.2}; ignore because it is low-value or mostly duplicate; signals={signal_text}")
        }
        MemoryCandidateAction::AskUser => {
            format!(
                "score={score:.2}; ask user before writing durable memory; signals={signal_text}"
            )
        }
        MemoryCandidateAction::NeedsClarification => {
            format!("score={score:.2}; possible correction or contradiction against related facts; ask user to clarify before accepting; signals={signal_text}")
        }
    }
}

fn relation_from_candidate_review(
    fact_statement: &str,
    score: f32,
    review: &MemoryCandidateReview,
) -> MemoryEdgeRelation {
    if has_correction_signal(fact_statement)
        || review
            .signals
            .contains(&MemoryCandidateSignal::CorrectionOrContradiction)
    {
        if contains_any(fact_statement, &["推翻", "不是", "错了", "不对"]) {
            MemoryEdgeRelation::Contradicts
        } else {
            MemoryEdgeRelation::Updates
        }
    } else if score >= 0.35 {
        MemoryEdgeRelation::SameTopic
    } else {
        MemoryEdgeRelation::SimilarTo
    }
}

fn edge_relation_priority(relation: MemoryEdgeRelation) -> u8 {
    match relation {
        MemoryEdgeRelation::Contradicts => 5,
        MemoryEdgeRelation::Updates => 4,
        MemoryEdgeRelation::Supports | MemoryEdgeRelation::DerivedBy => 3,
        MemoryEdgeRelation::SameTopic => 2,
        MemoryEdgeRelation::SimilarTo | MemoryEdgeRelation::TemporalNeighbor => 1,
    }
}

fn rerank_memory_score(base: f32, fact: &Fact, query_tokens: &BTreeSet<String>) -> f32 {
    let fact_tokens = tokenize(&format!("{} {}", fact.statement, fact.source))
        .into_iter()
        .collect::<Vec<_>>();
    let exact_boost = token_overlap_score(query_tokens, &fact_tokens).min(1.0) * 0.12;
    let recency_boost = memory_recency_boost(&fact.observed_at);
    let source_boost = if contains_any(&fact.source, &["用户", "明确", "确认", "决定"]) {
        0.04
    } else {
        0.0
    };
    (base + exact_boost + recency_boost + source_boost).min(1.5)
}

fn memory_recency_boost(observed_at: &str) -> f32 {
    let observed = parse_time_or_now(observed_at);
    let age_days = Utc::now().signed_duration_since(observed).num_days().max(0) as f32;
    if age_days <= 7.0 {
        0.08
    } else if age_days <= 30.0 {
        0.04
    } else if age_days <= 180.0 {
        0.015
    } else {
        0.0
    }
}

fn nann_style_recall(
    facts: &[Fact],
    signatures: &HashMap<String, SemanticSignature>,
    edges: &[MemoryEdge],
    query_tokens: &BTreeSet<String>,
    limit: usize,
) -> Vec<ScoredFact> {
    let fact_map = facts
        .iter()
        .map(|fact| (fact.id.clone(), fact.clone()))
        .collect::<HashMap<_, _>>();
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for edge in edges {
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
        adjacency
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }

    let mut entry_points = facts
        .iter()
        .filter_map(|fact| {
            let signature = signatures.get(&fact.id)?;
            Some((
                fact.id.clone(),
                token_overlap_score(query_tokens, &signature.tokens),
            ))
        })
        .collect::<Vec<_>>();
    entry_points.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    for (fact_id, _) in entry_points.iter().take(16) {
        queue.push_back(fact_id.clone());
    }
    if queue.is_empty() {
        for fact in facts.iter().take(16) {
            queue.push_back(fact.id.clone());
        }
    }

    let mut scored = BTreeMap::new();
    while let Some(fact_id) = queue.pop_front() {
        if !visited.insert(fact_id.clone()) || visited.len() > (limit.max(16) * 10) {
            continue;
        }
        let Some(fact) = fact_map.get(&fact_id) else {
            continue;
        };
        let score = signatures
            .get(&fact_id)
            .map(|signature| token_overlap_score(query_tokens, &signature.tokens))
            .unwrap_or_else(|| {
                let tokens = tokenize(&fact.statement).into_iter().collect::<Vec<_>>();
                token_overlap_score(query_tokens, &tokens)
            });
        if score > 0.0 {
            scored.insert(
                fact_id.clone(),
                ScoredFact {
                    fact: fact.clone(),
                    score,
                },
            );
        }

        let mut neighbors = adjacency.get(&fact_id).cloned().unwrap_or_default();
        neighbors.sort_by(|a, b| {
            let a_score = signatures
                .get(a)
                .map(|signature| token_overlap_score(query_tokens, &signature.tokens))
                .unwrap_or_default();
            let b_score = signatures
                .get(b)
                .map(|signature| token_overlap_score(query_tokens, &signature.tokens))
                .unwrap_or_default();
            b_score.partial_cmp(&a_score).unwrap_or(Ordering::Equal)
        });
        for neighbor in neighbors.into_iter().take(8) {
            if !visited.contains(&neighbor) {
                queue.push_back(neighbor);
            }
        }
    }

    let mut scored = scored.into_values().collect::<Vec<_>>();
    if scored.len() < limit {
        let already = scored
            .iter()
            .map(|scored| scored.fact.id.clone())
            .collect::<BTreeSet<_>>();
        for (fact_id, score) in entry_points {
            if already.contains(&fact_id) || score <= 0.0 {
                continue;
            }
            if let Some(fact) = fact_map.get(&fact_id) {
                scored.push(ScoredFact {
                    fact: fact.clone(),
                    score,
                });
            }
            if scored.len() >= limit {
                break;
            }
        }
    }

    scored
}

fn deterministic_hnsw_level(fact_id: &str) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fact_id.hash(&mut hasher);
    let mut value = hasher.finish();
    let mut level = 0;
    while level + 1 < HNSW_MAX_LAYER && value & 0b11 == 0 {
        level += 1;
        value >>= 2;
    }
    level
}

fn graph_vector_for_fact(
    fact_id: &str,
    signature: &SemanticSignature,
    dense_vectors: &HashMap<String, FactVector>,
) -> Vec<f32> {
    let semantic = semantic_hash_vector(&signature.index_text);
    dense_vectors
        .get(fact_id)
        .map(|dense| hybrid_memory_vector(&semantic, &dense.vector))
        .unwrap_or(semantic)
}

fn graph_vector_for_query(query: &str, dense_query: Option<&[f32]>) -> Vec<f32> {
    let semantic = semantic_hash_vector(query);
    dense_query
        .map(|dense| hybrid_memory_vector(&semantic, dense))
        .unwrap_or(semantic)
}

fn hnsw_vector_method(fact_id: &str, dense_vectors: &HashMap<String, FactVector>) -> String {
    dense_vectors
        .get(fact_id)
        .map(|vector| format!("hybrid_embedding:{}", vector.model))
        .unwrap_or_else(|| "semantic_hash".to_string())
}

fn graph_vectors_for_facts(
    facts: &[Fact],
    signatures: &HashMap<String, SemanticSignature>,
    dense_vectors: &HashMap<String, FactVector>,
) -> HashMap<String, Vec<f32>> {
    facts
        .iter()
        .filter_map(|fact| {
            let signature = signatures.get(&fact.id)?;
            Some((
                fact.id.clone(),
                graph_vector_for_fact(&fact.id, signature, dense_vectors),
            ))
        })
        .collect()
}

fn hnsw_adjacency(edges: &[HnswEdge]) -> HashMap<(String, usize), Vec<String>> {
    let mut scored: HashMap<(String, usize), Vec<(String, f32)>> = HashMap::new();
    for edge in edges {
        scored
            .entry((edge.from.clone(), edge.layer))
            .or_default()
            .push((edge.to.clone(), edge.score));
    }

    scored
        .into_iter()
        .map(|(key, mut neighbors)| {
            neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
            neighbors.dedup_by(|a, b| a.0 == b.0);
            (
                key,
                neighbors
                    .into_iter()
                    .take(HNSW_MAX_NEIGHBORS)
                    .map(|(fact_id, _)| fact_id)
                    .collect(),
            )
        })
        .collect()
}

fn greedy_hnsw_search_layer(
    query: &[f32],
    entry: &str,
    layer: usize,
    adjacency: &HashMap<(String, usize), Vec<String>>,
    vectors: &HashMap<String, Vec<f32>>,
) -> Option<String> {
    let mut current = entry.to_string();
    let mut current_score = vectors
        .get(&current)
        .map(|vector| vector_similarity(query, vector))?;
    loop {
        let mut improved = false;
        let neighbors = adjacency
            .get(&(current.clone(), layer))
            .cloned()
            .unwrap_or_default();
        for neighbor in neighbors {
            let Some(vector) = vectors.get(&neighbor) else {
                continue;
            };
            let score = vector_similarity(query, vector);
            if score > current_score {
                current = neighbor;
                current_score = score;
                improved = true;
            }
        }
        if !improved {
            return Some(current);
        }
    }
}

fn ef_hnsw_search_layer(
    query: &[f32],
    entry: &str,
    layer: usize,
    ef: usize,
    adjacency: &HashMap<(String, usize), Vec<String>>,
    vectors: &HashMap<String, Vec<f32>>,
) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut candidates = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(entry.to_string());

    while let Some(fact_id) = queue.pop_front() {
        if !visited.insert(fact_id.clone()) || visited.len() > ef.saturating_mul(4).max(ef) {
            continue;
        }
        if let Some(vector) = vectors.get(&fact_id) {
            candidates.push((fact_id.clone(), vector_similarity(query, vector)));
        }
        let mut neighbors = adjacency
            .get(&(fact_id, layer))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|neighbor| {
                let score = vectors
                    .get(&neighbor)
                    .map(|vector| vector_similarity(query, vector))?;
                Some((neighbor, score))
            })
            .collect::<Vec<_>>();
        neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        for (neighbor, _) in neighbors.into_iter().take(ef) {
            if !visited.contains(&neighbor) {
                queue.push_back(neighbor);
            }
        }
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    candidates.dedup_by(|a, b| a.0 == b.0);
    candidates
        .into_iter()
        .take(ef)
        .map(|(fact_id, _)| fact_id)
        .collect()
}

fn select_hnsw_neighbors(
    query: &[f32],
    candidates: Vec<String>,
    vectors: &HashMap<String, Vec<f32>>,
    limit: usize,
) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    let mut candidates = candidates
        .into_iter()
        .filter_map(|fact_id| {
            let vector = vectors.get(&fact_id)?;
            Some((fact_id, vector_similarity(query, vector)))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    for (candidate, score) in candidates {
        if selected.len() >= limit {
            break;
        }
        let Some(candidate_vector) = vectors.get(&candidate) else {
            continue;
        };
        let too_redundant = selected.iter().any(|existing| {
            vectors
                .get(existing)
                .map(|existing_vector| vector_similarity(candidate_vector, existing_vector) > score)
                .unwrap_or(false)
        });
        if !too_redundant || selected.len() < limit / 2 {
            selected.push(candidate);
        }
    }
    selected
}

fn semantic_hash_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0_f32; SPARSE_VECTOR_DIMS];
    for token in tokenize(text) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        token.hash(&mut hasher);
        let hash = hasher.finish();
        let idx = (hash as usize) % SPARSE_VECTOR_DIMS;
        let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
        vector[idx] += sign;
    }
    normalize_vector(vector)
}

fn hybrid_memory_vector(semantic: &[f32], dense: &[f32]) -> Vec<f32> {
    let projected = project_dense_vector(dense);
    let mut vector = vec![0.0_f32; SPARSE_VECTOR_DIMS];
    for (idx, value) in vector.iter_mut().enumerate() {
        *value = semantic.get(idx).copied().unwrap_or_default() * 0.35
            + projected.get(idx).copied().unwrap_or_default() * 0.65;
    }
    normalize_vector(vector)
}

fn project_dense_vector(dense: &[f32]) -> Vec<f32> {
    let mut projected = vec![0.0_f32; SPARSE_VECTOR_DIMS];
    for (idx, value) in dense.iter().enumerate() {
        if *value == 0.0 {
            continue;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        idx.hash(&mut hasher);
        let hash = hasher.finish();
        let bucket = (hash as usize) % SPARSE_VECTOR_DIMS;
        let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
        projected[bucket] += value * sign;
    }
    normalize_vector(projected)
}

fn normalize_vector(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn vector_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(left, right)| left * right).sum()
}

fn resolve_fact_refs(refs: &[String], fact_ref_map: &HashMap<String, String>) -> Vec<String> {
    refs.iter()
        .filter_map(|fact_ref| {
            let trimmed = fact_ref.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(
                    fact_ref_map
                        .get(trimmed)
                        .cloned()
                        .unwrap_or_else(|| trimmed.to_string()),
                )
            }
        })
        .collect::<Vec<_>>()
}

fn embed_with_fastembed(
    settings: &MemoryEmbeddingSettings,
    cache_dir: &Path,
    signature: &SemanticSignature,
) -> Result<Option<FactVector>> {
    let model = embedding_model(&settings.model)?;
    if let Some(mirror) = settings
        .mirror
        .as_deref()
        .filter(|mirror| !mirror.is_empty())
    {
        std::env::set_var("HF_ENDPOINT", mirror);
    }
    let options = fastembed::TextInitOptions::new(model)
        .with_cache_dir(cache_dir.to_path_buf())
        .with_show_download_progress(false)
        .with_intra_threads(2);
    let mut embedder = fastembed::TextEmbedding::try_new(options)
        .map_err(|err| PwError::Message(format!("embedding model init failed: {err}")))?;
    let embeddings = embedder
        .embed([signature.index_text.as_str()], Some(1))
        .map_err(|err| PwError::Message(format!("embedding failed: {err}")))?;
    let Some(vector) = embeddings.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(FactVector {
        fact_id: signature.fact_id.clone(),
        model: settings.model.clone(),
        dim: vector.len(),
        vector,
        indexed_at: Utc::now().to_rfc3339(),
    }))
}

fn embedding_model(model: &str) -> Result<fastembed::EmbeddingModel> {
    match model {
        "BAAI/bge-small-zh-v1.5" | "bge-small-zh-v1.5" => {
            Ok(fastembed::EmbeddingModel::BGESmallZHV15)
        }
        "BAAI/bge-large-zh-v1.5" | "bge-large-zh-v1.5" => {
            Ok(fastembed::EmbeddingModel::BGELargeZHV15)
        }
        "BAAI/bge-m3" | "bge-m3" => Ok(fastembed::EmbeddingModel::BGEM3),
        other => Err(PwError::Message(format!(
            "unsupported local embedding model '{other}'"
        ))),
    }
}

fn has_cached_embedding_model(cache_dir: &Path, model: &str) -> bool {
    if !cache_dir.exists() {
        return false;
    }
    let needle = model.replace('/', "--");
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().contains(&needle) || entry.path().join(model).exists()
    })
}

fn extract_candidate_statements(text: &str, _source: &str) -> Vec<String> {
    let mut statements = Vec::new();
    for paragraph in text.split("\n\n") {
        let normalized = normalize_space(paragraph);
        if normalized.chars().count() < 12 {
            continue;
        }
        if is_high_value_memory_text(&normalized) {
            statements.push(trim_sentence(&normalized, 420));
        }
        if statements.len() >= 3 {
            break;
        }
    }
    if statements.is_empty() && is_high_value_memory_text(text) {
        statements.push(trim_sentence(&normalize_space(text), 420));
    }
    statements
}

pub fn is_high_value_memory_text(text: &str) -> bool {
    let text = text.trim();
    if text.chars().count() < 12 {
        return false;
    }
    let keywords = [
        "认可",
        "确定",
        "决定",
        "要求",
        "必须",
        "不要",
        "不需要",
        "否定",
        "推翻",
        "更正",
        "事实",
        "推论",
        "猜想",
        "memory",
        "pwcli",
        "配置",
        "规则",
        "架构",
    ];
    keywords.iter().any(|keyword| text.contains(keyword))
}

fn read_jsonl_tree<T: for<'de> Deserialize<'de>>(root: &Path) -> Result<Vec<T>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    read_jsonl_tree_inner(root, &mut values)?;
    Ok(values)
}

fn read_jsonl_tree_inner<T: for<'de> Deserialize<'de>>(
    path: &Path,
    values: &mut Vec<T>,
) -> Result<()> {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            return Ok(());
        }
        let content = fs::read_to_string(path)?;
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            values.push(serde_json::from_str(line)?);
        }
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        read_jsonl_tree_inner(&entry?.path(), values)?;
    }
    Ok(())
}

fn tokenize(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut ascii = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ascii.push(ch.to_ascii_lowercase());
        } else {
            if ascii.len() >= 2 {
                tokens.insert(ascii.clone());
            }
            ascii.clear();
        }
    }
    if ascii.len() >= 2 {
        tokens.insert(ascii);
    }

    let chars = text
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .collect::<Vec<_>>();
    for window in chars.windows(2) {
        tokens.insert(window.iter().collect());
    }
    for window in chars.windows(3) {
        tokens.insert(window.iter().collect());
    }
    tokens
}

fn token_overlap_score(query_tokens: &BTreeSet<String>, doc_tokens: &[String]) -> f32 {
    if query_tokens.is_empty() || doc_tokens.is_empty() {
        return 0.0;
    }
    let intersection = doc_tokens
        .iter()
        .filter(|token| query_tokens.contains(*token))
        .count() as f32;
    if intersection == 0.0 {
        return 0.0;
    }
    intersection / (query_tokens.len() as f32).sqrt().max(1.0)
}

fn token_jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a = a.iter().cloned().collect::<BTreeSet<_>>();
    let b = b.iter().cloned().collect::<BTreeSet<_>>();
    let intersection = a.intersection(&b).count() as f32;
    let union = a.union(&b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn stable_id(prefix: &str, input: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{prefix}_{:016x}", hasher.finish())
}

fn canonical_candidate_facts_fingerprint(facts: &[CandidateFact]) -> String {
    let mut parts = facts
        .iter()
        .map(|fact| canonical_memory_statement(&fact.statement))
        .filter(|statement| !statement.is_empty())
        .map(|statement| format!("fact:{statement}"))
        .collect::<Vec<_>>();
    parts.sort();
    parts.join("\n")
}

fn canonical_candidate_fingerprint(candidate: &MemoryCandidate) -> String {
    let mut parts = Vec::new();
    for fact in &candidate.facts {
        let statement = canonical_memory_statement(&fact.statement);
        if !statement.is_empty() {
            parts.push(format!("fact:{statement}"));
        }
    }
    for chain in &candidate.logic_chains {
        let explanation = canonical_memory_statement(&chain.explanation);
        if explanation.is_empty() {
            continue;
        }
        let mut premises = chain
            .premises
            .iter()
            .map(|premise| canonical_memory_statement(premise))
            .filter(|premise| !premise.is_empty())
            .collect::<Vec<_>>();
        premises.sort();
        parts.push(format!("logic:{}=>{explanation}", premises.join("|")));
    }
    for inference in &candidate.inferences {
        let statement = canonical_memory_statement(&inference.statement);
        if !statement.is_empty() {
            parts.push(format!("inference:{statement}"));
        }
    }
    for hypothesis in &candidate.hypotheses {
        let statement = canonical_memory_statement(&hypothesis.statement);
        if !statement.is_empty() {
            parts.push(format!("hypothesis:{statement}"));
        }
    }
    parts.sort();
    parts.join("\n")
}

fn canonical_memory_statement(value: &str) -> String {
    let without_source = value.split("。该事实来自").next().unwrap_or(value);
    without_source
        .chars()
        .filter_map(|ch| {
            if ch.is_whitespace()
                || ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '。' | '，'
                        | '、'
                        | '；'
                        | '：'
                        | '！'
                        | '？'
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                        | '（'
                        | '）'
                        | '【'
                        | '】'
                        | '《'
                        | '》'
                )
            {
                None
            } else {
                Some(ch.to_ascii_lowercase())
            }
        })
        .collect()
}

fn parse_time_or_now(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn normalize_space(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_sentence(value: &str, max_chars: usize) -> String {
    let mut result = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        result.push_str("...");
    }
    result
}

fn default_true() -> bool {
    true
}

fn default_embedding_model() -> String {
    "BAAI/bge-small-zh-v1.5".to_string()
}

fn default_semantic_extraction_max_input_chars() -> usize {
    8000
}

fn default_semantic_extraction_max_facts() -> usize {
    3
}

fn default_semantic_extraction_max_logic_chains() -> usize {
    2
}

fn default_semantic_extraction_max_inferences() -> usize {
    2
}

fn default_semantic_extraction_max_hypotheses() -> usize {
    2
}
