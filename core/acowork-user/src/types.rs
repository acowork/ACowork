//! Shared DTOs for the user service.

use acowork_core::error_codes::StructuredErrorBody;
use acowork_core::operation::{OperationId, OperationRecord, OperationState};
use serde::Serialize;

/// ADR-059 §7.3/§7.4 — unified mutation ack returned by operation-bearing
/// write APIs. Carried over unchanged from the Gateway so the Desktop reads
/// the same shape across the extraction.
#[derive(Debug, Clone, Serialize)]
pub struct OperationAck {
    pub operation_id: OperationId,
    pub state: OperationState,
    pub resource_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<StructuredErrorBody>,
    /// ADR-073: instance identity created by this operation. Always `None`
    /// for the user domain (no operation here creates an agent instance);
    /// the field is kept so the wire shape matches the Gateway's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

impl OperationAck {
    pub fn from_record(record: &OperationRecord) -> Self {
        Self {
            operation_id: record.operation_id.clone(),
            state: record.state,
            resource_version: record.resource_version,
            terminal_error: record.terminal_error.clone(),
            instance_id: None,
        }
    }
}
