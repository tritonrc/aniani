//! Axum handlers for TraceQL query endpoints.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use super::eval::evaluate_traceql;
use super::parser::parse_traceql;
use crate::store::SharedState;
use crate::store::TraceStore;
use crate::store::trace_store::{AttributeValue, SpanStatus};

/// Hint included in TraceQL parse error responses to help agents construct valid queries.
const TRACEQL_HINT: &str = "Example: { resource.service.name = \"myapp\" && status = error }";

/// Default limit of traces returned when no query is provided.
const DEFAULT_RECENT_TRACES_LIMIT: usize = 20;
const MAX_TRACE_SEARCH_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    /// Optional start time filter (epoch seconds or nanoseconds).
    pub start: Option<u64>,
    /// Optional end time filter (epoch seconds or nanoseconds).
    pub end: Option<u64>,
    /// Maximum number of traces to return. Defaults to 20 when no query.
    pub limit: Option<usize>,
}

/// Convert a timestamp parameter to nanoseconds.
///
/// Accept epoch seconds, milliseconds, or nanoseconds using magnitude ranges.
fn param_to_ns(val: u64) -> i64 {
    if val < 100_000_000_000 {
        (val as i64).saturating_mul(1_000_000_000)
    } else if val < 100_000_000_000_000 {
        (val as i64).saturating_mul(1_000_000)
    } else if val > i64::MAX as u64 {
        i64::MAX
    } else {
        val as i64
    }
}

/// GET /api/search
pub async fn search(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let store = state.trace_store.read();

    // Convert start/end from epoch seconds (or nanoseconds) for filtering.
    let start_ns = params.start.map(param_to_ns);
    let end_ns = params.end.map(param_to_ns);

    let q = params.q.unwrap_or_default();
    if q.is_empty() {
        // No query: return recent traces, optionally filtered by time range.
        let limit = bounded_limit(params.limit, DEFAULT_RECENT_TRACES_LIMIT);
        let mut recent = if start_ns.is_some() || end_ns.is_some() {
            store.recent_traces(store.traces.len())
        } else {
            store.recent_traces(limit)
        };

        // Apply time range filter
        if start_ns.is_some() || end_ns.is_some() {
            recent.retain(|tr| {
                let trace_end_ns = tr.start_time_ns.saturating_add(tr.duration_ns);
                let after_start = start_ns.is_none_or(|st| trace_end_ns >= st);
                let before_end = end_ns.is_none_or(|en| tr.start_time_ns <= en);
                after_start && before_end
            });
        }

        recent.truncate(limit);

        let traces: Vec<Value> = recent
            .iter()
            .map(|tr| {
                json!({
                    "traceID": hex_encode(&tr.trace_id),
                    "rootServiceName": tr.root_service_name,
                    "rootTraceName": tr.root_span_name,
                    "startTimeUnixNano": tr.start_time_ns.to_string(),
                    "durationMs": tr.duration_ns / 1_000_000,
                    "errorCount": tr.error_count,
                    "spanSets": [],
                })
            })
            .collect();

        return (StatusCode::OK, Json(json!({"traces": traces})));
    }

    let expr = match parse_traceql(&q) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e.to_string(), "hint": TRACEQL_HINT})),
            );
        }
    };

    let results = evaluate_traceql(&expr, &store);

    // Filter traces by time range: keep only those with at least one span
    // whose time window overlaps [start_ns, end_ns].
    let mut results: Vec<_> = results
        .into_iter()
        .filter(|r| {
            r.matched_spans.iter().any(|s| {
                let span_end = s.start_time_ns.saturating_add(s.duration_ns);
                let after_start = start_ns.is_none_or(|st| span_end >= st);
                let before_end = end_ns.is_none_or(|en| s.start_time_ns <= en);
                after_start && before_end
            })
        })
        .collect();

    results.truncate(bounded_limit(params.limit, MAX_TRACE_SEARCH_LIMIT));

    let traces: Vec<Value> = results
        .iter()
        .map(|r| {
            let trace_id = hex::encode_upper(&r.trace_id);
            // Find the actual root span from the trace store, not the first matched span
            let root_info = store.trace_result(&r.trace_id);
            let (root_service, root_name, root_start_ns, root_duration_ns, error_count) =
                match &root_info {
                    Some(tr) => (
                        tr.root_service_name.as_str(),
                        tr.root_span_name.as_str(),
                        tr.start_time_ns,
                        tr.duration_ns,
                        tr.error_count,
                    ),
                    None => {
                        // Fallback to first matched span if trace_result unavailable
                        let first = r.matched_spans.first();
                        (
                            first.map(|s| s.service_name.as_str()).unwrap_or(""),
                            first.map(|s| s.name.as_str()).unwrap_or(""),
                            first.map(|s| s.start_time_ns).unwrap_or(0),
                            first.map(|s| s.duration_ns).unwrap_or(0),
                            r.matched_spans
                                .iter()
                                .filter(|s| s.status == SpanStatus::Error)
                                .count(),
                        )
                    }
                };
            json!({
                "traceID": trace_id.to_lowercase(),
                "rootServiceName": root_service,
                "rootTraceName": root_name,
                "startTimeUnixNano": root_start_ns.to_string(),
                "durationMs": root_duration_ns / 1_000_000,
                "errorCount": error_count,
                "spanSets": [{
                    "spans": r.matched_spans.iter().map(|s| {
                        json!({
                            "spanID": hex_encode(&s.span_id),
                            "name": s.name,
                            "serviceName": s.service_name,
                            "startTimeUnixNano": s.start_time_ns.to_string(),
                            "durationNanos": s.duration_ns.to_string(),
                            "status": match s.status {
                                SpanStatus::Ok => "ok",
                                SpanStatus::Error => "error",
                                SpanStatus::Unset => "unset",
                            },
                        })
                    }).collect::<Vec<Value>>(),
                    "matched": r.matched_spans.len(),
                }],
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "traces": traces,
        })),
    )
}

/// GET /api/traces/:trace_id
pub async fn get_trace(
    State(state): State<SharedState>,
    Path(trace_id_str): Path<String>,
) -> impl IntoResponse {
    let trace_id = match parse_trace_id(&trace_id_str) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid trace ID"})),
            );
        }
    };

    let store = state.trace_store.read();
    match store.get_trace(&trace_id) {
        Some(spans) => {
            // Group spans by service_name, preserving insertion order via Vec
            let mut service_order: Vec<String> = Vec::new();
            let mut service_spans: std::collections::HashMap<String, Vec<Value>> =
                std::collections::HashMap::new();

            for span in spans {
                let service_name = store.resolve(&span.service_name).to_string();
                let attrs: Vec<Value> = span
                    .attributes
                    .iter()
                    .map(|(k, v)| attribute_json(&store, k, v))
                    .collect();

                let events: Vec<Value> = span
                    .events
                    .iter()
                    .map(|ev| {
                        let ev_attrs: Vec<Value> = ev
                            .attributes
                            .iter()
                            .map(|(k, v)| attribute_json(&store, k, v))
                            .collect();
                        json!({
                            "name": store.resolve(&ev.name),
                            "timeUnixNano": ev.time_ns.to_string(),
                            "attributes": ev_attrs,
                        })
                    })
                    .collect();

                let span_value = json!({
                    "traceId": hex_encode(&span.trace_id),
                    "spanId": hex_encode(&span.span_id),
                    "parentSpanId": span.parent_span_id.as_ref().map(|p| hex_encode(p)).unwrap_or_default(),
                    "name": store.resolve(&span.name),
                    "kind": span.kind.as_otlp(),
                    "startTimeUnixNano": span.start_time_ns.to_string(),
                    "endTimeUnixNano": span.start_time_ns.saturating_add(span.duration_ns).to_string(),
                    "status": {
                        "code": match span.status {
                            SpanStatus::Unset => 0,
                            SpanStatus::Ok => 1,
                            SpanStatus::Error => 2,
                        }
                    },
                    "attributes": attrs,
                    "events": events,
                });

                if !service_spans.contains_key(&service_name) {
                    service_order.push(service_name.clone());
                }
                service_spans
                    .entry(service_name)
                    .or_default()
                    .push(span_value);
            }

            let batches: Vec<Value> = service_order
                .iter()
                .map(|svc| {
                    json!({
                        "resource": {
                            "attributes": [{
                                "key": "service.name",
                                "value": {"stringValue": svc}
                            }]
                        },
                        "scopeSpans": [{
                            "spans": service_spans.get(svc).unwrap_or(&Vec::new()),
                        }]
                    })
                })
                .collect();

            (
                StatusCode::OK,
                Json(json!({
                    "batches": batches
                })),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "trace not found"})),
        ),
    }
}

/// Render a single interned attribute as an OTLP-shaped `{key, value}` JSON object.
///
/// All values are emitted under `stringValue` (typed Int/Float/Bool are
/// stringified). This is intentional and matches the long-standing span-attribute
/// shape the web UI consumes; the typed value is preserved losslessly in the store.
fn attribute_json(store: &TraceStore, key: &lasso::Spur, value: &AttributeValue) -> Value {
    json!({
        "key": store.resolve(key),
        "value": {
            "stringValue": store.resolve_attribute_value(value),
        }
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

fn parse_trace_id(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    // Operate on bytes to avoid panicking on multi-byte UTF-8 that crosses
    // an even byte boundary (str slicing requires char boundaries).
    let b = s.as_bytes();
    if b.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(&b[i * 2..i * 2 + 2]).ok()?, 16).ok()?;
    }
    Some(bytes)
}

fn bounded_limit(limit: Option<usize>, default: usize) -> usize {
    limit.unwrap_or(default).min(MAX_TRACE_SEARCH_LIMIT)
}

// Inline hex encoding module (avoid dependency)
mod hex {
    pub fn encode_upper(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state;
    use crate::store::trace_store::{Span, SpanKind};
    use smallvec::SmallVec;

    fn insert_trace_with_one_error(state: &crate::store::SharedState, trace_id: [u8; 16]) {
        let mut store = state.trace_store.write();
        let root_name = store.interner.get_or_intern("root");
        let child_name = store.interner.get_or_intern("child");
        let service = store.interner.get_or_intern("svc");
        store.ingest_spans(vec![
            Span {
                trace_id,
                span_id: [1u8; 8],
                parent_span_id: None,
                name: root_name,
                service_name: service,
                start_time_ns: 1000,
                duration_ns: 500,
                status: SpanStatus::Ok,
                kind: SpanKind::Unspecified,
                attributes: SmallVec::new(),
                events: Vec::new(),
                ingest_seq: 0,
            },
            Span {
                trace_id,
                span_id: [2u8; 8],
                parent_span_id: Some([1u8; 8]),
                name: child_name,
                service_name: service,
                start_time_ns: 1100,
                duration_ns: 100,
                status: SpanStatus::Error,
                kind: SpanKind::Unspecified,
                attributes: SmallVec::new(),
                events: Vec::new(),
                ingest_seq: 0,
            },
        ]);
    }

    async fn search_json(
        state: crate::store::SharedState,
        params: SearchParams,
    ) -> (StatusCode, Value) {
        use http_body_util::BodyExt;

        let response = search(State(state), Query(params)).await.into_response();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn test_search_no_query_includes_error_count() {
        let state = empty_test_state();
        insert_trace_with_one_error(&state, [1u8; 16]);

        let (status, body) = search_json(
            state,
            SearchParams {
                q: None,
                start: None,
                end: None,
                limit: None,
            },
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["traces"][0]["errorCount"], json!(1));
    }

    #[tokio::test]
    async fn test_search_with_query_includes_error_count() {
        let state = empty_test_state();
        insert_trace_with_one_error(&state, [2u8; 16]);

        let (status, body) = search_json(
            state,
            SearchParams {
                q: Some("{ status = error }".to_string()),
                start: None,
                end: None,
                limit: None,
            },
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["traces"][0]["errorCount"], json!(1));
    }

    #[test]
    fn test_param_to_ns_saturates_huge_nanosecond_value() {
        assert_eq!(param_to_ns(u64::MAX), i64::MAX);
    }

    #[test]
    fn test_bounded_limit_caps_untrusted_limit() {
        assert_eq!(
            bounded_limit(None, DEFAULT_RECENT_TRACES_LIMIT),
            DEFAULT_RECENT_TRACES_LIMIT
        );
        assert_eq!(
            bounded_limit(
                Some(MAX_TRACE_SEARCH_LIMIT + 1),
                DEFAULT_RECENT_TRACES_LIMIT
            ),
            MAX_TRACE_SEARCH_LIMIT
        );
    }

    #[test]
    fn test_parse_trace_id_rejects_multibyte_utf8_without_panicking() {
        // 32-byte string where a multi-byte char crosses an even byte
        // boundary — previously panicked on str slicing.
        let crafted: String = "a".to_string() + &"à".repeat(15) + "a";
        assert_eq!(crafted.len(), 32);
        let result = std::panic::catch_unwind(|| parse_trace_id(&crafted));
        assert!(result.is_ok(), "parse_trace_id must not panic");
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_parse_trace_id_accepts_valid_hex() {
        let id = parse_trace_id("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(id[0], 0x00);
        assert_eq!(id[15], 0xff);
    }
}
