//! LPG graph operations for GrafeoStore.

use std::sync::Arc;

use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_core::graph::Direction;
use grafeo_core::graph::lpg::{Edge, Node};

use crate::error::Result;
use crate::grafeo::GrafeoStore;

impl GrafeoStore {
    /// Create a node with the given label and properties.
    ///
    /// An `embedding` property is inserted into the label's vector index: the
    /// engine never updates indexes on the node-write path, so without this a
    /// freshly written vector is invisible to `vector_search` until the index
    /// is rebuilt (see [`crate::index_persist`]).
    ///
    /// Returns the newly created [`NodeId`].
    pub fn store_node<'a>(
        &self,
        label: &str,
        properties: impl IntoIterator<Item = (&'a str, Value)>,
    ) -> Result<NodeId> {
        let mut embedded: Option<(&'a str, Arc<[f32]>)> = None;
        let props: Vec<(&'a str, Value)> = properties
            .into_iter()
            .map(|(k, v)| {
                if embedded.is_none()
                    && let Value::Vector(vec) = &v
                {
                    embedded = Some((k, Arc::clone(vec)));
                }
                (k, v)
            })
            .collect();
        let id = self.db.create_node_with_props(&[label], props);
        if let Some((property, vector)) = embedded {
            crate::index_persist::insert_vector(&self.db, label, property, id, &vector);
        }
        Ok(id)
    }

    /// Create an edge between two nodes.
    ///
    /// Returns the newly created [`EdgeId`].
    pub fn store_edge<'a>(
        &self,
        src: NodeId,
        dst: NodeId,
        edge_type: &str,
        properties: impl IntoIterator<Item = (&'a str, Value)>,
    ) -> Result<EdgeId> {
        let id = self
            .db
            .create_edge_with_props(src, dst, edge_type, properties);
        Ok(id)
    }

    /// Get a node by ID.
    ///
    /// Returns `None` if the node does not exist.
    pub fn get_node(&self, node_id: NodeId) -> Option<Node> {
        self.db.get_node(node_id)
    }

    /// Get all edges connected to a node in the given direction.
    ///
    /// Direction can be [`Direction::Outgoing`], [`Direction::Incoming`], or
    /// [`Direction::Both`].
    pub fn get_edges(&self, node_id: NodeId, direction: Direction) -> Vec<Edge> {
        let graph = self.db.graph_store();
        let edge_refs = graph.edges_from(node_id, direction);
        edge_refs
            .into_iter()
            .filter_map(|(_, edge_id)| self.db.get_edge(edge_id))
            .collect()
    }

    /// Update (merge) properties on an existing node.
    ///
    /// Existing properties are overwritten; missing properties are left untouched.
    pub fn update_node<'a>(
        &self,
        node_id: NodeId,
        properties: impl IntoIterator<Item = (&'a str, Value)>,
    ) -> Result<()> {
        for (key, value) in properties {
            self.set_node_property(node_id, key, value);
        }
        Ok(())
    }

    /// Set a single node property.
    ///
    /// Use this instead of `db().set_node_property` whenever the value may be an
    /// embedding: the engine's write path never updates vector indexes, so a
    /// vector written directly is invisible to `vector_search` until a rebuild.
    /// Non-vector values are unaffected (the sync is a no-op).
    pub fn set_node_property(&self, node_id: NodeId, key: &str, value: Value) {
        self.db.set_node_property(node_id, key, value.clone());
        crate::index_persist::sync_written_property(&self.db, node_id, key, &value);
    }

    /// Delete a node and all of its edges.
    ///
    /// Also drops the node from every vector index — the engine's delete path
    /// does not touch indexes.
    ///
    /// Returns `true` if the node existed and was deleted.
    pub fn delete_node(&self, node_id: NodeId) -> Result<bool> {
        let deleted = self.db.delete_node(node_id);
        if deleted {
            crate::index_persist::remove_vector(&self.db, node_id);
        }
        Ok(deleted)
    }
}
