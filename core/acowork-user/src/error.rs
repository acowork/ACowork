//! acowork-user unified HTTP error type.
//!
//! [`ApiError`] is the wire shape every handler returns on failure. It is
//! carried over unchanged from the Gateway (`http/routes.rs`) so the
//! Desktop's error handling sees byte-identical bodies across the
//! extraction (ADR-084 §3 goal 4) — same field names, same `code`
//! semantics, same `structured` skip.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use acowork_core::error_codes::StructuredErrorBody;
use serde::{Deserialize, Serialize};

/// Standard API error response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    pub code: u16,
    /// ADR-059 §6.3: structured protocol error body for mutation APIs.
    /// Absent for plain HTTP-layer errors; present for
    /// `resource_version_conflict` so clients can retry without parsing
    /// human-readable text.
    ///
    /// Boxed to keep the `Err` variant of every handler signature small
    /// (`result_large_err`) — same reasoning as the Gateway's copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<Box<StructuredErrorBody>>,
}

/// The wire shape is the source of truth; logging needs only the code and
/// the human-readable message, never the JSON body.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.error, self.code)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}

impl ApiError {
    pub fn not_found(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 404,
            structured: None,
        }
    }

    pub fn bad_request(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 400,
            structured: None,
        }
    }

    pub fn unauthorized(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 401,
            structured: None,
        }
    }

    /// 403 — authenticated but not permitted (e.g. a non-admin calling an
    /// admin-only account-management route).
    pub fn forbidden(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 403,
            structured: None,
        }
    }

    pub fn conflict(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 409,
            structured: None,
        }
    }

    /// 409 carrying a structured body (ADR-059 §6.3).
    pub fn conflict_structured(body: StructuredErrorBody) -> Self {
        Self {
            error: format!("{:?}", body.code),
            code: 409,
            structured: Some(Box::new(body)),
        }
    }

    pub fn unprocessable_entity(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 422,
            structured: None,
        }
    }

    /// 413 — the body is too large for the endpoint's own ceiling (e.g. an
    /// attachment over ADR-076 §决策 9's image/document limits).
    pub fn payload_too_large(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 413,
            structured: None,
        }
    }

    pub fn internal(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 500,
            structured: None,
        }
    }

    pub fn service_unavailable(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: 503,
            structured: None,
        }
    }
}
