//! Metadata endpoint for Grafana Prometheus datasource compatibility.
//!
//! `GET /api/v1/metadata` returns per-metric type/help/unit metadata, matching
//! the Prometheus API convention. Grafana calls this when configuring a
//! datasource to discover metric types.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::store::SharedState;

/// GET /api/v1/metadata
///
/// Returns metric metadata keyed by metric name. Each value is an array of
/// metadata objects (Prometheus allows multiple metadata entries per name for
/// different label sets; we return one entry per name).
pub async fn metadata(State(state): State<SharedState>) -> Json<Value> {
    let store = state.metric_store.read();

    let name_key = store.interner.get("__name__");
    let mut seen: rustc_hash::FxHashSet<lasso::Spur> = rustc_hash::FxHashSet::default();

    // Collect metric-name spurs from all series labels.
    if let Some(nk) = name_key {
        for series in store.series.values() {
            for (k, v) in &series.labels {
                if *k == nk {
                    seen.insert(*v);
                }
            }
        }
    }
    // Also include metric_metadata keys that may not yet have series (edge case).
    for spur in store.metric_metadata.keys() {
        seen.insert(*spur);
    }

    let mut map = serde_json::Map::new();
    for spur in &seen {
        let name = store.interner.resolve(spur).to_string();
        let md = store.metric_metadata.get(spur);
        let type_str = md
            .and_then(|m| m.metric_type)
            .map(|t| t.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let help = md.and_then(|m| m.help.as_deref()).unwrap_or("").to_string();
        let unit = md.and_then(|m| m.unit.as_deref()).unwrap_or("").to_string();
        map.insert(
            name,
            json!([{
                "type": type_str,
                "help": help,
                "unit": unit,
            }]),
        );
    }

    Json(json!({
        "status": "success",
        "data": Value::Object(map),
    }))
}
