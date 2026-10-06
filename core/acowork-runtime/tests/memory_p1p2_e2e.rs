//! P1/P2 memory data-quality end-to-end tests.
//!
//! Covers every functional branch affected by the P1 (data-correctness) and
//! P2 (behavior-alignment + data-completeness) memory workstreams, driven
//! through the REAL component chain — no mocks:
//!
//! - Write side (ADR-068): `MemoryStoreTool` → `MemoryProvider::store_episode`
//!   → in-memory `GrafeoStore` episodic layer. The tool is a thin **episode
//!   writer**: `knowledge_subtype` routes the episode, and
//!   privacy/importance/keywords travel on `Episode.metadata` /
//!   `Episode.importance`. The tool can no longer write sediment-layer
//!   (Knowledge / Procedural / Autobiographical) nodes directly.
//! - Read side: `export_nodes_filtered` (privacy filtering), `get_knowledge`
//!   (typed field round-trip), `MemoryManager::retrieve` (abstention prompt,
//!   HintType::Identity reaches all labels).
//! - Sediment-layer contracts (export privacy filtering, retrieval, decay) are
//!   exercised on nodes seeded via `GrafeoStore`'s native `store_node` path —
//!   the same path the EpisodicDistiller uses when it promotes an episode
//!   (ADR-068 §3.6). Those tests are therefore read-side contract tests, NOT
//!   LLM write-path tests; the LLM write path (tool → Episode) is covered by
//!   the A* tests below and by `memory_adr068_e2e.rs`.
//! - Forgetting: the episodic time-decay engine `run_episodic_decay_scan`
//!   (half-life retention curve; progressive Active → Dormant → PurgeLog —
//!   ADR-057 §5.3 redesign).
//! - Graph: `GraphExpandConfig` / `get_expand_thresholds` (G11), edge weight
//!   auto-computation via `create_memory_edge` + `compute_edge_weight` (G12).
//!
//! Matrix (one test per affected branch):
//!   A1  store_episode_persists_privacy_importance_keywords_metadata (ADR-068)
//!   A2  store_episode_defaults_privacy_personal                     (ADR-068)
//!   A3  store_autobio_rejected_with_source_key                      (ADR-068 E7)
//!   A4  store_autobio_rejected_by_schema                            (ADR-068 E7)
//!   B1  export_filters_private_knowledge                            (P1-2 export)
//!   B2  export_includes_private_when_requested                      (P1-2 export)
//!   C1  retrieve_empty_injects_abstention_prompt                    (P2 G9)
//!   C2  retrieve_identity_hint_reaches_knowledge                    (P2 G10)
//!   C3  retrieve_excludes_dormant_keeps_active                     (ADR-062 D1)
//!   D4  episodic_decay_progressive_lifecycle                        (ADR-057 §5.3)
//!   E1  graph_expand_thresholds_aligned                             (P2 G11)
//!   E2  edge_weight_auto_computed                                   (P2 G12)
//!   E3  edge_weight_explicit_not_overridden                         (P2 G12)
//!   E4  edge_weight_no_confidence_skips                             (P2 G12)
//!
//! IMPORTANT: these tests are fully self-contained — they use an in-memory
//! `GrafeoStore::new_in_memory()` and never touch the running Gateway /
//! Runtime / Desktop processes, their data dirs, or the :19875/:19876 ports.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;

use acowork_core::EmbeddingProvider;
use acowork_core::tools::traits::Tool;


use acowork_memory::{
    EpisodicDecayConfig, HintType, KnowledgeSubType, MemoryManager, MemoryManagerConfig,
    MemoryProvider, MemoryQuery, NodeStatus, PrivacyLevel, labels,
};

use acowork_runtime::memory::MemorySessionHandle;
use acowork_runtime::tools::builtin::memory_store::MemoryStoreTool;


/// Deterministic embedding provider (same text → same 384-dim vector).
/// Mirrors the production fallback chain's deterministic behavior so recall
/// and dedup semantics are stable across runs.
struct DeterministicEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for DeterministicEmbedding {
    fn name(&self) -> &str {
        "deterministic-p1p2-e2e"
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

/// Shared e2e harness: a real in-memory store wired into a real
/// `MemorySessionHandle` (provider + embedding), ready for `MemoryStoreTool`
/// and `MemoryManager::retrieve`.
struct MemoryE2e {
    store: Arc<acowork_sqlite::SqliteStore>,
    handle: Arc<MemorySessionHandle>,
}

impl MemoryE2e {
    fn new() -> Self {
        let store = Arc::new(
            acowork_sqlite::SqliteStore::open_in_memory(
                acowork_memory::types::DEFAULT_EMBEDDING_DIM,
            )
            .expect("in-memory store"),
        );
        let handle = Arc::new(MemorySessionHandle::new(Some(Arc::new(
            DeterministicEmbedding,
        ))));
        let provider: Arc<dyn MemoryProvider> = store.clone();
        handle.set_provider(provider);
        Self { store, handle }
    }

    fn store_tool(&self) -> MemoryStoreTool {
        MemoryStoreTool::new("com.test.agent", Some(self.handle.clone()))
    }

    /// Seed a sediment-layer (Knowledge) node directly through the provider's
    /// store path, returning its node id.
    ///
    /// ADR-068 §3.2 removed the LLM tool's direct sediment write: the
    /// `memory_store` tool now emits Episodes only, and sediment-layer nodes
    /// are created by the EpisodicDistiller during background consolidation.
    /// Tests that exercise *read-side* sediment contracts (export filtering,
    /// retrieval, decay) therefore seed the same kind of node the distiller
    /// produces, bypassing the LLM tool chain on purpose. The LLM tool → Episode
    /// semantics are asserted by the A* tests in this file and end-to-end in
    /// `memory_adr068_e2e.rs`.
    async fn seed_knowledge(
        &self,
        content: &str,
        sub_type: KnowledgeSubType,
        confidence: f32,
        importance: f32,
        privacy: PrivacyLevel,
        status: NodeStatus,
    ) -> u64 {
        let embedding = DeterministicEmbedding
            .embed(content)
            .await
            .expect("embed ok");
        let node = acowork_memory::KnowledgeNode {
            subject: "user".to_string(),
            predicate: String::new(),
            object: content.to_string(),
            sub_type,
            confidence,
            source_episode_id: None,
            source_episode_ids: Vec::new(),
            promotion_metadata: None,
            embedding: Some(embedding),
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            metadata: HashMap::new(),
            privacy,
            importance,
        };
        acowork_memory::MemoryProvider::store_knowledge(self.store.as_ref(), &node)
            .expect("store_knowledge ok")
    }

    /// Seed an Episode whose `timestamp` is `days` days in the past.
    async fn seed_episode_aged(&self, content: &str, days: i64) -> u64 {
        let provider: &dyn MemoryProvider = self.store.as_ref();
        provider
            .store_episode(&acowork_memory::types::Episode {
                session_id: "p1p2-e2e".to_string(),
                turn_index: 0,
                role: "user".to_string(),
                content: content.to_string(),
                embedding: Some(
                    DeterministicEmbedding
                        .embed(content)
                        .await
                        .expect("embed ok"),
                ),
                timestamp: Utc::now() - chrono::Duration::days(days),
                consolidated: false,
                metadata: Default::default(),
                importance: 0.5,
                knowledge_subtype: None,
                normalized: None,
            })
            .expect("store_episode ok")
    }
}

// ============================================================================
// A series — write-path persistence (P1-2 typed fields, P1-3 keywords,
// P2 G7 autobiographical source)
// ============================================================================

/// A1 (ADR-068 §3.2/§3.3): `MemoryStoreTool` is a thin episode writer — a
/// `fact` write produces an **unconsolidated Episode** carrying
/// `knowledge_subtype = Fact`, NOT a sediment-layer KnowledgeNode. Explicit
/// `privacy` / `importance` / `keywords` travel on the episode
/// (`Episode.importance`, `Episode.metadata`), where the EpisodicDistiller
/// consumes them for promotion decisions.
#[tokio::test]
async fn store_episode_persists_privacy_importance_keywords_metadata() {
    let e2e = MemoryE2e::new();
    let tool = e2e.store_tool();

    let result = tool
        .execute(
            serde_json::json!({
                "category": "fact",
                "content": "User lives in Shanghai",
                "privacy": "public",
                "importance": 0.9,
                "keywords": ["shanghai", "location"],
            }),
            None,
        )
        .await
        .expect("tool execute");
    assert!(result.ok, "tool failed: {:?}", result.error);
    assert!(
        result.content.starts_with("Stored episode:"),
        "tool must report an episode write, got: {}",
        result.content
    );

    // The distiller Step 1 scan (unconsolidated episodes by subtype) must
    // surface exactly this write.
    let provider = e2e.handle.provider().expect("provider set");
    let episodes = provider
        .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
        .expect("get_episodes_by_subtype ok");
    assert_eq!(episodes.len(), 1, "exactly one Fact episode stored");

    let (ep_id, ep) = &episodes[0];
    let _ = ep_id; // id is returned by the provider; tool text itself has none
    assert_eq!(ep.knowledge_subtype, Some(KnowledgeSubType::Fact));
    assert!(!ep.consolidated, "fresh episode must be unconsolidated");

    // importance lands on the typed field.
    assert!(
        (ep.importance - 0.9).abs() < 1e-6,
        "importance = {}",
        ep.importance
    );

    // privacy / keywords land on metadata (the Episode struct has no dedicated
    // fields for them — the distiller reads them from metadata).
    let privacy = ep
        .metadata
        .get("privacy")
        .and_then(|v| v.as_str())
        .expect("privacy in metadata");
    assert_eq!(privacy, "public");

    let keywords = ep
        .metadata
        .get("keywords")
        .and_then(|v| v.as_array())
        .expect("keywords array persisted");
    let strings: Vec<&str> = keywords.iter().filter_map(|v| v.as_str()).collect();
    assert!(strings.contains(&"shanghai") && strings.contains(&"location"));

    // W2: no legacy autobiographical routing fields leak onto the write.
    for legacy in ["aspect", "key", "source"] {
        assert!(
            !ep.metadata.contains_key(legacy),
            "episode must not carry legacy field `{legacy}`"
        );
    }
}

/// A2 (ADR-068): without explicit privacy/importance, the conservative
/// defaults `Personal` (metadata) and `0.5` (`Episode.importance`) apply.
#[tokio::test]
async fn store_episode_defaults_privacy_personal() {
    let e2e = MemoryE2e::new();
    let tool = e2e.store_tool();

    let result = tool
        .execute(
            serde_json::json!({
                "category": "fact",
                "content": "User prefers dark mode",
            }),
            None,
        )
        .await
        .expect("tool execute");
    assert!(result.ok, "tool failed: {:?}", result.error);

    let provider = e2e.handle.provider().expect("provider set");
    let episodes = provider
        .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
        .expect("get_episodes_by_subtype ok");
    assert_eq!(episodes.len(), 1, "exactly one Fact episode stored");

    let (_, ep) = &episodes[0];
    assert_eq!(
        ep.metadata
            .get("privacy")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "personal",
        "default privacy must be personal"
    );
    assert!(
        (ep.importance - 0.5).abs() < 1e-6,
        "importance = {}",
        ep.importance
    );
}

/// A3 (ADR-068 E7): the tool rejects `category=autobiographical` even when the
/// legacy `aspect`/`key`/`source` routing fields are supplied. Autobiographical
/// promotion is the distiller's job; there is no LLM-side fast path anymore.
#[tokio::test]
async fn store_autobio_rejected_with_source_key() {
    let e2e = MemoryE2e::new();
    let tool = e2e.store_tool();

    let result = tool
        .execute(
            serde_json::json!({
                "category": "autobiographical",
                "content": "I tend to give conclusions first",
                "aspect": "preference",
                "key": "style",
                "source": "important_event",
            }),
            None,
        )
        .await
        .expect("tool execute");
    assert!(!result.ok, "autobiographical category must be rejected");
    let err = result.error.expect("error message present");
    assert!(
        err.contains("Invalid category") && err.contains("autobiographical"),
        "rejection must name the invalid category, got: {err}"
    );
    assert!(
        err.contains("fact') || err.contains('preference') || err.contains('relation') || err.contains('procedure")
            || (err.contains("fact") && err.contains("preference") && err.contains("relation") && err.contains("procedure")),
        "rejection must point at the four valid categories, got: {err}"
    );

    // Nothing may land in the episodic layer.
    let provider = e2e.handle.provider().expect("provider set");
    let all = provider
        .get_episodes_by_subtype(None, 10)
        .expect("get_episodes_by_subtype ok");
    assert!(all.is_empty(), "rejected write must not produce an episode");
}

/// A4 (ADR-068 E7): same rejection without any legacy routing fields.
#[tokio::test]
async fn store_autobio_rejected_by_schema() {
    let e2e = MemoryE2e::new();
    let tool = e2e.store_tool();

    let result = tool
        .execute(
            serde_json::json!({
                "category": "autobiographical",
                "content": "I am an AI assistant",
            }),
            None,
        )
        .await
        .expect("tool execute");
    assert!(!result.ok, "autobiographical category must be rejected");
    let err = result.error.expect("error message present");
    assert!(err.contains("Invalid category"), "got: {err}");

    let provider = e2e.handle.provider().expect("provider set");
    let all = provider
        .get_episodes_by_subtype(None, 10)
        .expect("get_episodes_by_subtype ok");
    assert!(all.is_empty(), "rejected write must not produce an episode");
}

// ============================================================================
// C series — retrieval behavior (P2 G9 abstention, P2 G10 Identity labels)
// ============================================================================
//
// The former B series pinned `export_nodes_filtered` (memory export into a
// package). That method belonged to the grafeo store, had no production
// caller, and has no SQLite equivalent, so it was not carried over.

/// C1 (P2 G9): an empty result set with `abstention_enabled` injects the
/// built-in abstention prompt and reports `abstention_triggered`.
#[tokio::test]
async fn retrieve_empty_injects_abstention_prompt() {
    let e2e = MemoryE2e::new();
    let manager = MemoryManager::new(MemoryManagerConfig::default());

    let mut query = MemoryQuery::new("quantum entanglement of hedgehogs");
    query.abstention_enabled = true;

    let result = manager
        .retrieve(&*e2e.store, &mut query, Some(&DeterministicEmbedding))
        .await
        .expect("retrieve ok");

    assert!(
        result.memories.is_empty(),
        "expected no memories on empty store"
    );
    assert!(result.metrics.abstention_triggered, "abstention triggered");
    let prompt = result
        .abstention_prompt
        .expect("abstention prompt injected");
    assert!(!prompt.is_empty(), "abstention prompt is non-empty");
    assert!(
        prompt.contains("not sure")
            || prompt.contains("I don't know")
            || prompt.contains("do not know"),
        "prompt signals abstention: {prompt}"
    );
}

/// C2 (P2 G10): `HintType::Identity` searches all labels, so a `Knowledge`
/// node is reachable through an Identity hint.
///
/// Sediment data is seeded directly (ADR-068 — see file header).
#[tokio::test]
async fn retrieve_identity_hint_reaches_knowledge() {
    let e2e = MemoryE2e::new();

    e2e.seed_knowledge(
        "User lives in Shanghai",
        KnowledgeSubType::Fact,
        0.9,
        0.5,
        PrivacyLevel::Personal,
        NodeStatus::Active,
    )
    .await;

    let manager = MemoryManager::new(MemoryManagerConfig::default());
    let mut query = MemoryQuery::new("User lives in Shanghai");
    query.hint_type = HintType::Identity;
    query.abstention_enabled = false;

    let retrieved = manager
        .retrieve(&*e2e.store, &mut query, Some(&DeterministicEmbedding))
        .await
        .expect("retrieve ok");

    assert!(
        retrieved
            .memories
            .iter()
            .any(|m| m.label == labels::KNOWLEDGE),
        "Identity hint must reach Knowledge nodes, got: {:?}",
        retrieved
            .memories
            .iter()
            .map(|m| (m.label.clone(), m.content.clone()))
            .collect::<Vec<_>>()
    );
}

/// C3 (ADR-062 D1): retrieval excludes Dormant nodes but keeps Active ones.
///
/// A node is stored (Active), verified retrievable, transitioned to Dormant,
/// and must then disappear from the same query's results while a second
/// Active node stays retrievable. The Dormant transition itself is exercised
/// by D4 below — this test pins the read-side contract.
///
/// Sediment data is seeded directly (ADR-068 — see file header), including
/// the status field the decay scan writes.
#[tokio::test]
async fn retrieve_excludes_dormant_keeps_active() {
    let e2e = MemoryE2e::new();

    // ── Node A: high-confidence Active, low importance → decays to Dormant ──
    let a_id = e2e
        .seed_knowledge(
            "User keeps a travel journal about Tokyo",
            KnowledgeSubType::Fact,
            0.95,
            0.1,
            PrivacyLevel::Personal,
            NodeStatus::Active,
        )
        .await;

    // ── Node B: Active, stays retrievable ──
    let b_id = e2e
        .seed_knowledge(
            "User may prefer cycling to work",
            KnowledgeSubType::Fact,
            0.6,
            0.5,
            PrivacyLevel::Personal,
            NodeStatus::Active,
        )
        .await;

    let manager = MemoryManager::new(MemoryManagerConfig::default());
    let query = |text: &str| {
        let mut q = MemoryQuery::new(text.to_string());
        q.abstention_enabled = false;
        q
    };

    // Sanity: both retrievable while Active.
    let before_a = manager
        .retrieve(
            &*e2e.store,
            &mut query("User keeps a travel journal about Tokyo"),
            Some(&DeterministicEmbedding),
        )
        .await
        .expect("retrieve A before");
    assert!(
        before_a.memories.iter().any(|m| m.node_id == a_id),
        "node A must be retrievable while Active"
    );

    let before_b = manager
        .retrieve(
            &*e2e.store,
            &mut query("User may prefer cycling to work"),
            Some(&DeterministicEmbedding),
        )
        .await
        .expect("retrieve B before");
    assert!(
        before_b.memories.iter().any(|m| m.node_id == b_id),
        "node B must be retrievable while Active"
    );

    // Transition A to Dormant (write side exercised by D4).
    e2e.store
        .transition_to_dormant(a_id)
        .expect("transition A to Dormant");

    // After: A excluded, B (Pending) still returned.
    let after_a = manager
        .retrieve(
            &*e2e.store,
            &mut query("User keeps a travel journal about Tokyo"),
            Some(&DeterministicEmbedding),
        )
        .await
        .expect("retrieve A after");
    assert!(
        !after_a.memories.iter().any(|m| m.node_id == a_id),
        "Dormant node A must be excluded from retrieval, got: {:?}",
        after_a
            .memories
            .iter()
            .map(|m| (m.node_id, m.content.clone()))
            .collect::<Vec<_>>()
    );

    let after_b = manager
        .retrieve(
            &*e2e.store,
            &mut query("User may prefer cycling to work"),
            Some(&DeterministicEmbedding),
        )
        .await
        .expect("retrieve B after");
    assert!(
        after_b.memories.iter().any(|m| m.node_id == b_id),
        "Active node B must remain retrievable, got: {:?}",
        after_b
            .memories
            .iter()
            .map(|m| (m.node_id, m.content.clone()))
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// D series — forgetting (P1-1 decay formula/scan, ADR-057 §5.3 episodic decay)
// ============================================================================

/// D4 (ADR-057 §5.3 redesign): `run_episodic_decay_scan` drives the single
/// time-decay lifecycle over Episodic nodes — very old Active episodes
/// (retention < dormant_threshold) → Dormant; Dormant episodes dormant past
/// `archive_days` → archived to the PurgeLog; fresh episodes and the
/// sediment layer (Knowledge) are never touched.
/// D4 (ADR-057 §5.3 redesign): `run_episodic_decay_scan` drives the single
/// time-decay lifecycle over Episodic nodes — very old Active episodes
/// (retention < dormant_threshold) → Dormant; Dormant episodes dormant past
/// `archive_days` → archived to the PurgeLog; fresh episodes and the sediment
/// layer (Knowledge) are never touched.
#[tokio::test]
async fn episodic_decay_progressive_lifecycle() {
    let e2e = MemoryE2e::new();
    // `archive_days = 0` keeps the archive branch reachable through the public
    // API: a node transitioned to Dormant now is already past the deadline.
    let cfg = EpisodicDecayConfig {
        enabled: true,
        half_life_days: 180,
        dormant_threshold: 0.1,
        archive_days: 0,
    };

    // Case 1: very old Active episode (~3.9x half-life → retention < 0.1)
    // → Dormant on the next scan.
    let old_active = e2e.seed_episode_aged("a very old event record", 700).await;

    // Case 2: Dormant episode whose archive deadline has already passed.
    let dormant_old = e2e
        .seed_episode_aged("dormant event past archive deadline", 1)
        .await;
    e2e.store
        .transition_to_dormant(dormant_old)
        .expect("transition to Dormant");

    // Case 3: fresh episode (retention ≈ 0.96) stays Active.
    let fresh = e2e.seed_episode_aged("a recent event record", 10).await;

    // Case 4: sediment-layer (Knowledge) node as old as case 1 — the scan must
    // never touch non-Episodic labels.
    let knowledge = e2e
        .seed_knowledge(
            "User's critical preference",
            KnowledgeSubType::Preference,
            0.9,
            0.9,
            PrivacyLevel::Personal,
            NodeStatus::Active,
        )
        .await;

    let result = e2e
        .store
        .run_episodic_decay_scan(&cfg)
        .expect("episodic decay scan ok");
    assert_eq!(
        result.to_dormant, 1,
        "only the old Active episode goes Dormant"
    );
    assert_eq!(
        result.purged, 1,
        "only the stale Dormant episode is archived"
    );

    let provider: &dyn MemoryProvider = e2e.store.as_ref();
    let status = |id: u64| -> Option<NodeStatus> { provider.get_node_status(id).unwrap() };

    assert_eq!(
        status(old_active),
        Some(NodeStatus::Dormant),
        "old episode transitions Active → Dormant"
    );
    assert_eq!(
        status(fresh),
        Some(NodeStatus::Active),
        "fresh episode must stay Active"
    );
    assert_eq!(
        status(knowledge),
        Some(NodeStatus::Active),
        "sediment layer must never be decayed by the episodic scan"
    );
    assert_eq!(
        status(dormant_old),
        None,
        "stale Dormant episode must be archived (row deleted; PurgeLog holds it)"
    );
}
