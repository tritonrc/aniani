//! OTLP/HTTP traces ingestion handler.
//!
//! Decodes `ExportTraceServiceRequest` protobuf and stores spans.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use prost::Message;
use rustc_hash::FxHashSet;
use smallvec::SmallVec;
use std::sync::atomic::Ordering;

use super::label::extract_resource_labels;
use super::{decode_body, is_json_content_type, u64_to_i64_saturating};
use crate::store::trace_store::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink, SpanStatus};
use crate::store::{AppState, SharedState};

/// Accepted-count summary returned by [`ingest_traces`].
#[derive(Debug, Clone, Copy)]
pub struct TracesAccepted {
    pub traces: usize,
    pub spans: usize,
}

#[derive(Debug, Clone)]
enum PreparedAttributeValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Prepared (pre-intern) attribute pairs.
type PreparedAttrs = SmallVec<[(String, PreparedAttributeValue); 8]>;

#[derive(Debug, Clone)]
struct PreparedEvent {
    name: String,
    time_ns: i64,
    attributes: SmallVec<[(String, PreparedAttributeValue); 4]>,
}

#[derive(Debug, Clone)]
struct PreparedLink {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    trace_state: Option<String>,
    attributes: SmallVec<[(String, PreparedAttributeValue); 4]>,
    flags: u32,
}

#[derive(Debug, Clone)]
struct PreparedSpan {
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    name: String,
    service_name: String,
    start_time_ns: i64,
    duration_ns: i64,
    status: SpanStatus,
    status_message: Option<String>,
    kind: SpanKind,
    attributes: SmallVec<[(String, PreparedAttributeValue); 8]>,
    events: Vec<PreparedEvent>,
    links: Vec<PreparedLink>,
    ingest_seq: u64,
}

/// Handler for POST /v1/traces.
///
/// Accepts both protobuf (`application/x-protobuf`, default) and JSON
/// (`application/json`) encoded `ExportTraceServiceRequest` bodies.
pub async fn traces_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body = match decode_body(&headers, &body) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("failed to decode OTLP traces body: {}", e);
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    let request = if is_json_content_type(&headers) {
        match serde_json::from_slice::<ExportTraceServiceRequest>(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP traces JSON: {}", e);
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    } else {
        match ExportTraceServiceRequest::decode(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP traces: {}", e);
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    };

    let accepted = ingest_traces(&state, request);
    Json(serde_json::json!({
        "accepted": {
            "traces": accepted.traces,
            "spans": accepted.spans,
        }
    }))
    .into_response()
}

/// Ingest a decoded `ExportTraceServiceRequest` into the trace store.
///
/// Transport-agnostic: shared by the OTLP/HTTP handler and the OTLP/gRPC
/// service. Returns accepted trace/span counts.
pub fn ingest_traces(state: &AppState, request: ExportTraceServiceRequest) -> TracesAccepted {
    // Each batch pairs one resource group's prepared resource attributes with
    // its spans. Resource attributes are prepared once per group (not cloned
    // per span) and interned once under the write lock.
    let mut prepared_batches: Vec<(PreparedAttrs, Vec<PreparedSpan>)> = Vec::new();
    let mut trace_ids = FxHashSet::default();
    let mut total_spans: usize = 0;

    for resource_spans in &request.resource_spans {
        let resource_labels = extract_resource_labels(&resource_spans.resource);
        let service_name = resource_labels
            .iter()
            .find(|(k, _)| k == "service.name")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let resource_attrs: PreparedAttrs = resource_labels
            .iter()
            .map(|(k, v)| {
                let key = format!("resource.{}", k);
                let val = PreparedAttributeValue::String(v.clone());
                (key, val)
            })
            .collect();

        let mut spans = Vec::new();
        for scope_spans in &resource_spans.scope_spans {
            for otlp_span in &scope_spans.spans {
                let trace_id: [u8; 16] = match otlp_span.trace_id.as_slice().try_into() {
                    Ok(id) if id != [0u8; 16] => id,
                    _ => {
                        tracing::warn!(
                            "skipping span with invalid or zero trace_id (length: {})",
                            otlp_span.trace_id.len()
                        );
                        continue;
                    }
                };
                let span_id: [u8; 8] = match otlp_span.span_id.as_slice().try_into() {
                    Ok(id) if id != [0u8; 8] => id,
                    _ => {
                        tracing::warn!(
                            "skipping span with invalid or zero span_id (length: {})",
                            otlp_span.span_id.len()
                        );
                        continue;
                    }
                };
                let parent_span_id = if otlp_span.parent_span_id.is_empty()
                    || otlp_span.parent_span_id.iter().all(|&b| b == 0)
                {
                    None
                } else {
                    otlp_span.parent_span_id.as_slice().try_into().ok()
                };

                let status = match &otlp_span.status {
                    Some(s) => match s.code {
                        0 => SpanStatus::Unset,
                        1 => SpanStatus::Ok,
                        2 => SpanStatus::Error,
                        _ => SpanStatus::Unset,
                    },
                    None => SpanStatus::Unset,
                };
                let status_message = otlp_span
                    .status
                    .as_ref()
                    .map(|s| s.message.clone())
                    .filter(|m| !m.is_empty());

                let start_time_ns = u64_to_i64_saturating(otlp_span.start_time_unix_nano);
                let end_time_ns = u64_to_i64_saturating(otlp_span.end_time_unix_nano);
                let duration_ns = end_time_ns.saturating_sub(start_time_ns);
                let kind = SpanKind::from_otlp(otlp_span.kind);

                // Span-specific attributes only; resource attributes are added
                // once per resource group under the write lock.
                let mut attributes: SmallVec<[(String, PreparedAttributeValue); 8]> =
                    SmallVec::new();
                for attr in &otlp_span.attributes {
                    if let Some(val) = &attr.value
                        && let Some(av) = convert_any_value(val)
                    {
                        let key = format!("span.{}", attr.key);
                        attributes.push((key, av));
                    }
                }

                // Span events (timeline markers; exceptions arrive here). Event
                // attribute keys are kept raw — they are already namespaced
                // (e.g. `exception.message`) and the UI matches on them.
                let events: Vec<PreparedEvent> = otlp_span
                    .events
                    .iter()
                    .map(|ev| {
                        let attributes = ev
                            .attributes
                            .iter()
                            .filter_map(|attr| {
                                let av = convert_any_value(attr.value.as_ref()?)?;
                                Some((attr.key.clone(), av))
                            })
                            .collect();
                        PreparedEvent {
                            name: ev.name.clone(),
                            time_ns: u64_to_i64_saturating(ev.time_unix_nano),
                            attributes,
                        }
                    })
                    .collect();

                // Span links — cross-trace references. Invalid/zero trace or
                // span IDs are skipped (a link must identify a concrete span).
                let links: Vec<PreparedLink> = otlp_span
                    .links
                    .iter()
                    .filter_map(|link| {
                        let link_trace_id: [u8; 16] = link.trace_id.as_slice().try_into().ok()?;
                        let link_span_id: [u8; 8] = link.span_id.as_slice().try_into().ok()?;
                        if link_trace_id == [0u8; 16] || link_span_id == [0u8; 8] {
                            return None;
                        }
                        let attributes = link
                            .attributes
                            .iter()
                            .filter_map(|attr| {
                                let av = convert_any_value(attr.value.as_ref()?)?;
                                Some((attr.key.clone(), av))
                            })
                            .collect();
                        let trace_state = if link.trace_state.is_empty() {
                            None
                        } else {
                            Some(link.trace_state.clone())
                        };
                        Some(PreparedLink {
                            trace_id: link_trace_id,
                            span_id: link_span_id,
                            trace_state,
                            attributes,
                            flags: link.flags,
                        })
                    })
                    .collect();

                trace_ids.insert(trace_id);
                spans.push(PreparedSpan {
                    trace_id,
                    span_id,
                    parent_span_id,
                    name: otlp_span.name.clone(),
                    service_name: service_name.clone(),
                    start_time_ns,
                    duration_ns,
                    status,
                    status_message,
                    kind,
                    attributes,
                    events,
                    links,
                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                });
            }
        }

        total_spans += spans.len();
        prepared_batches.push((resource_attrs, spans));
    }

    let mut store = state.trace_store.write();
    for (resource_attrs, prepared_batch) in prepared_batches {
        // Intern resource attributes once for the whole batch.
        let resource_spurs: SmallVec<[(lasso::Spur, AttributeValue); 8]> = resource_attrs
            .into_iter()
            .map(|(key, value)| intern_attribute(&mut store, key, value))
            .collect();
        let spans: Vec<Span> = prepared_batch
            .into_iter()
            .map(|prepared| intern_prepared_span(&mut store, prepared, &resource_spurs))
            .collect();
        store.ingest_spans(spans);
    }

    TracesAccepted {
        traces: trace_ids.len(),
        spans: total_spans,
    }
}

fn convert_any_value(
    val: &opentelemetry_proto::tonic::common::v1::AnyValue,
) -> Option<PreparedAttributeValue> {
    match &val.value {
        Some(any_value::Value::StringValue(s)) => Some(PreparedAttributeValue::String(s.clone())),
        Some(any_value::Value::IntValue(i)) => Some(PreparedAttributeValue::Int(*i)),
        Some(any_value::Value::DoubleValue(f)) => Some(PreparedAttributeValue::Float(*f)),
        Some(any_value::Value::BoolValue(b)) => Some(PreparedAttributeValue::Bool(*b)),
        _ => None,
    }
}

fn intern_prepared_span(
    store: &mut crate::store::TraceStore,
    prepared: PreparedSpan,
    resource_spurs: &SmallVec<[(lasso::Spur, AttributeValue); 8]>,
) -> Span {
    let name = store.interner.get_or_intern(&prepared.name);
    let service_name = store.interner.get_or_intern(&prepared.service_name);
    // Resource attributes (interned once per resource group) are prepended; the
    // per-span clone is cheap Spur pairs (no String allocation). Span-specific
    // attributes are interned here.
    let mut attributes = resource_spurs.clone();
    attributes.extend(
        prepared
            .attributes
            .into_iter()
            .map(|(key, value)| intern_attribute(store, key, value)),
    );

    let events = prepared
        .events
        .into_iter()
        .map(|ev| SpanEvent {
            name: store.interner.get_or_intern(&ev.name),
            time_ns: ev.time_ns,
            attributes: ev
                .attributes
                .into_iter()
                .map(|(key, value)| intern_attribute(store, key, value))
                .collect(),
        })
        .collect();

    let links = prepared
        .links
        .into_iter()
        .map(|link| SpanLink {
            trace_id: link.trace_id,
            span_id: link.span_id,
            trace_state: link.trace_state.map(|s| store.interner.get_or_intern(s)),
            attributes: link
                .attributes
                .into_iter()
                .map(|(key, value)| intern_attribute(store, key, value))
                .collect(),
            flags: link.flags,
        })
        .collect();

    Span {
        trace_id: prepared.trace_id,
        span_id: prepared.span_id,
        parent_span_id: prepared.parent_span_id,
        name,
        service_name,
        start_time_ns: prepared.start_time_ns,
        duration_ns: prepared.duration_ns,
        status: prepared.status,
        status_message: prepared
            .status_message
            .map(|m| store.interner.get_or_intern(m)),
        kind: prepared.kind,
        attributes,
        events,
        links,
        ingest_seq: prepared.ingest_seq,
    }
}

/// Intern a single prepared attribute key/value pair against the store.
fn intern_attribute(
    store: &mut crate::store::TraceStore,
    key: String,
    value: PreparedAttributeValue,
) -> (lasso::Spur, AttributeValue) {
    let key = store.interner.get_or_intern(key);
    let value = match value {
        PreparedAttributeValue::String(s) => {
            AttributeValue::String(store.interner.get_or_intern(s))
        }
        PreparedAttributeValue::Int(i) => AttributeValue::Int(i),
        PreparedAttributeValue::Float(f) => AttributeValue::Float(f),
        PreparedAttributeValue::Bool(b) => AttributeValue::Bool(b),
    };
    (key, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::empty_test_state;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

    fn one_span_request(status: Option<Status>) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource::default()),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        status,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }
    #[test]
    fn status_message_is_captured_when_present() {
        let state = empty_test_state();
        let req = one_span_request(Some(Status {
            code: 2,
            message: "connection refused".into(),
        }));
        ingest_traces(&state, req);
        let store = state.trace_store.read();
        let spans = store.get_trace(&[1u8; 16]).unwrap();
        assert_eq!(spans[0].status, SpanStatus::Error);
        let msg = spans[0].status_message.expect("status message stored");
        assert_eq!(store.resolve(&msg), "connection refused");
    }

    #[test]
    fn status_message_is_none_when_empty() {
        let state = empty_test_state();
        let req = one_span_request(Some(Status {
            code: 2,
            message: String::new(),
        }));
        ingest_traces(&state, req);
        let store = state.trace_store.read();
        let spans = store.get_trace(&[1u8; 16]).unwrap();
        assert_eq!(spans[0].status, SpanStatus::Error);
        assert!(spans[0].status_message.is_none());
    }

    #[test]
    fn span_links_are_captured_when_present() {
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        use opentelemetry_proto::tonic::trace::v1::span::Link as OtlpLink;

        let state = empty_test_state();
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource::default()),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        links: vec![
                            OtlpLink {
                                trace_id: vec![0xaa; 16],
                                span_id: vec![0xbb; 8],
                                trace_state: "congo=1".into(),
                                attributes: vec![KeyValue {
                                    key: "link.cause".into(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue("retry".into())),
                                    }),
                                }],
                                flags: 1,
                                ..Default::default()
                            },
                            // Zero IDs must be skipped.
                            OtlpLink {
                                trace_id: vec![0; 16],
                                span_id: vec![0; 8],
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        ingest_traces(&state, req);
        let store = state.trace_store.read();
        let spans = store.get_trace(&[1u8; 16]).unwrap();
        assert_eq!(spans[0].links.len(), 1);
        let link = &spans[0].links[0];
        assert_eq!(link.trace_id, [0xaa; 16]);
        assert_eq!(link.span_id, [0xbb; 8]);
        assert_eq!(link.flags, 1);
        let ts = link.trace_state.expect("trace_state stored");
        assert_eq!(store.resolve(&ts), "congo=1");
        assert_eq!(link.attributes.len(), 1);
        let (k, v) = &link.attributes[0];
        assert_eq!(store.resolve(k), "link.cause");
        assert_eq!(store.resolve_attribute_value(v), "retry");
    }
}
