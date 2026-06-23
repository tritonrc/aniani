//! Integration test: span events (exceptions) and span kind survive the
//! round-trip from OTLP ingest through GET /api/traces/{traceID}.
//!
//! These two fields back the Jaeger-style trace view: `kind` annotates each
//! span (SERVER/CLIENT/…) and `events` carry timeline markers, including
//! exceptions recorded as an event named `exception`.

mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use helpers::make_state;
use http_body_util::BodyExt;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::span::Event;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use serde_json::Value;
use tower::ServiceExt;

fn str_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
    }
}

#[tokio::test]
async fn test_span_kind_and_exception_event_round_trip() {
    let state = make_state();
    let app = aniani::server::build_router(state);

    let trace_id: [u8; 16] = [0x0A; 16];
    let span_id: [u8; 8] = [0x0B; 8];

    // A SERVER span (kind = 2) that recorded an exception event.
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![str_attr("service.name", "gateway")],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![Span {
                    trace_id: trace_id.to_vec(),
                    span_id: span_id.to_vec(),
                    trace_state: String::new(),
                    parent_span_id: vec![],
                    name: "GET /checkout".into(),
                    kind: 2, // SPAN_KIND_SERVER
                    start_time_unix_nano: 1_000_000_000,
                    end_time_unix_nano: 1_500_000_000,
                    attributes: vec![str_attr("http.method", "GET")],
                    dropped_attributes_count: 0,
                    events: vec![Event {
                        time_unix_nano: 1_200_000_000,
                        name: "exception".into(),
                        attributes: vec![
                            str_attr("exception.type", "TimeoutError"),
                            str_attr("exception.message", "upstream timed out"),
                        ],
                        dropped_attributes_count: 0,
                    }],
                    dropped_events_count: 0,
                    links: vec![],
                    dropped_links_count: 0,
                    status: Some(Status {
                        message: String::new(),
                        code: 2, // STATUS_CODE_ERROR
                    }),
                    flags: 0,
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    };

    let req = Request::builder()
        .method("POST")
        .uri("/v1/traces")
        .header("content-type", "application/x-protobuf")
        .body(Body::from(request.encode_to_vec()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Fetch the trace and inspect the single span.
    let trace_hex = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
    let get = Request::builder()
        .method("GET")
        .uri(format!("/api/traces/{}", trace_hex))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(get).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let span = &json["batches"][0]["scopeSpans"][0]["spans"][0];

    // Kind is preserved as the OTLP integer (2 = SERVER), not the old hardcoded 1.
    assert_eq!(span["kind"], 2, "span kind should round-trip as SERVER");

    // The exception event is present with its attributes.
    let events = span["events"].as_array().expect("span should have events");
    assert_eq!(events.len(), 1, "expected one event");
    let event = &events[0];
    assert_eq!(event["name"].as_str().unwrap(), "exception");
    assert_eq!(event["timeUnixNano"].as_str().unwrap(), "1200000000");

    let ev_attrs = event["attributes"].as_array().unwrap();
    let find = |k: &str| {
        ev_attrs
            .iter()
            .find(|a| a["key"] == k)
            .map(|a| a["value"]["stringValue"].as_str().unwrap().to_string())
    };
    assert_eq!(find("exception.type").as_deref(), Some("TimeoutError"));
    assert_eq!(
        find("exception.message").as_deref(),
        Some("upstream timed out")
    );
}
