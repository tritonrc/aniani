//! End-to-end MCP tests over a real listener.

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use aniani::config::Config;
use aniani::store::{AppState, LogStore, MetricStore, SharedState, TraceStore};
use clap::Parser;
use parking_lot::RwLock;

async fn spawn() -> String {
    let state: SharedState = Arc::new(AppState {
        log_store: RwLock::new(LogStore::new()),
        metric_store: RwLock::new(MetricStore::new()),
        trace_store: RwLock::new(TraceStore::new()),
        config: Config::parse_from(["aniani"]),
        start_time: Instant::now(),
        ingest_seq: AtomicU64::new(0),
    });
    let app = aniani::server::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn get_mcp_returns_405() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/mcp"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

#[tokio::test]
async fn initialize_handshake() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                      "params":{"protocolVersion":"2025-11-25","capabilities":{},
                                "clientInfo":{"name":"test","version":"0"}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(body["result"]["serverInfo"]["name"], "aniani");
    assert!(body["result"]["instructions"].is_string());
}

/// Invoke a tool over HTTP and return the parsed JSON-RPC response.
async fn call_tool(base: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                      "params":{"name":name,"arguments":args}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn push_loki_error(base: &str, service: &str, line: &str) {
    reqwest::Client::new()
        .post(format!("{base}/loki/api/v1/push"))
        .json(
            &json!({"streams":[{"stream":{"service":service,"level":"error"},
              "values":[["1", line]]}]}),
        )
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn full_loop_reset_ingest_summarize() {
    let base = spawn().await;
    // reset for a clean baseline
    let r = call_tool(&base, "reset", json!({"scope":"all"})).await;
    assert_eq!(r["result"]["isError"], json!(false));
    // ingest a Loki error log directly via the REST push API
    push_loki_error(&base, "api", "kaboom").await;
    // summarize_activity reflects it
    let s = call_tool(&base, "summarize_activity", json!({"service":"api"})).await;
    assert_eq!(
        s["result"]["structuredContent"]["logs"]["error_count"],
        json!(1)
    );
    // reset clears: the service is no longer known, so summarize self-corrects
    // with an isError result (per the §5 unknown-service contract).
    call_tool(&base, "reset", json!({"scope":"all"})).await;
    let s2 = call_tool(&base, "summarize_activity", json!({"service":"api"})).await;
    assert_eq!(s2["result"]["isError"], json!(true));
}

#[tokio::test]
async fn since_uses_ingest_order_not_event_time() {
    let base = spawn().await;
    // mark a checkpoint against the empty store
    let cp = call_tool(&base, "mark_checkpoint", json!({})).await;
    let token = cp["result"]["structuredContent"]["checkpoint"]
        .as_u64()
        .unwrap();
    // ingest an error whose EVENT timestamp (1ns) predates "now" but which
    // ARRIVES after the checkpoint mark
    push_loki_error(&base, "api", "late but mine").await;
    // since=token still includes it (ingest-seq based, not event-time based)
    let s = call_tool(
        &base,
        "summarize_activity",
        json!({"service":"api","since": token}),
    )
    .await;
    assert_eq!(
        s["result"]["structuredContent"]["logs"]["error_count"],
        json!(1)
    );
}

#[tokio::test]
async fn notification_returns_202() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    assert!(resp.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn batch_array_rejected() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn foreign_origin_forbidden() {
    let base = spawn().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .header("origin", "https://evil.example.com")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}
