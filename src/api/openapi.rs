//! OpenAPI 3.0 specification endpoint.

mod helpers;
mod paths;
mod schemas;

#[cfg(test)]
mod tests;

use axum::Json;
use serde_json::{Map, Value, json};

use paths::build_paths;
use schemas::build_components;

/// GET /api/v1/openapi.json — returns the OpenAPI 3.0.3 specification for all Aniani endpoints.
pub async fn openapi_spec() -> Json<Value> {
    Json(spec())
}

/// Build the static OpenAPI 3.0.3 document.
fn spec() -> Value {
    let mut doc = Map::new();
    doc.insert("openapi".into(), json!("3.0.3"));
    doc.insert(
        "info".into(),
        json!({
            "title": "Aniani",
            "description": "Lightweight ephemeral observability engine exposing LogQL, PromQL, and TraceQL query surfaces over in-memory stores.",
            "version": "0.7.2"
        }),
    );
    doc.insert("paths".into(), Value::Object(build_paths()));
    doc.insert("components".into(), build_components());

    Value::Object(doc)
}
