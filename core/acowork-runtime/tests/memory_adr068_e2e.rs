use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use acowork_core::EmbeddingProvider;
use acowork_core::tools::traits::Tool;

use acowork_memory::consolidation::{
    ConsolidationLlm, DefaultEpisodicDistiller, DistillerConfig, EmbeddingFn, EpisodicDistiller,
    LlmMessage, LlmResponse, PromotionDecision, PromotionKind,
};
use acowork_memory::types::{AutobioCategory, Episode, KnowledgeSubType};
use acowork_memory::{MemoryManager, MemoryManagerConfig, MemoryProvider, MemoryQuery, labels};

use acowork_runtime::memory::MemorySessionHandle;
use acowork_runtime::tools::builtin::memory_store::MemoryStoreTool;

use chrono::{DateTime, Duration as ChronoDuration, Utc};

// ============================================================================
// Deterministic embedding provider (mirrors the production fallback chain)
// ============================================================================

struct DeterministicEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for DeterministicEmbedding {
    fn name(&self) -> &str {
        "deterministic-adr068-e2e"
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, acowork_core::EmbeddingError> {
        Ok(acowork_memory::manager::procedural_embedding_fallback(text))
    }
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, acowork_core::EmbeddingError> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
    fn dimension(&self) -> usize {
        384
    }
    async fn is_available(&self) -> bool {
        true
    }
}

// ============================================================================
// Scripted server-side LLM for distiller Step 2a (extract) + Step 4 (judge)
// ============================================================================

/// A fake `ConsolidationLlm` returning a fixed response queue. The distiller
/// pops one response per LLM call: first the batch extraction JSON, then one
/// judge JSON per candidate cluster.
struct ScriptedDistillerLlm {
    responses: Mutex<VecDeque<String>>,
}

impl ScriptedDistillerLlm {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait::async_trait]
impl ConsolidationLlm for ScriptedDistillerLlm {
    async fn chat(&self, _messages: Vec<LlmMessage>) -> std::result::Result<LlmResponse, String> {
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "ScriptedDistillerLlm: response queue exhausted".to_string())?;
        Ok(LlmResponse {
            content: resp,
            usage_tokens: None,
            ..Default::default()
        })
    }
}

/// Build one merge verdict (the distiller's only LLM touchpoint).
fn merge_verdict(action: &str, target_id: Option<u64>, statement: Option<&str>) -> String {
    serde_json::json!({
        "action": action,
        "target_id": target_id,
        "statement": statement,
        "reasoning": "scripted adr068 verdict",
    })
    .to_string()
}

/// Deterministic text->vector bridge, so a test can put the distiller on the
/// merge path (candidates exist) instead of the projection path.
fn embedding_fn() -> EmbeddingFn {
    Arc::new(|text: &str| acowork_memory::manager::procedural_embedding_fallback(text))
}

/// An embedding that makes every text identical, so *any* pair of episodes
/// recalls each other. Used to reach the `no_merge` / `contradicts` verdicts
/// with two statements that are genuinely different facts — the hash-based
/// [`embedding_fn`] puts unrelated texts near cosine 0, which would hide those
/// branches behind a recall that never fires.
fn blind_embedding_fn() -> EmbeddingFn {
    Arc::new(|_text: &str| vec![1.0; acowork_memory::manager::PROCEDURAL_FALLBACK_DIM])
}

// ============================================================================
// Harness
// ============================================================================

struct Adr068E2e {
    store: Arc<acowork_sqlite::SqliteStore>,
    handle: Arc<MemorySessionHandle>,
}

impl Adr068E2e {
    fn new() -> Self {
        let store = Arc::new(
            acowork_sqlite::SqliteStore::open_in_memory(acowork_memory::types::DEFAULT_EMBEDDING_DIM)
                .expect("in-memory store"),
        );
        let handle = Arc::new(MemorySessionHandle::new(Some(Arc::new(
            DeterministicEmbedding,
        ))));
        let provider: Arc<dyn MemoryProvider> = store.clone();
        handle.set_provider(provider);
        Self { store, handle }
    }

    fn provider(&self) -> Arc<dyn MemoryProvider> {
        self.handle.provider().expect("provider set")
    }

    fn store_tool(&self) -> MemoryStoreTool {
        MemoryStoreTool::new("com.test.adr068", Some(self.handle.clone()))
    }

    /// Store a classified episode directly (as if written earlier by the LLM
    /// tool at `timestamp`).
    fn seed_episode(&self, content: &str, subtype: KnowledgeSubType, ts: DateTime<Utc>) -> u64 {
        self.seed_episode_with(content, subtype, ts, 0.5)
    }

    /// As [`Self::seed_episode`], with an explicit `importance` so a test can
    /// sit an episode on either side of `min_importance`.
    fn seed_episode_with(
        &self,
        content: &str,
        subtype: KnowledgeSubType,
        ts: DateTime<Utc>,
        importance: f32,
    ) -> u64 {
        let ep = Episode {
            session_id: "com.test.adr068".to_string(),
            turn_index: 0,
            role: "assistant".to_string(),
            content: content.to_string(),
            embedding: None,
            timestamp: ts,
            consolidated: false,
            metadata: Default::default(),
            importance,
            knowledge_subtype: Some(subtype),
            normalized: None,
        };
        self.provider().store_episode(&ep).expect("store_episode ok")
    }
}

// ============================================================================
// W1 / E2 — LLM tool writes Episodes only
// ============================================================================

/// W1 (e2e): each of the four accepted `category` values routes into an
/// Episode carrying the matching `knowledge_subtype`.
#[tokio::test]
async fn tool_routes_all_four_subtypes() {
    let e2e = Adr068E2e::new();
    let tool = e2e.store_tool();

    let cases = [
        ("fact", "User lives in Shanghai", "Fact"),
        ("preference", "User prefers dark mode", "Preference"),
        ("relation", "Alice works with Bob at Acme", "Relation"),
        (
            "procedure",
            "When user asks for a summary, reply in 3 sentences",
            "Procedure",
        ),
    ];

    for (category, content, expect_subtype) in cases {
        let result = tool
            .execute(
                serde_json::json!({
                    "category": category,
                    "content": content,
                }),
                None,
            )
            .await
            .expect("tool execute");
        assert!(result.ok, "tool failed for {category}: {:?}", result.error);
        assert!(
            result.content.contains(expect_subtype),
            "result must echo subtype {expect_subtype}: {}",
            result.content
        );
    }

    let provider = e2e.provider();
    for (_, _, expect_subtype) in cases {
        let subtype = match expect_subtype {
            "Fact" => KnowledgeSubType::Fact,
            "Preference" => KnowledgeSubType::Preference,
            "Relation" => KnowledgeSubType::Relation,
            _ => KnowledgeSubType::Procedure,
        };
        let eps = provider
            .get_episodes_by_subtype(Some(subtype.clone()), 10)
            .expect("get_episodes_by_subtype ok");
        assert_eq!(eps.len(), 1, "one {expect_subtype} episode stored");
        let (_, ep) = &eps[0];
        assert!(!ep.consolidated, "fresh episode unconsolidated");
        assert_eq!(ep.knowledge_subtype, Some(subtype));
    }
}

/// E2: an agent-feedback `preference` write through the real tool lands as an
/// unconsolidated Episode with `knowledge_subtype = Preference` and no legacy
/// autobiographical routing fields (W2).
#[tokio::test]
async fn tool_write_creates_preference_episode() {
    let e2e = Adr068E2e::new();
    let tool = e2e.store_tool();

    let result = tool
        .execute(
            serde_json::json!({
                "category": "preference",
                "content": "You are too verbose — give shorter answers",
            }),
            None,
        )
        .await
        .expect("tool execute");
    assert!(result.ok, "tool failed: {:?}", result.error);
    assert!(
        result.content.starts_with("Stored episode:"),
        "{}",
        result.content
    );

    let eps = e2e
        .provider()
        .get_episodes_by_subtype(Some(KnowledgeSubType::Preference), 10)
        .expect("get_episodes_by_subtype ok");
    assert_eq!(eps.len(), 1, "exactly one Preference episode");
    let (_, ep) = &eps[0];
    assert_eq!(ep.knowledge_subtype, Some(KnowledgeSubType::Preference));
    assert!(!ep.consolidated, "unconsolidated, awaiting the distiller");
    // W2: no aspect/key/source on the episode.
    for legacy in ["aspect", "key", "source"] {
        assert!(
            !ep.metadata.contains_key(legacy),
            "no legacy `{legacy}` field"
        );
    }
}

// ============================================================================
// E3/E4 — EpisodicDistiller projects and merges episodes
// ============================================================================

/// E3: Preference episodes reporting agent feedback are user preferences, not
/// autobiographical lessons, so they project into `KnowledgeNode` rows. With no
/// embedding function nothing can be recalled, which means the whole backlog
/// consolidates **without a single LLM call** — the property that decouples
/// consolidation from model availability.
#[tokio::test]
async fn distiller_projects_preference_episodes_without_any_llm() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let ids: Vec<u64> = (0..3)
        .map(|i| {
            e2e.seed_episode(
                "You are too verbose, give shorter answers",
                KnowledgeSubType::Preference,
                now - ChronoDuration::days(i * 7),
            )
        })
        .collect();

    let llm = ScriptedDistillerLlm::new(vec![]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            Some(&llm),
            None,
            &DistillerConfig::default(),
        )
        .await
        .expect("distiller run ok");

    assert_eq!(result.preferences_promoted, 3);
    assert_eq!(result.episodes_marked_consolidated, 3);
    assert_eq!(result.funnel.llm_calls, 0, "projection spends no tokens");
    assert_eq!(result.funnel.projected, 3);
    assert_eq!(result.funnel.episodes_deferred, 0);

    // One audit entry per episode, not per cluster: the unit of work is now a
    // single memory, so consolidating three memories reports three decisions.
    assert_eq!(result.promotion_evaluations.len(), 3);
    let node_ids: Vec<Option<u64>> = result
        .promotion_evaluations
        .iter()
        .map(|e| e.promoted_node_id)
        .collect();
    assert!(
        node_ids.iter().all(|id| *id == node_ids[0]) && node_ids[0].is_some(),
        "all three episodes folded into the same node: {node_ids:?}"
    );
    for eval in &result.promotion_evaluations {
        assert!(matches!(eval.decision, PromotionDecision::Promoted));
        assert_eq!(eval.promoted_kind, PromotionKind::Preference);
    }

    // The store collapses identical statements onto one row and unions their
    // provenance, so every episode behind the statement stays traceable.
    let (node_id, node) = e2e
        .store
        .find_knowledge_by_subject("user", "you_are_too_verbose_give_shorter_answers")
        .expect("lookup ok")
        .expect("Preference KnowledgeNode exists");
    assert_eq!(node.sub_type, KnowledgeSubType::Preference);
    assert_eq!(node.source_episode_ids, ids);
    let meta = node.promotion_metadata.as_ref().expect("promotion metadata");
    assert_eq!(meta.promoted_by, "episodic_distiller");
    assert_eq!(meta.evidence_episode_ids, ids);
    assert_eq!(Some(node_id), node_ids[0]);

    assert!(
        e2e
            .provider()
            .get_episodes_by_subtype(None, 10)
            .expect("scan ok")
            .is_empty(),
        "a second run must find nothing left to do"
    );
}

/// E4: distinct Fact episodes with nothing similar to merge against still
/// consolidate with no LLM and no embedding function. This is the case the old
/// pipeline got wrong — an unavailable model used to mean zero output.
#[tokio::test]
async fn distiller_consolidates_distinct_facts_without_a_model() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let shanghai =
        e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let bicycle =
        e2e.seed_episode("User drives a bicycle", KnowledgeSubType::Fact, now - ChronoDuration::days(1));

    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            None,
            &DistillerConfig::default(),
        )
        .await
        .expect("distiller run ok");

    assert_eq!(result.episodes_scanned, 2);
    assert_eq!(result.facts_promoted, 2, "no LLM, no deferral: {result:?}");
    assert_eq!(result.funnel.projected, 2);
    assert_eq!(result.funnel.llm_calls, 0);

    let (node_id, node) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("Fact KnowledgeNode exists");
    assert_eq!(node.object, "User lives in Shanghai");
    assert_eq!(node.source_episode_ids, vec![shanghai]);
    assert!(node.promotion_metadata.is_some());
    let (other_id, other) = e2e
        .store
        .find_knowledge_by_subject("user", "user_drives_a_bicycle")
        .expect("lookup ok")
        .expect("second Fact node");
    assert_eq!(other.source_episode_ids, vec![bicycle]);

    // A4: the audit trail names the REAL storage ids, which rollback needs.
    let mut audited: Vec<u64> = result
        .promotion_evaluations
        .iter()
        .map(|e| e.promoted_node_id.expect("both episodes promoted"))
        .collect();
    audited.sort();
    assert_eq!(audited, vec![node_id, other_id], "one row per statement");
}

/// The merge path, and the retry that precedes it. Given an embedding function,
/// a restated episode *does* see the projected node as a candidate — so a run
/// with no model must defer it rather than guess, and the deferred episode must
/// come back next run instead of being tombstoned.
#[tokio::test]
async fn distiller_defers_a_candidate_without_a_model_then_merges_on_retry() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let older = e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let newer = e2e.seed_episode(
        "User lives in Shanghai",
        KnowledgeSubType::Fact,
        now + ChronoDuration::hours(1),
    );
    let embed = embedding_fn();

    // Run 1: no LLM. Newest-first, so `newer` projects and `older` is the one
    // left staring at a candidate nobody can judge.
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("run 1 ok");
    assert_eq!(result.episodes_marked_consolidated, 1);
    assert_eq!(result.funnel.projected, 1);
    assert_eq!(
        result.funnel.episodes_deferred, 1,
        "a candidate with no model to judge it defers: {result:?}"
    );
    let deferred = &result.promotion_evaluations[1];
    assert_eq!(deferred.source_episode_ids, vec![older]);
    assert!(matches!(
        deferred.decision,
        PromotionDecision::Deferred { .. }
    ));

    // Nothing was lost: the deferred episode is back in the backlog.
    let backlog = e2e
        .provider()
        .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
        .expect("scan ok");
    assert_eq!(
        backlog.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![older],
        "an unconsolidated episode must be re-offered on the next run"
    );

    // Run 2: the model is back and names the node run 1 wrote.
    let (node_id, _) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("node from run 1");
    let llm = ScriptedDistillerLlm::new(vec![merge_verdict(
        "merge",
        Some(node_id),
        Some("User lives in Shanghai"),
    )]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            Some(&llm),
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("run 2 ok");
    assert_eq!(result.funnel.llm_calls, 1, "exactly one merge call");
    assert_eq!(result.funnel.verdict_merged, 1);
    assert_eq!(result.funnel.projected, 0, "nothing was a lone voice");
    assert_eq!(result.episodes_marked_consolidated, 1);

    let (_, node) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("node survives the merge");
    assert_eq!(
        node.source_episode_ids,
        vec![newer, older],
        "merged into the existing row, appending the deferred episode"
    );
}

/// A hallucinated `target_id` must defer the episode rather than rewrite an
/// unrelated node.
#[tokio::test]
async fn distiller_rejects_a_target_id_that_was_not_a_candidate() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let older = e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let newer = e2e.seed_episode(
        "User lives in Shanghai",
        KnowledgeSubType::Fact,
        now + ChronoDuration::hours(1),
    );
    let embed = embedding_fn();
    DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("seed run ok");

    let llm = ScriptedDistillerLlm::new(vec![merge_verdict("merge", Some(9999), None)]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            Some(&llm),
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("second run ok");
    assert_eq!(
        result.funnel.episodes_deferred, 1,
        "a made-up id defers instead of corrupting a node"
    );
    assert_eq!(result.funnel.verdict_merged, 0);
    assert_eq!(result.funnel.llm_calls, 1);

    let (_, node) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("node untouched");
    assert_eq!(
        node.source_episode_ids,
        vec![newer],
        "the declined node kept its original provenance"
    );
    assert!(
        e2e
            .provider()
            .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
            .expect("scan ok")
            .iter()
            .any(|(id, _)| *id == older),
        "and its episode is still eligible"
    );
}

// ============================================================================
// E5 — promoted sediment nodes reach MemoryManager::retrieve
// ============================================================================

/// E5: a projected node is retrievable through the real `MemoryManager` chain,
/// i.e. consolidation actually feeds recall.
#[tokio::test]
async fn retrieve_surfaces_promoted_knowledge_node() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    for i in 0..3 {
        e2e.seed_episode(
            "User prefers concise answers",
            KnowledgeSubType::Preference,
            now - ChronoDuration::days(i * 7),
        );
    }
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            None,
            &DistillerConfig::default(),
        )
        .await
        .expect("distiller run ok");
    assert_eq!(result.preferences_promoted, 3);

    let manager = MemoryManager::new(MemoryManagerConfig::default());
    let mut query = MemoryQuery::new("concise answers".to_string());
    query.abstention_enabled = false;
    let retrieved = manager
        .retrieve(&*e2e.store, &mut query, Some(&DeterministicEmbedding))
        .await
        .expect("retrieve ok");

    assert!(
        retrieved.memories.iter().any(|m| m.label == labels::KNOWLEDGE),
        "promoted KnowledgeNode must be retrievable, got: {:?}",
        retrieved
            .memories
            .iter()
            .map(|m| (m.label.clone(), m.content.clone()))
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// Verdict branches
// ============================================================================

/// A `no_merge` verdict must create a second node. Recall is deliberately
/// permissive (a vector hit is only a *guess* that two statements match), so a
/// false positive must not be able to fold two different facts together.
#[tokio::test]
async fn distiller_keeps_two_nodes_when_the_model_says_no_merge() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let shanghai = e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let bicycle = e2e.seed_episode(
        "User drives a bicycle",
        KnowledgeSubType::Fact,
        now + ChronoDuration::hours(1),
    );
    let embed = blind_embedding_fn();

    // Run 1 projects the newest and defers the older one onto a candidate.
    DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("seed run ok");
    let (bicycle_id, _) = e2e
        .store
        .find_knowledge_by_subject("user", "user_drives_a_bicycle")
        .expect("lookup ok")
        .expect("projected node");

    let llm = ScriptedDistillerLlm::new(vec![merge_verdict("no_merge", None, None)]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            Some(&llm),
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("second run ok");
    assert_eq!(result.funnel.verdict_no_merge, 1);
    assert_eq!(result.funnel.verdict_merged, 0);
    assert_eq!(result.episodes_marked_consolidated, 1);

    let (shanghai_id, shanghai_node) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("no_merge wrote a second node");
    assert_ne!(shanghai_id, bicycle_id, "two statements, two rows");
    assert_eq!(shanghai_node.source_episode_ids, vec![shanghai]);
    let untouched = e2e
        .store
        .get_knowledge(bicycle_id)
        .expect("get ok")
        .expect("the declined node still exists");
    assert_eq!(
        untouched.source_episode_ids,
        vec![bicycle],
        "the false-positive recall must not have written into it"
    );
}

/// A `contradicts` verdict replaces the node's statement but keeps the
/// displaced one, so a wrong verdict is recoverable by a human reading the row.
#[tokio::test]
async fn distiller_supersedes_a_contradicted_node_and_keeps_the_old_statement() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let shanghai = e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let bicycle = e2e.seed_episode(
        "User drives a bicycle",
        KnowledgeSubType::Fact,
        now + ChronoDuration::hours(1),
    );
    let embed = blind_embedding_fn();
    DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("seed run ok");
    let (bicycle_id, _) = e2e
        .store
        .find_knowledge_by_subject("user", "user_drives_a_bicycle")
        .expect("lookup ok")
        .expect("projected node");

    let llm = ScriptedDistillerLlm::new(vec![merge_verdict(
        "contradicts",
        Some(bicycle_id),
        Some("User moved to Shanghai"),
    )]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            Some(&llm),
            Some(&embed),
            &DistillerConfig::default(),
        )
        .await
        .expect("second run ok");
    assert_eq!(result.funnel.verdict_superseded, 1);
    assert_eq!(result.episodes_marked_consolidated, 1);

    let (_, node) = e2e
        .store
        .find_knowledge_by_subject("user", "user_drives_a_bicycle")
        .expect("lookup ok")
        .expect("same row, new statement");
    assert_eq!(node.object, "User moved to Shanghai", "the new fact wins");
    assert_eq!(
        node.source_episode_ids,
        vec![bicycle, shanghai],
        "and both episodes are still cited"
    );
    assert_eq!(
        node.metadata.get("superseded_statements"),
        Some(&serde_json::json!(["User drives a bicycle"])),
        "the displaced statement stays recoverable"
    );
    assert!(
        e2e
            .store
            .find_knowledge_by_subject("user", "user_lives_in_shanghai")
            .expect("lookup ok")
            .is_none(),
        "supersede rewrites the target rather than adding a duplicate row"
    );
}

/// `min_importance` is the run's only remaining filter, and it is opt-in: at
/// the default 0.0 nothing is ever dropped for being unimportant.
#[tokio::test]
async fn distiller_honours_the_min_importance_floor() {
    let e2e = Adr068E2e::new();
    let now = Utc::now();
    let kept = e2e.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let quiet = e2e.seed_episode_with(
        "User glanced at a clock",
        KnowledgeSubType::Fact,
        now + ChronoDuration::hours(1),
        0.2,
    );

    let config = DistillerConfig {
        min_importance: 0.5,
        ..DistillerConfig::default()
    };
    let result = DefaultEpisodicDistiller
        .run(e2e.provider().as_ref(), None, None, &config)
        .await
        .expect("run ok");

    assert_eq!(result.funnel.below_importance, 1);
    assert_eq!(result.episodes_marked_consolidated, 1);
    assert_eq!(result.facts_promoted, 1);
    // Newest-first, so the floored episode is scanned *first* — select by
    // decision rather than by position.
    let promoted: Vec<&_> = result
        .promotion_evaluations
        .iter()
        .filter(|e| matches!(e.decision, PromotionDecision::Promoted))
        .collect();
    assert_eq!(promoted.len(), 1);
    assert_eq!(
        promoted[0].source_episode_ids,
        vec![kept],
        "only the episode at or above the floor consolidated"
    );
    assert!(result.promotion_evaluations.iter().any(|e| matches!(
        e.decision,
        PromotionDecision::Skipped { .. }
    )));

    // The floor is a filter for this run, not a tombstone: the quiet episode is
    // still in the backlog and a run without the floor picks it up.
    let backlog = e2e
        .provider()
        .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
        .expect("scan ok");
    assert_eq!(backlog.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![quiet]);
    let result = DefaultEpisodicDistiller
        .run(
            e2e.provider().as_ref(),
            None,
            None,
            &DistillerConfig::default(),
        )
        .await
        .expect("run without a floor");
    assert_eq!(result.episodes_marked_consolidated, 1);
}

// ============================================================================
// D16 — the Episode schema is two-axis clean
// ============================================================================

/// D16: serialized Episodes expose NO structured-knowledge fields
/// (subject/predicate/object/trigger/action) and NO autobiographical routing
/// fields (aspect/key/source/category) — the two axes (knowledge_subtype for
/// routing, content+metadata for evidence) are the only channels.
///
/// `normalized` is allowed because it stays inside those two axes: it is
/// free-text evidence the writing LLM attaches to its own episodic node (like
/// `content`, which it restates canonically), not a routing field and not a
/// structured-knowledge schema. The two-axis invariant that actually matters —
/// the semantic layer is produced ONLY by the distiller — is untouched: the
/// distiller reads `normalized`, the LLM never writes a semantic node.
/// Adding a real SPO field here would still fail this test.
#[test]
fn episode_schema_is_two_axis_clean() {
    let ep = Episode {
        session_id: "s1".to_string(),
        turn_index: 0,
        role: "assistant".to_string(),
        content: "User lives in Shanghai".to_string(),
        embedding: None,
        timestamp: Utc::now(),
        consolidated: false,
        metadata: Default::default(),
        importance: 0.5,
        knowledge_subtype: Some(KnowledgeSubType::Fact),
        normalized: None,
    };

    let value = serde_json::to_value(&ep).expect("episode serializes");
    let obj = value.as_object().expect("episode is an object");
    let keys: Vec<&String> = obj.keys().collect();

    let allowed = [
        "session_id",
        "turn_index",
        "role",
        "content",
        "embedding",
        "timestamp",
        "consolidated",
        "metadata",
        "importance",
        "knowledge_subtype",
        // Canonical restatement of `content`, written by the LLM into its own
        // episodic node. Free text, not a routing field and not an SPO schema —
        // see the D16 doc-comment for why this stays inside the two axes.
        "normalized",
    ];
    for key in &keys {
        assert!(
            allowed.contains(&key.as_str()),
            "unexpected Episode field `{key}` — schema must stay two-axis clean"
        );
    }
    // Negative assertions mirroring ADR-068 §3.3 (W2): no structured triples,
    // no autobio routing.
    for forbidden in [
        "subject",
        "predicate",
        "object",
        "trigger",
        "action",
        "aspect",
        "key_hint",
        "key",
        "source",
        "category",
        "autobio",
    ] {
        assert!(
            !obj.contains_key(forbidden),
            "Episode must not carry field `{forbidden}`"
        );
    }
    // The routing field is a single enum value.
    assert_eq!(
        obj.get("knowledge_subtype").and_then(|v| v.as_str()),
        Some("Fact")
    );
}

// ============================================================================
// E1 — manifest bootstrap of Identity + Capability nodes
// ============================================================================

/// E1: after agent initialization the manifest-declared Identity and
/// Capability nodes exist (ADR-068 M8 bootstrap scope). The bootstrap free
/// function is the testable exit point for the startup path.
#[tokio::test]
async fn bootstrap_creates_identity_and_capability_nodes() {
    let toml_str = r#"
        agent_id = "com.example.bootstrap"
        version = "1.0.0"
        name = "Bootstrap Agent"
        description = "An agent used to test manifest bootstrap"
        author = "acowork"
        runtime_version = "0.1.0"
        display_name = "Bootstrap"
        role = "tester"

        [memory]
        enabled = true

        [capabilities.weather]
        description = "Query weather forecasts"

        [capabilities.search]
        description = "Search the web"
    "#;
    let manifest =
        acowork_core::manifest::AgentManifest::from_toml(toml_str).expect("manifest parses");
    let e2e = Adr068E2e::new();
    let provider = e2e.provider();

    let outcome =
        acowork_runtime::agent::bootstrap_autobio::bootstrap_autobiographical_from_manifest(
            &manifest,
            provider.as_ref(),
        );
    assert!(!outcome.skipped_existing);
    // agent_id + name + description + display_name + role
    assert_eq!(outcome.identity_written, 5);
    assert_eq!(outcome.capability_written, 2);

    let identities = provider
        .find_autobiographical_by_category(AutobioCategory::Identity)
        .expect("identity lookup ok");
    assert_eq!(identities.len(), 5);
    let keys: Vec<&str> = identities.iter().map(|n| n.key.as_str()).collect();
    for expected in ["agent_id", "name", "description", "display_name", "role"] {
        assert!(keys.contains(&expected), "missing Identity key {expected}");
    }

    let capabilities = provider
        .find_autobiographical_by_category(AutobioCategory::Capability)
        .expect("capability lookup ok");
    assert_eq!(capabilities.len(), 2);
    let cap_keys: Vec<&str> = capabilities.iter().map(|n| n.key.as_str()).collect();
    assert!(cap_keys.contains(&"weather"));
    assert!(cap_keys.contains(&"search"));
    assert_eq!(capabilities[0].source, "manifest");
}

/// E1 (idempotency): a second bootstrap over an already-bootstrapped store
/// is a no-op and does not duplicate nodes.
#[tokio::test]
async fn bootstrap_is_idempotent() {
    let toml_str = r#"
        agent_id = "com.example.bootstrap2"
        version = "1.0.0"
        name = "Bootstrap Agent 2"
        description = "Idempotency test"
        author = "acowork"
        runtime_version = "0.1.0"

        [capabilities.weather]
        description = "Query weather"
    "#;
    let manifest =
        acowork_core::manifest::AgentManifest::from_toml(toml_str).expect("manifest parses");
    let e2e = Adr068E2e::new();
    let provider = e2e.provider();

    let first = acowork_runtime::agent::bootstrap_autobio::bootstrap_autobiographical_from_manifest(
        &manifest,
        provider.as_ref(),
    );
    assert_eq!(first.identity_written, 3); // agent_id, name, description
    assert_eq!(first.capability_written, 1);

    let second =
        acowork_runtime::agent::bootstrap_autobio::bootstrap_autobiographical_from_manifest(
            &manifest,
            provider.as_ref(),
        );
    assert!(second.skipped_existing, "second bootstrap must be skipped");
    assert_eq!(second.identity_written, 0);
    assert_eq!(second.capability_written, 0);

    let identities = provider
        .find_autobiographical_by_category(AutobioCategory::Identity)
        .expect("lookup ok");
    assert_eq!(identities.len(), 3, "no duplicate Identity nodes");
    let capabilities = provider
        .find_autobiographical_by_category(AutobioCategory::Capability)
        .expect("lookup ok");
    assert_eq!(capabilities.len(), 1, "no duplicate Capability nodes");
}
