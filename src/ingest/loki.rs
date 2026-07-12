//! Loki push API handler for log ingestion.
//!
//! Accepts JSON and snappy-compressed JSON log pushes.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use smallvec::SmallVec;

use crate::store::SharedState;
use crate::store::log_store::LogEntry;

const MAX_LOKI_LABELS_PER_STREAM: usize = 128;
const MAX_LOKI_STREAMS_PER_REQUEST: usize = 10_000;

/// Loki push JSON format.
#[derive(Debug, Deserialize)]
pub struct LokiPushRequest {
    pub streams: Vec<LokiStream>,
}

/// A single stream in the Loki push format.
///
/// Loki push values are arrays of `[timestamp_ns, line]` or
/// `[timestamp_ns, line, metadata]` where metadata is a JSON object of
/// structured per-entry key-value pairs. We accept both forms.
#[derive(Debug, Deserialize)]
pub struct LokiStream {
    pub stream: serde_json::Map<String, serde_json::Value>,
    #[serde(deserialize_with = "deserialize_log_values")]
    pub values: Vec<LokiValue>,
}

/// A single Loki log value: `(timestamp, line, optional metadata)`.
#[derive(Debug)]
pub struct LokiValue {
    pub ts: String,
    pub line: String,
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

fn deserialize_log_values<'de, D>(deserializer: D) -> Result<Vec<LokiValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Vec<Vec<serde_json::Value>> = Vec::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|entry| {
            if entry.len() >= 2 {
                let ts = match &entry[0] {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };
                let line = match &entry[1] {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let metadata = entry.get(2).and_then(|v| match v {
                    serde_json::Value::Object(map) => Some(map.clone()),
                    _ => None,
                });
                Some(LokiValue { ts, line, metadata })
            } else {
                None
            }
        })
        .collect())
}

/// Handler for POST /loki/api/v1/push.
pub async fn push_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    let request = if content_type.contains("application/x-protobuf")
        || content_type.contains("application/x-snappy")
    {
        // Snappy-compressed JSON path.
        // Loki clients typically send snappy-compressed JSON as application/x-protobuf.
        // Native Loki protobuf is NOT supported — if snappy-JSON decode fails,
        // report that clearly.
        match decode_snappy_json(&body) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "failed to decode Loki push (expected snappy-compressed JSON): {}",
                    e
                );
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("failed to decode Loki push: {}. Note: native Loki protobuf is not supported, use JSON or snappy-compressed JSON.", e)
                    })),
                ).into_response();
            }
        }
    } else {
        // JSON path
        if let Err(e) = super::ensure_body_size(&body) {
            tracing::warn!("Loki push body rejected: {}", e);
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        match serde_json::from_slice::<LokiPushRequest>(&body) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to parse Loki push JSON: {}", e);
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
    };

    if request.streams.len() > MAX_LOKI_STREAMS_PER_REQUEST {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("too many Loki streams: maximum is {}", MAX_LOKI_STREAMS_PER_REQUEST)
            })),
        )
            .into_response();
    }
    if request
        .streams
        .iter()
        .any(|stream| stream.stream.len() > MAX_LOKI_LABELS_PER_STREAM)
    {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("too many labels in Loki stream: maximum is {}", MAX_LOKI_LABELS_PER_STREAM)
            })),
        )
            .into_response();
    }

    let (streams, entries) = ingest_loki_push(&state, request);
    Json(serde_json::json!({
        "accepted": {
            "streams": streams,
            "entries": entries,
        }
    }))
    .into_response()
}

/// Decompress snappy body and parse as JSON.
///
/// Loki's native protobuf push format uses its own proto definitions (not OTLP).
/// We don't implement that -- this path only handles snappy-compressed JSON.
fn decode_snappy_json(body: &[u8]) -> Result<LokiPushRequest, anyhow::Error> {
    let decompressed = super::decode_snappy_body(body)
        .map_err(|e| anyhow::anyhow!("failed to decode snappy body: {}", e))?;

    serde_json::from_slice(&decompressed)
        .map_err(|e| anyhow::anyhow!("failed to parse decompressed push: {}", e))
}

/// Ingest a Loki push request. Returns `(stream_count, entry_count)`.
fn ingest_loki_push(state: &SharedState, request: LokiPushRequest) -> (usize, usize) {
    use crate::store::trace_store::AttributeValue;

    type RawMeta = Vec<(String, String)>;
    type StreamData = (Vec<(String, String)>, Vec<(LogEntry, RawMeta)>);
    let prepared: Vec<StreamData> = request
        .streams
        .into_iter()
        .map(|stream| {
            let labels: Vec<(String, String)> = stream
                .stream
                .into_iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, val)
                })
                .collect();
            let entries: Vec<(LogEntry, RawMeta)> = stream
                .values
                .into_iter()
                .filter_map(|val| {
                    let timestamp_ns: i64 = val.ts.parse().ok()?;
                    // Extract structured metadata: trace_id and span_id go
                    // to first-class fields, everything else becomes a
                    // per-entry string attribute.
                    let (trace_id, span_id, attrs) = val
                        .metadata
                        .as_ref()
                        .map(parse_loki_metadata)
                        .unwrap_or((None, None, Vec::new()));
                    Some((
                        LogEntry {
                            timestamp_ns,
                            line: val.line,
                            ingest_seq: state
                                .ingest_seq
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                            trace_id,
                            span_id,
                            severity_number: 0,
                            severity_text: None,
                            attributes: SmallVec::new(),
                        },
                        attrs,
                    ))
                })
                .collect();
            (labels, entries)
        })
        .collect();

    let stream_count = prepared.len();
    let entry_count: usize = prepared.iter().map(|(_, entries)| entries.len()).sum();

    let mut store = state.log_store.write();
    for (labels, mut entries) in prepared {
        for (entry, raw_attrs) in entries.iter_mut() {
            for (key, val) in raw_attrs.drain(..) {
                let key_spur = store.interner.get_or_intern(key);
                let attr_val = AttributeValue::String(store.interner.get_or_intern(val));
                entry.attributes.push((key_spur, attr_val));
            }
        }
        let entries: Vec<LogEntry> = entries.into_iter().map(|(e, _)| e).collect();
        store.ingest_stream(labels, entries);
    }

    (stream_count, entry_count)
}

/// Parsed Loki structured metadata: trace/span correlation IDs + remaining
/// key-value pairs stored as per-entry string attributes.
type ParsedMeta = (Option<[u8; 16]>, Option<[u8; 8]>, Vec<(String, String)>);

/// Parse Loki structured metadata into `(trace_id, span_id, attributes)`.
///
/// `trace_id` / `span_id` are hex-decoded if present and valid. All other
/// metadata entries become `(String, String)` pairs for per-entry attributes.
fn parse_loki_metadata(metadata: &serde_json::Map<String, serde_json::Value>) -> ParsedMeta {
    let mut trace_id = None;
    let mut span_id = None;
    let mut attrs = Vec::new();

    for (k, v) in metadata {
        let val_str = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match k.as_str() {
            "trace_id" | "traceId" => {
                trace_id = parse_hex_trace_id(&val_str);
            }
            "span_id" | "spanId" => {
                span_id = parse_hex_span_id(&val_str);
            }
            _ => attrs.push((k.clone(), val_str)),
        }
    }

    (trace_id, span_id, attrs)
}

fn parse_hex_trace_id(s: &str) -> Option<[u8; 16]> {
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

fn parse_hex_span_id(s: &str) -> Option<[u8; 8]> {
    let b = s.as_bytes();
    if b.len() != 16 {
        return None;
    }
    let mut bytes = [0u8; 8];
    for i in 0..8 {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(&b[i * 2..i * 2 + 2]).ok()?, 16).ok()?;
    }
    Some(bytes)
}
