//! Integration test: OTLP log ingest with a trace id -> LogQL query_range ->
//! verify the response carries structured-metadata `trace_id`, while
//! Loki-push entries (which never have a trace id) keep the plain 2-element
//! `[ts, line]` shape.

mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use helpers::make_state;
use http_body_util::BodyExt;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;
use serde_json::Value;
use tower::ServiceExt;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, json)
}

fn make_logs_request_with_trace_id(
    service_name: &str,
    body_text: &str,
    ts_ns: u64,
    trace_id: Vec<u8>,
) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".into(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(service_name.into())),
                    }),
                }],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            scope_logs: vec![ScopeLogs {
                scope: None,
                log_records: vec![LogRecord {
                    time_unix_nano: ts_ns,
                    observed_time_unix_nano: ts_ns,
                    severity_number: 9,
                    severity_text: String::new(),
                    body: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(body_text.into())),
                    }),
                    attributes: vec![],
                    dropped_attributes_count: 0,
                    flags: 0,
                    trace_id,
                    span_id: vec![],
                    event_name: String::new(),
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

#[tokio::test]
async fn test_otlp_log_trace_id_appears_in_query_range_response() {
    let state = make_state();
    let app = aniani::server::build_router(state);

    let ts_ns = 1_700_000_000_000_000_000u64;
    let trace_id: Vec<u8> = (0..16u8).collect(); // 000102...0f
    let payload =
        make_logs_request_with_trace_id("checkout", "charged card", ts_ns, trace_id.clone());

    let req = Request::builder()
        .method("POST")
        .uri("/v1/logs")
        .header("content-type", "application/x-protobuf")
        .body(Body::from(payload.encode_to_vec()))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let query = urlencoding::encode(r#"{service="checkout"}"#);
    let uri = format!(
        "/loki/api/v1/query_range?query={}&start={}&end={}",
        query,
        ts_ns - 1_000_000_000,
        ts_ns + 1_000_000_000
    );
    let req = Request::builder()
        .method("GET")
        .uri(&uri)
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let result = json["data"]["result"].as_array().unwrap();
    assert_eq!(result.len(), 1, "expected one stream, got: {json}");
    let values = result[0]["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    let entry = values[0].as_array().unwrap();
    assert_eq!(
        entry.len(),
        3,
        "OTLP entry with a trace id should be a 3-element array, got: {json}"
    );
    assert_eq!(entry[1], "charged card");
    let expected_hex: String = trace_id.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(entry[2]["trace_id"], expected_hex);
}

#[tokio::test]
async fn test_loki_push_entries_stay_two_element_alongside_otlp_trace_ids() {
    let state = make_state();
    let app = aniani::server::build_router(state);

    // OTLP log with a trace id.
    let ts_ns = 1_700_000_000_000_000_000u64;
    let trace_id = vec![0xabu8; 16];
    let payload = make_logs_request_with_trace_id("mixed", "otlp line", ts_ns, trace_id);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/logs")
        .header("content-type", "application/x-protobuf")
        .body(Body::from(payload.encode_to_vec()))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Loki push with no trace id, same service label so it's a distinct stream
    // (different label set) but queried via the same regex.
    let push_body = serde_json::json!({
        "streams": [{
            "stream": {"service": "mixed", "source": "loki"},
            "values": [[ts_ns.to_string(), "loki line"]]
        }]
    });
    let req = Request::builder()
        .method("POST")
        .uri("/loki/api/v1/push")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&push_body).unwrap()))
        .unwrap();
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let query = urlencoding::encode(r#"{service="mixed"}"#);
    let uri = format!(
        "/loki/api/v1/query_range?query={}&start={}&end={}",
        query,
        ts_ns - 1_000_000_000,
        ts_ns + 1_000_000_000
    );
    let req = Request::builder()
        .method("GET")
        .uri(&uri)
        .body(Body::empty())
        .unwrap();
    let (status, json) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);

    let result = json["data"]["result"].as_array().unwrap();
    assert_eq!(
        result.len(),
        2,
        "expected two distinct streams, got: {json}"
    );

    for stream in result {
        let values = stream["values"].as_array().unwrap();
        assert_eq!(values.len(), 1);
        let entry = values[0].as_array().unwrap();
        let line = entry[1].as_str().unwrap();
        if line == "loki line" {
            assert_eq!(
                entry.len(),
                2,
                "Loki-push entries must stay 2-element, got: {json}"
            );
        } else if line == "otlp line" {
            assert_eq!(
                entry.len(),
                3,
                "OTLP entry with a trace id should be 3-element, got: {json}"
            );
        } else {
            panic!("unexpected log line: {line}");
        }
    }
}
