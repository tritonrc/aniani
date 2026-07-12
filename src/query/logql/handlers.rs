//! Axum handlers for LogQL query endpoints.

use axum::Json;
use axum::extract::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use super::eval::{LogQLResult, ResolvedEntry, evaluate_logql_limited};
use super::parser::parse_logql;
use crate::store::SharedState;

/// Hint included in LogQL parse error responses to help agents construct valid queries.
const LOGQL_HINT: &str = r#"Example: {service="myapp", level="error"} |= "timeout""#;

/// Maximum number of steps allowed in a range query. Matches Prometheus default of 11,000.
const MAX_QUERY_STEPS: i64 = 11_000;
const DEFAULT_ENTRY_LIMIT: usize = 1000;
const MAX_ENTRY_LIMIT: usize = 10_000;

#[derive(Debug, Deserialize)]
pub struct QueryParams {
    pub query: String,
    pub time: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRangeParams {
    pub query: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub step: Option<String>,
    pub limit: Option<usize>,
}

/// GET /loki/api/v1/query
pub async fn query(
    State(state): State<SharedState>,
    Query(params): Query<QueryParams>,
) -> impl IntoResponse {
    query_inner(state, params).await
}

/// POST /loki/api/v1/query
pub async fn query_post(
    State(state): State<SharedState>,
    Form(params): Form<QueryParams>,
) -> impl IntoResponse {
    query_inner(state, params).await
}

async fn query_inner(state: SharedState, params: QueryParams) -> (StatusCode, Json<Value>) {
    let expr = match parse_logql(&params.query) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status": "error",
                    "error": e.to_string(),
                    "hint": LOGQL_HINT,
                })),
            );
        }
    };

    let now_ns = now_ns();
    let (start_ns, end_ns) = match params.time.as_deref() {
        Some(t) => match parse_timestamp_ns(t) {
            Some(ns) => (ns - 3_600_000_000_000, ns), // 1h lookback from specified time
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "error": format!("invalid time: {}", t)})),
                );
            }
        },
        None => (0, now_ns), // No time specified: search all data
    };

    let limit = bounded_limit(params.limit);
    let store = state.log_store.read();
    let result = evaluate_logql_limited(&expr, &store, start_ns, end_ns, None, Some(limit));

    (StatusCode::OK, Json(format_logql_result(result, limit)))
}

/// GET /loki/api/v1/query_range
pub async fn query_range(
    State(state): State<SharedState>,
    Query(params): Query<QueryRangeParams>,
) -> impl IntoResponse {
    query_range_inner(state, params).await
}

/// POST /loki/api/v1/query_range
pub async fn query_range_post(
    State(state): State<SharedState>,
    Form(params): Form<QueryRangeParams>,
) -> impl IntoResponse {
    query_range_inner(state, params).await
}

async fn query_range_inner(
    state: SharedState,
    params: QueryRangeParams,
) -> (StatusCode, Json<Value>) {
    let expr = match parse_logql(&params.query) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "status": "error",
                    "error": e.to_string(),
                    "hint": LOGQL_HINT,
                })),
            );
        }
    };

    let now_ns = now_ns();
    let start_ns = match params.start.as_deref() {
        Some(s) => match parse_timestamp_ns(s) {
            Some(ns) => ns,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "error": format!("invalid start: {}", s)})),
                );
            }
        },
        None => now_ns - 3_600_000_000_000,
    };
    let end_ns = match params.end.as_deref() {
        Some(s) => match parse_timestamp_ns(s) {
            Some(ns) => ns,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "error": format!("invalid end: {}", s)})),
                );
            }
        },
        None => now_ns,
    };
    let step_ns = match params.step.as_deref() {
        Some(s) => match crate::config::parse_duration(s).map(|d| {
            let ns = d.as_nanos();
            if ns > i64::MAX as u128 {
                i64::MAX
            } else {
                ns as i64
            }
        }) {
            Some(ns) => Some(ns),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"status": "error", "error": format!("invalid step: {}", s)})),
                );
            }
        },
        None => None,
    };

    if step_ns == Some(0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"status": "error", "error": "step must be positive"})),
        );
    }

    // Compute effective step for cap validation.
    // When step is omitted, the evaluator uses the query's range as the implicit step
    // for metric queries. Extract it from the AST to validate correctly.
    let effective_step_ns = match (&expr, step_ns) {
        (_, Some(s)) => Some(s),
        (super::parser::LogQLExpr::MetricQuery { range, .. }, None) => {
            let ns = range.as_nanos();
            Some(if ns > i64::MAX as u128 {
                i64::MAX
            } else {
                ns as i64
            })
        }
        _ => None, // Stream queries don't loop over steps
    };

    if let Some(step) = effective_step_ns
        && step > 0
    {
        let num_steps = end_ns.saturating_sub(start_ns).max(0) / step;
        if num_steps >= MAX_QUERY_STEPS {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"status": "error", "error": format!("query would produce {} steps, exceeding maximum of {}", num_steps, MAX_QUERY_STEPS)}),
                ),
            );
        }
    }

    let limit = bounded_limit(params.limit);
    let store = state.log_store.read();
    let result = evaluate_logql_limited(&expr, &store, start_ns, end_ns, step_ns, Some(limit));

    (StatusCode::OK, Json(format_logql_result(result, limit)))
}

/// GET /loki/api/v1/labels
pub async fn labels(State(state): State<SharedState>) -> impl IntoResponse {
    let store = state.log_store.read();
    let names = store.label_names();
    Json(json!({
        "status": "success",
        "data": names,
    }))
}

/// GET /loki/api/v1/label/:name/values
pub async fn label_values(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let store = state.log_store.read();
    let values = store.get_label_values(&name);
    Json(json!({
        "status": "success",
        "data": values,
    }))
}

fn format_logql_result(result: LogQLResult, limit: usize) -> Value {
    match result {
        LogQLResult::Streams(mut streams) => {
            // Apply limit globally across all streams, not per-stream.
            // Collect all (stream_idx, timestamp, line, trace_id) tuples, sort by
            // timestamp descending (newest first), take `limit`, then redistribute.
            let total_entries: usize = streams.iter().map(|s| s.entries.len()).sum();
            if total_entries > limit {
                let mut all: Vec<(usize, ResolvedEntry)> = streams
                    .iter_mut()
                    .enumerate()
                    .flat_map(|(idx, sr)| sr.entries.drain(..).map(move |entry| (idx, entry)))
                    .collect();
                all.sort_by_key(|b| std::cmp::Reverse(b.1.0)); // newest first
                all.truncate(limit);
                // Put entries back into their streams
                for (idx, entry) in all {
                    streams[idx].entries.push(entry);
                }
                // Re-sort each stream's entries by timestamp ascending
                for sr in &mut streams {
                    sr.entries.sort_by_key(|(ts, _, _, _, _)| *ts);
                }
            }

            let result_arr: Vec<Value> = streams
                .into_iter()
                .filter(|sr| !sr.entries.is_empty())
                .map(|sr| {
                    let labels_map: serde_json::Map<String, Value> = sr
                        .labels
                        .into_iter()
                        .map(|(k, v)| (k, Value::String(v)))
                        .collect();
                    // Loki structured-metadata shape: a 2-element array when there is
                    // no trace id (matches vanilla Loki), 3-element with a
                    // `{"trace_id": ...}` metadata object when there is one.
                    let values: Vec<Value> =
                        sr.entries
                            .into_iter()
                            .map(|(ts, line, trace_id, span_id, attrs)| {
                                let metadata: serde_json::Map<String, Value> = trace_id
                                    .iter()
                                    .map(|tid| ("trace_id".to_string(), Value::String(tid.clone())))
                                    .chain(span_id.iter().map(|sid| {
                                        ("span_id".to_string(), Value::String(sid.clone()))
                                    }))
                                    .chain(attrs.into_iter().map(|(k, v)| (k, Value::String(v))))
                                    .collect();
                                if metadata.is_empty() {
                                    json!([ts.to_string(), line])
                                } else {
                                    json!([ts.to_string(), line, metadata])
                                }
                            })
                            .collect();
                    json!({
                        "stream": labels_map,
                        "values": values,
                    })
                })
                .collect();
            json!({
                "status": "success",
                "data": {
                    "resultType": "streams",
                    "result": result_arr,
                }
            })
        }
        LogQLResult::Matrix(metrics) => {
            let result_arr: Vec<Value> = metrics
                .into_iter()
                .map(|mr| {
                    let labels_map: serde_json::Map<String, Value> = mr
                        .labels
                        .into_iter()
                        .map(|(k, v)| (k, Value::String(v)))
                        .collect();
                    let values: Vec<Value> = mr
                        .samples
                        .into_iter()
                        .map(|(ts, val)| json!([ts as f64 / 1_000_000_000.0, val.to_string()]))
                        .collect();
                    json!({
                        "metric": labels_map,
                        "values": values,
                    })
                })
                .collect();
            json!({
                "status": "success",
                "data": {
                    "resultType": "matrix",
                    "result": result_arr,
                }
            })
        }
    }
}

fn now_ns() -> i64 {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    if ns > i64::MAX as u128 {
        i64::MAX
    } else {
        ns as i64
    }
}

fn parse_timestamp_ns(s: &str) -> Option<i64> {
    crate::ingest::parse_timestamp_ns(s)
}

fn bounded_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_ENTRY_LIMIT).min(MAX_ENTRY_LIMIT)
}

#[cfg(test)]
mod tests;
