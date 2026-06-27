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
    let resp = reqwest::Client::new().get(format!("{base}/mcp")).send().await.unwrap();
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
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(body["result"]["serverInfo"]["name"], "aniani");
    assert!(body["result"]["instructions"].is_string());
}
