#![cfg(feature = "grafeo-backend")]
//! Field-completeness parity for the grafeo → SQLite memory import
//! (ADR-082 §4 step 2).
//!
//! The migration's whole risk is a field that exists on one side and not the
//! other, so this builds a source store through grafeo's *real* write path with
//! every field of every layer set to a distinctive value, imports it, and checks
//! each field on the way out. `report.skipped == 0` is the coarse signal (a type
//! that cannot be converted is skipped, not dropped quietly); the per-field
//! asserts are the ones that make it trustworthy.
//!
//! Also pinned here: embeddings survive, `status` does not come back Active,
//! the consolidated flag survives (or the distiller re-processes migrated
//! history and pays for it), the FTS blob is derived so recall works, episode
//! references are remapped, and a second import cannot run.

use std::collections::HashMap;
use std::sync::Arc;

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::GrafeoConfig;
use acowork_memory::types::{EpisodicDecayConfig, MemoryQuery};
use acowork_memory::{
    AutobioCategory, AutobiographicalNode, Episode, KnowledgeNode, KnowledgeSubType,
    MemoryProvider, NodeStatus, PrivacyLevel, ProceduralNode,
};
use acowork_runtime::memory::grafeo_import::{SOURCE_FILE, import_grafeo_memory};
use acowork_sqlite::SqliteStore;
use chrono::{DateTime, TimeZone, Utc};

const DIM: usize = 8;

fn t(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

fn emb(seed: f32) -> Vec<f32> {
    (0..DIM).map(|k| seed + k as f32 / 100.0).collect()
}

fn episode(content: &str, ts: DateTime<Utc>) -> Episode {
    Episode {
        session_id: "sess-9".to_string(),
        turn_index: 7,
        role: "assistant".to_string(),
        content: content.to_string(),
        embedding: Some(emb(0.5)),
        timestamp: ts,
        consolidated: true,
        metadata: HashMap::from([("topic".to_string(), serde_json::json!("migration"))]),
        importance: 0.75,
        knowledge_subtype: Some(KnowledgeSubType::Preference),
    }
}

/// Write a source store with one fully-populated node per layer. Returns the
/// grafeo id of the episode the knowledge node points at.
fn build_source(dir: &std::path::Path) {
    let source = dir.join(SOURCE_FILE);
    let store = Arc::new(
        GrafeoStore::open(&GrafeoConfig {
            db_path: source,
            embedding_dim: DIM,
        })
        .expect("open grafeo source"),
    );
    let provider: Arc<dyn MemoryProvider> = store.clone();
    // Written first, on purpose: the episode must not be the store's node #1, or
    // SQLite (which imports episodes first) would number it identically and the
    // reference-remap assertions below would pass without the map doing work.
    let decoy = KnowledgeNode {
        subject: "decoy".to_string(),
        predicate: "is".to_string(),
        object: "first".to_string(),
        sub_type: KnowledgeSubType::Fact,
        confidence: 0.5,
        source_episode_id: None,
        source_episode_ids: Vec::new(),
        promotion_metadata: None,
        embedding: Some(emb(0.15)),
        status: NodeStatus::Active,
        created_at: t(1_500_000_000),
        updated_at: t(1_500_000_000),
        metadata: HashMap::new(),
        privacy: PrivacyLevel::Personal,
        importance: 0.5,
    };
    provider
        .store_knowledge(&decoy)
        .expect("write ordering decoy");

    let episode_id = provider
        .store_episode(&episode(
            "migrated 中文 episode about the 网关 gateway",
            t(1_700_000_000),
        ))
        .expect("write episode");

    let knowledge = KnowledgeNode {
        subject: "gateway".to_string(),
        predicate: "runs_on".to_string(),
        object: "staging".to_string(),
        sub_type: KnowledgeSubType::Relation,
        confidence: 0.82,
        source_episode_id: Some(episode_id),
        source_episode_ids: vec![episode_id],
        promotion_metadata: None,
        embedding: Some(emb(0.25)),
        // A node the old store had already decayed: importing it as Active
        // would silently undo forgetting.
        status: NodeStatus::Dormant,
        created_at: t(1_600_000_000),
        updated_at: t(1_600_000_100),
        metadata: HashMap::from([("origin".to_string(), serde_json::json!("distiller"))]),
        privacy: PrivacyLevel::Sensitive,
        importance: 0.42,
    };
    provider.store_knowledge(&knowledge).expect("write knowledge");

    let procedural = ProceduralNode {
        id: None,
        name: "restart-gateway".to_string(),
        trigger_condition: "when the gateway is unresponsive".to_string(),
        action_pattern: "restart the gateway and watch the logs".to_string(),
        success_count: 3,
        fail_count: 1,
        confidence: 0.66,
        activation_count: 11,
        source_skill: Some("ops".to_string()),
        learned_from: "episode".to_string(),
        embedding: emb(0.75),
        status: NodeStatus::Active,
        created_at: t(1_610_000_000),
        updated_at: t(1_610_000_200),
        source_episode_ids: vec![episode_id],
        metadata: HashMap::from([("env".to_string(), serde_json::json!("staging"))]),
        promotion_metadata: None,
    };
    provider.store_procedural(&procedural).expect("write procedural");

    let autobiographical = AutobiographicalNode {
        id: None,
        category: AutobioCategory::Capability,
        key: "preferred_language".to_string(),
        value: "zh-CN".to_string(),
        confidence: 0.9,
        source_episode_id: Some(episode_id),
        embedding: Some(emb(0.9)),
        status: NodeStatus::Active,
        created_at: t(1_620_000_000),
        updated_at: t(1_620_000_300),
        source_episode_ids: vec![episode_id],
        promotion_metadata: None,
        source: "manual".to_string(),
        metadata: HashMap::from([("why".to_string(), serde_json::json!("stated by user"))]),
    };
    provider.store_autobiographical(&autobiographical).expect("write autobiographical");

    store.close().expect("close source");
}

#[test]
fn every_field_of_every_layer_survives_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();
    build_source(&memory_dir);

    let target = SqliteStore::open(memory_dir.join("private.sqlite"), DIM).unwrap();
    let report = import_grafeo_memory(&memory_dir, &target, DIM).expect("import ran");

    assert_eq!(report.total(), 5, "every layer must land: {report:?}");
    assert_eq!(
        report.skipped, 0,
        "a skipped node is a shape the conversion could not map: {report:?}"
    );
    assert_eq!(report.episodes, 1);
    assert_eq!(report.knowledge, 2, "the ordering decoy is knowledge too");
    assert_eq!(report.procedural, 1);
    assert_eq!(report.autobiographical, 1);
    assert_eq!(
        target.node_count().unwrap(),
        5,
        "no layer may be written twice"
    );

    // ── Episodic ─────────────────────────────────────────────────────────
    let episode = only_episode(&target);
    assert_eq!(episode.session_id, "sess-9");
    assert_eq!(episode.turn_index, 7);
    assert_eq!(episode.role, "assistant");
    assert_eq!(episode.content, "migrated 中文 episode about the 网关 gateway");
    assert_eq!(
        episode.embedding.as_deref(),
        Some(emb(0.5).as_slice()),
        "the episode embedding must survive, not just its text"
    );
    assert_eq!(episode.timestamp, t(1_700_000_000));
    assert!(
        episode.consolidated,
        "a consolidated episode that comes back unconsolidated is re-distilled"
    );
    assert_eq!(
        episode.metadata.get("topic"),
        Some(&serde_json::json!("migration"))
    );
    assert_eq!(episode.importance, 0.75);
    assert_eq!(episode.knowledge_subtype, Some(KnowledgeSubType::Preference));

    // ── Knowledge ────────────────────────────────────────────────────────
    let knowledge = only_knowledge(&target);
    assert_eq!(knowledge.subject, "gateway");
    assert_eq!(knowledge.predicate, "runs_on");
    assert_eq!(knowledge.object, "staging");
    assert_eq!(knowledge.sub_type, KnowledgeSubType::Relation);
    assert_eq!(knowledge.confidence, 0.82);
    assert_eq!(
        knowledge.embedding.as_deref(),
        Some(emb(0.25).as_slice())
    );
    assert_eq!(knowledge.status, NodeStatus::Dormant);
    assert_eq!(knowledge.created_at, t(1_600_000_000));
    assert_eq!(knowledge.updated_at, t(1_600_000_100));
    assert_eq!(
        knowledge.metadata.get("origin"),
        Some(&serde_json::json!("distiller"))
    );
    assert_eq!(knowledge.privacy, PrivacyLevel::Sensitive);
    assert_eq!(knowledge.importance, 0.42);

    // ── Procedural ───────────────────────────────────────────────────────
    let procedural = only_procedural(&target);
    assert_eq!(procedural.name, "restart-gateway");
    assert_eq!(procedural.trigger_condition, "when the gateway is unresponsive");
    assert_eq!(
        procedural.action_pattern,
        "restart the gateway and watch the logs"
    );
    assert_eq!(procedural.success_count, 3);
    assert_eq!(procedural.fail_count, 1);
    assert_eq!(procedural.confidence, 0.66);
    assert_eq!(procedural.activation_count, 11);
    assert_eq!(procedural.source_skill.as_deref(), Some("ops"));
    assert_eq!(procedural.learned_from, "episode");
    assert_eq!(procedural.embedding, emb(0.75));
    assert_eq!(procedural.status, NodeStatus::Active);
    assert_eq!(procedural.created_at, t(1_610_000_000));
    assert_eq!(procedural.updated_at, t(1_610_000_200));

    // ── Autobiographical ─────────────────────────────────────────────────
    let autobiographical = only_autobiographical(&target);
    assert_eq!(autobiographical.category, AutobioCategory::Capability);
    assert_eq!(autobiographical.key, "preferred_language");
    assert_eq!(autobiographical.value, "zh-CN");
    assert_eq!(autobiographical.confidence, 0.9);
    assert_eq!(
        autobiographical.embedding.as_deref(),
        Some(emb(0.9).as_slice())
    );
    assert_eq!(autobiographical.status, NodeStatus::Active);
    assert_eq!(autobiographical.created_at, t(1_620_000_000));
    assert_eq!(autobiographical.updated_at, t(1_620_000_300));
    assert_eq!(autobiographical.source, "manual");
    assert_eq!(
        autobiographical.metadata.get("why"),
        Some(&serde_json::json!("stated by user"))
    );

    // ── References to episodes are remapped, not inherited ───────────────
    let episode_id = search_episode_id(&target);
    // Fresh stores number their first episode 1 on both sides, so the remap
    // cannot be proven by the ids differing. It is proven by what the reference
    // resolves to: the source's id is meaningless in the target store, so this
    // only holds if the map rewrote it.
    let referenced = target
        .get_episode(
            knowledge
                .source_episode_id
                .expect("knowledge must keep its episode reference"),
        )
        .expect("query referenced episode")
        .expect("the remapped reference must point at a real episode");
    assert_eq!(
        referenced.content, "migrated 中文 episode about the 网关 gateway",
        "the reference must land on the imported episode"
    );
    assert_eq!(knowledge.source_episode_id, Some(episode_id));
    assert_eq!(knowledge.source_episode_ids, vec![episode_id]);
    assert_eq!(knowledge.source_episode_ids, vec![episode_id]);
    assert_eq!(autobiographical.source_episode_id, Some(episode_id));
    assert_eq!(autobiographical.source_episode_ids, vec![episode_id]);
    // 2 (knowledge) + 2 (autobiographical). The procedural node also carries
    // `source_episode_ids` in grafeo's properties, but grafeo's own reader
    // hardcodes it back to empty (`ProceduralNode::from_properties`), so there
    // is nothing for the migration to carry: the import copies what the source
    // can report, and the property grafeo writes but never reads back stays
    // write-only until that reader is fixed.
    assert_eq!(
        report.references_remapped, 4,
        "every reference must have gone through the map: {report:?}"
    );
    assert_eq!(report.references_dropped, 0);
    assert_eq!(
        procedural.source_episode_ids,
        Vec::<u64>::new(),
        "grafeo cannot report these back, so the import cannot either"
    );
}

#[test]
fn imported_memory_is_searchable_and_forgettable_after_import() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();
    build_source(&memory_dir);

    let target = SqliteStore::open(memory_dir.join("private.sqlite"), DIM).unwrap();
    import_grafeo_memory(&memory_dir, &target, DIM).expect("import ran");

    // The FTS blob must be derived with the same helper the write path uses,
    // or imported rows are invisible to recall while fresh ones are not.
    let hits = target
        .search_episodes(&MemoryQuery::new("网关 gateway"))
        .expect("search");
    assert_eq!(hits.len(), 1, "CJK recall over imported rows: {hits:?}");
    let episode_id = hits[0].node_id;
    assert_eq!(
        target.get_node_status(episode_id).unwrap(),
        Some(NodeStatus::Active)
    );

    // The decayed knowledge node must still read as Dormant through the trait.
    let knowledge = only_knowledge(&target);
    assert_eq!(knowledge.status, NodeStatus::Dormant);

    // Forgetting runs over imported rows like any others.
    let decay = EpisodicDecayConfig {
        enabled: true,
        half_life_days: 1,
        dormant_threshold: 0.5,
        archive_days: 90,
    };
    let result = target.run_episodic_decay_scan(&decay).unwrap();
    assert!(
        result.to_dormant >= 1,
        "the 2023 episode should decay: {result:?}"
    );

    // The source is the user's only copy; a migration never deletes it.
    assert!(memory_dir.join(SOURCE_FILE).exists());
}

#[test]
fn import_is_a_no_op_into_a_populated_store_and_without_a_source() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();
    build_source(&memory_dir);

    let target = SqliteStore::open(memory_dir.join("private.sqlite"), DIM).unwrap();
    assert!(import_grafeo_memory(&memory_dir, &target, DIM).is_some());

    // Running it again must not duplicate: the target is no longer empty.
    assert!(
        import_grafeo_memory(&memory_dir, &target, DIM).is_none(),
        "a populated store must refuse the import"
    );
    assert_eq!(target.node_count().unwrap(), 5);

    // And with no source at all there is nothing to do.
    let empty_dir = dir.path().join("elsewhere");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let fresh = SqliteStore::open(empty_dir.join("private.sqlite"), DIM).unwrap();
    assert!(import_grafeo_memory(&empty_dir, &fresh, DIM).is_none());
    assert_eq!(fresh.node_count().unwrap(), 0);
}

// ── helpers ──────────────────────────────────────────────────────────────

/// The single imported episode, read back through the store's typed getter.
/// The imported episode's SQLite id, found the way recall finds it.
fn search_episode_id(target: &SqliteStore) -> u64 {
    target
        .search_episodes(&MemoryQuery::new("中文"))
        .expect("search episode")
        .first()
        .map(|hit| hit.node_id)
        .expect("the imported episode is searchable")
}

fn only_episode(target: &SqliteStore) -> Episode {
    target
        .get_episode(search_episode_id(target))
        .expect("query episode")
        .expect("episode exists")
}

fn only_knowledge(target: &SqliteStore) -> KnowledgeNode {
    target
        .find_knowledge_by_subject("gateway", "runs_on")
        .expect("query knowledge")
        .expect("knowledge exists")
        .1
}

fn only_procedural(target: &SqliteStore) -> ProceduralNode {
    target
        .get_all_procedural_nodes()
        .expect("query procedural")
        .into_iter()
        .next()
        .expect("procedural exists")
}

fn only_autobiographical(target: &SqliteStore) -> AutobiographicalNode {
    target
        .find_autobiographical_by_key("preferred_language")
        .expect("query autobiographical")
        .expect("autobiographical exists")
}
