//! On-demand snapshot endpoint.

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::snapshot::save_from_state_reported;
use crate::store::SharedState;

/// POST /api/v1/snapshot — write a snapshot of all stores to the configured
/// snapshot directory.
///
/// This is the cross-platform equivalent of the Unix-only `SIGUSR1` trigger, so
/// Windows (and any non-Unix) clients have a portable way to request an
/// on-demand snapshot. Returns the number of bytes written on success, or 500
/// with the error message on failure.
pub async fn snapshot(
    State(state): State<SharedState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let dir = PathBuf::from(&state.config.snapshot_dir);
    match save_from_state_reported(&state, &dir) {
        Ok(bytes) => Ok(Json(json!({ "status": "ok", "bytes": bytes }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "error": e.to_string() })),
        )),
    }
}
