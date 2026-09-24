//! Memory manager integration tests against a real store.
//!
//! ADR-051 P2: the MemoryManager implementation lives in acowork-memory, and
//! its pure-logic tests (config, inject formatting) live there too. What stays
//! here needs a concrete backend, which is why it drives `acowork-sqlite`
//! rather than an in-crate fake.
//!
//! ADR-082 §4: the backend is SQLite; the grafeo store is gone.

// Re-export for backward compatibility.
pub use acowork_memory::{
    InjectedMemory, MemoryManager, MemoryManagerConfig, RetrievalResult, RetrievedMemory,
};

#[cfg(test)]
mod tests {
    use super::*;
    use acowork_memory::types::DEFAULT_EMBEDDING_DIM;
    use acowork_memory::{HintType, MemoryProvider, MemoryQuery, labels};
    use acowork_sqlite::SqliteStore as TestStore;

    /// Helper: create an in-memory store.
    fn test_store() -> TestStore {
        TestStore::open_in_memory(DEFAULT_EMBEDDING_DIM).unwrap()
    }

    /// Helper: generate a test embedding vector.
    fn test_embedding() -> Vec<f32> {
        vec![0.1f32; DEFAULT_EMBEDDING_DIM]
    }

    /// Helper: an embedding orthogonal to [`test_embedding`] (cosine ≈ 0.0),
    /// i.e. genuinely unrelated under the cosine floor.
    fn orthogonal_embedding() -> Vec<f32> {
        (0..DEFAULT_EMBEDDING_DIM)
            .map(|i| if i % 2 == 0 { 0.1 } else { -0.1 })
            .collect()
    }

    /// Helper: store an Episode with content and embedding.
    fn store_episode(store: &TestStore, content: &str, embedding: &[f32]) -> u64 {
        store_episode_at(store, content, embedding, chrono::Utc::now())
    }

    /// Helper: store an Episode with a specific `timestamp`.
    /// Used by the forgetting time-decay test (ADR-057 §5.3 redesign).
    fn store_episode_at(
        store: &TestStore,
        content: &str,
        embedding: &[f32],
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> u64 {
        let provider: &dyn MemoryProvider = store;
        provider
            .store_episode(&acowork_memory::types::Episode {
                session_id: "test-session".to_string(),
                turn_index: 0,
                role: "user".to_string(),
                content: content.to_string(),
                embedding: Some(embedding.to_vec()),
                timestamp,
                consolidated: false,
                metadata: Default::default(),
                importance: 0.5,
                knowledge_subtype: None,
            })
            .unwrap()
    }

    /// Helper: store an Episode aged `age_days` in the past.
    fn store_episode_with_age(
        store: &TestStore,
        content: &str,
        embedding: &[f32],
        age_days: i64,
    ) -> u64 {
        let created = chrono::Utc::now() - chrono::Duration::days(age_days);
        store_episode_at(store, content, embedding, created)
    }

    /// Helper: store a Knowledge node with embedding.
    fn store_knowledge(
        store: &TestStore,
        subject: &str,
        predicate: &str,
        object: &str,
        embedding: &[f32],
    ) -> u64 {
        // `content` is derived from the triple by the backend, so text search
        // reaches the node; without it only vector search would, and its
        // distance-based score would not clear a `min_cosine` floor.
        let provider: &dyn MemoryProvider = store;
        let now = chrono::Utc::now();
        provider
            .store_knowledge(&acowork_memory::types::KnowledgeNode {
                subject: subject.to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                sub_type: acowork_memory::types::KnowledgeSubType::Fact,
                confidence: 0.9,
                source_episode_id: None,
                source_episode_ids: Vec::new(),
                promotion_metadata: None,
                embedding: Some(embedding.to_vec()),
                status: acowork_memory::types::NodeStatus::Active,
                created_at: now,
                updated_at: now,
                metadata: Default::default(),
                privacy: acowork_memory::types::PrivacyLevel::Personal,
                importance: 0.5,
            })
            .unwrap()
    }

    /// Helper: store an Autobiographical node.
    #[allow(dead_code)]
    fn store_autobiographical(store: &TestStore, key: &str, value: &str, embedding: &[f32]) -> u64 {
        let provider: &dyn MemoryProvider = store;
        let now = chrono::Utc::now();
        provider
            .store_autobiographical(&acowork_memory::types::AutobiographicalNode {
                id: None,
                category: acowork_memory::types::AutobioCategory::Identity,
                key: key.to_string(),
                value: value.to_string(),
                confidence: 1.0,
                source_episode_id: None,
                source: "user_statement".to_string(),
                source_episode_ids: Vec::new(),
                promotion_metadata: None,
                embedding: Some(embedding.to_vec()),
                status: acowork_memory::types::NodeStatus::Active,
                created_at: now,
                updated_at: now,
                metadata: Default::default(),
            })
            .unwrap()
    }

    #[tokio::test]
    async fn test_retrieve_normal() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "user likes rust programming", &emb);
        store_knowledge(&store, "user", "lives_in", "Beijing", &emb);

        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "rust programming".to_string(),
            embedding: Some(emb),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: true,
            hint_type: HintType::Semantic,
        };

        let result = manager
            .retrieve(&store as &dyn MemoryProvider, &mut query, None)
            .await
            .unwrap();
        assert!(!result.memories.is_empty(), "expected at least one result");
        assert!(!result.metrics.abstention_triggered);
        // G9: non-empty result must NOT attach the abstention prompt.
        assert!(result.abstention_prompt.is_none());
    }

    #[tokio::test]
    async fn test_retrieve_empty() {
        let store = test_store();
        let emb = test_embedding();

        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "something completely unrelated".to_string(),
            embedding: Some(emb),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: Some(0.99), // Very high threshold — should filter everything.
            abstention_enabled: true,
            hint_type: HintType::Semantic,
        };

        let result = manager
            .retrieve(&store as &dyn MemoryProvider, &mut query, None)
            .await
            .unwrap();
        assert!(result.memories.is_empty());
        assert!(result.metrics.abstention_triggered);
        assert_eq!(result.metrics.result_count, 0);
        // G9: empty result + abstention enabled must attach the guidance prompt.
        assert!(result.abstention_prompt.is_some());
    }

    #[tokio::test]
    async fn test_retrieve_episodic_forgetting_decay() {
        // ADR-057 §5.3 redesign: with forgetting enabled, Episodic scores
        // are multiplied by the half-life retention factor, so an old node
        // ranks below a fresh one even when their raw scores are identical.
        let store = test_store();
        let emb = test_embedding();
        let fresh_id = store_episode(&store, "user discussed rust traits", &emb);
        let old_id = store_episode_with_age(&store, "user discussed rust traits", &emb, 360);

        // Baseline: forgetting disabled → identical raw scores, order ties.
        let base_config = MemoryManagerConfig::default();
        assert!(!base_config.forgetting.enabled);
        let mut base_query = MemoryQuery {
            query_text: "rust traits discussion".to_string(),
            embedding: Some(emb.clone()),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: false,
            hint_type: HintType::Semantic,
        };
        let base = MemoryManager::new(base_config)
            .retrieve(&store as &dyn MemoryProvider, &mut base_query, None)
            .await
            .unwrap();
        let base_scores: Vec<(u64, f64)> =
            base.memories.iter().map(|m| (m.node_id, m.score)).collect();
        // Both nodes present; without decay the old node is not strictly
        // penalized below the fresh one.
        assert!(base_scores.iter().any(|(id, _)| *id == old_id));

        // With forgetting enabled (half-life 180d) the 360-day-old node
        // must score strictly below the fresh one.
        let mut decayed_config = MemoryManagerConfig::default();
        decayed_config.forgetting.enabled = true;
        decayed_config.forgetting.half_life_days = 180;
        let mut decayed_query = MemoryQuery {
            query_text: "rust traits discussion".to_string(),
            embedding: Some(emb),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: false,
            hint_type: HintType::Semantic,
        };
        let decayed = MemoryManager::new(decayed_config)
            .retrieve(&store as &dyn MemoryProvider, &mut decayed_query, None)
            .await
            .unwrap();
        let decayed_scores: Vec<(u64, f64)> = decayed
            .memories
            .iter()
            .map(|m| (m.node_id, m.score))
            .collect();
        let fresh_score = decayed_scores
            .iter()
            .find(|(id, _)| *id == fresh_id)
            .map(|(_, s)| *s);
        let old_score = decayed_scores
            .iter()
            .find(|(id, _)| *id == old_id)
            .map(|(_, s)| *s);
        assert!(
            fresh_score.is_some() && old_score.is_some(),
            "both episodic nodes must be retrievable; got {decayed_scores:?}"
        );
        let fresh_score = fresh_score.unwrap();
        let old_score = old_score.unwrap();
        assert!(
            fresh_score > old_score,
            "old episodic node must be down-ranked by time decay: fresh={fresh_score} old={old_score}"
        );
        // Retention for 360d at 180d half-life is 2^-2 = 0.25 → old score
        // ≈ 0.25 × fresh score (raw scores identical).
        let ratio = old_score / fresh_score;
        assert!(
            (0.15..=0.35).contains(&ratio),
            "retention ratio out of expected band: {ratio}"
        );
    }

    #[tokio::test]
    async fn test_retrieve_abstention() {
        let store = test_store();
        // Episode content is lexically disjoint from the query ("test
        // content" vs "unrelated query"), so the BM25 text source cannot
        // match. The query embedding is orthogonal to the stored one, so the
        // vector source contributes a far hit but no longer has an absolute
        // cosine floor to keep it out. Recall quality now rests on `k` and
        // the eventual z-score gate (commit 2 follow-up); this test
        // exercises the simplest no-hit case — an empty store.
        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "unrelated query".to_string(),
            embedding: Some(orthogonal_embedding()),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            // `min_cosine` is now ignored by `MemoryManager::retrieve` — the
            // absolute cosine floor is unreliable as a relevance signal
            // (anisotropic embeddings cluster most pairs at cos 0.5–0.9);
            // recall quality is controlled by `k` instead. The field is kept
            // for backwards compatibility with callers that still set it.
            min_cosine: Some(0.99),
            abstention_enabled: true,
            hint_type: HintType::Semantic,
        };

        let result = manager
            .retrieve(&store as &dyn MemoryProvider, &mut query, None)
            .await
            .unwrap();
        assert!(result.memories.is_empty());
        assert!(result.metrics.abstention_triggered);
        // G9: abstention triggered → prompt must be present.
        assert!(result.abstention_prompt.is_some());
    }

    #[tokio::test]
    async fn test_retrieve_no_embedding_fallback() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "rust programming tutorial", &emb);

        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "rust programming".to_string(),
            embedding: None,
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: false,
            hint_type: HintType::Semantic,
        };

        let result = manager
            .retrieve(&store as &dyn MemoryProvider, &mut query, None)
            .await
            .unwrap();
        // Text search should still find results.
        assert!(!result.memories.is_empty());
    }

    #[tokio::test]
    async fn test_process_turn() {
        let store = test_store();
        let emb = test_embedding();
        store_episode(&store, "user prefers concise replies", &emb);

        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "concise".to_string(),
            embedding: Some(emb),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: true,
            hint_type: HintType::Semantic,
        };

        let (injected, metrics) = manager
            .process_turn(&store, &mut query, None)
            .await
            .unwrap();

        assert!(!injected.formatted_text.is_empty());
        assert!(metrics.result_count > 0);
        assert!(!metrics.abstention_triggered);
    }

    #[tokio::test]
    async fn test_process_turn_abstention() {
        let store = test_store();
        // Empty store: the new `manager.retrieve` no longer applies an
        // absolute cosine floor (see `test_retrieve_abstention` for the
        // rationale). With no episodes at all, no source can return a hit
        // and abstention must trigger.
        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "completely unrelated".to_string(),
            embedding: Some(orthogonal_embedding()),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: Some(0.99),
            abstention_enabled: true,
            hint_type: HintType::Semantic,
        };

        let (injected, metrics) = manager
            .process_turn(&store, &mut query, None)
            .await
            .unwrap();

        assert!(metrics.abstention_triggered);
        assert_eq!(injected.memory_count, 0);
        assert!(injected.formatted_text.is_empty());
    }

    #[tokio::test]
    async fn test_retrieve_identity_searches_all_labels() {
        let store = test_store();
        let emb = test_embedding();
        // G10 (§6.6): Identity hint must search ALL 4 labels, so a Knowledge
        // layer node must be reachable even when hint_type == Identity.
        store_knowledge(&store, "user", "lives_in", "Shanghai", &emb);

        let manager = MemoryManager::new(MemoryManagerConfig::default());
        let mut query = MemoryQuery {
            query_text: "user lives in Shanghai".to_string(),
            embedding: Some(emb),
            filters: Default::default(),
            limit: 5,
            expand_hops: 0,
            min_cosine: None,
            abstention_enabled: false,
            hint_type: HintType::Identity,
        };

        let result = manager
            .retrieve(&store as &dyn MemoryProvider, &mut query, None)
            .await
            .unwrap();
        assert!(
            !result.memories.is_empty(),
            "Identity hint should search all labels and hit Knowledge nodes"
        );
        assert!(
            result.memories.iter().any(|m| m.label == labels::KNOWLEDGE),
            "expected at least one Knowledge node, got: {:?}",
            result
                .memories
                .iter()
                .map(|m| m.label.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_extract_node_content_procedural() {
        let store = test_store();

        // Store a procedural node.
        let node = acowork_memory::types::ProceduralNode {
            id: None,
            name: "concise_summary".to_string(),
            trigger_condition: "user asks for summary".to_string(),
            action_pattern: "reply in 3 sentences max".to_string(),
            success_count: 5,
            fail_count: 1,
            confidence: 0.9,
            activation_count: 3,
            source_skill: None,
            learned_from: "user_feedback".to_string(),
            source_episode_ids: Vec::new(),
            promotion_metadata: None,
            embedding: test_embedding(),
            status: acowork_memory::types::NodeStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
        };
        let provider: &dyn MemoryProvider = &store;
        let id = provider.store_procedural(&node).unwrap();

        // extract_node_content should format it as "当 X 时，优先 Y".
        let content = provider.get_node_content(id).unwrap().unwrap_or_default();
        assert!(
            content.starts_with("当"),
            "Procedural content should start with '当', got: {}",
            content
        );
        assert!(
            content.contains("优先"),
            "Procedural content should contain '优先', got: {}",
            content
        );
        assert!(
            content.contains("user asks for summary"),
            "Should contain trigger_condition"
        );
        assert!(
            content.contains("reply in 3 sentences max"),
            "Should contain action_pattern"
        );
    }
}
