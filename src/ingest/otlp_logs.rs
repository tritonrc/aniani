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
use std::sync::atomic::Ordering;

use super::label::{extract_key_values, extract_resource_labels, promote_service_name};
use super::{decode_body, is_json_content_type, u64_to_i64_saturating};
use crate::store::log_store::LogEntry;
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
    type LogBatch = (Vec<(String, String)>, Vec<LogEntry>);
    let mut grouped: Vec<LogBatch> = Vec::new();
    let mut key_index: FxHashMap<Vec<(String, String)>, usize> = FxHashMap::default();

    for resource_logs in &request.resource_logs {
        let mut resource_labels = extract_resource_labels(&resource_logs.resource);
        promote_service_name(&mut resource_labels);

        for scope_logs in &resource_logs.scope_logs {
            for log_record in &scope_logs.log_records {
                let mut labels = resource_labels.clone();

                // Map severity number to a "level" label.
                let level = severity_to_level(log_record.severity_number);
                labels.push(("level".to_string(), level.to_string()));

                // Promote log record attributes to labels, mirroring the
                // metrics path, so they are queryable via LogQL label matchers
                // (and visible to describe_service). Skip keys that collide
                // with an already-set label (e.g. service/level/resource attrs).
                for (k, v) in extract_key_values(&log_record.attributes) {
                    if !labels.iter().any(|(ek, _)| *ek == k) {
                        labels.push((k, v));
                    }
                }

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
                    trace_id: trace_id_hex(&log_record.trace_id),
                };

                match key_index.get(&labels).copied() {
                    Some(idx) => grouped[idx].1.push(entry),
                    None => {
                        key_index.insert(labels.clone(), grouped.len());
                        grouped.push((labels, vec![entry]));
                    }
                }
            }
        }
    }

    // Acquire write lock and ingest.
    let entry_count: usize = grouped.iter().map(|(_, e)| e.len()).sum();
    let mut store = state.log_store.write();
    for (labels, entries) in grouped {
        store.ingest_stream(labels, entries);
    }

    tracing::debug!(entries = entry_count, "ingested OTLP logs");
    entry_count
}

/// Hex-encode a trace id from an OTLP log record.
///
/// Returns `None` for an empty id and for an all-zero id: OTLP allows all-zero
/// bytes but treats them as semantically "no trace" (the same convention the
/// trace ingestion path uses for absent parent span ids).
fn trace_id_hex(trace_id: &[u8]) -> Option<String> {
    use std::fmt::Write;

    if trace_id.is_empty() || trace_id.iter().all(|&b| b == 0) {
        return None;
    }
    let mut hex = String::with_capacity(trace_id.len() * 2);
    for b in trace_id {
        // Writing to a String is infallible; there's nothing to propagate.
        let _ = write!(hex, "{b:02x}");
    }
    Some(hex)
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
    fn log_record_attributes_become_queryable_labels() {
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
            // Collision with the severity-derived "level" label: must be ignored.
            KeyValue {
                key: "level".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("debug".into())),
                }),
            },
        ];
        ingest_logs(&st, req);

        let store = st.log_store.read();
        // The stream labels should now include http.method=POST (queryable via
        // LogQL label matchers) but keep level=info from the severity mapping.
        let labels: Vec<(String, String)> = store
            .streams
            .values()
            .next()
            .unwrap()
            .labels
            .iter()
            .map(|(k, v)| {
                (
                    store.interner.resolve(k).to_string(),
                    store.interner.resolve(v).to_string(),
                )
            })
            .collect();
        assert!(
            labels
                .iter()
                .any(|(k, v)| k == "http.method" && v == "POST")
        );
        assert!(labels.iter().any(|(k, v)| k == "level" && v == "info"));
        assert!(!labels.iter().any(|(k, v)| k == "level" && v == "debug"));
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
            entry.trace_id.as_deref(),
            Some("000102030405060708090a0b0c0d0e0f")
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
