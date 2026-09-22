//! Round-trip and retrieval tests for the SQLite storage backend.
//!
//! The load-bearing ones are the `*_roundtrip_full` tests plus
//! [`props_keys_are_complete_per_type`]: together they guarantee every field of
//! every memory node survives a write/read cycle, so the grafeo → SQLite
//! migration cannot silently drop data.

use super::*;
use acowork_memory::{KnowledgeSubType, PrivacyLevel, PromotionMetadata};
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
