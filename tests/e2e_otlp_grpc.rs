//! E2E gRPC tests: ingest all three OTLP signals over gRPC against the
//! single-port (h2c-multiplexed) server, then query each surface over HTTP.
//!
//! These exercise the riskiest part of the gRPC support — REST (HTTP/1.1) and
//! gRPC (HTTP/2 cleartext) sharing one listener — with a real tonic client.

mod helpers;

use helpers::{make_gauge_request, make_logs_request, make_state, make_trace_request};
use opentelemetry_proto::tonic::collector::logs::v1::logs_service_client::LogsServiceClient;
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
use serde_json::Value;
use tonic::codec::CompressionEncoding;

async fn spawn_server(state: aniani::store::SharedState) -> (String, tokio::task::JoinHandle<()>) {
    let app = aniani::server::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (base, handle)
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[tokio::test]
async fn test_otlp_grpc_metrics_roundtrip() {
    let state = make_state();
    let (base, server) = spawn_server(state).await;

    let mut client = MetricsServiceClient::connect(base.clone())
        .await
        .expect("gRPC connect");
    client
        .export(make_gauge_request(
            "grpc-metrics-svc",
            "grpc_gauge",
            42.0,
            now_ns(),
        ))
        .await
        .expect("gRPC metrics export");

    let http = reqwest::Client::new();
    let json: Value = http
        .get(format!(
            "{}/api/v1/query?query={}",
            base,
            urlencoding::encode(r#"grpc_gauge{service="grpc-metrics-svc"}"#)
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["status"], "success");
    let val: f64 = json["data"]["result"][0]["value"][1]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((val - 42.0).abs() < 0.01);

    server.abort();
}

#[tokio::test]
async fn test_otlp_grpc_logs_roundtrip() {
    let state = make_state();
    let (base, server) = spawn_server(state).await;

    let mut client = LogsServiceClient::connect(base.clone())
        .await
        .expect("gRPC connect");
    client
        .export(make_logs_request(
            "grpc-logs-svc",
            "hello over grpc",
            9,
            now_ns(),
        ))
        .await
        .expect("gRPC logs export");

    let http = reqwest::Client::new();
    let json: Value = http
        .get(format!(
            "{}/loki/api/v1/query?query={}",
            base,
            urlencoding::encode(r#"{service="grpc-logs-svc"}"#)
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !json["data"]["result"].as_array().unwrap().is_empty(),
        "expected log results, got {json}"
    );

    server.abort();
}

#[tokio::test]
async fn test_otlp_grpc_traces_roundtrip() {
    let state = make_state();
    let (base, server) = spawn_server(state).await;

    let now = now_ns();
    let mut client = TraceServiceClient::connect(base.clone())
        .await
        .expect("gRPC connect");
    client
        .export(make_trace_request(
            "grpc-trace-svc",
            "grpc-span",
            &[0xab; 16],
            &[2; 8],
            now - 1_000_000_000,
            now,
            1,
        ))
        .await
        .expect("gRPC traces export");

    let http = reqwest::Client::new();
    let json: Value = http
        .get(format!(
            "{}/api/search?q={}",
            base,
            urlencoding::encode(r#"{ resource.service.name = "grpc-trace-svc" }"#)
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !json["traces"].as_array().unwrap().is_empty(),
        "expected trace results, got {json}"
    );

    server.abort();
}

#[tokio::test]
async fn test_unknown_http_path_returns_404() {
    let state = make_state();
    let (base, server) = spawn_server(state).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/no/such/path"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "unknown HTTP path should 404, not be captured by the gRPC catch-all"
    );

    server.abort();
}

#[tokio::test]
async fn test_otlp_grpc_gzip_compressed_request() {
    let state = make_state();
    let (base, server) = spawn_server(state).await;

    let mut client = MetricsServiceClient::connect(base.clone())
        .await
        .expect("gRPC connect")
        .send_compressed(CompressionEncoding::Gzip);
    client
        .export(make_gauge_request(
            "grpc-gzip-svc",
            "grpc_gzip_gauge",
            1.5,
            now_ns(),
        ))
        .await
        .expect("gRPC gzip export");

    let http = reqwest::Client::new();
    let json: Value = http
        .get(format!(
            "{}/api/v1/query?query={}",
            base,
            urlencoding::encode(r#"grpc_gzip_gauge{service="grpc-gzip-svc"}"#)
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(json["status"], "success");
    assert!(
        !json["data"]["result"].as_array().unwrap().is_empty(),
        "expected gzip-compressed metric to be ingested, got {json}"
    );

    server.abort();
}
