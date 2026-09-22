//! Round-trip and retrieval tests for the SQLite storage backend.
//!
//! The load-bearing ones are the `*_roundtrip_full` tests plus
//! [`props_keys_are_complete_per_type`]: together they guarantee every field of
//! every memory node survives a write/read cycle, so the grafeo → SQLite
//! migration cannot silently drop data.

use super::*;
use acowork_memory::quality::MemoryQualityConfig;
use acowork_memory::types::{EpisodicDecayConfig, MemoryQuery};
use acowork_memory::{KnowledgeSubType, MemoryProvider, PrivacyLevel, PromotionMetadata};
use chrono::TimeZone;
use std::collections::HashMap;

const DIM: usize = 4;

fn store() -> SqliteStore {
    SqliteStore::open_in_memory(DIM).expect("open in-memory store")
}

fn t(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn emb(value: f32) -> Vec<f32> {
    vec![value; DIM]
}

fn episode(content: &str) -> Episode {
    Episode {
        session_id: "s1".to_string(),
        turn_index: 0,
        role: "user".to_string(),
        content: content.to_string(),
        embedding: None,
        timestamp: t(1_700_000_000),
        consolidated: false,
        metadata: HashMap::new(),
        importance: 0.5,
        knowledge_subtype: None,
    }
}

fn knowledge(subject: &str, predicate: &str) -> KnowledgeNode {
    KnowledgeNode {
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: "Beijing".to_string(),
        sub_type: KnowledgeSubType::Fact,
        confidence: 0.9,
        source_episode_id: None,
        source_episode_ids: Vec::new(),
        promotion_metadata: None,
        embedding: None,
        status: NodeStatus::Active,
        created_at: t(1_700_000_000),
        updated_at: t(1_700_000_000),
        metadata: HashMap::new(),
        privacy: PrivacyLevel::Personal,
        importance: 0.5,
    }
}

fn procedural() -> ProceduralNode {
    ProceduralNode {
        id: None,
        name: "concise_output".to_string(),
        trigger_condition: "user asks for summary".to_string(),
        action_pattern: "reply in three sentences".to_string(),
        success_count: 0,
        fail_count: 0,
        confidence: 0.5,
        activation_count: 0,
        source_skill: None,
        learned_from: "unknown".to_string(),
        embedding: Vec::new(),
        status: NodeStatus::Active,
        created_at: t(1_700_000_000),
        updated_at: t(1_700_000_000),
        source_episode_ids: Vec::new(),
        promotion_metadata: None,
        metadata: HashMap::new(),
    }
}

fn autobiographical() -> AutobiographicalNode {
    AutobiographicalNode {
        id: None,
        category: AutobioCategory::Identity,
        key: "language".to_string(),
        value: "zh-CN".to_string(),
        confidence: 0.9,
        source_episode_id: None,
        embedding: None,
        status: NodeStatus::Active,
        created_at: t(1_700_000_000),
        updated_at: t(1_700_000_000),
        source_episode_ids: Vec::new(),
        promotion_metadata: None,
        source: "user_statement".to_string(),
        metadata: HashMap::new(),
    }
}

fn promotion_metadata() -> PromotionMetadata {
    PromotionMetadata {
        promoted_at: t(1_700_000_500),
        promoted_by: "episodic_distiller".to_string(),
        evidence_episode_ids: vec![10, 11, 12],
        evidence_span_days: 3,
        llm_judge_confidence: 0.8,
        llm_judge_reasoning: "repeated across three episodes".to_string(),
    }
}

/// Compare two nodes by their full JSON, ignoring the store-assigned `id`.
fn assert_same_node<T: serde::Serialize>(original: &T, restored: &T) {
    let mut a = serde_json::to_value(original).unwrap();
    let mut b = serde_json::to_value(restored).unwrap();
    for value in [&mut a, &mut b] {
        if let Some(map) = value.as_object_mut() {
            map.insert("id".to_string(), serde_json::Value::Null);
        }
    }
    assert_eq!(a, b);
}

fn props_keys(store: &SqliteStore, id: u64, label: &str) -> Vec<String> {
    let props = store.load_props(id, label).unwrap().expect("row exists");
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&props).unwrap();
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

// ── Round-trips ──────────────────────────────────────────────────────────

#[test]
fn episode_roundtrip_full() {
    let store = store();
    let mut original = episode("Hello, 世界");
    original.turn_index = 7;
    original.role = "assistant".to_string();
    original.embedding = Some(emb(0.25));
    original.consolidated = true;
    original.importance = 0.75;
    original.knowledge_subtype = Some(KnowledgeSubType::Preference);
    original.metadata.insert(
        "topic".to_string(),
        serde_json::Value::String("greeting".to_string()),
    );
    original.metadata.insert(
        "sentiment".to_string(),
        serde_json::json!({ "score": 0.9, "label": "positive" }),
    );

    let id = store.store_episode(&original).unwrap();
    let restored = store.get_episode(id).unwrap().unwrap();

    assert_same_node(&original, &restored);
    assert_eq!(restored.timestamp, original.timestamp);
}

#[test]
fn episode_roundtrip_minimal() {
    let store = store();
    let original = episode("no embedding, no metadata");

    let id = store.store_episode(&original).unwrap();
    let restored = store.get_episode(id).unwrap().unwrap();

    assert_same_node(&original, &restored);
    assert!(restored.embedding.is_none());
    assert_eq!(store.count_nodes_with_embedding().unwrap(), 0);
}

#[test]
fn knowledge_roundtrip_full() {
    let store = store();
    let mut original = knowledge("user", "prefers");
    original.object = "concise output".to_string();
    original.sub_type = KnowledgeSubType::Preference;
    original.confidence = 0.85;
    original.source_episode_id = Some(42);
    original.source_episode_ids = vec![42, 43];
    original.promotion_metadata = Some(promotion_metadata());
    original.embedding = Some(emb(0.5));
    original.status = NodeStatus::Dormant;
    original.privacy = PrivacyLevel::Sensitive;
    original.importance = 0.9;
    original
        .metadata
        .insert("source".to_string(), serde_json::json!("manifest"));

    let id = store.store_knowledge(&original).unwrap();
    let restored = store.get_knowledge(id).unwrap().unwrap();

    assert_same_node(&original, &restored);
}

#[test]
fn procedural_roundtrip_full() {
    let store = store();
    let mut original = procedural();
    original.success_count = 5;
    original.fail_count = 2;
    original.confidence = 0.8;
    original.activation_count = 9;
    original.source_skill = Some("summarize".to_string());
    original.learned_from = "user_feedback".to_string();
    original.embedding = emb(0.1);
    original.status = NodeStatus::Dormant;
    original.source_episode_ids = vec![1, 2];
    original.promotion_metadata = Some(promotion_metadata());

    let id = store.store_procedural(&original).unwrap();
    let restored = store.get_procedural(id).unwrap().unwrap();

    assert_same_node(&original, &restored);
}

#[test]
fn autobiographical_roundtrip_full() {
    let store = store();
    let mut original = autobiographical();
    original.category = AutobioCategory::Preference;
    original.confidence = 0.7;
    original.source_episode_id = Some(7);
    original.source_episode_ids = vec![7];
    original.promotion_metadata = Some(promotion_metadata());
    original.embedding = Some(emb(0.3));
    original.source = "important_event".to_string();

    let id = store.store_autobiographical(&original).unwrap();
    let restored = store.get_autobiographical(id).unwrap().unwrap();

    assert_same_node(&original, &restored);
}

#[test]
fn timestamps_preserve_micros() {
    let store = store();
    let mut original = episode("micros");
    original.timestamp = Utc.timestamp_micros(1_700_000_000_123_456).unwrap();

    let id = store.store_episode(&original).unwrap();
    let restored = store.get_episode(id).unwrap().unwrap();

    assert_eq!(
        restored.timestamp.timestamp_micros(),
        original.timestamp.timestamp_micros()
    );
}

// ── Field completeness ───────────────────────────────────────────────────

#[test]
fn props_keys_are_complete_per_type() {
    let store = store();

    // `id` and `embedding` are the only projected-out fields; everything else
    // must be present or the migration would silently drop it.
    let ep_id = store.store_episode(&episode("x")).unwrap();
    assert_eq!(
        props_keys(&store, ep_id, labels::EPISODIC),
        [
            "consolidated",
            "content",
            "importance",
            "knowledge_subtype",
            "metadata",
            "role",
            "session_id",
            "timestamp",
            "turn_index",
        ]
    );

    let kn_id = store.store_knowledge(&knowledge("user", "likes")).unwrap();
    assert_eq!(
        props_keys(&store, kn_id, labels::KNOWLEDGE),
        [
            "confidence",
            "created_at",
            "importance",
            "metadata",
            "object",
            "predicate",
            "privacy",
            "promotion_metadata",
            "source_episode_id",
            "source_episode_ids",
            "status",
            "sub_type",
            "subject",
            "updated_at",
        ]
    );

    let pr_id = store.store_procedural(&procedural()).unwrap();
    assert_eq!(
        props_keys(&store, pr_id, labels::PROCEDURAL),
        [
            "action_pattern",
            "activation_count",
            "confidence",
            "created_at",
            "fail_count",
            "learned_from",
            "metadata",
            "name",
            "promotion_metadata",
            "source_episode_ids",
            "source_skill",
            "status",
            "success_count",
            "trigger_condition",
            "updated_at",
        ]
    );

    let au_id = store.store_autobiographical(&autobiographical()).unwrap();
    assert_eq!(
        props_keys(&store, au_id, labels::AUTOBIOGRAPHICAL),
        [
            "category",
            "confidence",
            "created_at",
            "key",
            "metadata",
            "promotion_metadata",
            "source",
            "source_episode_id",
            "source_episode_ids",
            "status",
            "updated_at",
            "value",
        ]
    );
}

// ── Persistence / lifecycle ──────────────────────────────────────────────

#[test]
fn persistence_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");

    let id = {
        let store = SqliteStore::open(&path, DIM).unwrap();
        let mut node = knowledge("user", "lives_in");
        node.embedding = Some(emb(0.5));
        store.store_knowledge(&node).unwrap()
    };

    let store = SqliteStore::open(&path, DIM).unwrap();
    let restored = store.get_knowledge(id).unwrap().unwrap();
    assert_eq!(restored.object, "Beijing");
    assert_eq!(restored.embedding.as_ref().map(Vec::len), Some(DIM));
}

#[test]
fn delete_node_removes_vector_and_fts() {
    let store = store();
    let mut ep = episode("deletable rust content");
    ep.embedding = Some(emb(1.0));
    let id = store.store_episode(&ep).unwrap();

    assert!(store.delete_node(id).unwrap());
    assert!(!store.delete_node(id).unwrap());
    assert_eq!(store.node_count().unwrap(), 0);
    assert_eq!(store.count_nodes_with_embedding().unwrap(), 0);
    assert!(
        store
            .text_search(labels::EPISODIC, "deletable", 10)
            .unwrap()
            .is_empty()
    );
}

// ── Episodic queries ─────────────────────────────────────────────────────

#[test]
fn episodes_by_session_and_recent_first() {
    let store = store();
    for (i, session) in ["a", "b", "a"].iter().enumerate() {
        let mut ep = episode(&format!("m{i}"));
        ep.session_id = (*session).to_string();
        ep.timestamp = t(1_700_000_000 + i as i64);
        store.store_episode(&ep).unwrap();
    }

    let sessions = store.search_episodes_by_session("a", 10).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].content, "m2"); // newest first

    let all = store.list_all_episodes(10).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn mark_and_cleanup_consolidated() {
    let store = store();
    let old = store.store_episode(&episode("old")).unwrap();
    let recent = store.store_episode(&episode("recent")).unwrap();
    store.mark_episode_consolidated(old).unwrap();
    assert_eq!(store.count_unconsolidated_episodes().unwrap(), 1);

    // Backdate the consolidated episode past the retention window.
    {
        let conn = store.lock();
        conn.execute(
            "UPDATE nodes SET created_at = ?1 WHERE id = ?2",
            params![ts_text(t(1_600_000_000)), old as i64],
        )
        .unwrap();
    }

    assert_eq!(store.cleanup_old_episodes(30).unwrap(), 1);
    assert!(store.get_episode(old).unwrap().is_none());
    assert!(store.get_episode(recent).unwrap().is_some());
    let unconsolidated = store.get_unconsolidated_episodes(10).unwrap();
    assert_eq!(unconsolidated.len(), 1);
    assert_eq!(unconsolidated[0].content, "recent");
}

// ── Text search ──────────────────────────────────────────────────────────

#[test]
fn text_search_matches_chinese_substring() {
    let store = store();
    store
        .store_episode(&episode("用户喜欢简洁的输出风格"))
        .unwrap();
    store.store_episode(&episode("今天天气不错")).unwrap();

    let hits = store.search_episodes_by_keyword("简洁的输出", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].0.content.contains("简洁"));
}

#[test]
fn text_search_short_query_falls_back_to_like() {
    let store = store();
    store.store_episode(&episode("用户喜欢简洁的输出")).unwrap();

    // Two characters — below the trigram minimum, so the LIKE path must fire.
    let hits = store.search_episodes_by_keyword("简洁", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, 0.0);
}

#[test]
fn text_search_is_label_isolated() {
    let store = store();
    store
        .store_episode(&episode("shared keyword alpha"))
        .unwrap();
    let mut node = knowledge("user", "alpha");
    node.object = "shared keyword alpha".to_string();
    store.store_knowledge(&node).unwrap();

    assert_eq!(
        store
            .text_search(labels::EPISODIC, "keyword", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .text_search(labels::KNOWLEDGE, "keyword", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn text_search_like_escapes_wildcards() {
    let store = store();
    store.store_episode(&episode("100% done")).unwrap();
    store.store_episode(&episode("unrelated")).unwrap();

    // A bare '%' would match everything without escaping.
    let hits = store.search_episodes_by_keyword("0%", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].0.content.contains('%'));
}

// ── Vector search ────────────────────────────────────────────────────────

#[test]
fn vector_search_ranks_by_cosine() {
    let store = store();
    let mut near = episode("near");
    near.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    store.store_episode(&near).unwrap();

    let mut far = episode("far");
    far.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
    store.store_episode(&far).unwrap();

    let hits = store
        .search_episodes_by_embedding(&[1.0, 0.0, 0.0, 0.0], 10)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0.content, "near");
    assert!((hits[0].1 - 1.0).abs() < 1e-6);
    assert!(hits[1].1.abs() < 1e-6);
}

#[test]
fn vector_search_skips_dim_mismatch() {
    let store = store();
    let mut ok = episode("right dim");
    ok.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    store.store_episode(&ok).unwrap();

    let mut other = episode("wrong dim");
    other.embedding = Some(vec![1.0, 0.0, 0.0]);
    store.store_episode(&other).unwrap();

    let hits = store
        .search_episodes_by_embedding(&[1.0, 0.0, 0.0, 0.0], 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.content, "right dim");
}

// ── Semantic helpers ─────────────────────────────────────────────────────

#[test]
fn store_knowledge_dedups_same_subject_predicate() {
    let store = store();
    let mut first = knowledge("user", "lives_in");
    first.object = "Beijing".to_string();
    let id = store.store_knowledge(&first).unwrap();

    let mut second = knowledge("user", "lives_in");
    second.object = "Shanghai".to_string();
    let second_id = store.store_knowledge(&second).unwrap();

    assert_eq!(id, second_id);
    assert_eq!(store.node_count_by_label(labels::KNOWLEDGE).unwrap(), 1);
    let stored = store.get_knowledge(id).unwrap().unwrap();
    assert_eq!(stored.object, "Shanghai");
}

#[test]
fn find_procedural_by_trigger_is_case_insensitive() {
    let store = store();
    let mut node = procedural();
    node.trigger_condition = "User asks for a Summary".to_string();
    store.store_procedural(&node).unwrap();

    let hits = store.find_procedural_by_trigger("summary", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "concise_output");
}

#[test]
fn find_autobiographical_by_key_and_category() {
    let store = store();
    store.store_autobiographical(&autobiographical()).unwrap();

    let by_key = store.find_autobiographical_by_key("language").unwrap();
    assert_eq!(by_key.unwrap().value, "zh-CN");

    let by_category = store
        .find_autobiographical_by_category(AutobioCategory::Identity)
        .unwrap();
    assert_eq!(by_category.len(), 1);
    assert!(
        store
            .find_autobiographical_by_category(AutobioCategory::History)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn store_autobiographical_forces_active() {
    let store = store();
    let mut node = autobiographical();
    node.status = NodeStatus::Dormant;

    let id = store.store_autobiographical(&node).unwrap();
    let restored = store.get_autobiographical(id).unwrap().unwrap();
    assert_eq!(restored.status, NodeStatus::Active);
}

// ── Hybrid retrieval fusion (ADR-082 D5) ─────────────────────────────────

/// ADR-082 §1.4 regression: a BM25-only hit must survive the cosine floor.
///
/// The engine-era fusion applied `min_score` in the *fused* score domain, which
/// on the vector-only path meant `cos >= 1` — every Chinese `memory_recall`
/// query returned nothing. Here the floor gates the vector source only, so the
/// lexical hit is kept even though its embedding is orthogonal to the query.
#[test]
fn hybrid_keeps_lexical_hit_whose_embedding_is_far() {
    let store = store();

    let mut lexical_only = episode("北京今天下雨吗");
    lexical_only.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
    let lexical_id = store.store_episode(&lexical_only).unwrap();

    let mut vector_only = episode("completely unrelated wording");
    vector_only.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    let vector_id = store.store_episode(&vector_only).unwrap();

    let hits = store
        .hybrid_search_full(
            labels::EPISODIC,
            "北京今天下雨吗",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            1.0,
            1.0,
            Some(0.3),
        )
        .unwrap();

    let ids: Vec<u64> = hits.iter().map(|(id, _)| *id).collect();
    assert!(
        ids.contains(&lexical_id),
        "lexical hit dropped by the cosine floor: {ids:?}"
    );
    assert!(ids.contains(&vector_id));

    // Both are single-source rank-1 hits, so the id tie-break decides; what
    // matters is that neither was filtered out.
    assert_eq!(hits.len(), 2);
    // Scores live in the normalized-cosine domain, not the RRF domain
    // (`~0.016`), so downstream `min_score` / abstention thresholds still mean
    // what they used to.
    for (id, score) in &hits {
        assert!((0.0..=1.0).contains(score), "score out of range: {score}");
        if *id == vector_id {
            assert!((score - 1.0).abs() < 1e-9);
        } else {
            // Cosine was not recovered (gated out of the vector source), so the
            // hit is scored as orthogonal: (1 + 0) / 2.
            assert!((score - 0.5).abs() < 1e-9);
        }
    }
}

/// A hit found by *both* sources must outrank a hit found by only one.
#[test]
fn hybrid_ranks_dual_source_hit_first() {
    let store = store();

    let mut both = episode("project codename is blue whale");
    both.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    let both_id = store.store_episode(&both).unwrap();

    let mut vector_only = episode("nothing lexically shared here");
    vector_only.embedding = Some(vec![0.9, 0.1, 0.0, 0.0]);
    let vector_id = store.store_episode(&vector_only).unwrap();

    let hits = store
        .hybrid_search_full(
            labels::EPISODIC,
            "project codename is blue whale",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            1.0,
            1.0,
            Some(0.3),
        )
        .unwrap();

    assert_eq!(
        hits[0].0, both_id,
        "dual-source hit lost to a vector-only hit"
    );
    assert!(hits.iter().any(|(id, _)| *id == vector_id));
}

/// The vector source still honours the absolute cosine floor; the floor is not
/// applied to the text source.
#[test]
fn hybrid_gates_vector_source_by_min_cosine() {
    let store = store();

    let mut far = episode("qxz wording that the query cannot lexically match");
    far.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
    let far_id = store.store_episode(&far).unwrap();

    let query = &[1.0, 0.0, 0.0, 0.0];

    let gated = store
        .hybrid_search_full(labels::EPISODIC, "zzz", query, 10, 1.0, 1.0, Some(0.3))
        .unwrap();
    assert!(gated.is_empty(), "cos 0 survived a 0.3 floor: {gated:?}");

    let ungated = store
        .hybrid_search_full(labels::EPISODIC, "zzz", query, 10, 1.0, 1.0, Some(-1.0))
        .unwrap();
    assert_eq!(ungated.len(), 1);
    assert_eq!(ungated[0].0, far_id);
    assert!((ungated[0].1 - 0.5).abs() < 1e-9, "cos 0 -> (1+0)/2");
}

/// Even with an impossibly high floor, a lexical hit is retained: a BM25 match
/// is independent evidence, and a weak/absent embedding must not kill it.
#[test]
fn hybrid_text_source_ignores_min_cosine() {
    let store = store();

    let mut node = episode("独一无二的中文词组");
    node.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
    let id = store.store_episode(&node).unwrap();

    let hits = store
        .hybrid_search_full(
            labels::EPISODIC,
            "独一无二的中文词组",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            1.0,
            1.0,
            Some(0.99),
        )
        .unwrap();

    assert_eq!(hits.len(), 1, "lexical hit gated away: {hits:?}");
    assert_eq!(hits[0].0, id);
}

/// Fusion must not leak across labels.
#[test]
fn hybrid_search_is_label_isolated() {
    let store = store();
    let mut lexical = episode("shared keyword alpha");
    lexical.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    store.store_episode(&lexical).unwrap();

    let hits = store
        .hybrid_search_full(
            labels::KNOWLEDGE,
            "shared keyword alpha",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            1.0,
            1.0,
            Some(0.3),
        )
        .unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

/// `k` caps the fused output, and an empty query still allows vector-only hits.
#[test]
fn hybrid_respects_limit_and_handles_empty_query() {
    let store = store();
    for i in 0..5 {
        let mut node = episode(&format!("filler number {i}"));
        node.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
        store.store_episode(&node).unwrap();
    }

    let hits = store
        .hybrid_search_full(
            labels::EPISODIC,
            "filler",
            &[1.0, 0.0, 0.0, 0.0],
            2,
            1.0,
            1.0,
            None,
        )
        .unwrap();
    assert_eq!(hits.len(), 2);

    let vector_only = store
        .hybrid_search_full(
            labels::EPISODIC,
            "   ",
            &[1.0, 0.0, 0.0, 0.0],
            10,
            1.0,
            1.0,
            None,
        )
        .unwrap();
    assert_eq!(vector_only.len(), 5);
}

/// `hybrid_search_filtered` is the equal-weight, ungated entry point.
#[test]
fn hybrid_search_filtered_delegates() {
    let store = store();
    let mut node = episode("delegation works");
    node.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    let id = store.store_episode(&node).unwrap();

    let hits = store
        .hybrid_search_filtered(
            labels::EPISODIC,
            "content",
            "embedding",
            "delegation works",
            &[1.0, 0.0, 0.0, 0.0],
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, id);
}

/// Equal-weight RRF makes single-source rank-1 hits tie exactly, so the result
/// order must not depend on `HashMap` iteration order (randomized per process).
#[test]
fn hybrid_order_is_deterministic_across_calls() {
    let store = store();
    for i in 0..5 {
        let mut node = episode(&format!("filler number {i}"));
        node.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
        store.store_episode(&node).unwrap();
    }

    let expected = store
        .hybrid_search_full(
            labels::EPISODIC,
            "filler",
            &[1.0, 0.0, 0.0, 0.0],
            5,
            1.0,
            1.0,
            None,
        )
        .unwrap();
    assert_eq!(expected.len(), 5);
    assert_eq!(
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
        "ties must fall back to node id"
    );

    for _ in 0..20 {
        let again = store
            .hybrid_search_full(
                labels::EPISODIC,
                "filler",
                &[1.0, 0.0, 0.0, 0.0],
                5,
                1.0,
                1.0,
                None,
            )
            .unwrap();
        assert_eq!(expected, again);
    }
}

// ── MemoryProvider integration (ADR-082 D1) ──────────────────────────────
//
// These exercise the trait surface the Runtime actually holds, so they are the
// tests that prove the four feature chains (memory_store / memory_recall /
// distillation / forgetting) survive the backend swap.

/// Content rendering must match the grafeo provider byte-for-byte, because
/// prompts and the `memory_recall` tool consume it directly.
#[test]
fn provider_renders_content_per_node_type() {
    let store = store();

    let episode_id = store.store_episode(&episode("用户问了天气")).unwrap();
    let knowledge_id = store
        .store_knowledge(&knowledge("user", "lives in"))
        .unwrap();

    let mut proc = procedural();
    proc.trigger_condition = "用户要求详细解释".to_string();
    proc.action_pattern = "先给结论".to_string();
    let proc_id = store.store_procedural(&proc).unwrap();

    let mut auto = autobiographical();
    auto.category = AutobioCategory::Identity;
    auto.key = "name".to_string();
    auto.value = "大鱼".to_string();
    let auto_id = store.store_autobiographical(&auto).unwrap();

    assert_eq!(
        store.get_node_content(episode_id).unwrap().as_deref(),
        Some("用户问了天气")
    );
    assert_eq!(
        store.get_node_content(knowledge_id).unwrap().as_deref(),
        Some("user lives in Beijing")
    );
    assert_eq!(
        store.get_node_content(proc_id).unwrap().as_deref(),
        Some("当 用户要求详细解释 时，优先 先给结论")
    );
    assert_eq!(
        store.get_node_content(auto_id).unwrap().as_deref(),
        Some("Identity: name: 大鱼")
    );
    assert_eq!(store.get_node_content(9_999).unwrap(), None);
}

/// `MemoryManager` filters the current session out of retrieval results through
/// this accessor; it must answer for episodic nodes and stay `None` elsewhere.
#[test]
fn provider_exposes_session_id_and_status() {
    let store = store();
    let mut ep = episode("session scoped");
    ep.session_id = "sess-7".to_string();
    let ep_id = store.store_episode(&ep).unwrap();
    let kn_id = store.store_knowledge(&knowledge("user", "likes")).unwrap();

    assert_eq!(
        store.get_node_session_id(ep_id).unwrap().as_deref(),
        Some("sess-7")
    );
    assert_eq!(store.get_node_session_id(kn_id).unwrap(), None);
    assert_eq!(
        store.get_node_status(ep_id).unwrap(),
        Some(NodeStatus::Active)
    );
    assert_eq!(store.get_node_status(9_999).unwrap(), None);
}

/// `memory_recall --since/--until` filters on `created_at`. Episodes carry
/// their timestamp in that column instead of a `created_at` property, so the
/// accessor must answer for them too (the grafeo path returned `None`, which
/// silently disabled the filter).
#[test]
fn provider_created_at_covers_episodes() {
    let store = store();
    let mut old = episode("old news");
    old.timestamp = t(1_600_000_000);
    let old_id = store.store_episode(&old).unwrap();

    assert_eq!(
        store.get_node_created_at(old_id).unwrap(),
        Some(t(1_600_000_000))
    );

    let query = MemoryQuery {
        filters: acowork_memory::types::MemoryFilters {
            time_range: Some((t(1_700_000_000), t(1_800_000_000))),
            ..Default::default()
        },
        ..MemoryQuery::new("news")
    };
    // The provider itself does not filter; the accessor is what makes the
    // manager-side filter possible.
    let created = store.get_node_created_at(old_id).unwrap().unwrap();
    let (since, until) = query.filters.time_range.unwrap();
    assert!(created < since && created <= until);
}

/// The `status` column is authoritative: forgetting flips it without touching
/// `props`, so reading `props.status` would resurrect decayed episodes.
#[test]
fn provider_status_reads_column_not_props() {
    let store = store();
    let mut ep = episode("will decay");
    ep.timestamp = t(1_700_000_000);
    let id = store.store_episode(&ep).unwrap();

    store.transition_to_dormant(id).unwrap();
    assert_eq!(
        store.get_node_status(id).unwrap(),
        Some(NodeStatus::Dormant)
    );
}

/// A skipped episode is a sticky judge verdict: it must leave the distiller
/// backlog (both count and enumeration) while staying retrievable.
#[test]
fn provider_skip_tombstone_removes_from_distiller_backlog() {
    let store = store();
    let keep = store.store_episode(&episode("keep me")).unwrap();
    let skip = store.store_episode(&episode("skip me")).unwrap();
    assert_eq!(store.count_unconsolidated_episodes().unwrap(), 2);

    store
        .mark_episodes_skipped(&[skip], "cluster-1", "declined")
        .unwrap();

    assert_eq!(store.count_unconsolidated_episodes().unwrap(), 1);
    let backlog = store.get_episodes_by_subtype(None, 10).unwrap();
    assert_eq!(backlog.len(), 1);
    assert_eq!(backlog[0].0, keep);
    // Still retrievable — the content stays in the episodic layer.
    assert_eq!(
        store.get_node_content(skip).unwrap().as_deref(),
        Some("skip me")
    );
}

/// `get_episodes_by_subtype` honours the subtype filter and orders oldest
/// first so evidence accumulates deterministically across distiller runs.
#[test]
fn provider_episodes_by_subtype_filters_and_orders() {
    let store = store();
    let mut older = episode("older fact");
    older.timestamp = t(1_600_000_000);
    older.knowledge_subtype = Some(KnowledgeSubType::Fact);
    let older_id = store.store_episode(&older).unwrap();

    let mut newer = episode("newer preference");
    newer.timestamp = t(1_700_000_000);
    newer.knowledge_subtype = Some(KnowledgeSubType::Preference);
    let newer_id = store.store_episode(&newer).unwrap();

    let facts = store
        .get_episodes_by_subtype(Some(KnowledgeSubType::Fact), 10)
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].0, older_id);

    let all = store.get_episodes_by_subtype(None, 10).unwrap();
    assert_eq!(
        all.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![older_id, newer_id]
    );
}

/// Forgetting: Active → Dormant once retention drops below the threshold, but
/// only when the config is enabled.
#[test]
fn provider_decay_scan_dormants_old_episodes() {
    let store = store();
    let mut ancient = episode("ancient");
    // ~3.3 half-lives of age with the default 180-day half-life → retention
    // well under the 0.1 dormant threshold.
    ancient.timestamp = Utc::now() - TimeDelta::days(1200);
    let ancient_id = store.store_episode(&ancient).unwrap();

    // Note: the shared `episode()` fixture is pinned at 2023-11-14, which is
    // already decades past the threshold — "fresh" must be explicit.
    let mut fresh = episode("fresh");
    fresh.timestamp = Utc::now();
    let fresh_id = store.store_episode(&fresh).unwrap();

    let disabled = EpisodicDecayConfig::default();
    assert!(!disabled.enabled);
    let noop = store.run_episodic_decay_scan(&disabled).unwrap();
    assert_eq!(noop.to_dormant, 0);
    assert_eq!(
        store.get_node_status(ancient_id).unwrap(),
        Some(NodeStatus::Active)
    );

    let enabled = EpisodicDecayConfig {
        enabled: true,
        ..Default::default()
    };
    let result = store.run_episodic_decay_scan(&enabled).unwrap();
    assert_eq!(result.to_dormant, 1);
    assert_eq!(result.purged, 0);
    assert_eq!(
        store.get_node_status(ancient_id).unwrap(),
        Some(NodeStatus::Dormant)
    );
    assert_eq!(
        store.get_node_status(fresh_id).unwrap(),
        Some(NodeStatus::Active)
    );
}

/// The archive step must copy the node into `purge_log` before deleting it —
/// forgetting is recoverable, never destructive.
#[test]
fn provider_decay_archives_before_deleting() {
    let store = store();
    let mut ancient = episode("archived memory");
    ancient.timestamp = Utc::now() - TimeDelta::days(1200);
    let id = store.store_episode(&ancient).unwrap();

    let enabled = EpisodicDecayConfig {
        enabled: true,
        ..Default::default()
    };
    store.run_episodic_decay_scan(&enabled).unwrap();

    // Age the dormancy past the 90-day archive deadline.
    {
        let conn = store.lock();
        conn.execute(
            "UPDATE nodes SET props = json_set(props, '$.dormant_since', json_quote(?2)) WHERE id = ?1",
            params![id as i64, ts_text(Utc::now() - TimeDelta::days(120))],
        )
        .unwrap();
    }

    let result = store.run_episodic_decay_scan(&enabled).unwrap();
    assert_eq!(result.purged, 1);
    assert!(store.get_episode(id).unwrap().is_none());

    let conn = store.lock();
    let (content, reason): (String, String) = conn
        .query_row(
            "SELECT content, reason FROM purge_log WHERE node_id = ?1",
            params![id as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(content, "archived memory");
    assert!(reason.contains("dormant_days=120"), "{reason}");
}

/// `apply_quality_config` must actually reach the write path, not just be
/// recorded: raising the dedup threshold stops the (subject, predicate) merge.
///
/// The two stores are deliberate — a merge rewrites the stored embedding, so
/// probing both thresholds on one store would compare against a moved target.
#[test]
fn provider_quality_config_drives_dedup() {
    let similar = |store: &SqliteStore| {
        let mut first = knowledge("user", "lives in");
        first.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
        let first_id = store.store_knowledge(&first).unwrap();

        // cos 0.96 against the first embedding.
        let mut second = knowledge("user", "lives in");
        second.embedding = Some(vec![0.96, 0.28, 0.0, 0.0]);
        let second_id = store.store_knowledge(&second).unwrap();
        (first_id, second_id)
    };

    let lenient = store();
    let (a, b) = similar(&lenient);
    assert_eq!(a, b, "the 0.95 default must merge cos 0.96");

    let strict = store();
    strict
        .apply_quality_config(&MemoryQualityConfig {
            dedup: acowork_memory::quality::DedupQuality {
                knowledge_threshold: 0.99,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    let (a, b) = similar(&strict);
    assert_ne!(a, b, "a raised threshold must not merge cos 0.96");
    assert_eq!(strict.node_count_by_label(labels::KNOWLEDGE).unwrap(), 2);
}

/// Graph operations are documented no-ops (ADR-082 D4): no expansion, no
/// topology boost, no edge writes.
#[test]
fn provider_graph_ops_are_noops() {
    let store = store();
    let id = store.store_episode(&episode("seed")).unwrap();

    assert!(
        store
            .graph_expand_seeded(&[(id, 0.9)], "s")
            .unwrap()
            .is_empty()
    );
    store
        .create_memory_edge(id, id, "REFERENCES", vec![])
        .unwrap();

    let mut scores = vec![(id, 0.5)];
    store.apply_pagerank_boost(&mut scores, 0.1).unwrap();
    assert_eq!(scores, vec![(id, 0.5)]);

    assert!(!store.should_trigger_confirmation().unwrap());
    assert_eq!(store.generate_confirmation_hint().unwrap(), None);
}

/// Health and stats must reflect the real store, and stats must not count
/// episodic nodes twice (they are reported separately from semantic nodes).
#[test]
fn provider_stats_and_health() {
    let store = store();
    assert!(store.health_check().unwrap().is_healthy);
    assert!(store.health_check().unwrap().latency_ms < 5_000);

    store.store_episode(&episode("one")).unwrap();
    let mut kn = knowledge("user", "likes");
    kn.status = NodeStatus::Dormant;
    store.store_knowledge(&kn).unwrap();

    let stats = store.stats().unwrap();
    assert_eq!(stats.episode_count, 1);
    assert_eq!(stats.node_count, 1);
    assert_eq!(stats.active_node_count, 0);
    assert_eq!(stats.dormant_node_count, 1);
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.index_count, 5);
    assert!(stats.storage_size_bytes > 0);
}

/// `collaboration_span` feeds the 30-day Relationship generation; it must be
/// `None` on an empty store and report the earliest episode timestamp once
/// there is history.
#[test]
fn provider_collaboration_span() {
    let store = store();
    assert!(store.collaboration_span().unwrap().is_none());

    let mut older = episode("first contact");
    older.timestamp = t(1_600_000_000);
    store.store_episode(&older).unwrap();
    store.store_episode(&episode("later")).unwrap();

    let span = store.collaboration_span().unwrap().unwrap();
    assert_eq!(span.earliest_episode_at, t(1_600_000_000));
    assert_eq!(span.episode_count, 2);
}

/// `search_episodes` / `hybrid_search` are the `MemoryQuery` entry points; they
/// must return hits with real content and node ids.
#[test]
fn provider_search_episodes_and_hybrid_search() {
    let store = store();
    let mut ep = episode("北京今天下雨吗");
    ep.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    let id = store.store_episode(&ep).unwrap();
    store.store_knowledge(&knowledge("user", "likes")).unwrap();

    let mut query = MemoryQuery::deep_recall("北京今天下雨吗".to_string(), None);
    query.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);

    let episodes = store.search_episodes(&query).unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].node_id, id);
    assert_eq!(episodes[0].content, "北京今天下雨吗");

    let all = store.hybrid_search(&query).unwrap();
    assert!(all.iter().any(|hit| hit.node_id == id));
    assert!(all.iter().all(|hit| !hit.content.is_empty()));
}
