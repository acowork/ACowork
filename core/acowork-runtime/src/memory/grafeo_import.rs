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
    /// Nodes left behind, keyed `"<layer>/<reason>"`. A count alone cannot be
    /// acted on: the operator needs to know whether the stragglers are a legacy
    /// shape worth a fallback, or something that was never a memory node.
    pub skipped_reasons: std::collections::BTreeMap<String, usize>,
    /// First failure message behind each entry of `skipped_reasons`. "151 nodes
    /// failed to convert" is not actionable; "unknown role \"tool\"" is.
    pub skipped_examples: std::collections::BTreeMap<String, String>,
    /// Structural properties filled in for legacy nodes, keyed `"<layer>/<what>"`.
    /// A non-empty map is not a failure: nothing in it carries information.
    pub normalized: std::collections::BTreeMap<String, usize>,
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

/// The embedding width actually stored in `{memory_dir}/private.grafeo`.
///
/// `GrafeoConfig::embedding_dim` sizes the vector index, not the properties, so
/// a store opens with a wrong value and still hands back its data — which makes
/// an actual stored vector the only trustworthy source for this number. SQLite
/// records the width it is created with, so guessing here would leave the
/// runtime skipping every migrated vector.
pub fn detect_embedding_dim(memory_dir: &Path) -> Option<usize> {
    let source = source_path(memory_dir);
    if !source.exists() {
        return None;
    }
    // The open dimension is a guess only until the first stored vector answers.
    for candidate in [384usize, 512, 768, 1024, 1536, 2560, 3072] {
        let Ok(src) = GrafeoStore::open(&GrafeoConfig {
            db_path: source.clone(),
            embedding_dim: candidate,
        }) else {
            continue;
        };
        let graph = src.db().graph_store();
        for id in graph.nodes_by_label(labels::EPISODIC) {
            let Some(props) = read_props(&src, id) else {
                continue;
            };
            if let Ok(episode) = acowork_grafeo::types::Episode::from_properties(id, &props)
                && let Some(vector) = episode.embedding
                && !vector.is_empty()
            {
                return Some(vector.len());
            }
        }
    }
    None
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
            let Some(mut props) = read_props(src, id) else {
                report.skip(labels::EPISODIC, "props_unreadable");
                continue;
            };
            normalize_props(&mut props, labels::EPISODIC, &mut report);
            let grafeo_node = match acowork_grafeo::types::Episode::from_properties(id, &props) {
                Ok(node) => node,
                Err(e) => {
                    report.skip_with(labels::EPISODIC, "from_properties_failed", e.to_string());
                    continue;
                }
            };
            let episode = match to_memory::<Episode>(&grafeo_node) {
                Ok(node) => node,
                Err(e) => {
                    report.skip_with(labels::EPISODIC, "field_mismatch", e.to_string());
                    continue;
                }
            };
            let Ok(json) = serde_json::to_string(&episode) else {
                report.skip(labels::EPISODIC, "serialize_failed");
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
            let Some(mut props) = read_props(src, id) else {
                report.skip(labels::KNOWLEDGE, "props_unreadable");
                continue;
            };
            normalize_props(&mut props, labels::KNOWLEDGE, &mut report);
            let grafeo_node =
                match acowork_grafeo::types::KnowledgeNode::from_properties(id, &props) {
                    Ok(node) => node,
                    Err(e) => {
                        report.skip_with(
                            labels::KNOWLEDGE,
                            "from_properties_failed",
                            e.to_string(),
                        );
                        continue;
                    }
                };
            let mut kn = match to_memory::<KnowledgeNode>(&grafeo_node) {
                Ok(node) => node,
                Err(e) => {
                    report.skip_with(labels::KNOWLEDGE, "field_mismatch", e.to_string());
                    continue;
                }
            };
            remap_one(&mut kn.source_episode_id, &id_map, &mut report);
            remap_many(&mut kn.source_episode_ids, &id_map, &mut report);
            let Ok(json) = serde_json::to_string(&kn) else {
                report.skip(labels::KNOWLEDGE, "serialize_failed");
                continue;
            };
            let status = status_of(&props);
            let new_id =
                target.import_node(labels::KNOWLEDGE, &json, status, kn.embedding.as_deref())?;
            written.push(new_id);
            report.knowledge += 1;
        }

        for id in graph.nodes_by_label(labels::PROCEDURAL) {
            let Some(mut props) = read_props(src, id) else {
                report.skip(labels::PROCEDURAL, "props_unreadable");
                continue;
            };
            normalize_props(&mut props, labels::PROCEDURAL, &mut report);
            let grafeo_node =
                match acowork_grafeo::types::ProceduralNode::from_properties(id, &props) {
                    Ok(node) => node,
                    Err(e) => {
                        report.skip_with(
                            labels::PROCEDURAL,
                            "from_properties_failed",
                            e.to_string(),
                        );
                        continue;
                    }
                };
            let mut pn = match to_memory::<ProceduralNode>(&grafeo_node) {
                Ok(node) => node,
                Err(e) => {
                    report.skip_with(labels::PROCEDURAL, "field_mismatch", e.to_string());
                    continue;
                }
            };
            remap_many(&mut pn.source_episode_ids, &id_map, &mut report);
            pn.id = None;
            let Ok(json) = serde_json::to_string(&pn) else {
                report.skip(labels::PROCEDURAL, "serialize_failed");
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
            let Some(mut props) = read_props(src, id) else {
                report.skip(labels::AUTOBIOGRAPHICAL, "props_unreadable");
                continue;
            };
            normalize_props(&mut props, labels::AUTOBIOGRAPHICAL, &mut report);
            let grafeo_node =
                match acowork_grafeo::types::AutobiographicalNode::from_properties(id, &props) {
                    Ok(node) => node,
                    Err(e) => {
                        report.skip_with(
                            labels::AUTOBIOGRAPHICAL,
                            "from_properties_failed",
                            e.to_string(),
                        );
                        continue;
                    }
                };
            let mut an = match to_memory::<AutobiographicalNode>(&grafeo_node) {
                Ok(node) => node,
                Err(e) => {
                    report.skip_with(labels::AUTOBIOGRAPHICAL, "field_mismatch", e.to_string());
                    continue;
                }
            };
            remap_one(&mut an.source_episode_id, &id_map, &mut report);
            remap_many(&mut an.source_episode_ids, &id_map, &mut report);
            an.id = None;
            let Ok(json) = serde_json::to_string(&an) else {
                report.skip(labels::AUTOBIOGRAPHICAL, "serialize_failed");
                continue;
            };
            let status = status_of(&props);
            let new_id = target.import_node(
                labels::AUTOBIOGRAPHICAL,
                &json,
                status,
                an.embedding.as_deref(),
            )?;
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

impl ImportReport {
    /// Record one node that stayed behind, and why.
    fn skip(&mut self, label: &str, reason: &str) {
        self.count_skip(&format!("{label}/{reason}"));
    }

    /// As [`Self::skip`], keeping the first failure message for that reason.
    fn skip_with(&mut self, label: &str, reason: &str, detail: String) {
        let key = format!("{label}/{reason}");
        self.skipped_examples.entry(key.clone()).or_insert(detail);
        self.count_skip(&key);
    }

    /// Record a structural property filled in for a legacy node.
    fn normalized(&mut self, key: String) {
        *self.normalized.entry(key).or_insert(0) += 1;
    }

    fn count_skip(&mut self, key: &str) {
        self.skipped += 1;
        *self.skipped_reasons.entry(key.to_string()).or_insert(0) += 1;
    }
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

/// Fill in the structural properties a node written by an older version may
/// predate, so an absent default cannot cost the node itself.
///
/// Deliberately narrow. `turn_index` is filled because nothing reads it (every
/// writer in the workspace sets `0`), and `status` is made parseable because the
/// reader already defaults an absent one. Anything that *does* carry meaning —
/// content, a timestamp, `consolidated` — still fails the conversion, because
/// inventing that is data loss wearing a default's clothes, and it has to show
/// up in the report instead.
fn normalize_props(props: &mut Vec<(String, Value)>, label: &str, report: &mut ImportReport) {
    if !props.iter().any(|(key, _)| key == "turn_index") {
        props.push(("turn_index".to_string(), Value::from(0i64)));
        report.normalized(format!("{label}/turn_index absent => 0"));
    }
    match props.iter().position(|(key, _)| key == "status") {
        None => {
            props.push(("status".to_string(), Value::from("Active")));
            report.normalized(format!("{label}/status absent => Active"));
        }
        Some(i) => {
            let seen = props[i].1.as_str().map(str::to_string);
            if !matches!(seen.as_deref(), Some("Active") | Some("Dormant")) {
                let seen = seen.unwrap_or_else(|| "<not a string>".to_string());
                // Not a status this enum knows. Dormant rather than Active: the
                // node is kept and stays out of retrieval, where promoting an
                // unknown lifecycle state into an active node would be a guess.
                props[i].1 = Value::from("Dormant");
                report.normalized(format!("{label}/status {seen} => Dormant"));
            }
        }
    }
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
fn to_memory<M: serde::de::DeserializeOwned>(
    grafeo_node: &impl serde::Serialize,
) -> serde_json::Result<M> {
    serde_json::from_value(serde_json::to_value(grafeo_node)?)
}

fn remap_one(slot: &mut Option<u64>, id_map: &HashMap<u64, u64>, report: &mut ImportReport) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn get<'a>(props: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
        props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The real store that prompted this: 151 of 286 episodes predate
    /// `turn_index`, and 12 procedural nodes carry a `Pending` status the enum
    /// cannot parse. Both used to make the node unreadable, and unreadable also
    /// meant unmigratable — the whole node was lost, not the field.
    #[test]
    fn legacy_nodes_are_normalized_rather_than_dropped() {
        let mut report = ImportReport::default();

        let mut episode = props(&[
            ("session_id", Value::from("s1")),
            (
                "content",
                Value::from("an episode from before turn_index existed"),
            ),
        ]);
        normalize_props(&mut episode, labels::EPISODIC, &mut report);
        assert_eq!(get(&episode, "turn_index"), Some(&Value::from(0i64)));
        assert_eq!(
            get(&episode, "status").and_then(Value::as_str),
            Some("Active")
        );

        let mut procedural = props(&[
            ("trigger", Value::from("run the tests")),
            ("status", Value::from("Pending")),
        ]);
        normalize_props(&mut procedural, labels::PROCEDURAL, &mut report);
        // Not promoted to Active: a lifecycle value the enum rejects must not
        // become an retrievable node just because it is being migrated.
        assert_eq!(
            get(&procedural, "status").and_then(Value::as_str),
            Some("Dormant")
        );

        assert_eq!(report.skipped, 0, "normalizing is not skipping");
        // One entry per filled field, per node: both nodes were missing
        // `turn_index`, and `status` was missing / unparseable.
        assert_eq!(report.normalized.len(), 4);
        assert!(
            report
                .normalized
                .contains_key("Procedural/status Pending => Dormant")
        );
    }

    /// The boundary: `normalize_props` fills structural fields only. Content is
    /// meaning, so it stays missing and the node still fails the conversion —
    /// which the report has to show.
    #[test]
    fn normalization_does_not_invent_meaning() {
        let mut report = ImportReport::default();
        let mut episode = props(&[("session_id", Value::from("s1"))]);
        normalize_props(&mut episode, labels::EPISODIC, &mut report);

        assert!(get(&episode, "content").is_none());
        assert!(get(&episode, "created_at").is_none());
        assert!(get(&episode, "consolidated").is_none());
    }
}
