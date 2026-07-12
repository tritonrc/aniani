//! OTLP/HTTP logs ingestion handler.
//!
//! Decodes `ExportLogsServiceRequest` protobuf and stores log entries.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value;
use prost::Message;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::sync::atomic::Ordering;

use super::label::{extract_resource_labels, promote_service_name};
use super::{decode_body, is_json_content_type, u64_to_i64_saturating};
use crate::store::log_store::LogEntry;
use crate::store::trace_store::AttributeValue;
use crate::store::{AppState, SharedState};

/// Handler for POST /v1/logs.
pub async fn logs_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body = match decode_body(&headers, &body) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("failed to decode OTLP logs body: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    let request = if is_json_content_type(&headers) {
        match serde_json::from_slice::<ExportLogsServiceRequest>(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP logs JSON: {}", e);
                return StatusCode::BAD_REQUEST;
            }
        }
    } else {
        match ExportLogsServiceRequest::decode(body.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to decode OTLP logs: {}", e);
                return StatusCode::BAD_REQUEST;
            }
        }
    };

    ingest_logs(&state, request);
    StatusCode::NO_CONTENT
}

/// Ingest a decoded `ExportLogsServiceRequest` into the log store.
///
/// Transport-agnostic: shared by the OTLP/HTTP handler and the OTLP/gRPC
/// service. Returns the number of log entries ingested.
pub fn ingest_logs(state: &AppState, request: ExportLogsServiceRequest) -> usize {
    // Prepare all ingestion data outside the write lock. Group records that
    // share an identical label set so each stream receives one batched
    // `ingest_stream` call rather than N single-entry calls (which would make
    // the per-append sort check quadratic for a busy stream).
    //
    // Per-entry OTLP attributes are captured as typed data alongside each
    // entry (not promoted to stream labels), preventing cardinality explosion.
    type RawAttr = (String, PreparedLogAttr);
    type LogBatch = (
        Vec<(String, String)>,
        Vec<(LogEntry, Vec<RawAttr>, Option<String>)>,
    );
    let mut grouped: Vec<LogBatch> = Vec::new();
    let mut key_index: FxHashMap<Vec<(String, String)>, usize> = FxHashMap::default();

    for resource_logs in &request.resource_logs {
        let mut resource_labels = extract_resource_labels(&resource_logs.resource);
        promote_service_name(&mut resource_labels);

        for scope_logs in &resource_logs.scope_logs {
            for log_record in &scope_logs.log_records {
                // Stream labels: resource attrs + severity-derived level only.
                // Per-entry attributes are NOT promoted (avoids cardinality
                // explosion).
                let mut labels = resource_labels.clone();
                let level = severity_to_level(log_record.severity_number);
                labels.push(("level".to_string(), level.to_string()));

                // Capture per-entry typed attributes.
                let raw_attrs: Vec<RawAttr> = log_record
                    .attributes
                    .iter()
                    .filter_map(|kv| convert_any_value(&kv.value).map(|v| (kv.key.clone(), v)))
                    .collect();

                // Extract the log line from the body field.
                let line = match &log_record.body {
                    Some(val) => any_value_to_string(val),
                    None => String::new(),
                };

                let timestamp_ns = if log_record.time_unix_nano == 0 {
                    current_time_ns()
                } else {
                    u64_to_i64_saturating(log_record.time_unix_nano)
                };

                let entry = LogEntry {
                    timestamp_ns,
                    line,
                    ingest_seq: state.ingest_seq.fetch_add(1, Ordering::Relaxed),
                    trace_id: trace_id_bytes(&log_record.trace_id),
                    span_id: span_id_bytes(&log_record.span_id),
                    severity_number: log_record.severity_number,
                    severity_text: None,
                    attributes: SmallVec::new(),
                };

                // Capture severity_text for interning inside the lock.
                let sev_text = if log_record.severity_text.is_empty() {
                    None
                } else {
                    Some(log_record.severity_text.clone())
                };

                match key_index.get(&labels).copied() {
                    Some(idx) => grouped[idx].1.push((entry, raw_attrs, sev_text)),
                    None => {
                        key_index.insert(labels.clone(), grouped.len());
                        grouped.push((labels, vec![(entry, raw_attrs, sev_text)]));
                    }
                }
            }
        }
    }

    // Acquire write lock and ingest. Intern per-entry attributes inside the
    // lock since the interner lives behind the RwLock.
    let entry_count: usize = grouped.iter().map(|(_, e)| e.len()).sum();
    let mut store = state.log_store.write();
    for (labels, mut entries) in grouped {
        for (entry, raw_attrs, sev_text) in entries.iter_mut() {
            for (key, val) in raw_attrs.drain(..) {
                let key_spur = store.interner.get_or_intern(key);
                let attr_val = match val {
                    PreparedLogAttr::String(s) => {
                        AttributeValue::String(store.interner.get_or_intern(s))
                    }
                    PreparedLogAttr::Int(i) => AttributeValue::Int(i),
                    PreparedLogAttr::Float(f) => AttributeValue::Float(f),
                    PreparedLogAttr::Bool(b) => AttributeValue::Bool(b),
                };
                entry.attributes.push((key_spur, attr_val));
            }
            if let Some(text) = sev_text.take() {
                entry.severity_text = Some(store.interner.get_or_intern(text));
            }
        }
        let entries: Vec<LogEntry> = entries.into_iter().map(|(e, _, _)| e).collect();
        store.ingest_stream(labels, entries);
    }

    tracing::debug!(entries = entry_count, "ingested OTLP logs");
    entry_count
}

/// Extract a 16-byte trace id from an OTLP log record.
///
/// Returns `None` for an empty id, an all-zero id, or an id that is not
/// exactly 16 bytes (malformed). OTLP allows all-zero bytes but treats them
/// as semantically "no trace" (same convention as trace ingestion).
fn trace_id_bytes(trace_id: &[u8]) -> Option<[u8; 16]> {
    if trace_id.len() == 16 && !trace_id.iter().all(|&b| b == 0) {
        let mut buf = [0u8; 16];
        buf.copy_from_slice(trace_id);
        Some(buf)
    } else {
        None
    }
}

/// Extract an 8-byte span id from an OTLP log record.
///
/// Returns `None` for empty, all-zero, or wrong-length span ids.
fn span_id_bytes(span_id: &[u8]) -> Option<[u8; 8]> {
    if span_id.len() == 8 && !span_id.iter().all(|&b| b == 0) {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(span_id);
        Some(buf)
    } else {
        None
    }
}

fn current_time_ns() -> i64 {
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

/// Map OTLP severity number to a human-readable level string.
fn severity_to_level(severity_number: i32) -> &'static str {
    match severity_number {
        1..=4 => "trace",
        5..=8 => "debug",
        9..=12 => "info",
        13..=16 => "warn",
        17..=20 => "error",
        21..=24 => "fatal",
        _ => "unknown",
    }
}

/// Owned representation of an OTLP attribute value for pre-lock preparation.
/// Interned into `AttributeValue` inside the write lock.
enum PreparedLogAttr {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Convert an OTLP `AnyValue` into a `PreparedLogAttr`. Returns `None` for
/// complex types (Array, Kvlist, Bytes) — these are stringified into the log
/// `line` instead of being stored as structured attributes.
fn convert_any_value(
    val: &Option<opentelemetry_proto::tonic::common::v1::AnyValue>,
) -> Option<PreparedLogAttr> {
    let val = val.as_ref()?;
    match &val.value {
        Some(any_value::Value::StringValue(s)) => Some(PreparedLogAttr::String(s.clone())),
        Some(any_value::Value::IntValue(i)) => Some(PreparedLogAttr::Int(*i)),
        Some(any_value::Value::DoubleValue(f)) => Some(PreparedLogAttr::Float(*f)),
        Some(any_value::Value::BoolValue(b)) => Some(PreparedLogAttr::Bool(*b)),
        // Complex types are stringified into the body, not stored as attrs.
        _ => None,
    }
}

/// Convert an AnyValue to its string representation.
///
/// Primitive types are converted directly. Complex types (arrays, key-value lists,
/// bytes) are serialized as JSON so log lines are never silently empty.
fn any_value_to_string(val: &opentelemetry_proto::tonic::common::v1::AnyValue) -> String {
    match &val.value {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(any_value::Value::IntValue(i)) => i.to_string(),
        Some(any_value::Value::DoubleValue(f)) => f.to_string(),
        Some(any_value::Value::BoolValue(b)) => b.to_string(),
        Some(any_value::Value::BytesValue(bytes)) => format!("<bytes len={}>", bytes.len()),
        Some(any_value::Value::ArrayValue(arr)) => {
            serde_json::to_string(&arr).unwrap_or_else(|_| "<array>".to_string())
        }
        Some(any_value::Value::KvlistValue(kvlist)) => {
            serde_json::to_string(&kvlist).unwrap_or_else(|_| "<kvlist>".to_string())
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod ingest_seq_tests {
    use super::*;
    use crate::store::{AppState, LogStore, MetricStore, TraceStore};
    use clap::Parser;
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    fn state() -> Arc<AppState> {
        Arc::new(AppState {
            log_store: RwLock::new(LogStore::new()),
            metric_store: RwLock::new(MetricStore::new()),
            trace_store: RwLock::new(TraceStore::new()),
            config: crate::config::Config::parse_from(["aniani"]),
            start_time: Instant::now(),
            ingest_seq: AtomicU64::new(0),
        })
    }

    fn one_log(msg: &str) -> ExportLogsServiceRequest {
        one_log_with_trace_id(msg, vec![])
    }

    fn one_log_with_trace_id(msg: &str, trace_id: Vec<u8>) -> ExportLogsServiceRequest {
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    schema_url: String::new(),
                    log_records: vec![LogRecord {
                        time_unix_nano: 1,
                        observed_time_unix_nano: 0,
                        severity_number: 9,
                        severity_text: "INFO".into(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(msg.into())),
                        }),
                        attributes: vec![],
                        dropped_attributes_count: 0,
                        flags: 0,
                        trace_id,
                        span_id: vec![],
                        event_name: String::new(),
                    }],
                }],
                schema_url: String::new(),
            }],
        }
    }

    #[test]
    fn ingested_entries_carry_increasing_ingest_seq() {
        let st = state();
        ingest_logs(&st, one_log("a"));
        ingest_logs(&st, one_log("b"));
        assert_eq!(st.ingest_seq.load(Ordering::Relaxed), 2);
        let store = st.log_store.read();
        let mut seqs: Vec<u64> = store
            .streams
            .values()
            .flat_map(|s| s.entries.iter().map(|e| e.ingest_seq))
            .collect();
        seqs.sort();
        assert_eq!(seqs, vec![0, 1]);
    }

    #[test]
    fn log_record_attributes_stored_per_entry_not_as_labels() {
        use crate::store::trace_store::AttributeValue;
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        let st = state();
        let mut req = one_log("boom");
        req.resource_logs[0].scope_logs[0].log_records[0].attributes = vec![
            KeyValue {
                key: "http.method".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("POST".into())),
                }),
            },
            KeyValue {
                key: "status_code".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::IntValue(500)),
                }),
            },
        ];
        ingest_logs(&st, req);

        let store = st.log_store.read();
        let stream = store.streams.values().next().unwrap();

        // Stream labels should include only resource attrs + level — NOT
        // per-entry attributes like http.method or status_code.
        let labels: Vec<(String, String)> = stream
            .labels
            .iter()
            .map(|(k, v)| {
                (
                    store.interner.resolve(k).to_string(),
                    store.interner.resolve(v).to_string(),
                )
            })
            .collect();
        assert!(!labels.iter().any(|(k, _)| k == "http.method"));
        assert!(!labels.iter().any(|(k, _)| k == "status_code"));
        assert!(labels.iter().any(|(k, v)| k == "level" && v == "info"));

        // Per-entry attributes should carry the typed values.
        let entry = &stream.entries[0];
        let get_attr = |key: &str| -> Option<&AttributeValue> {
            entry
                .attributes
                .iter()
                .find(|(k, _)| store.interner.resolve(k) == key)
                .map(|(_, v)| v)
        };
        match get_attr("http.method") {
            Some(AttributeValue::String(s)) => {
                assert_eq!(store.interner.resolve(s), "POST");
            }
            other => panic!("expected String attribute, got {other:?}"),
        }
        match get_attr("status_code") {
            Some(AttributeValue::Int(i)) => assert_eq!(*i, 500),
            other => panic!("expected Int attribute, got {other:?}"),
        }
    }

    #[test]
    fn log_record_trace_id_is_hex_encoded_onto_the_entry() {
        let st = state();
        let trace_id: Vec<u8> = (0..16u8).collect(); // 000102...0f
        ingest_logs(&st, one_log_with_trace_id("boom", trace_id));

        let store = st.log_store.read();
        let entry = store
            .streams
            .values()
            .next()
            .unwrap()
            .entries
            .first()
            .unwrap();
        assert_eq!(
            entry.trace_id,
            Some([
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f
            ])
        );
    }

    #[test]
    fn log_record_without_trace_id_leaves_it_none() {
        let st = state();
        ingest_logs(&st, one_log("no trace"));

        let store = st.log_store.read();
        let entry = store
            .streams
            .values()
            .next()
            .unwrap()
            .entries
            .first()
            .unwrap();
        assert_eq!(entry.trace_id, None);
    }

    #[test]
    fn log_record_all_zero_trace_id_is_treated_as_absent() {
        let st = state();
        ingest_logs(&st, one_log_with_trace_id("zeroed", vec![0u8; 16]));

        let store = st.log_store.read();
        let entry = store
            .streams
            .values()
            .next()
            .unwrap()
            .entries
            .first()
            .unwrap();
        assert_eq!(entry.trace_id, None);
    }
}
