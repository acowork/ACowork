//! ADR-071 e2e — manual-distiller full chain + opt-in gate.
//!
//! Lives in-crate (not `tests/`) because `AgentCore::new` is `pub(crate)`
//! and this suite must inject `memory_provider` / `embedding_provider` /
//! `consolidation_timer` (all `pub(crate)` fields) to wire a REAL
//! real store behind the HTTP endpoint. `prompts_reload_e2e` documents
//! why an integration test cannot construct an `AgentCore`.
//!
//! Covers the ADR-071 W2 claim that was previously only stated in a commit
//! message: "A full end-to-end run (episodes -> promoted nodes) wires a real
//! AgentCore." Scenarios:
//!
//! 1. **E1 — HTTP manual distill promotes episodes**: seed two classified
//!    episodes (Fact + Preference) into a real in-memory store, run
//!    `POST /memory/distill`, and assert the HTTP `DistillResponse`, the
//!    promoted `KnowledgeNode`s (with `promotion_metadata`), the
//!    consolidated-episode cleanup, and the `GET /memory/consolidation/status`
//!    `last_run` summary.
//! 2. **E2 — disabled distiller refuses the manual trigger**: manifest has no
//!    `[memory.distiller]` section (opt-in off); `POST /memory/distill`
//!    returns 409 and produces zero semantic-layer nodes.
//!
//! The LLM is a scripted `MockProvider` (response queue) behind the SAME
//! `ProviderLlmAdapter` the production path uses — no distiller internals are
//! bypassed. The multi-thread runtime also exercises the W2 `block_in_place`
//! embedding bridge (current-thread runtimes degrade to exact-key clustering).

#![cfg(test)]

use std::sync::Arc;

use acowork_core::EmbeddingProvider;
use acowork_core::providers::mock::{MockProvider, MockResponse};
use acowork_memory::MemoryProvider;
use acowork_sqlite::SqliteStore;
use acowork_memory::types::{Episode, KnowledgeSubType};
use chrono::{Duration as ChronoDuration, Utc};

use crate::agent::agent_core::{AgentCore, BuiltinToolEntry};
use crate::memory::consolidation_bg::ConsolidationTimer;

const AGENT_ID: &str = "com.test.adr071-e2e";

// ============================================================================
// Deterministic embedding (same fallback as the production embedding chain)
// ============================================================================

struct DeterministicEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for DeterministicEmbedding {
    fn name(&self) -> &str {
        "deterministic-adr071-e2e"
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
// Scripted LLM responses (one merge verdict per call)
// ============================================================================

fn merge_response(action: &str, target_id: Option<u64>, statement: Option<&str>) -> String {
    serde_json::json!({
        "action": action,
        "target_id": target_id,
        "statement": statement,
        "reasoning": "scripted e2e verdict",
    })
    .to_string()
}

// ============================================================================
// Harness
// ============================================================================

/// Width of `DeterministicEmbedding`, and therefore of the store's vectors.
const EMBED_DIM: usize = 384;

struct Adr071E2e {
    core: Arc<AgentCore>,
    store: Arc<SqliteStore>,
    timer: Arc<ConsolidationTimer>,
}

/// Build a real AgentCore wired to the given real in-memory store, with the
/// given scripted LLM response queue. `enabled=false` omits the
/// `[memory.distiller]` manifest section (opt-in off).
///
/// The store is passed in so callers can seed episodes and read their REAL
/// node ids FIRST and then build the extract JSON with those ids, instead of
/// hard-coding ids the store is free to assign.
fn build_core(
    enabled: bool,
    store: Arc<SqliteStore>,
    llm_responses: Vec<MockResponse>,
) -> Adr071E2e {
    let config = crate::config::RuntimeConfig::default();
    let distiller_toml = if enabled {
        "[memory.distiller]\nenabled = true\nbatch_size = 20\n"
    } else {
        ""
    };
    let manifest = acowork_core::AgentManifest::from_toml(&format!(
        r#"
        agent_id = "{AGENT_ID}"
        version = "1.0.0"
        name = "Test ADR-071 distiller e2e"
        description = "Manual distiller full-chain e2e"
        author = "test"
        runtime_version = "0.1.0"

        [llm]
        provider = "mock"
        model = "test-model"

        {distiller_toml}
        "#
    ))
    .expect("manifest parse ok");

    let provider = Arc::new(MockProvider::new(llm_responses));
    let mut core = AgentCore::new(config, manifest, provider, Vec::<BuiltinToolEntry>::new());

    // Inject the providers + timer the production session_init wires in
    // Phase B (see `startup::session_init`). The timer's scheduler policy
    // comes from `distiller_scheduler_config()` — the SAME source the
    // production `start_consolidation_pipeline` uses — so the status
    // endpoint reports the effective switch/interval.
    core.memory_provider = Some(store.clone());
    core.embedding_provider = Some(Arc::new(DeterministicEmbedding));
    let timer = Arc::new(ConsolidationTimer::new(core.distiller_scheduler_config()));
    core.consolidation_timer = Some(timer.clone());

    let core = Arc::new(core);
    Adr071E2e { core, store, timer }
}

impl Adr071E2e {
    /// Seed a classified episode directly (as if written earlier by the
    /// memory_store tool at `ts`), returning its real node id.
    fn seed_episode(
        &self,
        content: &str,
        subtype: KnowledgeSubType,
        ts: chrono::DateTime<Utc>,
    ) -> u64 {
        let ep = Episode {
            session_id: AGENT_ID.to_string(),
            turn_index: 0,
            role: "assistant".to_string(),
            content: content.to_string(),
            embedding: None,
            timestamp: ts,
            consolidated: false,
            metadata: Default::default(),
            importance: 0.5,
            knowledge_subtype: Some(subtype),
            normalized: None,
        };
        let provider: Arc<dyn MemoryProvider> = self.store.clone();
        provider.store_episode(&ep).expect("store_episode ok")
    }

    /// Unconsolidated episodes in timestamp order (the distiller's Step 1
    /// candidate set), so tests can map real ids onto the scripted extract
    /// JSON.
    fn unconsolidated_ids(&self) -> Vec<u64> {
        self.store
            .get_episodes_by_subtype(None, 20)
            .expect("scan ok")
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }
}

/// Spawn a `RuntimeHttpServer` with the AgentCore + consolidation-timer
/// slots populated, so the manual-distill + status endpoints read the real
/// objects the background loop would use. Every other slot is a minimal
/// stub (`None` / empty) — same pattern as `prompts_reload_e2e::spawn_server`,
/// which documents why each unused slot can be empty.
async fn spawn_server(e2e: &Adr071E2e) -> u16 {
    let temp_dir =
        std::env::temp_dir().join(format!("acowork-test-adr071-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    let snapshots = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    let latest = Arc::new(std::sync::RwLock::new(None));
    let dispatch_tx = Arc::new(tokio::sync::Mutex::new(None));
    let embed_dim = Arc::new(std::sync::RwLock::new(0));
    let degraded_reasons = Arc::new(std::sync::RwLock::new(Vec::new()));
    let mqtt_client = Arc::new(tokio::sync::Mutex::new(None));
    let session_metadata = Arc::new(tokio::sync::Mutex::new(None));
    let memory_query = Arc::new(tokio::sync::Mutex::new(None));
    let workspace_query = Arc::new(tokio::sync::Mutex::new(None));
    let workspace_mutation = Arc::new(tokio::sync::Mutex::new(None));
    let agent_tools = Arc::new(tokio::sync::Mutex::new(None));
    let agent_config = Arc::new(tokio::sync::Mutex::new(None));
    let attachment = Arc::new(tokio::sync::Mutex::new(None));
    let session_config = Arc::new(tokio::sync::Mutex::new(None));
    let consolidation_timer: Arc<
        std::sync::RwLock<Option<Arc<crate::memory::ConsolidationTimer>>>,
    > = Arc::new(std::sync::RwLock::new(Some(e2e.timer.clone())));
    let rag_provider: Arc<std::sync::RwLock<Option<Arc<dyn acowork_core::rag::RagProvider>>>> =
        Arc::new(std::sync::RwLock::new(None));
    let debug_service = Arc::new(tokio::sync::Mutex::new(None));
    let workspace_resolver = Arc::new(std::sync::RwLock::new(
        crate::tools::workspace_resolver::WorkspaceResolver::new_for_test(vec![]),
    ));
    let session_manager_slot: Arc<
        tokio::sync::RwLock<Option<Arc<tokio::sync::Mutex<crate::agent::session::SessionManager>>>>,
    > = Arc::new(tokio::sync::RwLock::new(None));
    let agent_core_slot: Arc<std::sync::RwLock<Option<Arc<crate::agent::agent_core::AgentCore>>>> =
        Arc::new(std::sync::RwLock::new(Some(e2e.core.clone())));

    let server = crate::http::RuntimeHttpServer::start(
        temp_dir.clone(),
        temp_dir.clone(),
        AGENT_ID.to_string(),
        String::new(), // ADR-073: root-level /memory/* routes carry no agent-path guard, so the
        // instance identity is unused here
        snapshots,
        latest,
        dispatch_tx,
        embed_dim,
        degraded_reasons,
        mqtt_client,
        session_metadata,
        memory_query,
            std::sync::Arc::new(std::sync::RwLock::new(None)),
        workspace_query,
        workspace_mutation,
        agent_tools,
        Arc::new(tokio::sync::Mutex::new(None)),
        agent_config,
        attachment,
        session_config,
        consolidation_timer,
        rag_provider,
        debug_service,
        workspace_resolver,
        session_manager_slot,
        agent_core_slot,
    )
    .await
    .expect("runtime http server should start");

    server.port
}

// ============================================================================
// E1 — HTTP manual distill: episodes -> promoted nodes (real store)
// ============================================================================

/// Run `POST /memory/distill` three times against a real store and assert both
/// consolidation paths end to end:
///
/// 1. an empty semantic layer projects each episode with **zero LLM calls**;
/// 2. a restated episode recalls the node written by pass 1 and merges into it,
///    appending its episode id to the provenance list;
/// 3. several restatements fold into the same node in one run.
///
/// Pass 2 needs the node id that pass 1 created, which is why these are
/// separate runs rather than one scripted queue.
#[tokio::test(flavor = "multi_thread")]
async fn e1_http_manual_distill_promotes_episodes() {
    let now = Utc::now();
    let store = Arc::new(SqliteStore::open_in_memory(EMBED_DIM).expect("in-memory store"));
    let client = reqwest::Client::new();
    let distill = |port: u16| {
        let client = client.clone();
        async move {
            let resp = client
                .post(format!("http://127.0.0.1:{port}/memory/distill"))
                .send()
                .await
                .expect("POST /memory/distill");
            assert_eq!(resp.status(), 200, "manual distill must succeed");
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    // ── Pass 1: projection. Nothing similar exists yet, so the backlog
    // consolidates without consulting the model at all. The empty response
    // queue makes any LLM call visible as a failed merge in the funnel.
    let e2e = build_core(true, store.clone(), Vec::new());
    let fact_ep = e2e.seed_episode(
        "User lives in Shanghai",
        KnowledgeSubType::Fact,
        now - ChronoDuration::days(2),
    );
    let pref_ep = e2e.seed_episode(
        "User prefers dark mode",
        KnowledgeSubType::Preference,
        now - ChronoDuration::days(1),
    );
    assert_eq!(
        e2e.unconsolidated_ids(),
        vec![pref_ep, fact_ep],
        "newest first (the backlog can no longer starve)"
    );

    let port = spawn_server(&e2e).await;
    let body = distill(port).await;
    assert_eq!(body["started"], true);
    assert_eq!(body["episodes_scanned"], 2);
    assert_eq!(body["facts_promoted"], 1, "fact episode consolidated: {body}");
    assert_eq!(
        body["preferences_promoted"], 1,
        "preference episode consolidated: {body}"
    );
    assert_eq!(body["episodes_marked_consolidated"], 2);
    let funnel = &body["funnel"];
    assert_eq!(
        funnel["llm_calls"], 0,
        "projection costs no LLM calls: {funnel}"
    );
    assert_eq!(funnel["projected"], 2, "both took the projection path");
    assert_eq!(funnel["episodes_deferred"], 0);

    // Projected nodes carry the same provenance the old judge used to write.
    let (fact_id, fact) = e2e
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("Fact KnowledgeNode exists");
    let meta = fact.promotion_metadata.as_ref().expect("promotion metadata");
    assert_eq!(meta.promoted_by, "episodic_distiller");
    assert_eq!(meta.evidence_episode_ids, vec![fact_ep]);
    assert_eq!(fact.sub_type, KnowledgeSubType::Fact);
    assert_eq!(fact.object, "User lives in Shanghai");

    let (_, pref) = e2e
        .store
        .find_knowledge_by_subject("user", "user_prefers_dark_mode")
        .expect("lookup ok")
        .expect("Preference KnowledgeNode exists");
    assert_eq!(pref.sub_type, KnowledgeSubType::Preference);
    assert_eq!(pref.promotion_metadata.as_ref().unwrap().evidence_episode_ids, vec![pref_ep]);

    // Episodes consumed: a re-run finds an empty backlog.
    assert!(
        e2e
            .store
            .get_episodes_by_subtype(None, 10)
            .expect("scan ok")
            .is_empty(),
        "all evidence episodes consolidated"
    );

    // ── Status endpoint surfaces the run summary ─────────────────────────
    let status = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/memory/consolidation/status"))
        .send()
        .await
        .expect("GET status");
    assert_eq!(status.status(), 200);
    let status: serde_json::Value = status.json().await.unwrap();
    let d = &status["distiller"];
    assert_eq!(d["enabled"], true);
    let last_run = d["last_run"].as_object().expect("last_run present");
    assert_eq!(
        last_run["episodes_scanned"].as_u64().unwrap(),
        2,
        "last_run: {last_run:?}"
    );
    assert!(
        last_run["total_promoted"].as_u64().unwrap() >= 2,
        "last_run total_promoted: {last_run:?}"
    );
    assert_eq!(last_run["funnel"]["llm_calls"], 0, "last_run: {last_run:?}");
    assert!(
        last_run["at"].as_str().is_some(),
        "last_run carries a timestamp"
    );

    // ── Pass 2: merge. DeterministicEmbedding maps the restated text to the
    // same vector, so pass 1's Fact node comes back above the recall threshold.
    // The scripted verdict names that node — an id only knowable because pass 1
    // already ran.
    let e2e2 = build_core(
        true,
        store.clone(),
        vec![MockResponse::Text {
            content: merge_response("merge", Some(fact_id), Some("User lives in Shanghai")),
        }],
    );
    let restated = e2e2.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now);
    let body2 = distill(spawn_server(&e2e2).await).await;
    assert_eq!(body2["episodes_scanned"], 1, "only the restatement is left");
    assert_eq!(body2["funnel"]["llm_calls"], 1, "the merge path asks once");
    assert_eq!(body2["funnel"]["verdict_merged"], 1);
    assert_eq!(body2["funnel"]["projected"], 0, "nothing was a lone voice");

    let (fact_id2, fact2) = e2e2
        .store
        .find_knowledge_by_subject("user", "user_lives_in_shanghai")
        .expect("lookup ok")
        .expect("Fact node still exists");
    assert_eq!(fact_id2, fact_id, "merged into the existing row, not a new one");
    assert_eq!(
        fact2.promotion_metadata.as_ref().unwrap().evidence_episode_ids,
        vec![fact_ep, restated],
        "the restatement's episode id is appended to the provenance list"
    );

    // ── Pass 3: several restatements fold into the same node in one run.
    // Both episodes are the same text, so both scripted verdicts are identical
    // and the order the two calls land in stops mattering.
    let e2e3 = build_core(
        true,
        store.clone(),
        vec![
            MockResponse::Text {
                content: merge_response("merge", Some(fact_id), Some("User lives in Shanghai")),
            },
            MockResponse::Text {
                content: merge_response("merge", Some(fact_id), Some("User lives in Shanghai")),
            },
        ],
    );
    let more = [
        e2e3.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now + ChronoDuration::hours(1)),
        e2e3.seed_episode("User lives in Shanghai", KnowledgeSubType::Fact, now + ChronoDuration::hours(2)),
    ];
    let body3 = distill(spawn_server(&e2e3).await).await;
    assert_eq!(body3["episodes_scanned"], 2);
    assert_eq!(body3["funnel"]["verdict_merged"], 2, "both folded in: {body3}");
    let mut evidence = e2e3
        .store
        .get_knowledge(fact_id)
        .expect("get ok")
        .expect("node exists")
        .promotion_metadata
        .unwrap()
        .evidence_episode_ids;
    evidence.sort();
    let mut want = vec![fact_ep, restated];
    want.extend_from_slice(&more);
    want.sort();
    assert_eq!(evidence, want, "every episode behind the statement is cited");
}

// ============================================================================
// E2 — opt-in gate: disabled distiller refuses the manual trigger
// ============================================================================

/// Manifest without `[memory.distiller]` → `POST /memory/distill` returns
/// 409 and must not produce ANY semantic-layer node, even with a non-empty
/// episode backlog (ADR-068/071 opt-in invariant).
#[tokio::test(flavor = "multi_thread")]
async fn e2_disabled_distiller_refuses_manual_trigger() {
    let now = Utc::now();
    let store = Arc::new(SqliteStore::open_in_memory(EMBED_DIM).expect("in-memory store"));
    let e2e = build_core(false, store.clone(), Vec::new());
    e2e.seed_episode(
        "User lives in Beijing",
        KnowledgeSubType::Fact,
        now - ChronoDuration::days(1),
    );

    let port = spawn_server(&e2e).await;
    let base = format!("http://127.0.0.1:{port}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/memory/distill"))
        .send()
        .await
        .expect("POST /memory/distill");
    assert_eq!(
        resp.status(),
        409,
        "disabled distiller must refuse the manual trigger"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("distiller is disabled"),
        "409 body explains the opt-in gate: {body}"
    );

    // No semantic-layer node and the episode stays unconsolidated.
    let fact = e2e
        .store
        .find_knowledge_by_subject("user", "lives_in")
        .expect("lookup ok");
    assert!(fact.is_none(), "disabled distiller must not promote");
    let remaining = e2e
        .store
        .get_episodes_by_subtype(None, 10)
        .expect("scan ok");
    assert_eq!(remaining.len(), 1, "episode must remain unconsolidated");
    assert!(!remaining[0].1.consolidated);
}
