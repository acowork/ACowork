//! One-shot import of a grafeo memory store into the SQLite backend
//! (ADR-082 §4 step 2).
//!
//! Flipping `ACOWORK_MEMORY_BACKEND=sqlite` on a machine with history must not
//! start from an empty store: the grafeo file is the user's memory. This reads
//! `{memory_dir}/private.grafeo` and writes every node into SQLite, once, before
//! anything else can write.
//!
//! Two things make it lossless rather than best-effort:
//!
//! * the field mapping is not hand-written. Each node goes through
//!   `from_properties` → `serde_json` → the `acowork-memory` struct, so a field
//!   that exists on one side and not the other fails the conversion instead of
//!   defaulting away. The parity test pins the whole key set per type.
//! * the write goes through `SqliteStore::import_node`, which bypasses the
//!   write path's business logic (similarity dedup, status rewriting) and keeps
//!   each node's own `status` and `created_at`. A decayed node that came back
//!   Active would silently undo forgetting.
//!
//! Node ids are re-allocated by SQLite, so `source_episode_id(s)` references are
//! remapped through the old-id → new-id map; a reference whose episode could not
//! be imported is cleared rather than left pointing at an unrelated node.
//!
//! The source is never deleted, and the import only runs into an empty store, so
//! a failed run leaves nothing behind to re-import over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acowork_grafeo::grafeo::GrafeoStore;
use acowork_grafeo::types::GrafeoConfig;
use acowork_memory::labels;
use acowork_memory::types::{AutobiographicalNode, Episode, KnowledgeNode, ProceduralNode};
use acowork_sqlite::{Error, SqliteStore};
use grafeo_common::types::{NodeId, Value};

/// File name of the grafeo memory store inside the agent's `memory/` directory.
pub const SOURCE_FILE: &str = "private.grafeo";

/// What one import moved.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub episodes: usize,
    pub knowledge: usize,
    pub procedural: usize,
    pub autobiographical: usize,
    /// Nodes present in the source that could not be converted — an unexpected
    /// shape, not an empty node. Non-zero means the source needs a look.
    pub skipped: usize,
    pub references_remapped: usize,
    /// References to an episode that was itself skipped: cleared, not kept.
    pub references_dropped: usize,
}

impl ImportReport {
    pub fn total(&self) -> usize {
        self.episodes + self.knowledge + self.procedural + self.autobiographical
    }

    /// Whether anything was actually moved.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Path of the grafeo store this importer reads.
pub fn source_path(memory_dir: &Path) -> PathBuf {
    memory_dir.join(SOURCE_FILE)
}

/// Import `{memory_dir}/private.grafeo` into `target`, when it exists and
/// `target` is still empty.
///
/// Returns `None` when there is nothing to do (no source, target not empty, or
/// the source could not be opened). Never deletes or truncates the source.
pub fn import_grafeo_memory(
    memory_dir: &Path,
    target: &SqliteStore,
    embedding_dim: usize,
) -> Option<ImportReport> {
    let source = source_path(memory_dir);
    if !source.exists() {
        return None;
    }
    if target.node_count().unwrap_or(0) > 0 {
        tracing::info!(
            source = %source.display(),
            "memory import: target already populated, skipped"
        );
        return None;
    }
    let src = match GrafeoStore::open(&GrafeoConfig {
        db_path: source.clone(),
        embedding_dim,
    }) {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(
                source = %source.display(),
                error = %e,
                "memory import: source unreadable, skipped"
            );
            return None;
        }
    };

    match run(&src, target) {
        Ok(report) => {
            tracing::info!(
                source = %source.display(),
                episodes = report.episodes,
                knowledge = report.knowledge,
                procedural = report.procedural,
                autobiographical = report.autobiographical,
                skipped = report.skipped,
                remapped = report.references_remapped,
                dropped = report.references_dropped,
                "memory import: grafeo store imported into SQLite"
            );
            Some(report)
        }
        Err(e) => {
            tracing::warn!(
                source = %source.display(),
                error = %e,
                "memory import: aborted, rolled back"
            );
            None
        }
    }
}

/// Read the whole source and write it into `target`.
///
/// A per-node conversion failure is counted and skipped (a legacy node with an
/// unexpected shape must not abort the migration). A storage failure aborts and
/// rolls back, so the target is either complete or empty — never half, which
/// would otherwise be frozen in place by the "only into an empty store" guard.
fn run(src: &GrafeoStore, target: &SqliteStore) -> Result<ImportReport, Error> {
    let graph = src.db().graph_store();
    let mut report = ImportReport::default();
    let mut written: Vec<u64> = Vec::new();
    let mut id_map: HashMap<u64, u64> = HashMap::new();

    let outcome = (|| -> Result<(), Error> {
        // Episodes first: every other layer references them.
        for id in graph.nodes_by_label(labels::EPISODIC) {
            let Some(props) = read_props(src, id) else {
                report.skipped += 1;
                continue;
            };
            let Ok(grafeo_node) = acowork_grafeo::types::Episode::from_properties(id, &props)
            else {
                report.skipped += 1;
                continue;
            };
            let Ok(episode) = to_memory::<Episode>(&grafeo_node) else {
                report.skipped += 1;
                continue;
            };
            let Ok(json) = serde_json::to_string(&episode) else {
                report.skipped += 1;
                continue;
            };
            let status = status_of(&props);
            let new_id = target.import_node(
                labels::EPISODIC,
                &json,
                status,
                episode.embedding.as_deref(),
            )?;
            written.push(new_id);
            id_map.insert(id.0, new_id);
            report.episodes += 1;
        }

        for id in graph.nodes_by_label(labels::KNOWLEDGE) {
            let Some(props) = read_props(src, id) else {
                report.skipped += 1;
                continue;
            };
            let Ok(grafeo_node) = acowork_grafeo::types::KnowledgeNode::from_properties(id, &props)
            else {
                report.skipped += 1;
                continue;
            };
            let Ok(mut kn) = to_memory::<KnowledgeNode>(&grafeo_node) else {
                report.skipped += 1;
                continue;
            };
            remap_one(&mut kn.source_episode_id, &id_map, &mut report);
            remap_many(&mut kn.source_episode_ids, &id_map, &mut report);
            let Ok(json) = serde_json::to_string(&kn) else {
                report.skipped += 1;
                continue;
            };
            let status = status_of(&props);
            let new_id =
                target.import_node(labels::KNOWLEDGE, &json, status, kn.embedding.as_deref())?;
            written.push(new_id);
            report.knowledge += 1;
        }

        for id in graph.nodes_by_label(labels::PROCEDURAL) {
            let Some(props) = read_props(src, id) else {
                report.skipped += 1;
                continue;
            };
            let Ok(grafeo_node) = acowork_grafeo::types::ProceduralNode::from_properties(id, &props)
            else {
                report.skipped += 1;
                continue;
            };
            let Ok(mut pn) = to_memory::<ProceduralNode>(&grafeo_node) else {
                report.skipped += 1;
                continue;
            };
            remap_many(&mut pn.source_episode_ids, &id_map, &mut report);
            pn.id = None;
            let Ok(json) = serde_json::to_string(&pn) else {
                report.skipped += 1;
                continue;
            };
            // `ProceduralNode::embedding` is a plain Vec, unlike every other
            // layer: an empty one means "no embedding", not a zero-width vector.
            let embedding = (!pn.embedding.is_empty()).then_some(pn.embedding.as_slice());
            let status = status_of(&props);
            let new_id = target.import_node(labels::PROCEDURAL, &json, status, embedding)?;
            written.push(new_id);
            report.procedural += 1;
        }

        for id in graph.nodes_by_label(labels::AUTOBIOGRAPHICAL) {
            let Some(props) = read_props(src, id) else {
                report.skipped += 1;
                continue;
            };
            let Ok(grafeo_node) =
                acowork_grafeo::types::AutobiographicalNode::from_properties(id, &props)
            else {
                report.skipped += 1;
                continue;
            };
            let Ok(mut an) = to_memory::<AutobiographicalNode>(&grafeo_node) else {
                report.skipped += 1;
                continue;
            };
            remap_one(&mut an.source_episode_id, &id_map, &mut report);
            remap_many(&mut an.source_episode_ids, &id_map, &mut report);
            an.id = None;
            let Ok(json) = serde_json::to_string(&an) else {
                report.skipped += 1;
                continue;
            };
            let status = status_of(&props);
            let new_id =
                target.import_node(labels::AUTOBIOGRAPHICAL, &json, status, an.embedding.as_deref())?;
            written.push(new_id);
            report.autobiographical += 1;
        }

        Ok(())
    })();

    if let Err(e) = outcome {
        for id in written {
            let _ = target.delete_node(id);
        }
        return Err(e);
    }
    Ok(report)
}

/// Read one node's properties in the shape `from_properties` wants.
fn read_props(src: &GrafeoStore, id: NodeId) -> Option<Vec<(String, Value)>> {
    let node = src.get_node(id)?;
    Some(
        node.properties_as_btree()
            .into_iter()
            .map(|(k, v)| (k.as_str().to_string(), v))
            .collect(),
    )
}

/// The node's persisted status. Every layer stores one as a property; a legacy
/// node without it defaults to Active, matching the grafeo reader.
fn status_of(props: &[(String, Value)]) -> &str {
    props
        .iter()
        .find(|(key, _)| key == "status")
        .and_then(|(_, value)| value.as_str())
        .unwrap_or("Active")
}

/// grafeo's typed node → JSON → the `acowork-memory` node.
///
/// The JSON hop is the field check: a field named differently on either side
/// fails the conversion rather than silently defaulting.
fn to_memory<M: serde::de::DeserializeOwned>(grafeo_node: &impl serde::Serialize) -> serde_json::Result<M> {
    serde_json::from_value(serde_json::to_value(grafeo_node)?)
}

fn remap_one(
    slot: &mut Option<u64>,
    id_map: &HashMap<u64, u64>,
    report: &mut ImportReport,
) {
    let Some(old) = *slot else { return };
    match id_map.get(&old) {
        Some(new) => {
            *slot = Some(*new);
            report.references_remapped += 1;
        }
        // The episode it pointed at was skipped. Keeping the old id would point
        // at whatever node happens to carry that id now — worse than no link.
        None => {
            *slot = None;
            report.references_dropped += 1;
        }
    }
}

fn remap_many(slot: &mut Vec<u64>, id_map: &HashMap<u64, u64>, report: &mut ImportReport) {
    let mut kept = Vec::with_capacity(slot.len());
    for old in slot.iter() {
        match id_map.get(old) {
            Some(new) => {
                kept.push(*new);
                report.references_remapped += 1;
            }
            None => report.references_dropped += 1,
        }
    }
    *slot = kept;
}
